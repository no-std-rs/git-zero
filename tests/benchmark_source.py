"""Read a pinned caller repository without fetching or changing its state."""
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import threading

from benchmark_fixture import export, initialize
from upstream_http import git, object_manifest


def object_ids(source, revision, env, maximum):
    """Stop enumeration at the object bound, without buffering the whole listing."""
    with tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen([env["GIT_ZERO_TEST_GIT"], "-C", str(source), "rev-list",
                                    "--objects", "--no-object-names", revision],
                                   env=env, stdout=subprocess.PIPE, stderr=stderr,
                                   start_new_session=True)

        def stop():
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

        watchdog = threading.Timer(30, stop)
        watchdog.start()
        try:
            ids = []
            while line := process.stdout.readline(42):
                if not re.fullmatch(b"[0-9a-f]{40}\n", line):
                    raise ValueError("invalid object enumeration")
                if len(ids) >= maximum:
                    raise ValueError(f"source exceeds --max-source-objects {maximum}")
                ids.append(line.rstrip(b"\n"))
            if process.wait() != 0:
                raise ValueError("cannot enumerate the complete pinned object graph")
            return sorted(ids)
        finally:
            watchdog.cancel()
            watchdog.join()
            if process.poll() is None:
                stop()
            process.wait()
            process.stdout.close()


def check_destinations(description, destinations):
    roots = [Path(description[key]) for key in
             ("source_repository", "source_git_directory", "source_common_directory")]
    for destination in destinations:
        if destination is None:
            continue
        resolved = Path(destination).resolve()
        if any(resolved == root or root in resolved.parents for root in roots):
            raise ValueError("report and fixture destinations must be outside the source repository and Git directories")


def read(source, revision, env, max_objects, max_bytes, destinations=()):
    if not re.fullmatch("[0-9a-f]{40}", revision or ""):
        raise ValueError("--revision must be a full lowercase SHA-1 commit ID")
    if min(max_objects, max_bytes) < 1:
        raise ValueError("source object and byte limits must be positive")
    source = Path(source).resolve()
    readonly = {**env, "GIT_NO_REPLACE_OBJECTS": "1", "GIT_NO_LAZY_FETCH": "1",
                "GIT_OPTIONAL_LOCKS": "0", "GIT_ALLOW_PROTOCOL": ""}
    if git(source, "rev-parse", "--show-object-format", env=readonly).strip() != b"sha1":
        raise ValueError("source must use SHA-1 objects")
    if git(source, "rev-parse", "--is-shallow-repository", env=readonly).strip() != b"false":
        raise ValueError("source must contain complete history, not a shallow clone")
    if git(source, "cat-file", "-t", revision, env=readonly).strip() != b"commit":
        raise ValueError("--revision must identify a commit")
    if git(source, "rev-parse", "--is-bare-repository", env=readonly).strip() == b"false":
        source = Path(os.fsdecode(git(source, "rev-parse", "--show-toplevel", env=readonly).strip())).resolve()
    description = {"kind": "caller-repository", "source_repository": str(source),
                   "pinned_revision": revision,
                   "source_limits": {"objects": max_objects, "raw_object_bytes": max_bytes}}
    for key, argument in (("source_git_directory", "--absolute-git-dir"),
                          ("source_common_directory", "--git-common-dir")):
        path = Path(os.fsdecode(git(source, "rev-parse", argument, env=readonly).strip()))
        description[key] = str((source / path).resolve())
    if (Path(description["source_common_directory"]) / "info/grafts").exists():
        raise ValueError("source must not use legacy graft files")
    if any(line.startswith(b"alternate: ") for line in
           git(source, "count-objects", "-v", env=readonly).splitlines()):
        raise ValueError("source must not borrow objects through alternates")
    check_destinations(description, destinations)
    ids = object_ids(source, revision, readonly, max_objects)
    headers = git(source, "cat-file", "--batch-check", env=readonly,
                  data=b"\n".join(ids) + b"\n").splitlines()
    if len(headers) != len(ids):
        raise ValueError("incomplete object metadata")
    total = 0
    for expected, header in zip(ids, headers):
        fields = header.split()
        if len(fields) != 3 or fields[0] != expected or fields[1] not in (b"commit", b"tree", b"blob"):
            raise ValueError("missing or unsupported source object")
        size = int(fields[2])
        if size < 0:
            raise ValueError("invalid source object size")
        total += size
        if total > max_bytes:
            raise ValueError(f"source exceeds --max-source-bytes {max_bytes}")
    manifest = object_manifest(source, revision, readonly)
    if sorted(manifest) != ids or sum(len(body) for _, body in manifest.values()) != total:
        raise ValueError("source graph changed while preparing the fixture")
    description["history_commits"] = sum(kind == b"commit" for kind, _ in manifest.values())
    return manifest, description


def create(root, env, revision, manifest, description, compression):
    check_destinations(description, (root,))
    bare = initialize(root, env, compression)
    return export(root, bare, revision, manifest, env, compression, description)
