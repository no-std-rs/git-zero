#!/usr/bin/env python3
"""Run selected unchanged Git v2.53.0 GPL HTTP tests against two CGI backends.

Only this independent runner/CGI shim is part of git-zero. The pinned external
Git source, shell tests and Apache configuration are never copied or modified.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys

from interop import environment, run
from upstream_http import compare_repository, object_manifest
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


def selected_cases(tap):
    found = {}
    for line in tap.splitlines():
        match = re.match(r"^(ok|not ok) (\d+)(.*)$", line)
        if match and int(match[2]) in SELECTED:
            number = int(match[2])
            if number in found or match[1] != "ok" or match[3] != " - " + SELECTED[number]:
                raise ValueError(f"selected upstream case failed, skipped or changed: {line}")
            found[number] = SELECTED[number]
    if found != SELECTED:
        raise ValueError("missing selected upstream case results")
    return list(found.values())


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
        child = subprocess.Popen(["sh", f"./{SCRIPT}.sh", "--run=2-7", "--debug"],
                                 cwd=source / "t", env=env, stdout=log,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            result = child.wait(timeout=timeout)
            if result:
                raise RuntimeError(f"upstream shell exited {result}; see {output}")
        finally:
            if child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            child.wait()
    return selected_cases(output.read_text())


def trial(args, backend, protocol, object_format):
    root = args.output / f"{object_format}-v{protocol}-{backend}"
    root.mkdir()
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
                "LIB_HTTPD_MODULE_PATH": str(args.modules)})
    # Reserve an ephemeral loopback port rather than sharing the suite's default.
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        env["LIB_HTTPD_PORT"] = str(reservation.getsockname()[1])
    if backend == "git-zero":
        env["GIT_TEST_EXEC_PATH"] = str(private_exec(root, args.git, config))
    cases = run_shell(args.source, root, env, args.timeout)
    bare = document / "repo.git"
    compare_repository(bare, trash / "clone", "--all", env)
    manifest = object_manifest(bare, "--all", env)
    (root / "manifest.json").write_text(json.dumps({oid.decode(): {
        "kind": kind.decode(), "bytes": len(body), "body_sha256": hashlib.sha256(body).hexdigest(),
    } for oid, (kind, body) in sorted(manifest.items())}, indent=2) + "\n")
    fingerprint = hashlib.sha256()
    for oid, (kind, body) in sorted(manifest.items()):
        fingerprint.update(oid + b" " + kind + b" " + str(len(body)).encode() + b"\0" + body)
    trace = trace_proof(root / "cgi.jsonl", native_hash, protocol) if backend == "git-zero" else []
    return {"backend": backend, "protocol": protocol, "object_format": object_format,
            "cases": cases, "objects": len(manifest), "manifest_sha256": fingerprint.hexdigest(),
            "cgi_requests": len(trace), "artifacts": str(root)}


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


def main():
    args = argument_parser().parse_args()
    for key in ("source", "git", "native", "apache", "modules", "output"):
        setattr(args, key, getattr(args, key).resolve())
    install = args.git.parent.parent if args.git.parent.name == "bin" else args.git.parent
    if args.output.is_relative_to(args.source) or args.output.is_relative_to(install):
        raise ValueError("output must be outside the upstream source and Git installation")
    args.output.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": 1, "status": "failed", "git_version": "2.53.0",
              "source_commit": SOURCE_COMMIT, "source_hashes": SOURCE_HASHES,
              "selection": SELECTED, "fixture_provider": "per-request exported raw objects",
              "results": [], "errors": []}
    verified = False
    try:
        if sys.platform != "linux" or os.geteuid() == 0 or args.timeout <= 0:
            raise ValueError("adapter requires non-root Linux and a positive timeout")
        verify_source(args.source, args.git)
        verified = True
        for object_format in ("sha1", "sha256"):
            for protocol in (0, 2):
                expected = trial(args, "git-reference", protocol, object_format)
                report["results"].append(expected)
                actual = trial(args, "git-zero", protocol, object_format)
                report["results"].append(actual)
                if expected["manifest_sha256"] != actual["manifest_sha256"]:
                    raise ValueError("reference and git-zero fixture object graphs differ")
        report["status"] = "complete"
    except Exception as error:
        report["errors"].append(str(error))
    finally:
        if verified:
            try:
                verify_source(args.source, args.git)
            except Exception as error:
                report["status"] = "failed"
                report["errors"].append(str(error))
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return 0 if report["status"] == "complete" else 1


if __name__ == "__main__":
    sys.exit(main())
