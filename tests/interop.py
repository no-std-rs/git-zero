#!/usr/bin/env python3
"""Real Git client/server matrix. Standard library only; safe to run concurrently."""
import argparse
import concurrent.futures
import contextlib
import functools
import hashlib
import http.server
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import threading
import urllib.parse
import zlib


def run(*args, cwd=None, env=None, data=None):
    result = subprocess.run(args, cwd=cwd, env=env, input=data, capture_output=True, timeout=30)
    if result.returncode:
        raise AssertionError(f"{args!r}:\n{result.stdout.decode(errors='replace')}\n{result.stderr.decode(errors='replace')}")
    return result.stdout


def environment():
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update({
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0", "GIT_AUTHOR_NAME": "Fixture", "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_NAME": "Fixture", "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z",
    })
    return env


def fixture(root, env):
    source = root / "source"
    run("git", "init", "-q", "--object-format=sha1", "-b", "main", str(source), env=env)
    (source / "README.md").write_text("real Git fixture\n")
    # A multi-block blob exercises pack size encoding and the zlib boundary.
    (source / "large.txt").write_bytes(b"allocation-free git\n" * 8000)
    run("git", "-C", str(source), "add", ".", env=env)
    run("git", "-C", str(source), "commit", "-qm", "root", env=env)
    (source / "second.txt").write_text("second commit\n")
    run("git", "-C", str(source), "add", ".", env=env)
    run("git", "-C", str(source), "commit", "-qm", "second", env=env)
    # Add a different branch and tag to exercise discovery beyond one snapshot ref.
    run("git", "-C", str(source), "branch", "other", "HEAD~1", env=env)
    run("git", "-C", str(source), "tag", "v1", env=env)
    bare = root / "repo.git"
    run("git", "clone", "-q", "--bare", "--no-hardlinks", str(source), str(bare), env=env)
    # Dumb HTTP fixture intentionally uses loose objects, then publishes info/refs.
    run("git", "-C", str(bare), "update-server-info", env=env)
    head = run("git", "-C", str(bare), "rev-parse", "HEAD", env=env).strip().decode()
    return bare, head


@contextlib.contextmanager
def reference_server(root, env, smart):
    backend = Path(run("git", "--exec-path", env=env).strip().decode()) / "git-http-backend"

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if smart:
                self.cgi()
            else:
                super().do_GET()

        def do_POST(self):
            if smart:
                self.cgi()
            else:
                self.send_error(405)

        def cgi(self):
            url = urllib.parse.urlsplit(self.path)
            length = int(self.headers.get("Content-Length", "0"))
            cgi_env = dict(env, GIT_PROJECT_ROOT=str(root), GIT_HTTP_EXPORT_ALL="1",
                           PATH_INFO=url.path, QUERY_STRING=url.query, REQUEST_METHOD=self.command,
                           CONTENT_TYPE=self.headers.get("Content-Type", ""), CONTENT_LENGTH=str(length),
                           REMOTE_ADDR=self.client_address[0])
            protocol = self.headers.get("Git-Protocol")
            if protocol:
                cgi_env["HTTP_GIT_PROTOCOL"] = protocol
            try:
                response = run(str(backend), env=cgi_env, data=self.rfile.read(length))
                header, body = response.split(b"\r\n\r\n", 1)
                fields = [line.decode().split(":", 1) for line in header.split(b"\r\n")]
                status = next((int(value.strip().split()[0]) for name, value in fields if name.lower() == "status"), 200)
                self.send_response(status)
                for name, value in fields:
                    if name.lower() not in ("status", "content-length", "connection"):
                        self.send_header(name, value.strip())
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)
            except Exception as error:
                self.send_error(500, str(error))

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(root)))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/repo.git"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@contextlib.contextmanager
def zero_server(binary, env):
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen([binary, "serve", "0"], stdout=subprocess.PIPE, stderr=errors, env=env)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                if not selector.select(timeout=15):
                    raise AssertionError("git-zero server did not report a bound port")
            url = process.stdout.readline().decode().strip()
            if not url.startswith("http://127.0.0.1:"):
                errors.seek(0)
                raise AssertionError(errors.read().decode())
            yield url
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            process.stdout.close()


def verify_pack(root, env, pack, head):
    target = root / "download.git"
    run("git", "init", "-q", "--bare", "--object-format=sha1", str(target), env=env)
    run("git", "-C", str(target), "index-pack", "--strict", "--stdin", env=env, data=pack)
    run("git", "-C", str(target), "update-ref", "refs/heads/main", head, env=env)
    run("git", "-C", str(target), "fsck", "--strict", env=env)
    run("git", "-C", str(target), "cat-file", "-e", f"{head}:README.md", env=env)


def case(name, binary):
    with tempfile.TemporaryDirectory(prefix=f"git-zero-{name}-") as directory:
        root = Path(directory)
        env = environment()
        client, server, protocol = name.split("-")
        smart = protocol == "smart"
        if server == "git":
            _, head = fixture(root, env)
            context = reference_server(root, env, smart)
        else:
            context = zero_server(binary, env)
            head = None
        with context as url:
            client_env = dict(env)
            if not smart and server == "zero":
                client_env["GIT_SMART_HTTP"] = "0"
            if client == "git":
                clone = root / "clone"
                run("git", "clone", "-q", url, str(clone), env=client_env)
                run("git", "-C", str(clone), "fsck", "--strict", env=env)
                run("git", "-C", str(clone), "fetch", "origin", env=client_env)
                content = (clone / "README.md").read_bytes()
                expected = b"real Git fixture\n" if server == "git" else b"Hello from an allocation-free in-memory Git repository.\n"
                assert content == expected
                if server == "zero":
                    assert run("git", "-C", str(clone), "rev-list", "--count", "HEAD", env=env).strip() == b"1"
                    assert run("git", "-C", str(clone), "symbolic-ref", "--short", "HEAD", env=env).strip() == b"main"
            else:
                # For zero-to-zero dumb mode use an HTTP proxy serving only loose
                # responses; the native server also supports smart discovery.
                if not smart and server == "zero":
                    return zero_dumb_client(root, env, binary, url)
                refs = run(binary, "ls-remote", url, env=env)
                if head is None:
                    head = refs.splitlines()[0].split()[0].decode()
                assert head.encode() in refs
                data = run(binary, "fetch", url, env=env)
                if smart:
                    verify_pack(root, env, data, head)
                else:
                    loose = zlib.decompress(data)
                    assert hashlib.sha1(loose).hexdigest() == head
                    assert loose.startswith(b"commit ")


def zero_dumb_client(root, env, binary, upstream):
    # Force the discovery response to use dumb info/refs, preserving the actual
    # rustix server for every object/HEAD request.
    import urllib.request

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            path = urllib.parse.urlsplit(self.path).path.removeprefix("/repo.git/")
            with urllib.request.urlopen(f"{upstream}/{path}", timeout=10) as response:
                body = response.read()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    thread = threading.Thread(target=proxy.serve_forever, daemon=True)
    thread.start()
    try:
        url = f"http://127.0.0.1:{proxy.server_port}/repo.git"
        refs = run(binary, "ls-remote", url, env=env)
        head = refs.splitlines()[0].split()[0].decode()
        data = run(binary, "fetch", url, env=env)
        loose = zlib.decompress(data)
        assert hashlib.sha1(loose).hexdigest() == head
        assert loose.startswith(b"commit ")
    finally:
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(Path(__file__).resolve().parents[1] / "target/debug/git-zero"))
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--case", action="append", help="Run selected matrix case(s)")
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    cases = args.case or [f"{client}-{server}-{protocol}" for client in ("git", "zero") for server in ("git", "zero") for protocol in ("smart", "dumb")]
    failures = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = {executor.submit(case, name, str(Path(args.binary).resolve())): name for name in cases}
        for future in concurrent.futures.as_completed(futures):
            name = futures[future]
            try:
                future.result()
                print(f"PASS {name}", flush=True)
            except Exception as error:
                failures.append(name)
                print(f"FAIL {name}: {error}", flush=True)
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
