#!/usr/bin/env python3
"""Run selected unchanged Git v2.53.0 GPL HTTP tests against two CGI backends.

Only this independent runner/CGI shim is part of git-zero. The pinned external
Git source, shell tests and Apache configuration are never copied or modified.
"""
import argparse
from contextlib import ExitStack
import hashlib
from itertools import product
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys

from interop import environment, run
from upstream_http import compare_repository
from upstream_process import subreaper


SCRIPT = "t5551-http-fetch-smart"
SOURCE_COMMIT = "67ad42147a7acc2af6074753ebd03d904476118f"
SELECTED = {2: "setup repository", 3: "create http-accessible bare repository",
            4: "setup askpass helper", 5: "clone http repository",
            6: "fetch changes via http", 7: "used upload-pack service"}
SOURCE_HASHES = {
    "t/t5551-http-fetch-smart.sh": "39cbf65fd451980e1b863f1b3f242c1a01e0791076324186d79e7215ed8ff211",
    "t/test-lib.sh": "bb4febae1bafabbe00c2ff05087202f193097aab9d9a0e04182acb61f7126498",
    "t/lib-httpd.sh": "e5458b2634d20b3771fe813474dfbd9537f97928f5e0833cd67bb746511f12d3",
    "t/lib-httpd/apache.conf": "0f97af9f11a834e2dbe35db1e98d3811f8cfcf32c9c3fc57ffc68cf182de57a4",
}


class UpstreamFailure(RuntimeError):
    pass


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_source(source, git):
    env = environment(str(git))
    if run(str(git), "--version", env=env).strip() != b"git version 2.53.0":
        raise ValueError("Git must match the 2.53.0 pin")
    if run(str(git), "-C", str(source), "rev-parse", "HEAD", env=env).strip().decode() != SOURCE_COMMIT:
        raise ValueError("Git source checkout must match the v2.53.0 commit")
    run(str(git), "-C", str(source), "diff", "--quiet", "HEAD", "--", env=env)
    for name, expected in SOURCE_HASHES.items():
        if digest(source / name) != expected:
            raise ValueError(f"pinned upstream source differs: {name}")
    if not (source / "t/helper/test-tool").is_file():
        raise ValueError("build Git's test helpers before running the adapter")


def selected_tap_lines(tap):
    return [line for line in tap.splitlines()
            if (match := re.match(r"^(?:ok|not ok) (\d+)(?:\D|$)", line))
            and int(match[1]) in SELECTED]


def selected_cases(tap):
    expected = [f"ok {number} - {name}" for number, name in SELECTED.items()]
    if sorted(selected_tap_lines(tap)) != sorted(expected):
        raise ValueError("selected upstream cases failed, skipped, changed, duplicated or missing")
    return list(SELECTED.values())


def trace_proof(path, native_hash, protocol):
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    if not rows or any(row["backend"] != "git-zero" or row["native_sha256"] != native_hash for row in rows):
        raise ValueError("missing or foreign CGI backend trace")
    if not {("GET", "info/refs"), ("POST", "git-upload-pack")} <= {(r["method"], r["path"]) for r in rows}:
        raise ValueError("CGI trace does not prove discovery and upload-pack")
    if len({row["head"] for row in rows}) != 2:
        raise ValueError("CGI trace does not prove both publication heads")
    expected = "version=2" if protocol == 2 else ""
    if any(row["protocol"] != expected for row in rows):
        raise ValueError("CGI protocol trace differs from selected protocol")
    return rows


def private_exec(root, git, config):
    exec_path = Path(run(str(git), "--exec-path").strip().decode())
    target = root / "exec"
    target.mkdir()
    for helper in exec_path.iterdir():
        if helper.name != "git-http-backend":
            (target / helper.name).symlink_to(helper.resolve())
    launcher = target / "git-http-backend"
    adapter = Path(__file__).with_name("upstream_cgi.py").resolve()
    launcher.write_text(f"#!{sys.executable}\nimport runpy, sys\nsys.path.insert(0, {str(adapter.parent)!r})\nsys.argv = [{str(adapter)!r}, {str(config)!r}]\nrunpy.run_path({str(adapter)!r}, run_name='__main__')\n")
    launcher.chmod(0o755)
    return target


def run_shell(source, root, env, timeout):
    output = root / "tap.log"
    with subreaper(), output.open("wb") as log:
        result = subprocess.run(["sh", f"./{SCRIPT}.sh", "--run=2-7", "--debug", "--verbose"],
                                cwd=source / "t", env=env, stdout=log,
                                stderr=subprocess.STDOUT, start_new_session=True,
                                timeout=timeout, check=False)
        if result.returncode:
            raise UpstreamFailure(f"upstream shell exited {result.returncode}; see {output}")
    return selected_cases(output.read_text())


def select_port(excluded=()):
    # Apache's unchanged harness cannot inherit a listening descriptor. Release
    # this socket before startup and retry only a proven pre-test bind collision.
    # Keep rejected candidates reserved until a new port is selected. At most
    # len(excluded) + 1 sockets are needed, even if the kernel reuses old ports.
    with ExitStack() as sockets:
        for _ in range(len(excluded) + 1):
            reservation = sockets.enter_context(socket.socket())
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
            if port not in excluded:
                return port


def apache_bind_collision(root, port):
    tap = (root / "tap.log").read_text()
    if selected_tap_lines(tap):
        return False
    trace = root / "cgi.jsonl"
    if trace.exists() and trace.stat().st_size:
        return False
    address = rf"could not bind to address 127\.0\.0\.1:{port}(?:\s|$)"
    return any("Address already in use" in line and re.search(address, line)
               for line in tap.splitlines())


def configure_trial(args, root, backend, protocol, object_format, port):
    trash = root / f"trash directory.{SCRIPT}"
    document = trash / "httpd/www"
    config = root / "cgi.json"
    native_hash = digest(args.native)
    snapshots = root / "snapshots"
    snapshots.mkdir()
    config.write_text(json.dumps({"git": str(args.git), "native": str(args.native),
                                 "document_root": str(document), "snapshots": str(snapshots),
                                 "native_sha256": native_hash, "trace": str(root / "cgi.jsonl")}))
    env = environment(str(args.git))
    env.update({"GIT_TEST_INSTALLED": str(args.git.parent), "GIT_TEST_HTTPD": "true",
                "GIT_TEST_PROTOCOL_VERSION": str(protocol), "GIT_TEST_DEFAULT_HASH": object_format,
                "TEST_OUTPUT_DIRECTORY": str(root), "LIB_HTTPD_PATH": str(args.apache),
                "LIB_HTTPD_MODULE_PATH": str(args.modules), "LIB_HTTPD_PORT": str(port)})
    if backend == "git-zero":
        env["GIT_TEST_EXEC_PATH"] = str(private_exec(root, args.git, config))
    return env, native_hash, document


def fixture_manifest(root, document, env):
    trash = document.parent.parent
    bare = document / "repo.git"
    manifest = compare_repository(bare, trash / "clone", "--all", env)
    artifact = root / "manifest.json"
    artifact.write_text(json.dumps({oid.decode(): {
        "kind": kind.decode(), "bytes": len(body), "body_sha256": hashlib.sha256(body).hexdigest(),
    } for oid, (kind, body) in sorted(manifest.items())}, sort_keys=True, indent=2) + "\n")
    # Bind the comparison to the persisted, sorted object/body digest manifest.
    return len(manifest), digest(artifact)


def run_trial(args, root, backend, protocol, object_format, port):
    root.mkdir()
    env, native_hash, document = configure_trial(args, root, backend, protocol, object_format, port)
    cases = run_shell(args.source, root, env, args.timeout)
    count, manifest = fixture_manifest(root, document, env)
    trace = trace_proof(root / "cgi.jsonl", native_hash, protocol) if backend == "git-zero" else []
    return {"backend": backend, "protocol": protocol, "object_format": object_format,
            "cases": cases, "objects": count, "manifest_sha256": manifest,
            "cgi_requests": len(trace), "artifacts": str(root)}


def trial(args, backend, protocol, object_format, bind_failures):
    name = f"{object_format}-v{protocol}-{backend}"
    ports = set()
    for attempt in range(1, 4):
        root = args.output / (name if attempt == 1 else f"{name}-retry-{attempt}")
        port = select_port(ports)
        ports.add(port)
        try:
            return run_trial(args, root, backend, protocol, object_format, port)
        except UpstreamFailure as error:
            if not apache_bind_collision(root, port):
                raise
            bind_failures.append({"artifacts": str(root), "port": port, "error": str(error)})
            if attempt == 3:
                raise


def argument_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--git", required=True, type=Path)
    parser.add_argument("--native", required=True, type=Path)
    parser.add_argument("--apache", required=True, type=Path)
    parser.add_argument("--modules", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=120)
    return parser


def arguments():
    args = argument_parser().parse_args()
    for key in ("source", "git", "native", "apache", "modules", "output"):
        setattr(args, key, getattr(args, key).resolve())
    install = args.git.parent.parent if args.git.parent.name == "bin" else args.git.parent
    if args.output.is_relative_to(args.source) or args.output.is_relative_to(install):
        raise ValueError("output must be outside the upstream source and Git installation")
    args.output.mkdir(parents=True, exist_ok=False)
    return args


def matrix(args, report):
    for object_format, protocol in product(("sha1", "sha256"), (0, 2)):
        expected = trial(args, "git-reference", protocol, object_format, report["port_bind_failures"])
        report["results"].append(expected)
        actual = trial(args, "git-zero", protocol, object_format, report["port_bind_failures"])
        report["results"].append(actual)
        if expected["manifest_sha256"] != actual["manifest_sha256"]:
            raise ValueError("reference and git-zero fixture object graphs differ")


def capture_failure(report, operation, *arguments):
    try:
        operation(*arguments)
    except Exception as error:
        report["errors"].append(str(error))


def checked_matrix(args, report):
    if sys.platform != "linux" or os.geteuid() == 0 or args.timeout <= 0:
        raise ValueError("adapter requires non-root Linux and a positive timeout")
    verify_source(args.source, args.git)
    with ExitStack() as checks:
        checks.callback(capture_failure, report, verify_source, args.source, args.git)
        capture_failure(report, matrix, args, report)


def main():
    args = arguments()
    report = {"schema_version": 1, "status": "failed", "git_version": "2.53.0",
              "source_commit": SOURCE_COMMIT, "source_hashes": SOURCE_HASHES,
              "selection": SELECTED, "fixture_provider": "per-request exported raw objects",
              "manifest_fingerprint": "SHA-256 of manifest.json bytes (sorted JSON keys, indent=2, UTF-8, trailing newline)",
              "results": [], "errors": [], "port_bind_failures": []}
    capture_failure(report, checked_matrix, args, report)
    if not report["errors"]:
        report["status"] = "complete"
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return 0 if report["status"] == "complete" else 1


if __name__ == "__main__":
    sys.exit(main())
