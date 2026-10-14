// SPDX-License-Identifier: GPL-2.0
/* Pinned advertised refs: branch/msg and branch/tree, sharing one object cache. */
#include <linux/slab.h>
#include "gitfs.h"

static const struct inode_operations refs_inode_ops;
static const struct file_operations refs_file_ops;

static struct gitfs_ref *new_ref(struct gitfs_mount *mount, const char *name)
{
	size_t charge = sizeof(struct gitfs_ref) + strlen(name) + 1;
	struct gitfs_ref *ref;

	if (mount->ref_count >= 4096 || charge > mount->cache_limit - mount->refs_bytes)
		return ERR_PTR(-EFBIG);
	ref = kzalloc(charge, GFP_KERNEL);
	if (!ref)
		return ERR_PTR(-ENOMEM);
	strcpy(ref->name, name);
	list_add_tail(&ref->allocation, &mount->refs);
	mount->refs_bytes += charge;
	mount->ref_count++;
	return ref;
}

int gitfs_refs_add(struct gitfs_mount *mount, const struct gitfs_ref_entry *entry)
{
	struct gitfs_ref *parent, *child, **link;
	char *path, *cursor, *component;
	int error = 0;

	if (!mount->ref_root) {
		parent = new_ref(mount, "");
		if (IS_ERR(parent))
			return PTR_ERR(parent);
		mount->ref_root = parent;
	}
	if (entry->name_length == 4 && !memcmp(entry->name, "HEAD", 4))
		path = kstrdup(".refs/HEAD", GFP_KERNEL);
	else if (entry->name_length > 11 && !memcmp(entry->name, "refs/heads/", 11))
		path = kstrndup(entry->name + 11, entry->name_length - 11, GFP_KERNEL);
	else
		path = kasprintf(GFP_KERNEL, ".refs/%.*s", (int)entry->name_length - 5,
				entry->name + 5);
	if (!path)
		return -ENOMEM;
	parent = mount->ref_root;
	cursor = path;
	while ((component = strsep(&cursor, "/"))) {
		if (parent->leaf) {
			error = -EBADMSG;
			goto out;
		}
		link = &parent->children;
		while (*link && strcmp((*link)->name, component) < 0)
			link = &(*link)->next;
		child = *link;
		if (!child || strcmp(child->name, component)) {
			child = new_ref(mount, component);
			if (IS_ERR(child)) {
				error = PTR_ERR(child);
				goto out;
			}
			child->next = *link;
			*link = child;
		}
		parent = child;
	}
	if (parent->leaf || parent->children) {
		error = -EBADMSG;
		goto out;
	}
	parent->leaf = true;
	memcpy(parent->id, entry->id, 20);
out:
	kfree(path);
	return error;
}

int gitfs_refs_prepare(struct gitfs_mount *mount)
{
	struct gitfs_ref *ref;
	struct gitfs_object *object;
	struct gitfs_decoded message;
	u8 id[20];
	u32 expected_kind;
	int depth, error;

	list_for_each_entry(ref, &mount->refs, allocation) {
		if (!ref->leaf)
			continue;
		memcpy(id, ref->id, 20);
		expected_kind = 0;
		for (depth = 0; ; depth++) {
			object = gitfs_object_get(mount, id);
			if (IS_ERR(object))
				return PTR_ERR(object);
			if (expected_kind && object->kind != expected_kind)
				return -EBADMSG;
			if (object->kind != GITFS_TAG)
				break;
			if (depth == 8)
				return -EBADMSG;
			error = gitfs_tag_target(object->data, object->length, id, &expected_kind);
			if (error)
				return error;
		}
		memcpy(ref->id, id, 20);
		if (object->kind == GITFS_COMMIT) {
			error = gitfs_commit_tree(object->data, object->length, ref->tree);
			if (!error)
				error = gitfs_commit_message(object->data, object->length, &message);
			if (error)
				return error;
			ref->message = true;
			object = gitfs_object_get(mount, ref->tree);
			if (IS_ERR(object))
				return PTR_ERR(object);
			if (object->kind != GITFS_TREE)
				return -EBADMSG;
		} else {
			memcpy(ref->tree, id, 20);
		}
		if (object->kind != GITFS_TREE && object->kind != GITFS_BLOB)
			return -EBADMSG;
		ref->tree_mode = object->kind == GITFS_TREE ? 040000 : 0100644;
	}
	return 0;
}

struct inode *gitfs_refs_inode(struct super_block *sb, struct gitfs_ref *ref)
{
	struct inode *inode = gitfs_alloc_inode(sb);
	struct gitfs_inode *node;

	if (IS_ERR(inode))
		return inode;
	node = inode->i_private;
	node->ref = ref;
	inode->i_mode = S_IFDIR | 0555;
	inode->i_op = &refs_inode_ops;
	inode->i_fop = &refs_file_ops;
	set_nlink(inode, 2);
	return inode;
}

static struct dentry *refs_lookup(struct inode *dir, struct dentry *dentry,
				  unsigned int flags)
{
	struct gitfs_mount *mount = dir->i_sb->s_fs_info;
	struct gitfs_inode *node = dir->i_private;
	struct gitfs_ref *ref = node->ref, *child;
	struct inode *inode = NULL;

	mutex_lock(&mount->lock);
	if (ref->leaf) {
		if (ref->message && !strcmp(dentry->d_name.name, "msg"))
			inode = gitfs_new_inode(dir->i_sb, ref->id, GITFS_MESSAGE);
		else if (!strcmp(dentry->d_name.name, "tree"))
			inode = gitfs_new_inode(dir->i_sb, ref->tree, ref->tree_mode);
	} else {
		for (child = ref->children; child; child = child->next) {
			if (!strcmp(child->name, dentry->d_name.name)) {
				inode = gitfs_refs_inode(dir->i_sb, child);
				break;
			}
		}
	}
	mutex_unlock(&mount->lock);
	if (IS_ERR(inode))
		return ERR_CAST(inode);
	return d_splice_alias(inode, dentry);
}

static int refs_readdir(struct file *file, struct dir_context *ctx)
{
	struct gitfs_inode *node = file_inode(file)->i_private;
	struct gitfs_ref *ref = node->ref, *child;
	loff_t index = 2;

	if (!dir_emit_dots(file, ctx))
		return 0;
	if (ref->leaf) {
		if (ref->message) {
			if (ctx->pos == 2 && !dir_emit(ctx, "msg", 3, 0, DT_REG))
				return 0;
			index++;
			if (ctx->pos < index)
				ctx->pos = index;
		}
		if (ctx->pos == index && !dir_emit(ctx, "tree", 4, 0,
			ref->tree_mode == 040000 ? DT_DIR : DT_REG))
			return 0;
		if (ctx->pos == index)
			ctx->pos++;
		return 0;
	}
	for (child = ref->children; child; child = child->next) {
		if (index++ < ctx->pos)
			continue;
		if (!dir_emit(ctx, child->name, strlen(child->name), 0, DT_DIR))
			break;
		ctx->pos = index;
	}
	return 0;
}

static const struct inode_operations refs_inode_ops = {
	.lookup = refs_lookup,
};

static const struct file_operations refs_file_ops = {
	.read = generic_read_dir,
	.iterate_shared = refs_readdir,
	.llseek = generic_file_llseek,
};
