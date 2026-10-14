// SPDX-License-Identifier: GPL-2.0
#include <linux/slab.h>
#include <linux/unaligned.h>
#include <linux/vmalloc.h>
#include "gitfs.h"

int gitfs_fetch_snapshot(struct gitfs_mount *mount)
{
	u8 *wire = NULL, *scratch = NULL, *window = NULL;
	struct gitfs_decoded pack;
	char request[192], hex[41], want[96];
	size_t length, count, index_bytes, arena_length, request_length, want_length;
	u8 head[20];
	int shallow, error;

	wire = kvmalloc(mount->pack_limit, GFP_KERNEL);
	scratch = kvmalloc(mount->object_limit, GFP_KERNEL);
	window = kvzalloc(32768, GFP_KERNEL);
	if (!wire || !scratch || !window) {
		error = -ENOMEM;
		goto out;
	}
	error = gitfs_http_get(mount, "info/refs?service=git-upload-pack",
			       wire, min_t(size_t, mount->pack_limit, 1 << 20), &length);
	if (error)
		goto out;
	shallow = gitfs_smart_head(wire, length, head);
	if (shallow < 0) {
		error = shallow;
		goto out;
	}
	if (!mount->explicit_commit)
		memcpy(mount->commit, head, 20);
	bin2hex(hex, mount->commit, 20);
	hex[40] = 0;
	want_length = scnprintf(want, sizeof(want), "want %s ofs-delta no-progress%s\n",
				 hex, shallow ? " shallow" : "");
	request_length = scnprintf(request, sizeof(request), "%04zx%s", want_length + 4, want);
	if (shallow)
		request_length += scnprintf(request + request_length, sizeof(request) - request_length, "000ddeepen 1\n");
	request_length += scnprintf(request + request_length, sizeof(request) - request_length, "00000009done\n");
	error = gitfs_http_post(mount, "git-upload-pack", request, request_length,
				wire, mount->pack_limit, &length);
	if (error)
		goto out;
	error = gitfs_smart_pack(wire, length, &pack);
	if (error)
		goto out;
	count = get_unaligned_be32(wire + pack.offset + 8);
	if (!count || count > 32768) {
		error = -EFBIG;
		goto out;
	}
	index_bytes = count * sizeof(struct gitfs_pack_object);
	if (index_bytes >= mount->cache_limit) {
		error = -EFBIG;
		goto out;
	}
	arena_length = mount->cache_limit - index_bytes;
	mount->snapshot_index = kvzalloc(index_bytes, GFP_KERNEL);
	mount->snapshot_data = kvmalloc(arena_length, GFP_KERNEL);
	if (!mount->snapshot_index || !mount->snapshot_data) {
		error = -ENOMEM;
		goto out;
	}
	error = gitfs_pack_import(wire + pack.offset, pack.length,
				  mount->snapshot_index, count,
				  mount->snapshot_data, arena_length,
				  scratch, mount->object_limit, window, mount->object_limit,
				  &mount->snapshot_used);
	if (!error) {
		u8 *compact = kvmalloc(mount->snapshot_used ?: 1, GFP_KERNEL);

		if (compact) {
			memcpy(compact, mount->snapshot_data, mount->snapshot_used);
			kvfree(mount->snapshot_data);
			mount->snapshot_data = compact;
		}
		mount->snapshot_count = count;
		mount->cache_used = mount->snapshot_used + index_bytes;
	}
out:
	kvfree(wire);
	kvfree(scratch);
	kvfree(window);
	return error;
}
