// SPDX-License-Identifier: GPL-2.0
#include <linux/jhash.h>
#include <linux/jiffies.h>
#include <linux/slab.h>
#include <linux/vmalloc.h>

#include "gitfs.h"

static void gitfs_object_free(struct gitfs_mount *mount, struct gitfs_object *object)
{
	hash_del(&object->hash);
	list_del(&object->lru);
	mount->cache_used -= object->charge;
	kvfree(object->data);
	kfree(object);
}

void gitfs_cache_destroy(struct gitfs_mount *mount)
{
	struct gitfs_object *object, *next;
	struct gitfs_ref *ref, *following;

	list_for_each_entry_safe(object, next, &mount->lru, lru)
		gitfs_object_free(mount, object);
	kvfree(mount->snapshot_index);
	kvfree(mount->snapshot_data);
	list_for_each_entry_safe(ref, following, &mount->refs, allocation) {
		list_del(&ref->allocation);
		kfree(ref);
	}
}

struct gitfs_object *gitfs_object_get(struct gitfs_mount *mount, const u8 *id)
{
	struct gitfs_object *object;
	struct gitfs_decoded decoded;
	u8 *wire = NULL, *staging = NULL, *window = NULL;
	char hex[41], path[50];
	size_t length, charge;
	u32 key = jhash(id, 20, 0);
	int error;

	lockdep_assert_held(&mount->lock);
	if (mount->snapshot_count) {
		error = gitfs_pack_find(mount->snapshot_index, mount->snapshot_count, id, &decoded);
		mount->hits++;
		if (error)
			return ERR_PTR(error);
		mount->snapshot_view.data = mount->snapshot_data + decoded.offset;
		mount->snapshot_view.length = decoded.length;
		mount->snapshot_view.kind = decoded.kind;
		return &mount->snapshot_view;
	}
	hash_for_each_possible(mount->objects, object, hash, key) {
		if (memcmp(object->id, id, 20))
			continue;
		if (object->error && time_after_eq(jiffies, object->expires)) {
			gitfs_object_free(mount, object);
			break;
		}
		mount->hits++;
		list_move(&object->lru, &mount->lru);
		return object->error ? ERR_PTR(object->error) : object;
	}
	mount->misses++;
	object = kzalloc(sizeof(*object), GFP_KERNEL);
	if (!object)
		return ERR_PTR(-ENOMEM);
	memcpy(object->id, id, 20);
	bin2hex(hex, id, 20);
	hex[40] = 0;
	snprintf(path, sizeof(path), "objects/%.2s/%s", hex, hex + 2);
	wire = kvmalloc(mount->object_limit + 4096, GFP_KERNEL);
	staging = kvmalloc(mount->object_limit + 32, GFP_KERNEL);
	window = kvzalloc(32768, GFP_KERNEL);
	if (!wire || !staging || !window) {
		error = -ENOMEM;
		goto out;
	}
	error = gitfs_http_get(mount, path, wire, mount->object_limit + 4096, &length);
	if (error)
		goto out;
	error = gitfs_decode_loose(wire, length, id, staging,
				   mount->object_limit + 32, window, &decoded);
	if (error)
		goto out;
	if (decoded.length > mount->object_limit) {
		error = -EFBIG;
		goto out;
	}
	object->data = kvmalloc(decoded.length ?: 1, GFP_KERNEL);
	if (!object->data) {
		error = -ENOMEM;
		goto out;
	}
	memcpy(object->data, staging + decoded.offset, decoded.length);
	object->length = decoded.length;
	object->kind = decoded.kind;
out:
	kvfree(window);
	kvfree(staging);
	kvfree(wire);
	if (error == -ENOMEM || error == -EINTR || error == -ERESTARTSYS) {
		kvfree(object->data);
		kfree(object);
		return ERR_PTR(error);
	}
	object->error = error;
	object->expires = jiffies + mount->error_ttl;
	charge = sizeof(*object) + (object->length ?: 1);
	/* Evict only here. A caller must copy IDs before fetching another object. */
	while (mount->cache_used + charge > mount->cache_limit && !list_empty(&mount->lru)) {
		struct gitfs_object *victim = list_last_entry(&mount->lru, struct gitfs_object, lru);

		gitfs_object_free(mount, victim);
		mount->evictions++;
	}
	object->charge = charge;
	mount->cache_used += charge;
	hash_add(mount->objects, &object->hash, key);
	list_add(&object->lru, &mount->lru);
	return error ? ERR_PTR(error) : object;
}
