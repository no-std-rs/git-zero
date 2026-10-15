"""Independent MIT OR Apache-2.0 CGI adapter; no Git test bodies are copied."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import NamedTuple

from interop import environment
from server_fixture import export_fixture
from upstream_http import git, object_ids


BODY_LIMIT = 65_536
OBJECT_LIMIT = 256
BYTE_LIMIT = 8 * 1024 * 1024


class CgiRequest(NamedTuple):
    method: str
    path: str
    query: str
    git_protocol: str
    content_type: str


def request(config, env, stream):
    document = Path(env["DOCUMENT_ROOT"]).resolve()
    if document != Path(config["document_root"]).resolve():
        raise ValueError("unexpected Apache document root")
    path = env["PATH_INFO"]
    if path not in ("/repo.git/info/refs", "/repo.git/git-upload-pack"):
        raise ValueError("unsupported CGI path")
    repo = (document / "repo.git").resolve()
    if repo.parent != document:
        raise ValueError("published repository escapes document root")
    method = env["REQUEST_METHOD"]
    length = int(env.get("CONTENT_LENGTH") or "0")
    if method not in ("GET", "POST") or not 0 <= length <= BODY_LIMIT:
        raise ValueError("unsupported method or request body limit")
    body = stream.read(length)
    if len(body) != length:
        raise ValueError("truncated CGI body")
    args = CgiRequest(method, path.removeprefix("/repo.git/"), env.get("QUERY_STRING", ""),
                      env.get("HTTP_GIT_PROTOCOL", ""), env.get("CONTENT_TYPE", ""))
    return repo, args, body


def export_bounded(root, repo, env):
    ids = object_ids(repo, "--all", "HEAD", env=env)
    if not 0 < len(ids) <= OBJECT_LIMIT:
        raise ValueError("fixture object count exceeds limit")
    sizes = git(repo, "cat-file", "--batch-check=%(objectsize)",
                data=b"\n".join(ids) + b"\n", env=env).splitlines()
    if len(sizes) != len(ids) or sum(map(int, sizes)) > BYTE_LIMIT:
        raise ValueError("fixture object bytes exceed limit")
    refs = git(repo, "for-each-ref", "--format=%(refname)", env=env)
    if len(refs.splitlines()) > 32:
        raise ValueError("fixture ref count exceeds limit")
    manifest = export_fixture(root, repo, env)
    if set(manifest) != set(ids):
        raise ValueError("fixture changed while exporting")
    return manifest


def attested_response(config, root, args, body):
    result = subprocess.run([config["native"], str(root), *args], input=body,
                            capture_output=True, timeout=20, check=True)
    proof = f"git-zero-upstream-cgi: served {args.method} {args.path}\n".encode()
    if result.stderr != proof or not result.stdout.startswith(b"Status: 200 OK\r\n"):
        raise ValueError("native encoder did not attest successful request")
    if len(result.stdout) > BYTE_LIMIT + 4096:
        raise ValueError("CGI response exceeds limit")
    return result.stdout


def append_trace(path, entry):
    data = (json.dumps(entry, sort_keys=True) + "\n").encode()
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600),
                   "wb", buffering=0) as trace:
        if trace.write(data) != len(data):
            raise OSError("short CGI trace write")


def serve(config, env, stream):
    repo, args, body = request(config, env, stream)
    git_env = environment(config["git"])
    with tempfile.TemporaryDirectory(prefix="request-", dir=config["snapshots"]) as temp:
        root = Path(temp)
        manifest = export_bounded(root, repo, git_env)
        response = attested_response(config, root, args, body)
        entry = {"backend": "git-zero", "native_sha256": config["native_sha256"],
                 "method": args.method, "path": args.path, "protocol": args.git_protocol,
                 "head": (root / "head.txt").read_text().splitlines()[0],
                 "objects": len(manifest), "body_bytes": len(body),
                 "response_bytes": len(response)}
        append_trace(config["trace"], entry)
        return response


def main(config_path):
    try:
        config = json.loads(Path(config_path).read_text())
        sys.stdout.buffer.write(serve(config, os.environ, sys.stdin.buffer))
    except Exception as error:
        print(f"git-zero CGI failed: {error}", file=sys.stderr)
        sys.stdout.buffer.write(b"Status: 500 Internal Server Error\r\nContent-Type: text/plain\r\n\r\nCGI adapter failed\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
