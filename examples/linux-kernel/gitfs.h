/* SPDX-License-Identifier: GPL-2.0 */
#ifndef GITFS_H
#define GITFS_H

#include <linux/fs.h>
#include <linux/hashtable.h>
#include <linux/in.h>
#include <linux/list.h>
#include <linux/mutex.h>
#include <net/net_namespace.h>

#define GITFS_MAGIC 0x67697430
#define GITFS_COMMIT 1
#define GITFS_TREE 2
#define GITFS_BLOB 3
#define GITFS_BUCKET_BITS 8
#define GITFS_URL_MAX 1024
#define GITFS_HTTP_HEADERS 8192

struct gitfs_decoded {
	size_t offset, length;
	u32 kind;
};

struct gitfs_tree_entry {
	const u8 *name;
	size_t name_length;
	u32 mode;
	u8 id[20];
};

struct gitfs_object {
	struct hlist_node hash;
	struct list_head lru;
	u8 id[20];
	u32 kind;
	u8 *data;
	size_t length, charge;
	int error;
	unsigned long expires;
};

struct gitfs_mount {
	struct net *net;
	struct sockaddr_in address;
	char host[280];
	char hostname[254];
	char path[GITFS_URL_MAX];
	u8 commit[20];
	bool explicit_commit, https, smart, address_known;
	__be32 dns;
	size_t pack_limit;
	void *snapshot_index;
	u8 *snapshot_data;
	size_t snapshot_count, snapshot_used;
	struct gitfs_object snapshot_view;
	size_t cache_limit, cache_used, object_limit;
	unsigned long timeout, error_ttl;
	struct mutex lock;
	DECLARE_HASHTABLE(objects, GITFS_BUCKET_BITS);
	struct list_head lru;
	u64 requests, hits, misses, evictions;
	atomic64_t next_ino;
};

struct gitfs_inode {
	u8 id[20];
	bool gitlink;
};

void __noreturn gitfs_codec_panic(void);

int gitfs_decode_loose(const u8 *wire, size_t length, const u8 *id,
		       u8 *output, size_t capacity, u8 *window,
		       struct gitfs_decoded *result);
int gitfs_commit_tree(const u8 *data, size_t length, u8 *id);
int gitfs_tree_next(const u8 *data, size_t length, size_t *offset,
		   struct gitfs_tree_entry *entry);

int gitfs_parse_url(struct gitfs_mount *mount, const char *url);
int gitfs_resolve_address(struct gitfs_mount *mount);
int gitfs_http_get(struct gitfs_mount *mount, const char *path,
		   u8 *body, size_t capacity, size_t *length);
int gitfs_http_post(struct gitfs_mount *mount, const char *path,
		    const u8 *request, size_t request_length,
		    u8 *body, size_t capacity, size_t *length);
int gitfs_fetch_snapshot(struct gitfs_mount *mount);

struct gitfs_socket_io {
	void *context;
	ssize_t (*receive)(void *, u8 *, size_t);
	ssize_t (*send)(void *, const u8 *, size_t);
	void (*random)(u8 *, size_t);
};
struct gitfs_pack_object {
	u8 id[20];
	u32 kind;
	u64 pack_offset;
	size_t offset, length;
	u32 depth;
};
size_t gitfs_tls_size(void);
int gitfs_tls_open(void *state, u8 *read, u8 *write, struct gitfs_socket_io io,
		   const u8 *host, size_t host_length);
ssize_t gitfs_tls_read(void *state, u8 *output, size_t length);
ssize_t gitfs_tls_write(void *state, const u8 *input, size_t length);
int gitfs_smart_head(const u8 *data, size_t length, u8 *head);
int gitfs_smart_pack(const u8 *data, size_t length, struct gitfs_decoded *result);
int gitfs_pack_import(const u8 *pack, size_t length,
		      struct gitfs_pack_object *index, size_t count,
		      u8 *arena, size_t arena_length, u8 *scratch, size_t scratch_length,
		      u8 *window, size_t object_limit, size_t *used);
int gitfs_pack_find(const void *index, size_t count, const u8 *id, struct gitfs_decoded *result);
/* Cache access holds mount->lock, including all uses of a returned pointer. */
struct gitfs_object *gitfs_object_get(struct gitfs_mount *mount, const u8 *id);
void gitfs_cache_destroy(struct gitfs_mount *mount);

#endif
