#!/usr/bin/env python3
"""Capture Cargo publish requests locally for Runnerless to relay to crates.io.

The local registry never forwards a publish request. Its sparse index mirrors
crates.io and adds packages captured earlier in this run, so packages can be
prepared using the same dependency index Cargo uses for crates.io.
"""

from contextlib import contextmanager
import hashlib
import hmac
import http.server
import json
import os
import re
from pathlib import Path
import secrets
import struct
import subprocess
import sys
import threading
import urllib.error
import urllib.request
from release_packages import PACKAGES, require


ROOT = Path(__file__).resolve().parents[1]
MAX_UPLOAD = 32 * 1024 * 1024
MAX_INDEX = 4 * 1024 * 1024


def index_path(name):
    if len(name) == 1:
        return "1/" + name
    if len(name) == 2:
        return "2/" + name
    if len(name) == 3:
        return "3/" + name[0] + "/" + name
    return name[:2] + "/" + name[2:4] + "/" + name


def parse_upload(body, expected_name, version):
    require(8 <= len(body) <= MAX_UPLOAD, "invalid Cargo upload size")
    metadata_size = struct.unpack_from("<I", body)[0]
    require(0 < metadata_size <= 128 * 1024 and len(body) >= 8 + metadata_size,
            "invalid Cargo metadata size")
    metadata = json.loads(body[4 : 4 + metadata_size])
    archive_size = struct.unpack_from("<I", body, 4 + metadata_size)[0]
    require(0 < archive_size <= MAX_UPLOAD and len(body) == 8 + metadata_size + archive_size,
            "invalid Cargo archive size")
    require(metadata.get("name") == expected_name and metadata.get("vers") == version,
            "Cargo package identity differs from release")
    archive = body[8 + metadata_size :]
    dependencies = [
        {
            "name": item.get("explicit_name_in_toml") or item["name"],
            "package": item["name"] if item.get("explicit_name_in_toml") else None,
            "req": item["version_req"],
            "features": item["features"],
            "optional": item["optional"],
            "default_features": item["default_features"],
            "target": item["target"],
            "kind": item["kind"],
            "registry": item["registry"],
        }
        for item in metadata["deps"]
    ]
    # Sparse index v2 is required for namespaced and weak dependency features.
    record = {
        "v": 2,
        "name": expected_name,
        "vers": version,
        "deps": dependencies,
        "cksum": hashlib.sha256(archive).hexdigest(),
        "features": {},
        "features2": metadata["features"],
        "yanked": False,
    }
    return (json.dumps(record, separators=(",", ":")) + "\n").encode()


class RegistryError(Exception):
    def __init__(self, status):
        self.status = status


def fetch(url, maximum, error_status=None):
    try:
        with urllib.request.urlopen(url, timeout=20) as response:
            data = response.read(maximum + 1)
    except urllib.error.HTTPError as error:
        raise RegistryError(error_status or error.code) from error
    except (OSError, TimeoutError) as error:
        raise RegistryError(503) from error
    if len(data) > maximum:
        raise RegistryError(413)
    return data


class LocalRegistry(http.server.ThreadingHTTPServer):
    allow_reuse_address = True

    def __init__(self, output, version):
        super().__init__(("127.0.0.1", 0), RegistryHandler)
        self.output = output
        self.version = version
        self.expected = ""
        self.index = {}
        self.error = None
        self.token = secrets.token_hex(32)

    def read(self, path):
        if path == "/index/config.json":
            origin = f"http://127.0.0.1:{self.server_port}"
            return json.dumps({"dl": origin + "/crates", "api": origin}).encode()
        if path.startswith("/index/"):
            key = path.removeprefix("/index/")
            if key in self.index:
                return self.index[key]
            return fetch("https://index.crates.io/" + key, MAX_INDEX)
        if path.startswith("/crates/"):
            return self.crate(path)
        raise RegistryError(404)

    def crate(self, path):
        parts = path.split("/")
        if len(parts) != 5 or parts[4] != "download":
            raise RegistryError(404)
        name, version = parts[2:4]
        asset = self.output / f"{name}-{version}.cargo-upload"
        if name in PACKAGES and version == self.version and asset.is_file():
            body = asset.read_bytes()
            return body[8 + struct.unpack_from("<I", body)[0]:]
        return fetch(f"https://static.crates.io/crates/{name}/{name}-{version}.crate", MAX_UPLOAD, 503)

    def capture(self, body):
        record = parse_upload(body, self.expected, self.version)
        key = index_path(self.expected)
        require(key not in self.index, "duplicate Cargo upload")
        self.output.joinpath(f"{self.expected}-{self.version}.cargo-upload").write_bytes(body)
        self.index[key] = record


class RegistryHandler(http.server.BaseHTTPRequestHandler):
    def send_data(self, data):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        try:
            data = self.server.read(self.path)
        except RegistryError as error:
            self.send_error(error.status)
            return
        self.send_data(data)

    def do_PUT(self):
        if self.path != "/api/v1/crates/new" or not self.server.expected:
            self.send_error(404)
            return
        if not hmac.compare_digest(self.headers.get("Authorization", ""), self.server.token):
            self.send_error(403)
            return
        try:
            size = int(self.headers.get("Content-Length", "0"))
            require(0 < size <= MAX_UPLOAD, "invalid upload length")
            self.server.capture(self.rfile.read(size))
        except (ValueError, KeyError, TypeError, OSError) as error:
            self.server.error = str(error)
            self.send_error(422)
            return
        self.send_data(b'{"warnings":{"invalid_categories":[],"invalid_badges":[],"other":[]}}')

    def log_message(self, _format, *_args):
        pass


@contextmanager
def registry(output, version):
    server = LocalRegistry(output, version)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def capture_packages(server):
    # Redirect both resolution and publication; credentials exist only locally.
    index = f"sparse+http://127.0.0.1:{server.server_port}/index/"
    environment = os.environ.copy()
    environment["CARGO_REGISTRIES_RUNNERLESS_CAPTURE_INDEX"] = index
    environment["CARGO_REGISTRIES_RUNNERLESS_CAPTURE_TOKEN"] = server.token
    command = [
        "cargo",
        "--config", 'source.crates-io.replace-with="local-mirror"',
        "--config", f'source.local-mirror.registry="{index}"',
        "publish", "--registry", "runnerless-capture", "--allow-dirty", "--locked",
    ]
    for name in PACKAGES:
        server.expected = name
        result = subprocess.run(command + ["-p", name], cwd=ROOT, env=environment, timeout=120, check=False)
        asset = server.output / f"{name}-{server.version}.cargo-upload"
        if result.returncode or not asset.is_file() or server.error:
            raise RuntimeError(f"could not package {name}: {server.error or result.returncode}")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: package-registry-uploads.py OUTPUT_DIRECTORY")
    version = ROOT.joinpath("version.txt").read_text().strip()
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise SystemExit("version.txt must contain a stable semantic version")
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    with registry(output, version) as server:
        capture_packages(server)
    digest_lines = [
        f"{hashlib.sha256((output / f'{name}-{version}.cargo-upload').read_bytes()).hexdigest()}  {name}-{version}.cargo-upload"
        for name in PACKAGES
    ]
    output.joinpath("SHA256SUMS").write_text("\n".join(digest_lines) + "\n")


if __name__ == "__main__":
    main()
