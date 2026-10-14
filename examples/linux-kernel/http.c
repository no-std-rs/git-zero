// SPDX-License-Identifier: GPL-2.0
/* Deliberately bounded HTTP/1.0 transport. No userspace upcalls or sockets. */
#include <linux/ctype.h>
#include <linux/inet.h>
#include <linux/jiffies.h>
#include <linux/net.h>
#include <linux/slab.h>
#include <net/sock.h>

#include "gitfs.h"

int gitfs_parse_url(struct gitfs_mount *mount, const char *url)
{
	char authority[32], *colon;
	const char *start, *slash;
	size_t length, i;
	u16 port = 80;

	if (strncmp(url, "http://", 7))
		return -EPROTONOSUPPORT;
	start = url + 7;
	slash = strchr(start, '/');
	length = slash ? slash - start : strlen(start);
	if (!length || length >= sizeof(authority))
		return -EINVAL;
	memcpy(authority, start, length);
	authority[length] = 0;
	strscpy(mount->host, authority, sizeof(mount->host));
	colon = strchr(authority, ':');
	if (colon) {
		*colon++ = 0;
		if (kstrtou16(colon, 10, &port) || !port)
			return -EINVAL;
	}
	if (!in4_pton(authority, -1, (u8 *)&mount->address.sin_addr.s_addr,
		      -1, NULL))
		return -EINVAL;
	mount->address.sin_family = AF_INET;
	mount->address.sin_port = htons(port);
	if (!slash)
		slash = "";
	length = strlen(slash);
	if (length >= sizeof(mount->path))
		return -ENAMETOOLONG;
	for (i = 0; i < length; i++)
		if (slash[i] <= ' ' || slash[i] > '~' ||
		    strchr("?#\\", slash[i]))
			return -EINVAL;
	memcpy(mount->path, slash, length + 1);
	while (length && mount->path[length - 1] == '/')
		mount->path[--length] = 0;
	return 0;
}

static int gitfs_receive(struct socket *sock, void *data, size_t capacity,
			 unsigned long deadline)
{
	struct msghdr message = {};
	struct kvec vector = { .iov_base = data, .iov_len = capacity };

	if (time_after_eq(jiffies, deadline))
		return -ETIMEDOUT;
	sock->sk->sk_rcvtimeo = deadline - jiffies;
	return kernel_recvmsg(sock, &message, &vector, 1, capacity, 0);
}

static int gitfs_content_length(const char *value, size_t capacity, size_t *out)
{
	size_t length = 0;

	if (!*value)
		return -EBADMSG;
	for (; *value; value++) {
		if (!isdigit(*value))
			return -EBADMSG;
		if (length > capacity / 10 ||
		    (length == capacity / 10 && *value - '0' > capacity % 10))
			return -EFBIG;
		length = length * 10 + *value - '0';
	}
	*out = length;
	return 0;
}

/* Buffer ends at the header separator; body bytes remain untouched. */
static int gitfs_headers(char *headers, size_t capacity, bool *has_length,
			 size_t *length)
{
	char *line, *next, *colon, *value;
	int status, error;

	next = strstr(headers, "\r\n");
	if (!next)
		return -EBADMSG;
	*next = 0;
	if (strlen(headers) < 12 ||
	    (strncmp(headers, "HTTP/1.0 ", 9) && strncmp(headers, "HTTP/1.1 ", 9)) ||
	    !isdigit(headers[9]) || !isdigit(headers[10]) || !isdigit(headers[11]) ||
	    (headers[12] && headers[12] != ' '))
		return -EBADMSG;
	status = (headers[9] - '0') * 100 + (headers[10] - '0') * 10 + headers[11] - '0';
	if (status == 404)
		return -ENOENT;
	if (status != 200)
		return -EIO;
	line = next + 2;
	while (*line) {
		next = strstr(line, "\r\n");
		if (next)
			*next = 0;
		colon = strchr(line, ':');
		if (!colon || colon == line || isspace(*line))
			return -EBADMSG;
		*colon = 0;
		value = strim(colon + 1);
		if (!strcasecmp(line, "Content-Length")) {
			/* Ambiguous framing is rejected, including duplicate lengths. */
			if (*has_length)
				return -EBADMSG;
			error = gitfs_content_length(value, capacity, length);
			if (error)
				return error;
			*has_length = true;
		} else if (!strcasecmp(line, "Transfer-Encoding")) {
			return -EOPNOTSUPP;
		} else if (!strcasecmp(line, "Content-Encoding") &&
			   strcasecmp(value, "identity")) {
			return -EOPNOTSUPP;
		}
		if (!next)
			break;
		line = next + 2;
	}
	return 0;
}

int gitfs_http_get(struct gitfs_mount *mount, const char *path,
		   u8 *body, size_t capacity, size_t *length)
{
	struct socket *sock = NULL;
	struct msghdr message = { .msg_flags = MSG_NOSIGNAL };
	struct kvec vector;
	char *request, *headers, *separator;
	size_t request_length, sent = 0, used = 0, received, expected = 0;
	unsigned long deadline = jiffies + mount->timeout;
	bool has_length = false;
	u8 extra;
	int error;

	request = kasprintf(GFP_KERNEL,
		"GET %s/%s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\nAccept-Encoding: identity\r\n\r\n",
		mount->path, path, mount->host);
	headers = kmalloc(GITFS_HTTP_HEADERS + 1, GFP_KERNEL);
	if (!request || !headers) {
		error = -ENOMEM;
		goto out;
	}
	request_length = strlen(request);
	error = sock_create_kern(mount->net, AF_INET, SOCK_STREAM, IPPROTO_TCP, &sock);
	if (error)
		goto out;
	sock->sk->sk_sndtimeo = mount->timeout;
	sock->sk->sk_rcvtimeo = mount->timeout;
	mount->requests++;
	error = kernel_connect(sock, (struct sockaddr *)&mount->address,
			       sizeof(mount->address), 0);
	if (error)
		goto out;
	while (sent < request_length) {
		if (time_after_eq(jiffies, deadline)) {
			error = -ETIMEDOUT;
			goto out;
		}
		sock->sk->sk_sndtimeo = deadline - jiffies;
		vector.iov_base = request + sent;
		vector.iov_len = request_length - sent;
		error = kernel_sendmsg(sock, &message, &vector, 1, vector.iov_len);
		if (error <= 0) {
			error = error ?: -EIO;
			goto out;
		}
		sent += error;
	}
	for (;;) {
		if (used == GITFS_HTTP_HEADERS) {
			error = -EFBIG;
			goto out;
		}
		error = gitfs_receive(sock, headers + used, GITFS_HTTP_HEADERS - used, deadline);
		if (error <= 0) {
			error = error ?: -EBADMSG;
			goto out;
		}
		used += error;
		headers[used] = 0;
		separator = strstr(headers, "\r\n\r\n");
		if (separator)
			break;
		if (memchr(headers, 0, used)) {
			error = -EBADMSG;
			goto out;
		}
	}
	if (memchr(headers, 0, separator - headers)) {
		error = -EBADMSG;
		goto out;
	}
	received = used - (separator + 4 - headers);
	/* Preserve the last line's CRLF, including a headerless response. */
	separator[2] = 0;
	error = gitfs_headers(headers, capacity, &has_length, &expected);
	if (error)
		goto out;
	if (received > capacity || (has_length && received > expected)) {
		error = -EBADMSG;
		goto out;
	}
	memcpy(body, separator + 4, received);
	while (!has_length || received < expected) {
		size_t available = (has_length ? expected : capacity) - received;

		/* A close-delimited body exactly at the limit still needs EOF. */
		error = gitfs_receive(sock, available ? body + received : &extra,
				      available ?: 1, deadline);
		if (error < 0)
			goto out;
		if (!error) {
			if (has_length && received != expected)
				error = -EBADMSG;
			break;
		}
		if (!available) {
			error = -EFBIG;
			goto out;
		}
		received += error;
	}
	if (error >= 0) {
		*length = received;
		error = 0;
	}
out:
	if (sock)
		sock_release(sock);
	kfree(headers);
	kfree(request);
	return error;
}
