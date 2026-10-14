"""A local smart-Git HTTPS origin used only by the QEMU tests."""
import contextlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import ssl
import subprocess
import threading
from urllib.parse import urlsplit


def command(*args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, **kwargs).stdout


def fixture(root):
    source = root.parent / "smart-source"
    source.mkdir()
    command("git", "init", "--initial-branch=main", str(source))
    env = {**os.environ, "GIT_AUTHOR_NAME": "gitfs test",
           "GIT_AUTHOR_EMAIL": "test@example.invalid",
           "GIT_COMMITTER_NAME": "gitfs test",
           "GIT_COMMITTER_EMAIL": "test@example.invalid"}
    (source / "README.md").write_text("old revision\n")
    command("git", "-C", str(source), "add", ".", env=env)
    command("git", "-C", str(source), "commit", "-m", "old revision", env=env)
    (source / "README.md").write_text("hello from kernel gitfs\n")
    (source / "dir").mkdir()
    (source / "dir/note.txt").write_text("nested\n")
    (source / "run").write_text("#!/bin/sh\n")
    (source / "run").chmod(0o755)
    (source / "link").symlink_to("README.md")
    for seed in range(3):
        (source / f"big-{chr(97+seed)}").write_bytes(
            bytes((i + seed) % 251 for i in range(700000)))
    command("git", "-C", str(source), "add", ".", env=env)
    command("git", "-C", str(source), "commit", "-m", "current snapshot", env=env)
    command("git", "-C", str(source), "branch", "old", "HEAD~1")
    command("git", "-C", str(source), "branch", "topic/demo", "HEAD~1")
    command("git", "-C", str(source), "branch", "alias", "HEAD")
    command("git", "-C", str(source), "tag", "-a", "v1", "-m", "annotated tag", env=env)
    command("git", "-C", str(source), "tag", "tree", "HEAD^{tree}")
    command("git", "-C", str(source), "tag", "blob", "HEAD:README.md")
    command("git", "clone", "--bare", str(source), str(root / "fixture/smart.git"))
    certs = root.parent / "certs"
    certs.mkdir()
    command("openssl", "req", "-x509", "-newkey", "ec",
            "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-days", "2",
            # Intentionally self-signed and mismatched: this example handshakes
            # without checking certificate trust or hostnames.
            "-subj", "/CN=fixture.invalid", "-addext", "subjectAltName=DNS:fixture.invalid",
            "-keyout", str(certs / "server.key"),
            "-out", str(certs / "server.pem"))
    return certs


@contextlib.contextmanager
def origin(root, certs):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def handle_git(self):
            target = urlsplit(self.path)
            restricted = target.path.startswith("/restricted.git/")
            many_refs = target.path.startswith("/manyrefs.git/")
            path = target.path
            if restricted or many_refs:
                path = "/smart.git/" + target.path.split("/", 2)[2]
            env = {**os.environ, "GIT_PROJECT_ROOT": str(root / "fixture"),
                   "GIT_HTTP_EXPORT_ALL": "1", "PATH_INFO": path,
                   "QUERY_STRING": target.query, "REQUEST_METHOD": self.command,
                   "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                   "CONTENT_LENGTH": self.headers.get("Content-Length", "0"),
                   "SERVER_PROTOCOL": "HTTP/1.1"}
            body = self.rfile.read(int(env["CONTENT_LENGTH"]))
            if restricted and any(cap in body for cap in (b" ofs-delta", b" no-progress")):
                self.send_error(400, "requested an unadvertised capability")
                return
            response = command("git", "http-backend", input=body, env=env)
            headers, payload = response.split(b"\r\n\r\n", 1)
            if self.command == "GET" and target.query == "service=git-upload-pack":
                first_length = int(payload[34:38], 16)
                first = payload[38:34+first_length]
                if restricted:
                    reference, capabilities = first.split(b"\0", 1)
                    capabilities = b" ".join(cap for cap in capabilities.split()
                                             if cap not in (b"ofs-delta", b"no-progress"))
                    first = reference + b"\0" + capabilities + b"\n"
                    payload = payload[:34] + f"{len(first)+4:04x}".encode() + first + payload[34+first_length:]
                if many_refs:
                    aliases = bytearray()
                    for i in range(16000):
                        ref = first[:40] + f" refs/heads/generated/ref-{i:05}\n".encode()
                        aliases.extend(f"{len(ref)+4:04x}".encode() + ref)
                    payload = payload[:-4] + aliases + b"0000"
                    assert len(payload) > 1024*1024
            status = 200
            for line in headers.split(b"\r\n"):
                if line.lower().startswith(b"status:"):
                    status = int(line.split()[1])
            self.send_response(status)
            for line in headers.split(b"\r\n"):
                name, value = line.split(b":", 1)
                if name.lower() not in (b"status", b"content-length"):
                    self.send_header(name.decode(), value.strip().decode())
            self.send_header("Transfer-Encoding", "chunked")
            self.send_header("Connection", "close")
            self.end_headers()
            # Fragment records/chunks and include extensions/trailers.
            for offset in range(0, len(payload), 193):
                chunk = payload[offset:offset+193]
                self.wfile.write(f"{len(chunk):x};test=fragment\r\n".encode()
                                 + chunk + b"\r\n")
            self.wfile.write(b"0\r\nX-Test-Trailer: done\r\n\r\n")
            self.wfile.flush()
            self.close_connection = True

        do_GET = handle_git
        do_POST = handle_git

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(certs / "server.pem", certs / "server.key")
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_port
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
