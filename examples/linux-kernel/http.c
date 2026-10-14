// SPDX-License-Identifier: GPL-2.0
/* Bounded HTTP over kernel sockets; TLS is supplied by the no_std Rust codec. */
#include <linux/ctype.h>
#include <linux/inet.h>
#include <linux/jiffies.h>
#include <linux/net.h>
#include <linux/random.h>
#include <linux/slab.h>
#include <linux/vmalloc.h>
#include <net/sock.h>
#include "gitfs.h"

#define TLS_RECORD_SIZE 16640

struct gitfs_connection {
	struct socket *socket;
	unsigned long deadline;
	void *tls;
	u8 *tls_read, *tls_write;
	int io_error;
};

struct gitfs_input {
	struct gitfs_connection *connection;
	const u8 *prefix;
	size_t length, position;
};

int gitfs_parse_url(struct gitfs_mount *mount, const char *url)
{
	char authority[280], *colon;
	const char *start, *slash;
	size_t length, i;
	u16 port;

	mount->https = !strncmp(url, "https://", 8);
	if (!mount->https && strncmp(url, "http://", 7))
		return -EPROTONOSUPPORT;
	start = url + (mount->https ? 8 : 7);
	port = mount->https ? 443 : 80;
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
	if (strlen(authority) >= sizeof(mount->hostname))
		return -EINVAL;
	for (i = 0; authority[i]; i++)
		if (!isalnum(authority[i]) && authority[i] != '-' && authority[i] != '.')
			return -EINVAL;
	strscpy(mount->hostname, authority, sizeof(mount->hostname));
	if (!mount->address_known)
		mount->address_known = in4_pton(authority, -1,
			(u8 *)&mount->address.sin_addr.s_addr, -1, NULL);
	mount->address.sin_family = AF_INET;
	mount->address.sin_port = htons(port);
	if (!slash)
		slash = "";
	length = strlen(slash);
	if (length >= sizeof(mount->path))
		return -ENAMETOOLONG;
	for (i = 0; i < length; i++)
		if (slash[i] <= ' ' || slash[i] > '~' || strchr("?#\\", slash[i]))
			return -EINVAL;
	memcpy(mount->path, slash, length + 1);
	while (length && mount->path[length - 1] == '/')
		mount->path[--length] = 0;
	if (!strcmp(mount->hostname, "github.com") &&
	    (length < 4 || strcmp(mount->path + length - 4, ".git"))) {
		if (length + 4 >= sizeof(mount->path))
			return -ENAMETOOLONG;
		strcat(mount->path, ".git");
	}
	mount->smart |= mount->https;
	return 0;
}

static ssize_t raw_receive(void *context, u8 *bytes, size_t length)
{
	struct gitfs_connection *connection = context;
	struct msghdr message = {};
	struct kvec vector = { .iov_base = bytes, .iov_len = length };
	ssize_t count;

	if (time_after_eq(jiffies, connection->deadline))
		count = -ETIMEDOUT;
	else {
		connection->socket->sk->sk_rcvtimeo = connection->deadline - jiffies;
		count = kernel_recvmsg(connection->socket, &message, &vector, 1, length, 0);
	}
	if (count < 0)
		connection->io_error = count;
	return count;
}

static ssize_t raw_send(void *context, const u8 *bytes, size_t length)
{
	struct gitfs_connection *connection = context;
	struct msghdr message = { .msg_flags = MSG_NOSIGNAL };
	struct kvec vector = { .iov_base = (void *)bytes, .iov_len = length };
	ssize_t count;

	if (time_after_eq(jiffies, connection->deadline))
		count = -ETIMEDOUT;
	else {
		connection->socket->sk->sk_sndtimeo = connection->deadline - jiffies;
		count = kernel_sendmsg(connection->socket, &message, &vector, 1, length);
	}
	if (count < 0)
		connection->io_error = count;
	return count;
}

static void tls_random(u8 *bytes, size_t length)
{
	get_random_bytes(bytes, length);
}

static int connect_http(struct gitfs_mount *mount, struct gitfs_connection *connection)
{
	struct gitfs_socket_io io = {
		.context = connection, .receive = raw_receive,
		.send = raw_send, .random = tls_random,
	};
	int error;

	connection->deadline = jiffies + mount->timeout;
	error = sock_create_kern(mount->net, AF_INET, SOCK_STREAM, IPPROTO_TCP, &connection->socket);
	if (error)
		return error;
	connection->socket->sk->sk_sndtimeo = mount->timeout;
	connection->socket->sk->sk_rcvtimeo = mount->timeout;
	mount->requests++;
	error = kernel_connect(connection->socket, (struct sockaddr *)&mount->address,
			       sizeof(mount->address), 0);
	if (error || !mount->https)
		return error;
	error = wait_for_random_bytes();
	if (error)
		return error;
	connection->tls = kvzalloc(gitfs_tls_size(), GFP_KERNEL);
	connection->tls_read = kvmalloc(TLS_RECORD_SIZE, GFP_KERNEL);
	connection->tls_write = kvmalloc(TLS_RECORD_SIZE, GFP_KERNEL);
	if (!connection->tls || !connection->tls_read || !connection->tls_write)
		return -ENOMEM;
	error = gitfs_tls_open(connection->tls, connection->tls_read, connection->tls_write, io,
			       mount->hostname, strlen(mount->hostname));
	return connection->io_error ?: error;
}

static void close_http(struct gitfs_connection *connection)
{
	if (connection->tls)
		memzero_explicit(connection->tls, gitfs_tls_size());
	if (connection->tls_read)
		memzero_explicit(connection->tls_read, TLS_RECORD_SIZE);
	if (connection->tls_write)
		memzero_explicit(connection->tls_write, TLS_RECORD_SIZE);
	kvfree(connection->tls);
	kvfree(connection->tls_read);
	kvfree(connection->tls_write);
	if (connection->socket)
		sock_release(connection->socket);
}

static ssize_t receive_http(struct gitfs_connection *connection, u8 *bytes, size_t length)
{
	ssize_t result = connection->tls ?
		gitfs_tls_read(connection->tls, bytes, length) : raw_receive(connection, bytes, length);

	return result < 0 && connection->io_error ? connection->io_error : result;
}

static int send_http(struct gitfs_connection *connection, const u8 *bytes, size_t length)
{
	while (length) {
		ssize_t count = connection->tls ?
			gitfs_tls_write(connection->tls, bytes, length) : raw_send(connection, bytes, length);

		if (count <= 0)
			return connection->io_error ?: (count ?: -EIO);
		bytes += count;
		length -= count;
	}
	return 0;
}

static int content_length(const char *value, size_t capacity, size_t *out)
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

static int parse_headers(char *headers, size_t capacity, bool *has_length,
			 size_t *length, bool *chunked)
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
			if (*has_length)
				return -EBADMSG;
			error = content_length(value, capacity, length);
			if (error)
				return error;
			*has_length = true;
		} else if (!strcasecmp(line, "Transfer-Encoding")) {
			if (*chunked || strcasecmp(value, "chunked"))
				return -EBADMSG;
			*chunked = true;
		} else if (!strcasecmp(line, "Content-Encoding") && strcasecmp(value, "identity")) {
			return -EOPNOTSUPP;
		}
		if (!next)
			break;
		line = next + 2;
	}
	return *has_length && *chunked ? -EBADMSG : 0;
}

static int input_read(struct gitfs_input *input, u8 *bytes, size_t length)
{
	size_t buffered = min(length, input->length - input->position);

	if (buffered) {
		memcpy(bytes, input->prefix + input->position, buffered);
		input->position += buffered;
		return buffered;
	}
	return receive_http(input->connection, bytes, length);
}

static int input_exact(struct gitfs_input *input, u8 *bytes, size_t length)
{
	while (length) {
		int count = input_read(input, bytes, length);

		if (count <= 0)
			return count ?: -EBADMSG;
		length -= count;
		bytes += count;
	}
	return 0;
}

static int input_line(struct gitfs_input *input, char *line, size_t capacity)
{
	size_t length = 0;
	int error;
	u8 byte;

	while (length + 1 < capacity) {
		error = input_exact(input, &byte, 1);
		if (error)
			return error;
		if (byte == '\r') {
			error = input_exact(input, &byte, 1);
			if (error || byte != '\n')
				return error ?: -EBADMSG;
			line[length] = 0;
			return length;
		}
		if (byte < ' ' || byte > '~')
			return -EBADMSG;
		line[length++] = byte;
	}
	return -EFBIG;
}

static int read_chunked(struct gitfs_input *input, u8 *body, size_t capacity, size_t *length)
{
	char line[256], *extension;
	size_t used = 0, size, i, trailers = 0;
	u8 ending[2];
	int error, digits;

	for (;;) {
		digits = input_line(input, line, sizeof(line));
		if (digits <= 0)
			return digits ?: -EBADMSG;
		extension = strchr(line, ';');
		if (extension)
			*extension = 0;
		size = 0;
		if (!*line)
			return -EBADMSG;
		for (i = 0; line[i]; i++) {
			int value = hex_to_bin(line[i]);

			if (value < 0)
				return -EBADMSG;
			if (size > (capacity - used) / 16 ||
			    (size == (capacity - used) / 16 && value > (capacity - used) % 16))
				return -EFBIG;
			size = size * 16 + value;
		}
		if (!size) {
			do {
				error = input_line(input, line, sizeof(line));
				if (error < 0)
					return error;
				trailers += error + 2;
				if (trailers > GITFS_HTTP_HEADERS)
					return -EFBIG;
			} while (error);
			*length = used;
			return 0;
		}
		error = input_exact(input, body + used, size);
		if (error)
			return error;
		error = input_exact(input, ending, 2);
		if (error || memcmp(ending, "\r\n", 2))
			return error ?: -EBADMSG;
		used += size;
	}
}

static int http_request(struct gitfs_mount *mount, const char *path,
			const u8 *payload, size_t payload_length,
			u8 *body, size_t capacity, size_t *length)
{
	struct gitfs_connection connection = {};
	struct gitfs_input input;
	char *request = NULL, *headers = NULL, *separator;
	size_t used = 0, received, expected = 0;
	bool has_length = false, chunked = false;
	u8 extra;
	int error;

	request = kasprintf(GFP_KERNEL,
		"%s %s/%s HTTP/1.0\r\nHost: %s\r\nUser-Agent: git-zero-kernel/0.1\r\nConnection: close\r\nAccept-Encoding: identity\r\n%sContent-Length: %zu\r\n\r\n",
		payload ? "POST" : "GET", mount->path, path, mount->host,
		payload ? "Content-Type: application/x-git-upload-pack-request\r\n" : "", payload_length);
	headers = kmalloc(GITFS_HTTP_HEADERS + 1, GFP_KERNEL);
	if (!request || !headers) {
		error = -ENOMEM;
		goto out;
	}
	error = connect_http(mount, &connection);
	if (error)
		goto out;
	error = send_http(&connection, request, strlen(request));
	if (!error && payload)
		error = send_http(&connection, payload, payload_length);
	if (error)
		goto out;
	for (;;) {
		if (used == GITFS_HTTP_HEADERS) {
			error = -EFBIG;
			goto out;
		}
		error = receive_http(&connection, headers + used, GITFS_HTTP_HEADERS - used);
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
	separator[2] = 0;
	error = parse_headers(headers, capacity, &has_length, &expected, &chunked);
	if (error)
		goto out;
	input = (struct gitfs_input) {
		.connection = &connection, .prefix = separator + 4, .length = received,
	};
	if (chunked) {
		error = read_chunked(&input, body, capacity, length);
		goto out;
	}
	if (received > capacity || (has_length && received > expected)) {
		error = -EBADMSG;
		goto out;
	}
	memcpy(body, separator + 4, received);
	while (!has_length || received < expected) {
		size_t available = (has_length ? expected : capacity) - received;

		error = receive_http(&connection, available ? body + received : &extra, available ?: 1);
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
	close_http(&connection);
	kfree(request);
	kfree(headers);
	return error;
}

int gitfs_http_get(struct gitfs_mount *mount, const char *path, u8 *body, size_t capacity, size_t *length)
{
	return http_request(mount, path, NULL, 0, body, capacity, length);
}

int gitfs_http_post(struct gitfs_mount *mount, const char *path, const u8 *request, size_t request_length,
		    u8 *body, size_t capacity, size_t *length)
{
	return http_request(mount, path, request, request_length, body, capacity, length);
}
