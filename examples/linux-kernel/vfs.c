// SPDX-License-Identifier: GPL-2.0
#include <linux/fs_context.h>
#include <linux/fs_parser.h>
#include <linux/highmem.h>
#include <linux/inet.h>
#include <linux/module.h>
#include <linux/mount.h>
#include <linux/namei.h>
#include <linux/nsproxy.h>
#include <linux/pagemap.h>
#include <linux/seq_file.h>
#include <linux/slab.h>

#include "gitfs.h"

static const struct inode_operations gitfs_dir_ops;
static const struct file_operations gitfs_dir_fops;

void __noreturn gitfs_codec_panic(void)
{
	BUG();
}

static int gitfs_read_folio(struct file *file, struct folio *folio)
{
	struct inode *inode = folio->mapping->host;
	struct gitfs_inode *node = inode->i_private;
	struct gitfs_mount *mount = inode->i_sb->s_fs_info;
	struct gitfs_object *object;
	loff_t position = folio_pos(folio);
	size_t copied = 0;
	int error = 0;

	mutex_lock(&mount->lock);
	object = gitfs_object_get(mount, node->id);
	if (IS_ERR(object)) {
		error = PTR_ERR(object);
	} else if (object->kind != node->kind ||
		   object->length != node->offset + i_size_read(inode)) {
		error = -EBADMSG;
	} else if (position < i_size_read(inode)) {
		copied = min_t(size_t, folio_size(folio), i_size_read(inode) - position);
		memcpy_to_folio(folio, 0, object->data + node->offset + position, copied);
	}
	mutex_unlock(&mount->lock);
	folio_zero_range(folio, copied, folio_size(folio) - copied);
	folio_end_read(folio, !error);
	return error;
}

static const struct address_space_operations gitfs_aops = {
	.read_folio = gitfs_read_folio,
};

static const struct file_operations gitfs_file_fops = {
	.llseek = generic_file_llseek,
	.read_iter = generic_file_read_iter,
	.mmap = generic_file_readonly_mmap,
	.splice_read = filemap_splice_read,
};

static const struct inode_operations gitfs_symlink_ops = {
	.get_link = simple_get_link,
};

int gitfs_checkout_fetch(void *context, const u8 *id, struct gitfs_object_slice *result)
{
	struct gitfs_object *object = gitfs_object_get(context, id);

	if (IS_ERR(object))
		return PTR_ERR(object);
	result->data = object->data;
	result->length = object->length;
	result->kind = object->kind;
	return 0;
}

struct inode *gitfs_alloc_inode(struct super_block *sb)
{
	struct gitfs_mount *mount = sb->s_fs_info;
	struct gitfs_inode *node;
	struct inode *inode;

	lockdep_assert_held(&mount->lock);
	inode = new_inode(sb);
	if (!inode)
		return ERR_PTR(-ENOMEM);
	node = kzalloc(sizeof(*node), GFP_KERNEL);
	if (!node) {
		iput(inode);
		return ERR_PTR(-ENOMEM);
	}
	inode->i_private = node;
	inode->i_ino = atomic64_inc_return(&mount->next_ino);
	inode->i_uid = GLOBAL_ROOT_UID;
	inode->i_gid = GLOBAL_ROOT_GID;
	inode_set_atime(inode, 0, 0);
	inode_set_mtime(inode, 0, 0);
	inode_set_ctime(inode, 0, 0);
	return inode;
}

struct inode *gitfs_new_inode(struct super_block *sb, const u8 *id, u32 mode)
{
	struct gitfs_mount *mount = sb->s_fs_info;
	struct inode *inode = gitfs_alloc_inode(sb);
	struct gitfs_inode *node;
	struct gitfs_object *object;
	struct gitfs_decoded message;
	int error;

	if (IS_ERR(inode))
		return inode;
	node = inode->i_private;
	memcpy(node->id, id, 20);
	node->kind = mode == GITFS_MESSAGE ? GITFS_COMMIT : GITFS_BLOB;
	if (mode == 040000 || mode == 0160000) {
		node->gitlink = mode == 0160000;
		inode->i_mode = S_IFDIR | 0555;
		inode->i_op = &gitfs_dir_ops;
		inode->i_fop = &gitfs_dir_fops;
		set_nlink(inode, 2);
		return inode;
	}
	object = gitfs_object_get(mount, id);
	if (IS_ERR(object)) {
		error = PTR_ERR(object);
		goto fail;
	}
	if (mode == GITFS_MESSAGE) {
		if (object->kind != GITFS_COMMIT) {
			error = -EBADMSG;
			goto fail;
		}
		error = gitfs_commit_message(object->data, object->length, &message);
	} else {
		error = gitfs_content_info(object->data, object->length, object->kind, mode, &message);
	}
	if (error)
		goto fail;
	node->offset = message.offset;
	i_size_write(inode, message.length);
	inode->i_blocks = (i_size_read(inode) + 511) >> 9;
	if (mode == 0120000) {
		if (!object->length || object->length >= PATH_MAX) {
			error = -EBADMSG;
			goto fail;
		}
		inode->i_link = kmemdup_nul(object->data, object->length, GFP_KERNEL);
		if (!inode->i_link) {
			error = -ENOMEM;
			goto fail;
		}
		inode->i_mode = S_IFLNK | 0777;
		inode->i_op = &gitfs_symlink_ops;
	} else {
		inode->i_mode = S_IFREG | (mode == 0100755 ? 0555 : 0444);
		inode->i_fop = &gitfs_file_fops;
		inode->i_mapping->a_ops = &gitfs_aops;
	}
	return inode;
fail:
	iput(inode);
	return ERR_PTR(error);
}

static struct dentry *gitfs_lookup(struct inode *dir, struct dentry *dentry,
				 unsigned int flags)
{
	struct gitfs_mount *mount = dir->i_sb->s_fs_info;
	struct gitfs_inode *node = dir->i_private;
	struct gitfs_object *tree;
	struct gitfs_tree_entry found;
	struct inode *inode = NULL;
	int error = 0;

	mutex_lock(&mount->lock);
	if (node->gitlink)
		goto out;
	tree = gitfs_object_get(mount, node->id);
	if (IS_ERR(tree)) {
		error = PTR_ERR(tree);
		goto out;
	}
	if (tree->kind != GITFS_TREE) {
		error = -EBADMSG;
		goto out;
	}
	error = gitfs_tree_lookup(tree->data, tree->length, dentry->d_name.name,
				  dentry->d_name.len, &found);
	if (error > 0) {
		error = 0;
		/* Copy the ID: fetching the blob can evict the parent tree. */
		inode = gitfs_new_inode(dir->i_sb, found.id, found.mode);
		if (IS_ERR(inode)) {
			error = PTR_ERR(inode);
			inode = NULL;
		}
	}
out:
	mutex_unlock(&mount->lock);
	if (error < 0)
		return ERR_PTR(error);
	/* An absent entry becomes a negative dentry; no object GET is attempted. */
	return d_splice_alias(inode, dentry);
}

static int gitfs_readdir(struct file *file, struct dir_context *ctx)
{
	struct inode *dir = file_inode(file);
	struct gitfs_mount *mount = dir->i_sb->s_fs_info;
	struct gitfs_inode *node = dir->i_private;
	struct gitfs_object *tree;
	struct gitfs_tree_entry entry;
	size_t offset = 0;
	loff_t index = 2;
	unsigned int type;
	int error = 0;

	if (!dir_emit_dots(file, ctx) || node->gitlink)
		return 0;
	mutex_lock(&mount->lock);
	tree = gitfs_object_get(mount, node->id);
	if (IS_ERR(tree)) {
		error = PTR_ERR(tree);
		goto out;
	}
	if (tree->kind != GITFS_TREE) {
		error = -EBADMSG;
		goto out;
	}
	while ((error = gitfs_tree_next(tree->data, tree->length, &offset, &entry)) > 0) {
		if (index++ < ctx->pos)
			continue;
		type = entry.mode == 040000 || entry.mode == 0160000 ? DT_DIR :
		       entry.mode == 0120000 ? DT_LNK : DT_REG;
		/* Unknown inode number: lookup assigns a unique number per path. */
		if (!dir_emit(ctx, entry.name, entry.name_length, 0, type)) {
			error = 0;
			break;
		}
		ctx->pos = index;
	}
out:
	mutex_unlock(&mount->lock);
	return error < 0 ? error : 0;
}

static const struct inode_operations gitfs_dir_ops = {
	.lookup = gitfs_lookup,
};

static const struct file_operations gitfs_dir_fops = {
	.read = generic_read_dir,
	.iterate_shared = gitfs_readdir,
	.llseek = generic_file_llseek,
};

static void gitfs_evict_inode(struct inode *inode)
{
	truncate_inode_pages_final(&inode->i_data);
	clear_inode(inode);
}

static void gitfs_free_inode(struct inode *inode)
{
	/* i_link shares storage with directory sequence state. RCU readers can
	 * still observe symlink targets until this deferred callback executes.
	 */
	if (S_ISLNK(inode->i_mode))
		kfree(inode->i_link);
	kfree(inode->i_private);
	free_inode_nonrcu(inode);
}

static int gitfs_show_options(struct seq_file *seq, struct dentry *root)
{
	struct gitfs_mount *mount = root->d_sb->s_fs_info;
	char hex[41];

	if (!mount->ref_view && !mount->reference[0]) {
		bin2hex(hex, mount->commit, 20);
		hex[40] = 0;
		seq_printf(seq, ",commit=%s", hex);
	}
	seq_printf(seq, ",cache_mb=%zu,object_mb=%zu,timeout_ms=%u,error_ttl_ms=%u",
		   mount->cache_limit >> 20, mount->object_limit >> 20,
		   jiffies_to_msecs(mount->timeout), jiffies_to_msecs(mount->error_ttl));
	if (mount->smart)
		seq_printf(seq, ",smart,pack_mb=%zu", mount->pack_limit >> 20);
	if (mount->ref_view)
		seq_puts(seq, ",view=refs");
	else
		seq_puts(seq, ",view=checkout");
	if (mount->reference[0])
		seq_show_option(seq, "ref", mount->reference);
	return 0;
}

static const struct super_operations gitfs_super_ops = {
	.statfs = simple_statfs,
	.drop_inode = generic_delete_inode,
	.evict_inode = gitfs_evict_inode,
	.free_inode = gitfs_free_inode,
	.show_options = gitfs_show_options,
};

static int gitfs_reference_get(void *context, const u8 *name, size_t name_length,
                               u8 *response, size_t capacity, size_t *length)
{
	char *path = kmemdup_nul(name, name_length, GFP_KERNEL);
	int error;

	if (!path)
		return -ENOMEM;
	error = gitfs_http_get(context, path, response, capacity, length);
	kfree(path);
	return error;
}

static int gitfs_resolve_head(struct gitfs_mount *mount)
{
	const char *name = mount->reference[0] ? mount->reference : "HEAD";
	u8 *response = kmalloc(GITFS_URL_MAX, GFP_KERNEL);
	u8 *path = kmalloc(GITFS_URL_MAX * 3, GFP_KERNEL);
	int error;

	if (!response || !path)
		error = -ENOMEM;
	else
		error = gitfs_loose_reference(mount, gitfs_reference_get, name, strlen(name),
			response, GITFS_URL_MAX, path, GITFS_URL_MAX * 3, mount->commit);
	kfree(response);
	kfree(path);
	return error;
}

static int gitfs_fill_super(struct super_block *sb, struct fs_context *fc)
{
	/* sget_fc has already transferred ownership from fc to the superblock. */
	struct gitfs_mount *mount = sb->s_fs_info;
	struct gitfs_checkout_root root;
	struct inode *inode;
	int error = 0;

	sb->s_flags |= SB_RDONLY | SB_NOATIME;
	sb->s_magic = GITFS_MAGIC;
	sb->s_blocksize = PAGE_SIZE;
	sb->s_blocksize_bits = PAGE_SHIFT;
	sb->s_maxbytes = mount->object_limit;
	sb->s_op = &gitfs_super_ops;
	sb->s_time_gran = 1;
	/* Enable normal page-cache readahead, reclaim and fadvise behavior. */
	error = super_setup_bdi(sb);
	if (error)
		return error;
	mutex_lock(&mount->lock);
	if (mount->smart) {
		error = gitfs_fetch_snapshot(mount);
		if (error)
			goto out;
	} else if (!mount->explicit_commit) {
		error = gitfs_resolve_head(mount);
		if (error)
			goto out;
	}
	if (mount->ref_view) {
		error = gitfs_refs_prepare(mount);
		if (error)
			goto out;
		inode = gitfs_refs_inode(sb, mount->ref_root);
		goto make_root;
	}
	error = gitfs_resolve_checkout(mount, gitfs_checkout_fetch, mount->commit, &root);
	if (error)
		goto out;
	if (root.mode != 040000 || (mount->explicit_commit &&
	    (!root.message || memcmp(root.id, mount->commit, 20)))) {
		error = -EBADMSG;
		goto out;
	}
	inode = gitfs_new_inode(sb, root.tree, root.mode);
make_root:
	if (IS_ERR(inode)) {
		error = PTR_ERR(inode);
		goto out;
	}
	sb->s_root = d_make_root(inode);
	if (!sb->s_root)
		error = -ENOMEM;
out:
	mutex_unlock(&mount->lock);
	return error;
}

enum { Opt_commit, Opt_cache, Opt_object, Opt_timeout, Opt_error_ttl,
       Opt_smart, Opt_pack, Opt_dns, Opt_address, Opt_refs, Opt_checkout, Opt_view, Opt_ref };

static const struct fs_parameter_spec gitfs_parameters[] = {
	fsparam_string("commit", Opt_commit),
	fsparam_u32("cache_mb", Opt_cache),
	fsparam_u32("object_mb", Opt_object),
	fsparam_u32("timeout_ms", Opt_timeout),
	fsparam_u32("error_ttl_ms", Opt_error_ttl),
	fsparam_flag("smart", Opt_smart),
	fsparam_u32("pack_mb", Opt_pack),
	fsparam_string("dns", Opt_dns),
	fsparam_string("addr", Opt_address),
	fsparam_flag("refs", Opt_refs),
	fsparam_flag("checkout", Opt_checkout),
	fsparam_string("view", Opt_view),
	fsparam_string("ref", Opt_ref),
	{}
};

static int gitfs_set_view(struct gitfs_mount *mount, u32 view)
{
	if (mount->view != GITFS_VIEW_DEFAULT && mount->view != view)
		return -EINVAL;
	mount->view = view;
	return 0;
}

static int gitfs_parse_param(struct fs_context *fc, struct fs_parameter *param)
{
	struct gitfs_mount *mount = fc->s_fs_info;
	struct fs_parse_result result;
	int option = fs_parse(fc, gitfs_parameters, param, &result);

	if (fc->purpose == FS_CONTEXT_FOR_RECONFIGURE)
		return -EOPNOTSUPP;
	if (option == -ENOPARAM)
		return vfs_parse_fs_param_source(fc, param);
	if (option < 0)
		return option;
	switch (option) {
	case Opt_commit:
		if (strlen(param->string) != 40 || hex2bin(mount->commit, param->string, 20))
			return -EINVAL;
		mount->explicit_commit = true;
		break;
	case Opt_cache:
		if (!result.uint_32 || result.uint_32 > 1024)
			return -EINVAL;
		mount->cache_limit = (size_t)result.uint_32 << 20;
		break;
	case Opt_object:
		if (!result.uint_32 || result.uint_32 > 64)
			return -EINVAL;
		mount->object_limit = (size_t)result.uint_32 << 20;
		break;
	case Opt_timeout:
		if (!result.uint_32 || result.uint_32 > 60000)
			return -EINVAL;
		mount->timeout = msecs_to_jiffies(result.uint_32);
		break;
	case Opt_error_ttl:
		if (result.uint_32 > 60000)
			return -EINVAL;
		mount->error_ttl = msecs_to_jiffies(result.uint_32);
		break;
	case Opt_smart:
		mount->smart = true;
		break;
	case Opt_refs:
		return gitfs_set_view(mount, GITFS_VIEW_REFS);
	case Opt_checkout:
		return gitfs_set_view(mount, GITFS_VIEW_CHECKOUT);
	case Opt_view:
		if (!strcmp(param->string, "refs"))
			return gitfs_set_view(mount, GITFS_VIEW_REFS);
		if (!strcmp(param->string, "checkout"))
			return gitfs_set_view(mount, GITFS_VIEW_CHECKOUT);
		return -EINVAL;
	case Opt_ref:
		if (gitfs_reference_name(param->string, strlen(param->string),
				 mount->reference, sizeof(mount->reference)))
			return -EINVAL;
		break;
	case Opt_pack:
		if (!result.uint_32 || result.uint_32 > 1024)
			return -EINVAL;
		mount->pack_limit = (size_t)result.uint_32 << 20;
		break;
	case Opt_dns:
		if (!in4_pton(param->string, -1, (u8 *)&mount->dns, -1, NULL))
			return -EINVAL;
		break;
	case Opt_address:
		if (!in4_pton(param->string, -1,
			     (u8 *)&mount->address.sin_addr.s_addr, -1, NULL))
			return -EINVAL;
		mount->address_known = true;
		break;
	}
	return 0;
}

static int gitfs_get_tree(struct fs_context *fc)
{
	struct gitfs_mount *mount = fc->s_fs_info;
	int error;

	if (!fc->source)
		return -EINVAL;
	if (mount->reference[0] && mount->explicit_commit)
		return -EINVAL;
	if (mount->view == GITFS_VIEW_REFS && (mount->explicit_commit || mount->reference[0]))
		return -EINVAL;
	mount->ref_view = mount->view != GITFS_VIEW_CHECKOUT &&
		!mount->explicit_commit && !mount->reference[0];
	if (mount->ref_view)
		mount->smart = true;
	/* Every allowed object, including its entry, must fit in the cache. */
	if (mount->cache_limit <= mount->object_limit + sizeof(struct gitfs_object))
		return -EINVAL;
	error = gitfs_parse_url(mount, fc->source);
	if (error)
		return error;
	error = gitfs_resolve_address(mount);
	if (error)
		return error;
	return get_tree_nodev(fc, gitfs_fill_super);
}

static void gitfs_mount_free(struct gitfs_mount *mount)
{
	if (!mount)
		return;
	gitfs_cache_destroy(mount);
	put_net(mount->net);
	kfree(mount);
}

static void gitfs_free_context(struct fs_context *fc)
{
	gitfs_mount_free(fc->s_fs_info);
}

static int gitfs_reconfigure(struct fs_context *fc)
{
	return fc->sb_flags & SB_RDONLY ? 0 : -EROFS;
}

static const struct fs_context_operations gitfs_context_ops = {
	.free = gitfs_free_context,
	.parse_param = gitfs_parse_param,
	.get_tree = gitfs_get_tree,
	.reconfigure = gitfs_reconfigure,
};

static int gitfs_init_context(struct fs_context *fc)
{
	struct gitfs_mount *mount;

	mount = kzalloc(sizeof(*mount), GFP_KERNEL);
	if (!mount)
		return -ENOMEM;
	mount->net = get_net(current->nsproxy->net_ns);
	mount->cache_limit = 64UL << 20;
	mount->object_limit = 8UL << 20;
	mount->pack_limit = 64UL << 20;
	mount->dns = htonl(0x01010101);
	mount->timeout = msecs_to_jiffies(5000);
	mount->error_ttl = msecs_to_jiffies(5000);
	mutex_init(&mount->lock);
	hash_init(mount->objects);
	INIT_LIST_HEAD(&mount->lru);
	INIT_LIST_HEAD(&mount->refs);
	atomic64_set(&mount->next_ino, 0);
	fc->s_fs_info = mount;
	fc->ops = &gitfs_context_ops;
	return 0;
}

static void gitfs_kill_super(struct super_block *sb)
{
	struct gitfs_mount *mount = sb->s_fs_info;

	/* Cached bodies outlive every inode and in-flight filesystem operation. */
	/* Dentries have ordinary VFS references; none have ramfs-style pins. */
	kill_anon_super(sb);
	pr_info("gitfs: requests=%llu hits=%llu misses=%llu evictions=%llu cache_bytes=%zu\n",
		mount->requests, mount->hits, mount->misses, mount->evictions, mount->cache_used);
	gitfs_mount_free(mount);
}

static struct file_system_type gitfs_type = {
	.owner = THIS_MODULE,
	.name = "gitfs",
	.init_fs_context = gitfs_init_context,
	.parameters = gitfs_parameters,
	.kill_sb = gitfs_kill_super,
};

static int __init gitfs_init(void)
{
	return register_filesystem(&gitfs_type);
}

static void __exit gitfs_exit(void)
{
	unregister_filesystem(&gitfs_type);
	rcu_barrier();
}

module_init(gitfs_init);
module_exit(gitfs_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Read-only Git HTTP filesystem with kernel-owned object caching");
