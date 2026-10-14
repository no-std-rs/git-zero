// SPDX-License-Identifier: GPL-2.0
/* A bounded DNS A query through a kernel UDP socket; no resolver upcall. */
#include <linux/ctype.h>
#include <linux/net.h>
#include <linux/random.h>
#include <linux/slab.h>
#include <linux/unaligned.h>
#include <net/sock.h>
#include "gitfs.h"

static int skip_name(const u8 *packet, size_t length, size_t *offset)
{
	while (*offset < length) {
		u8 label = packet[(*offset)++];

		if (!label)
			return 0;
		if ((label & 0xc0) == 0xc0) {
			if (*offset >= length ||
			    (((label & 0x3f) << 8) | packet[*offset]) >= length)
				return -EBADMSG;
			(*offset)++;
			return 0;
		}
		if (label > 63 || label > length - *offset)
			return -EBADMSG;
		*offset += label;
	}
	return -EBADMSG;
}

int gitfs_resolve_address(struct gitfs_mount *mount)
{
	struct sockaddr_in address = {
		.sin_family = AF_INET, .sin_port = htons(53),
		.sin_addr.s_addr = mount->dns,
	};
	struct socket *socket = NULL;
	struct msghdr message = { .msg_flags = MSG_NOSIGNAL };
	struct kvec vector;
	u8 *query, *response;
	size_t offset = 12, question_length, length, i;
	const char *label = mount->hostname, *end;
	u16 id = get_random_u32(), answers;
	int error;

	if (mount->address_known)
		return 0;
	query = kzalloc(512, GFP_KERNEL);
	response = kmalloc(4096, GFP_KERNEL);
	if (!query || !response) {
		error = -ENOMEM;
		goto out;
	}
	put_unaligned_be16(id, query);
	put_unaligned_be16(0x0100, query + 2);
	put_unaligned_be16(1, query + 4);
	while (*label) {
		end = strchrnul(label, '.');
		length = end - label;
		if (!length || length > 63 || offset + length + 6 > 512) {
			error = -EINVAL;
			goto out;
		}
		query[offset++] = length;
		memcpy(query + offset, label, length);
		offset += length;
		label = *end ? end + 1 : end;
	}
	query[offset++] = 0;
	put_unaligned_be16(1, query + offset);
	put_unaligned_be16(1, query + offset + 2);
	question_length = offset + 4;
	error = sock_create_kern(mount->net, AF_INET, SOCK_DGRAM, IPPROTO_UDP, &socket);
	if (error)
		goto out;
	socket->sk->sk_sndtimeo = mount->timeout;
	socket->sk->sk_rcvtimeo = mount->timeout;
	error = kernel_connect(socket, (struct sockaddr *)&address, sizeof(address), 0);
	if (error)
		goto out;
	vector.iov_base = query;
	vector.iov_len = question_length;
	error = kernel_sendmsg(socket, &message, &vector, 1, vector.iov_len);
	if (error != question_length) {
		error = error < 0 ? error : -EIO;
		goto out;
	}
	message.msg_flags = 0;
	vector.iov_base = response;
	vector.iov_len = 4096;
	error = kernel_recvmsg(socket, &message, &vector, 1, 4096, 0);
	if (error < 0)
		goto out;
	length = error;
	error = -EBADMSG;
	if (length < question_length || get_unaligned_be16(response) != id ||
	    (get_unaligned_be16(response + 2) & 0xfa0f) != 0x8000 ||
	    get_unaligned_be16(response + 4) != 1 ||
	    memcmp(response + 12, query + 12, question_length - 12))
		goto out;
	answers = get_unaligned_be16(response + 6);
	offset = question_length;
	for (i = 0; i < answers; i++) {
		u16 type, class, size;

		error = skip_name(response, length, &offset);
		if (error)
			goto out;
		if (length - offset < 10) {
			error = -EBADMSG;
			goto out;
		}
		type = get_unaligned_be16(response + offset);
		class = get_unaligned_be16(response + offset + 2);
		size = get_unaligned_be16(response + offset + 8);
		offset += 10;
		if (size > length - offset) {
			error = -EBADMSG;
			goto out;
		}
		if (type == 1 && class == 1 && size == 4) {
			memcpy(&mount->address.sin_addr.s_addr, response + offset, 4);
			mount->address_known = true;
			error = 0;
			goto out;
		}
		offset += size;
	}
	error = -ENOENT;
out:
	if (socket)
		sock_release(socket);
	kfree(query);
	kfree(response);
	return error;
}
