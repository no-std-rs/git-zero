// SPDX-License-Identifier: GPL-2.0
/* QEMU PID 1 and test HTTP origin. This is test code, never a mount helper. */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/if.h>
#include <net/route.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#include <time.h>

static volatile unsigned *requests;

static void check(int condition, const char *label)
{
	if (!condition) {
		printf("GITFS_TEST_FAIL: %s (errno=%d: %s)\n", label, errno, strerror(errno));
		fflush(stdout);
		reboot(RB_POWER_OFF);
		_exit(1);
	}
	printf("ok: %s\n", label);
}

static void send_all(int fd, const void *bytes, size_t size)
{
	while (size) {
		ssize_t sent = send(fd, bytes, size, MSG_NOSIGNAL);
		if (sent <= 0)
			return;
		bytes = (const char *)bytes + sent;
		size -= sent;
	}
}

static void serve(int listener)
{
	for (;;) {
		char request[8192] = {}, target[4096], path[4096], header[256], data[8192];
		int fd = accept(listener, NULL, NULL), object, used = 0;
		struct stat st;
		ssize_t count;

		if (fd < 0)
			continue;
		while (used < (int)sizeof(request) - 1 && !strstr(request, "\r\n\r\n")) {
			count = read(fd, request + used, sizeof(request) - used - 1);
			if (count <= 0)
				break;
			used += count;
		}
		(*requests)++;
		if (sscanf(request, "GET %4095s", target) != 1) {
			close(fd);
			continue;
		}
		if (!strncmp(target, "/chunked.git/", 13)) {
			const char *response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
			send_all(fd, response, strlen(response));
			close(fd);
			continue;
		}
		if (!strncmp(target, "/truncated.git/", 15)) {
			const char *response = "HTTP/1.0 200 OK\r\nContent-Length: 100\r\n\r\nshort";
			send_all(fd, response, strlen(response));
			close(fd);
			continue;
		}
		if (!strncmp(target, "/corrupt.git/", 13)) {
			const char *response = "HTTP/1.0 200 OK\r\nContent-Length: 4\r\n\r\nnope";
			send_all(fd, response, strlen(response));
			close(fd);
			continue;
		}
		if (strstr(target, "..") || snprintf(path, sizeof(path), "/fixture%s", target) >= (int)sizeof(path)) {
			close(fd);
			continue;
		}
		object = open(path, O_RDONLY);
		if (object < 0 || fstat(object, &st) || !S_ISREG(st.st_mode)) {
			const char *response = "HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n";
			send_all(fd, response, strlen(response));
		} else {
			/* Alternate framing to exercise both close and length delimiters. */
			if (*requests % 2)
				snprintf(header, sizeof(header), "HTTP/1.0 200 OK\r\nContent-Length: %lld\r\n\r\n", (long long)st.st_size);
			else
				strcpy(header, "HTTP/1.0 200 OK\r\n\r\n");
			/* Deliberately fragment the headers and payload. */
			send_all(fd, header, 9);
			usleep(1000);
			send_all(fd, header + 9, strlen(header) - 9);
			while ((count = read(object, data, sizeof(data))) > 0)
				send_all(fd, data, count);
		}
		if (object >= 0)
			close(object);
		close(fd);
	}
}

static void expect_file(const char *path, const char *contents)
{
	char buffer[128] = {};
	int fd = open(path, O_RDONLY);
	ssize_t count;
	void *mapping;
	int pipe_fds[2];

	check(fd >= 0, path);
	count = read(fd, buffer, sizeof(buffer));
	check(count == (ssize_t)strlen(contents) && !memcmp(buffer, contents, count), "file bytes");
	mapping = mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, fd, 0);
	check(mapping != MAP_FAILED && !memcmp(mapping, contents, strlen(contents)), "mmap bytes");
	munmap(mapping, 4096);
	check(!pipe(pipe_fds), "splice pipe");
	check(lseek(fd, 0, SEEK_SET) == 0 &&
	      splice(fd, NULL, pipe_fds[1], NULL, 4096, 0) == (ssize_t)strlen(contents),
	      "splice file");
	check(read(pipe_fds[0], buffer, sizeof(buffer)) == (ssize_t)strlen(contents) &&
	      !memcmp(buffer, contents, strlen(contents)), "splice bytes");
	close(pipe_fds[0]);
	close(pipe_fds[1]);
	/* Force the next read through read_folio, exercising the object cache. */
	check(!posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED), "discard page cache");
	close(fd);
}

static void expect_big(const char *path)
{
	unsigned char buffer[4096];
	size_t position = 0;
	ssize_t length;
	int fd = open(path, O_RDONLY);
	int correct = 1;
	unsigned seed = strstr(path, "big-b") ? 1 : strstr(path, "big-c") ? 2 : 0;

	check(fd >= 0, path);
	while ((length = read(fd, buffer, sizeof(buffer))) > 0) {
		for (ssize_t i = 0; i < length; i++)
			if (buffer[i] != (unsigned char)((position + i + seed) % 251))
				correct = 0;
		position += length;
	}
	check(correct && length == 0 && position == 700000, "large file bytes and length");
	posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED);
	close(fd);
}

static void missing_probes(void)
{
	char path[128];
	struct stat st;
	unsigned before = *requests;
	int correct = 1;

	for (int i = 0; i < 100; i++) {
		snprintf(path, sizeof(path), "/mnt/not-present-%d", i);
		if (stat(path, &st) != -1 || errno != ENOENT)
			correct = 0;
	}
	check(correct && *requests == before, "100 absent names cause zero HTTP requests");
}

static int origin(void)
{
	struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(8080), .sin_addr.s_addr = htonl(INADDR_LOOPBACK) };
	int listener = socket(AF_INET, SOCK_STREAM, 0), one = 1;
	pid_t server;

	setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
	check(!bind(listener, (struct sockaddr *)&address, sizeof(address)) && !listen(listener, 16), "HTTP origin");
	server = fork();
	check(server >= 0, "fork test origin");
	if (!server)
		serve(listener);
	close(listener);
	return server;
}

static void concurrent_reads(void)
{
	int start[2], status;
	pid_t children[2];
	unsigned before = *requests;

	check(!pipe(start), "concurrent read barrier");
	for (int i = 0; i < 2; i++) {
		children[i] = fork();
		check(children[i] >= 0, "fork concurrent reader");
		if (!children[i]) {
			char byte;
			close(start[1]);
			check(read(start[0], &byte, 1) == 1, "wait for concurrent read");
			close(start[0]);
			expect_file(i ? "/mnt/parallel-b" : "/mnt/parallel-a", "concurrent\n");
			_exit(0);
		}
	}
	close(start[0]);
	check(write(start[1], "go", 2) == 2, "start concurrent readers");
	close(start[1]);
	for (int i = 0; i < 2; i++)
		check(waitpid(children[i], &status, 0) == children[i] &&
		      WIFEXITED(status) && !WEXITSTATUS(status), "concurrent reader completed");
	check(*requests == before + 1, "concurrent aliases download their blob once");
}

static void setup_network(void)
{
	struct ifreq interface = { .ifr_name = "eth0" };
	struct sockaddr_in *address = (struct sockaddr_in *)&interface.ifr_addr;
	struct rtentry route = {};
	int fd = open("/e1000.ko", O_RDONLY), socket_fd;

	check(fd >= 0 && !syscall(SYS_finit_module, fd, "", 0), "load guest NIC driver");
	close(fd);
	socket_fd = socket(AF_INET, SOCK_DGRAM, 0);
	check(socket_fd >= 0, "network configuration socket");
	address->sin_family = AF_INET;
	inet_pton(AF_INET, "10.0.2.15", &address->sin_addr);
	check(!ioctl(socket_fd, SIOCSIFADDR, &interface), "guest IPv4 address");
	inet_pton(AF_INET, "255.255.255.0", &address->sin_addr);
	check(!ioctl(socket_fd, SIOCSIFNETMASK, &interface), "guest IPv4 mask");
	interface.ifr_flags = IFF_UP | IFF_RUNNING;
	check(!ioctl(socket_fd, SIOCSIFFLAGS, &interface), "guest NIC up");
	((struct sockaddr_in *)&route.rt_dst)->sin_family = AF_INET;
	((struct sockaddr_in *)&route.rt_genmask)->sin_family = AF_INET;
	address = (struct sockaddr_in *)&route.rt_gateway;
	address->sin_family = AF_INET;
	inet_pton(AF_INET, "10.0.2.2", &address->sin_addr);
	route.rt_flags = RTF_UP | RTF_GATEWAY;
	route.rt_dev = "eth0";
	check(!ioctl(socket_fd, SIOCADDRT, &route), "guest default route");
	close(socket_fd);
	sleep(1);
}

static void set_network_up(int up)
{
	struct ifreq interface = { .ifr_name = "eth0" };
	struct rtentry route = {};
	int fd = socket(AF_INET, SOCK_DGRAM, 0);

	check(fd >= 0, "network state socket");
	check(!ioctl(fd, SIOCGIFFLAGS, &interface), "read NIC flags");
	if (up)
		interface.ifr_flags |= IFF_UP;
	else
		interface.ifr_flags &= ~IFF_UP;
	check(!ioctl(fd, SIOCSIFFLAGS, &interface), up ? "restore NIC" : "take NIC offline");
	if (up) {
		((struct sockaddr_in *)&route.rt_dst)->sin_family = AF_INET;
		((struct sockaddr_in *)&route.rt_genmask)->sin_family = AF_INET;
		struct sockaddr_in *gateway = (struct sockaddr_in *)&route.rt_gateway;
		gateway->sin_family = AF_INET;
		inet_pton(AF_INET, "10.0.2.2", &gateway->sin_addr);
		route.rt_flags = RTF_UP | RTF_GATEWAY;
		route.rt_dev = "eth0";
		check(!ioctl(fd, SIOCADDRT, &route) || errno == EEXIST, "restore default route");
		unsigned attempt;
		for (attempt = 0; attempt < 500; attempt++) {
			if (ioctl(fd, SIOCGIFFLAGS, &interface))
				check(0, "read restored NIC carrier");
			if (interface.ifr_flags & IFF_RUNNING)
				break;
			usleep(10000);
		}
		printf("guest NIC carrier after %u polls\n", attempt);
		check(interface.ifr_flags & IFF_RUNNING, "restored NIC carrier ready");
	}
	close(fd);
}

static void expect_prefix(const char *path, const char *prefix)
{
	char buffer[128];
	int fd = open(path, O_RDONLY);
	ssize_t size;

	check(fd >= 0, path);
	size = read(fd, buffer, sizeof(buffer));
	check(size >= (ssize_t)strlen(prefix) && !memcmp(buffer, prefix, strlen(prefix)),
	      "checkout file prefix");
	close(fd);
}

static unsigned list_directory(const char *path)
{
	DIR *directory = opendir(path);
	struct dirent *entry;
	unsigned count = 0;

	check(directory != NULL, path);
	errno = 0;
	while ((entry = readdir(directory))) {
		if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, ".."))
			continue;
		printf("entry: %s/%s\n", path, entry->d_name);
		count++;
	}
	check(!errno, "directory iteration reaches EOF");
	rewinddir(directory);
	unsigned repeated = 0;
	while ((entry = readdir(directory)))
		if (strcmp(entry->d_name, ".") && strcmp(entry->d_name, ".."))
			repeated++;
	check(!errno && repeated == count, "rewind directory");
	closedir(directory);
	return count;
}

static void content_tests(const char *root)
{
	char path[256], target[64], bytes[16];
	struct stat st;
	ssize_t length;
	int fd;

	snprintf(path, sizeof(path), "%s/binary", root);
	fd = open(path, O_RDONLY);
	check(fd >= 0, "open binary contents");
	check(read(fd, bytes, sizeof(bytes)) == 10 && !memcmp(bytes, "\0binary\xff\r\n", 10),
	      "file bytes preserve NUL, non-UTF-8 and CRLF");
	close(fd);
	snprintf(path, sizeof(path), "%s/empty-file", root);
	check(!stat(path, &st) && S_ISREG(st.st_mode) && !st.st_size, "empty blob is a regular file");
	snprintf(path, sizeof(path), "%s/dir/parent-link", root);
	length = readlink(path, target, sizeof(target));
	check(length == 12 && !memcmp(target, "../README.md", 12), "relative parent symlink bytes");
	expect_file(path, "hello from kernel gitfs\n");
	snprintf(path, sizeof(path), "%s/dir-link/note.txt", root);
	expect_file(path, "nested\n");
	snprintf(path, sizeof(path), "%s/dangling", root);
	check(!lstat(path, &st) && S_ISLNK(st.st_mode), "dangling Git symlink is visible");
	check(stat(path, &st) == -1 && errno == ENOENT, "dangling symlink follows Linux lookup");
	snprintf(path, sizeof(path), "%s/loop", root);
	length = readlink(path, target, sizeof(target));
	check(length == 4 && !memcmp(target, "loop", 4), "cyclic symlink target is readable");
	check(stat(path, &st) == -1 && errno == ELOOP, "Linux bounds cyclic symlink following");
}

static void https_tests(void)
{
	char port[16] = {}, url[256], buffer[128], options[160];
	struct stat st;
	int fd = open("/fixture/tls-port", O_RDONLY);
	unsigned before;

	check(fd >= 0 && read(fd, port, sizeof(port) - 1) > 0, "test TLS origin port");
	close(fd);
	setup_network();
	snprintf(url, sizeof(url), "https://localhost:%s/smart.git", port);
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY,
		     "checkout,addr=10.0.2.2,timeout_ms=60000"),
	      "mount no_std HTTPS snapshot with a self-signed, mismatched certificate");
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	expect_file("/mnt/dir/note.txt", "nested\n");
	expect_big("/mnt/big-a");
	expect_big("/mnt/big-b");
	expect_big("/mnt/big-c");
	check(!stat("/mnt/run", &st) && (st.st_mode & 0111), "HTTPS executable mode");
	set_network_up(0);
	content_tests("/mnt");
	set_network_up(1);
	missing_probes();
	check(!umount("/mnt"), "unmount HTTPS snapshot");
	snprintf(url, sizeof(url), "https://localhost:%s/restricted.git", port);
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "checkout,addr=10.0.2.2,timeout_ms=60000"),
	      "mount origin without ofs-delta or no-progress capabilities");
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	check(!umount("/mnt"), "unmount restricted-capability origin");
	snprintf(url, sizeof(url), "https://localhost:%s/headless.git", port);
	check(mount(url, "/mnt", "gitfs", MS_RDONLY, "checkout,addr=10.0.2.2,timeout_ms=60000") == -1 && errno == ENOENT,
	      "single-snapshot mount requires HEAD without an explicit commit");
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "refs,addr=10.0.2.2,timeout_ms=60000"),
	      "mount refs with an unborn default branch and no advertised HEAD");
	expect_file("/mnt/main/msg", "current snapshot\n");
	expect_file("/mnt/old/tree/README.md", "old revision\n");
	check(stat("/mnt/.refs/HEAD", &st) == -1 && errno == ENOENT, "headless refs do not invent HEAD");
	check(!umount("/mnt"), "unmount headless refs view");
	snprintf(url, sizeof(url), "https://localhost:%s/sha256.git", port);
	check(mount(url, "/mnt", "gitfs", MS_RDONLY, "refs,addr=10.0.2.2,timeout_ms=60000") == -1 && errno == EOPNOTSUPP,
	      "reject SHA-256 discovery before copying IDs into the SHA-1 kernel ABI");
	snprintf(url, sizeof(url), "https://localhost:%s/compressed.git", port);
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY,
		     "checkout,addr=10.0.2.2,object_mb=32,timeout_ms=60000"), "mount maximally compressed smart object");
	fd = open("/mnt/repeated.bin", O_RDONLY);
	check(fd >= 0 && !fstat(fd, &st) && st.st_size == 32*1024*1024, "compressed smart object fits its absolute body budget");
	check(read(fd, buffer, sizeof(buffer)) == (ssize_t)sizeof(buffer), "read compressed smart object");
	int correct = 1;
	for (size_t i = 0; i < sizeof(buffer); i++)
		correct &= buffer[i] == 'a';
	check(correct, "compressed smart bytes");
	close(fd);
	check(!umount("/mnt"), "unmount compressed smart object");
	snprintf(url, sizeof(url), "https://localhost:%s/manyrefs.git", port);
	check(mount(url, "/mnt", "gitfs", MS_RDONLY, "checkout,addr=10.0.2.2,pack_mb=1,timeout_ms=60000") == -1 && errno == EFBIG,
	      "discovery respects a small configured response budget");
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "checkout,addr=10.0.2.2,pack_mb=2,timeout_ms=60000"),
	      "mount discovery larger than 1 MiB with pack_mb=2");
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	check(!umount("/mnt"), "unmount many-ref origin");
	snprintf(url, sizeof(url), "https://localhost:%s/smart.git", port);
	check(mount(url, "/mnt", "gitfs", MS_RDONLY,
		    "addr=10.0.2.2,commit=0123456789abcdef0123456789abcdef01234567,timeout_ms=60000") == -1 && errno == EINVAL,
	      "reject an unadvertised smart want before upload-pack");
	check(mount(url, "/mnt", "gitfs", MS_RDONLY,
		    "refs,addr=10.0.2.2,timeout_ms=60000,cache_mb=2,object_mb=1") == -1 &&
	      errno == EFBIG, "refs snapshot rejects an insufficient cache budget");
	check(mount(url, "/mnt", "gitfs", MS_RDONLY,
		    "refs,addr=10.0.2.2,commit=0123456789abcdef0123456789abcdef01234567") == -1 &&
	      errno == EINVAL, "refs view rejects an explicit single commit");
	const char *invalid_options[] = {
		"refs,ref=main", "ref=main,view=refs", "refs,checkout", "checkout,refs",
		"view=unknown", "ref=../escape", "ref=main,commit=0123456789abcdef0123456789abcdef01234567",
	};
	for (size_t i = 0; i < sizeof(invalid_options)/sizeof(invalid_options[0]); i++)
		check(mount(url, "/mnt", "gitfs", MS_RDONLY, invalid_options[i]) == -1 && errno == EINVAL,
		      "reject conflicting views or invalid selectors before network access");
	check(mount(url, "/mnt", "gitfs", MS_RDONLY, "ref=missing,addr=10.0.2.2,timeout_ms=60000") == -1 && errno == ENOENT,
	      "unknown advertised branch is not silently replaced with HEAD");
	fd = open("/fixture/smart-tag-revision", O_RDONLY);
	check(fd >= 0 && read(fd, buffer, 40) == 40, "annotated tag ID");
	close(fd);
	buffer[40] = 0;
	snprintf(options, sizeof(options), "commit=%.40s,addr=10.0.2.2,timeout_ms=60000", buffer);
	check(mount(url, "/mnt", "gitfs", MS_RDONLY, options) == -1 && errno == EBADMSG,
	      "commit selector still requires a commit rather than an annotated tag");
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "ref=old,addr=10.0.2.2,timeout_ms=60000"), "select branch directly at root");
	expect_file("/mnt/README.md", "old revision\n");
	check(!umount("/mnt"), "unmount selected old branch");
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "view=checkout,ref=topic/demo,addr=10.0.2.2,timeout_ms=60000"), "select nested branch name");
	expect_file("/mnt/README.md", "old revision\n");
	check(!umount("/mnt"), "unmount selected nested branch");
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "ref=refs/tags/v1,addr=10.0.2.2,timeout_ms=60000"), "checkout annotated tag through shared traversal");
	set_network_up(0);
	content_tests("/mnt");
	check(!umount("/mnt"), "unmount offline annotated tag checkout");
	set_network_up(1);
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY, "ref=refs/tags/tree,addr=10.0.2.2,timeout_ms=60000"), "checkout direct tree tag");
	expect_file("/mnt/dir/note.txt", "nested\n");
	check(!umount("/mnt"), "unmount direct tree checkout");
	check(!mount(url, "/mnt", "gitfs", MS_RDONLY,
		     "addr=10.0.2.2,timeout_ms=60000"), "mount default pinned refs view");
	set_network_up(0);
	expect_file("/mnt/main/msg", "current snapshot\n");
	expect_file("/mnt/main/tree/README.md", "hello from kernel gitfs\n");
	expect_file("/mnt/main/tree/dir/note.txt", "nested\n");
	expect_big("/mnt/main/tree/big-c");
	expect_file("/mnt/alias/msg", "current snapshot\n");
	expect_file("/mnt/old/msg", "old revision\n");
	expect_file("/mnt/old/tree/README.md", "old revision\n");
	expect_file("/mnt/topic/demo/msg", "old revision\n");
	expect_file("/mnt/.refs/HEAD/msg", "current snapshot\n");
	expect_file("/mnt/.refs/tags/v1/msg", "current snapshot\n");
	expect_file("/mnt/.refs/tags/tree/tree/dir/note.txt", "nested\n");
	expect_file("/mnt/.refs/tags/blob/tree", "hello from kernel gitfs\n");
	check(stat("/mnt/.refs/tags/blob/msg", &st) == -1 && errno == ENOENT,
	      "non-commit ref has no commit message");
	check(!stat("/mnt/main/tree/run", &st) && (st.st_mode & 0111), "refs executable mode");
	expect_file("/mnt/main/tree/link", "hello from kernel gitfs\n");
	content_tests("/mnt/main/tree");
	check(list_directory("/mnt") == 5, "list refs and namespace directories");
	check(list_directory("/mnt/main") == 2, "list commit message and tree");
	check(list_directory("/mnt/topic") == 1, "list nested ref namespace");
	missing_probes();
	check(!umount("/mnt"), "unmount offline refs view");
	set_network_up(1);
	if (!access("/fixture/test-github", F_OK)) {
		check(!mount("https://github.com/octocat/Hello-World", "/mnt", "gitfs", MS_RDONLY,
			     "view=checkout,timeout_ms=60000"),
		      "mount live GitHub repository through kernel DNS and no_std HTTPS");
		fd = open("/mnt/README", O_RDONLY);
		check(fd >= 0, "GitHub checkout README");
		before = read(fd, buffer, sizeof(buffer));
		close(fd);
		check(before == 13 && !memcmp(buffer, "Hello World!\n", 13), "GitHub checkout bytes");
		missing_probes();
		check(!umount("/mnt"), "unmount GitHub checkout");
		check(!mount("https://github.com/no-std-rs/git-zero", "/mnt", "gitfs", MS_RDONLY,
			     "timeout_ms=60000"), "mount default git-zero refs from GitHub");
		set_network_up(0);
		expect_prefix("/mnt/main/tree/README.md", "# git-zero\n");
		expect_prefix("/mnt/main/tree/src/lib.rs", "//! Allocation-free");
		expect_prefix("/mnt/main/tree/crates/git-zero-core/src/lib.rs", "//! Allocation-free Git objects");
		fd = open("/mnt/main/msg", O_RDONLY);
		check(fd >= 0, "git-zero main commit message");
		ssize_t size = read(fd, buffer, sizeof(buffer) - 1);
		close(fd);
		check(size > 0, "git-zero commit message is nonempty");
		buffer[size] = 0;
		printf("git-zero main commit: %s\n", buffer);
		check(list_directory("/mnt") > 1, "list git-zero branch refs");
		check(list_directory("/mnt/main/tree") > 1, "list git-zero tree objects");
		check(list_directory("/mnt/main/tree/crates/git-zero-core/src") > 1, "list git-zero subtree objects");
		missing_probes();
		check(!umount("/mnt"), "unmount git-zero refs");
		set_network_up(1);
	}
}

int main(void)
{
	struct ifreq interface = { .ifr_name = "lo", .ifr_flags = IFF_UP | IFF_RUNNING };
	struct stat st;
	char target[64], revision[41] = {}, options[128], data[128];
	int socket_fd, module, fd, length;
	pid_t server;
	unsigned before;

	setbuf(stdout, NULL);
	mkdir("/proc", 0755);
	check(!mount("proc", "/proc", "proc", 0, NULL), "mount proc");
	mkdir("/mnt", 0755);
	requests = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	check(requests != MAP_FAILED, "shared origin counter");
	socket_fd = socket(AF_INET, SOCK_STREAM, 0);
	check(socket_fd >= 0 && !ioctl(socket_fd, SIOCSIFFLAGS, &interface), "bring loopback up");
	close(socket_fd);
	server = origin();
	module = open("/gitfs.ko", O_RDONLY);
	check(module >= 0 && !syscall(SYS_finit_module, module, "", 0), "load kernel module");
	close(module);
	check(!mount("http://127.0.0.1:8080/repo.git", "/mnt", "gitfs", MS_RDONLY, "checkout,error_ttl_ms=100"), "mount HEAD");
	check(*requests == 4, "mount fetches HEAD, branch, commit and root tree only");
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	before = *requests;
	expect_file("/mnt/alias.txt", "hello from kernel gitfs\n");
	check(*requests == before, "identical blobs share the object cache");
	concurrent_reads();
	expect_file("/mnt/dir/note.txt", "nested\n");
	length = readlink("/mnt/link", target, sizeof(target));
	check(length == 9 && !memcmp(target, "README.md", 9), "symlink target");
	check(!stat("/mnt/run", &st) && (st.st_mode & 0111) == 0111, "executable mode");
	check(!stat("/mnt/empty", &st) && S_ISDIR(st.st_mode), "empty tree");
	check(!stat("/mnt/submodule", &st) && S_ISDIR(st.st_mode), "opaque submodule directory");
	{
		DIR *dir = opendir("/mnt");
		unsigned entries = 0;
		check(dir != NULL, "open root directory");
		while (readdir(dir)) entries++;
		closedir(dir);
		check(entries == 15, "readdir includes entries and dot entries");
	}
	missing_probes();
	before = *requests;
	check(stat("/mnt/unavailable.txt", &st) == -1 && errno == ENOENT, "missing referenced object");
	for (int i = 0; i < 10; i++)
		check(stat("/mnt/unavailable.txt", &st) == -1 && errno == ENOENT, "cached origin failure");
	check(*requests == before + 1, "failed object GET cached");
	usleep(150000);
	check(stat("/mnt/unavailable.txt", &st) == -1 && *requests == before + 2, "failure retry after TTL");
	check(open("/mnt/created", O_WRONLY | O_CREAT, 0644) == -1 && errno == EROFS, "writes rejected");
	check(mount(NULL, "/mnt", NULL, MS_REMOUNT, NULL) == -1 && errno == EROFS, "read-write remount rejected");
	fd = open("/fixture/repo.git/refs/heads/main", O_WRONLY | O_TRUNC);
	check(fd >= 0, "update upstream branch");
	memset(data, '0', 40);
	check(write(fd, data, 40) == 40, "move upstream HEAD");
	close(fd);
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	check(!kill(server, SIGTERM), "stop origin");
	waitpid(server, NULL, 0);
	before = *requests;
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	expect_file("/mnt/dir/note.txt", "nested\n");
	missing_probes();
	check(*requests == before, "warm reads and misses work offline");
	check(!umount("/mnt"), "unmount");
	/* Restart the origin and select the original immutable commit explicitly. */
	server = origin();
	fd = open("/fixture/revision", O_RDONLY);
	check(fd >= 0 && read(fd, revision, 40) == 40, "read pinned revision");
	close(fd);
	snprintf(options, sizeof(options), "commit=%s,cache_mb=2,object_mb=1", revision);
	before = *requests;
	check(!mount("http://127.0.0.1:8080/repo.git", "/mnt", "gitfs", MS_RDONLY,
		     "ref=snapshot"), "select branch over ordinary HTTP");
	check(*requests == before + 3, "named loose checkout skips HEAD and fetches ref, commit and tree");
	expect_file("/mnt/README.md", "hello from kernel gitfs\n");
	check(!umount("/mnt"), "unmount named loose checkout");
	before = *requests;
	check(!mount("http://127.0.0.1:8080/repo.git", "/mnt", "gitfs", MS_RDONLY, options), "mount pinned commit");
	check(*requests == before + 2, "explicit commit skips ref roundtrips");
	expect_big("/mnt/big-a");
	expect_big("/mnt/big-b");
	expect_big("/mnt/big-c");
	before = *requests;
	expect_big("/mnt/big-a");
	check(*requests > before, "LRU eviction refetches a verified blob");
	check(!umount("/mnt"), "unmount after eviction");
	for (int i = 0; i < 3; i++) {
		const char *urls[] = { "http://127.0.0.1:8080/corrupt.git", "http://127.0.0.1:8080/truncated.git", "http://127.0.0.1:8080/chunked.git" };
		int errors[] = { EBADMSG, EBADMSG, EBADMSG };
		check(mount(urls[i], "/mnt", "gitfs", MS_RDONLY, options) == -1 && errno == errors[i],
		      "reject malformed HTTP or object with expected errno");
	}
	check(mount("ftp://127.0.0.1/repo.git", "/mnt", "gitfs", MS_RDONLY, options) == -1 && errno == EPROTONOSUPPORT, "reject unsupported URL");
	check(mount("http://127.0.0.1:8080/repo.git", "/mnt", "gitfs", MS_RDONLY, "cache_mb=1,object_mb=1") == -1 && errno == EINVAL, "reject undersized cache");
	memset(revision, 0, sizeof(revision));
	fd = open("/fixture/large-revision", O_RDONLY);
	check(fd >= 0 && read(fd, revision, 40) == 40, "large loose-object revision");
	close(fd);
	snprintf(options, sizeof(options), "commit=%s,cache_mb=64,object_mb=32,timeout_ms=60000", revision);
	check(!mount("http://127.0.0.1:8080/repo.git", "/mnt", "gitfs", MS_RDONLY, options), "mount large loose object");
	fd = open("/mnt/large.bin", O_RDONLY);
	check(fd >= 0 && !fstat(fd, &st) && st.st_size == 32*1024*1024, "incompressible object fits configured body limit");
	check(read(fd, data, 5) == 5 && !memcmp(data, "first", 5), "large object prefix");
	check(lseek(fd, -4, SEEK_END) == st.st_size-4 && read(fd, data, 4) == 4 && !memcmp(data, "last", 4), "large object suffix");
	close(fd);
	check(!umount("/mnt"), "unmount large loose object");
	memset(revision, 0, sizeof(revision));
	fd = open("/fixture/repeated-revision", O_RDONLY);
	check(fd >= 0 && read(fd, revision, 40) == 40, "compressed loose-object revision");
	close(fd);
	snprintf(options, sizeof(options), "commit=%s,cache_mb=64,object_mb=32,timeout_ms=60000", revision);
	check(!mount("http://127.0.0.1:8080/repo.git", "/mnt", "gitfs", MS_RDONLY, options), "mount maximally compressed loose object");
	fd = open("/mnt/repeated.bin", O_RDONLY);
	check(fd >= 0 && !fstat(fd, &st) && st.st_size == 32*1024*1024, "compressed loose object fits its absolute body budget");
	check(lseek(fd, -4, SEEK_END) == st.st_size-4 && read(fd, data, 4) == 4 && !memcmp(data, "aaaa", 4), "compressed loose object suffix");
	close(fd);
	check(!umount("/mnt"), "unmount compressed loose object");
	kill(server, SIGTERM);
	waitpid(server, NULL, 0);
	https_tests();
	check(!syscall(SYS_delete_module, "gitfs", O_NONBLOCK), "unload module");
	puts("GITFS_TEST_PASS");
	reboot(RB_POWER_OFF);
	return 0;
}
