"""Independent differential cases inspired by Git v2.53.0 t5550/t5551.

Assertions are authored here; no GPL shell fixtures are copied. Every case owns
its repository, environment, ephemeral servers and subprocesses. Git remains
the oracle for refs, reachable objects, strict integrity and checkout contents.
"""
import hashlib
import os
import zlib

from interop import command, fixture, reference_server, run, zero_server


SCENARIOS = ("refs", "history", "packed", "detached", "incremental", "snapshot", "empty")
CASES = [f"upstream-{scenario}-{protocol}" for scenario in SCENARIOS
         for protocol in ("smart", "dumb")] + ["upstream-corrupt-pack-dumb", "upstream-corrupt-index-dumb"]


def git(repo, *args, env, data=None):
    return run("git", "-C", str(repo), *args, env=env, data=data)


def refs(data):
    result = {}
    for line in data.splitlines():
        oid, name = line.split(b"\t", 1)
        assert name not in result, f"duplicate advertised ref {name!r}"
        result[name] = oid
    return result


def object_ids(repo, *revisions, env):
    output = git(repo, "rev-list", "--objects", "--no-object-names", *revisions, env=env)
    return sorted(set(output.splitlines()))


def object_manifest(repo, revision, env):
    ids = object_ids(repo, revision, env=env)
    if not ids:
        return {}
    object_format = git(repo, "rev-parse", "--show-object-format", env=env).strip().decode()
    output = git(repo, "cat-file", "--batch", env=env, data=b"\n".join(ids) + b"\n")
    manifest = {}
    offset = 0
    for expected in ids:
        end = output.index(b"\n", offset)
        oid, kind, size = output[offset:end].split()
        size = int(size)
        body = output[end + 1:end + 1 + size]
        assert oid == expected and len(body) == size
        assert output[end + 1 + size:end + 2 + size] == b"\n"
        assert hashlib.new(object_format, kind + b" " + str(size).encode() + b"\0" + body).hexdigest().encode() == oid
        manifest[oid] = (kind, body)
        offset = end + 2 + size
    assert offset == len(output)
    return manifest


def compare_repository(source, target, revision, env):
    git(target, "fsck", "--strict", env=env)
    expected = object_manifest(source, revision, env)
    actual = object_manifest(target, revision, env)
    assert actual == expected, f"object graph differs: {set(actual) ^ set(expected)}"
    tree_revision = git(source, "rev-parse", "HEAD", env=env).strip().decode() if revision == "--all" else revision
    assert git(source, "ls-tree", "-rz", tree_revision, env=env) == git(target, "ls-tree", "-rz", tree_revision, env=env)
    return expected


def verify_checkout(repo, checkout, head, env):
    listing = git(repo, "ls-tree", "-rz", head, env=env)
    for record in listing.split(b"\0"):
        if not record:
            continue
        metadata, name = record.split(b"\t", 1)
        mode, kind, oid = metadata.split()
        assert kind == b"blob"
        body = git(repo, "cat-file", "blob", oid.decode(), env=env)
        path = checkout / os.fsdecode(name)
        if mode == b"120000":
            assert path.is_symlink()
            assert os.fsencode(os.readlink(path)) == body
        else:
            assert path.read_bytes() == body
            assert bool(path.stat().st_mode & 0o111) == (mode == b"100755")


def rich_fixture(root, env):
    bare, _ = fixture(root, env)
    source = root / "source"
    git(source, "branch", "-m", "z-default", env=env)
    nested = source / "nested/deep/tree"
    nested.mkdir(parents=True)
    (nested / "binary.bin").write_bytes(b"".join(hashlib.sha256(str(i).encode()).digest() for i in range(2048)))
    (nested / "empty").write_bytes(b"")
    script = source / "run.sh"
    script.write_bytes(b"#!/bin/sh\nprintf fixture\\n\n")
    script.chmod(0o755)
    (source / "link").symlink_to("nested/deep/tree/binary.bin")
    git(source, "add", ".", env=env)
    # Similar historical blobs make delta creation reproducible, rather than
    # merely assuming that an aggressively packed fixture contains deltas.
    for revision in range(12):
        (source / "large.txt").write_bytes(b"allocation-free git\n" * 8000 + f"revision {revision}\n".encode())
        git(source, "add", "large.txt", env=env)
        git(source, "commit", "-qm", f"history {revision}", env=env)
    tree = git(source, "rev-parse", "HEAD^{tree}", env=env).strip().decode()
    parent = git(source, "rev-parse", "HEAD", env=env).strip().decode()
    base = git(source, "rev-parse", "other", env=env).strip().decode()
    side = git(source, "commit-tree", tree, "-p", base, env=env, data=b"side history\n").strip().decode()
    merge = git(source, "commit-tree", tree, "-p", parent, "-p", side, env=env, data=b"merge history\n").strip().decode()
    git(source, "update-ref", "refs/heads/z-default", merge, env=env)
    git(source, "branch", "releases/current#%é+", merge, env=env)
    git(source, "tag", "-am", "annotated fixture", "annotated", merge, env=env)
    git(bare, "symbolic-ref", "HEAD", "refs/heads/z-default", env=env)
    git(source, "push", "--mirror", str(bare), env=env)
    git(bare, "update-server-info", env=env)
    git(bare, "fsck", "--strict", env=env)
    return bare, merge


def pack_fixture(bare, env):
    git(bare, "repack", "-a", "-d", "-f", "--window=64", "--depth=32", env=env)
    git(bare, "prune-packed", env=env)
    git(bare, "update-server-info", env=env)
    indexes = sorted((bare / "objects/pack").glob("*.idx"))
    assert indexes
    delta_count = 0
    for index in indexes:
        listing = git(bare, "verify-pack", "-v", str(index), env=env)
        delta_count += sum(len(line.split()) == 7 for line in listing.splitlines())
    assert delta_count > 0, "fixture must actually contain delta-compressed objects"
    head = git(bare, "rev-parse", "HEAD", env=env).strip().decode()
    assert not (bare / "objects" / head[:2] / head[2:]).exists()
    assert (bare / "objects/info/packs").read_bytes().startswith(b"P pack-")


def verify_zero_fetch(root, bare, head, env, binary, url, smart, suffix="", expect_deltas=False):
    data = run(binary, "fetch", url, env=env)
    (root / f"response{suffix}.{'pack' if smart else 'zlib'}").write_bytes(data)
    if smart:
        target = root / f"import{suffix}.git"
        run("git", "init", "-q", "--bare", "--object-format=sha1", str(target), env=env)
        git(target, "index-pack", "--strict", "--stdin", env=env, data=data)
        git(target, "update-ref", "refs/heads/main", head, env=env)
        compare_repository(bare, target, head, env)
        if expect_deltas:
            indexes = list((target / "objects/pack").glob("*.idx"))
            assert any(len(line.split()) == 7 for index in indexes
                       for line in git(target, "verify-pack", "-v", str(index), env=env).splitlines()), \
                "downloaded pack must actually exercise delta entries"
    else:
        inflater = zlib.decompressobj()
        loose = inflater.decompress(data) + inflater.flush()
        assert inflater.eof and not inflater.unused_data and not inflater.unconsumed_tail
        header, body = loose.split(b"\0", 1)
        assert header == f"commit {len(body)}".encode()
        assert hashlib.sha1(loose).hexdigest() == head
        assert body == git(bare, "cat-file", "commit", head, env=env)


def snapshot_fixture(root, env):
    bare = root / "repo.git"
    run("git", "init", "-q", "--bare", "--object-format=sha1", "-b", "main", str(bare), env=env)
    blob = git(bare, "hash-object", "-w", "--stdin", env=env,
               data=b"Hello from an allocation-free in-memory Git repository.\n").strip()
    tree = git(bare, "mktree", env=env, data=b"100644 blob " + blob + b"\tREADME.md\n").strip().decode()
    identity_env = dict(env, GIT_AUTHOR_NAME="git-zero", GIT_AUTHOR_EMAIL="git-zero@example.invalid",
                        GIT_COMMITTER_NAME="git-zero", GIT_COMMITTER_EMAIL="git-zero@example.invalid",
                        GIT_AUTHOR_DATE="@0 +0000", GIT_COMMITTER_DATE="@0 +0000")
    head = git(bare, "commit-tree", tree, env=identity_env, data=b"In-memory snapshot\n").strip().decode()
    git(bare, "update-ref", "refs/heads/main", head, env=env)
    git(bare, "update-server-info", env=env)
    return bare, head


def snapshot_case(root, env, binary, smart):
    bare, head = snapshot_fixture(root, env)
    client_env = dict(env)
    if not smart:
        client_env["GIT_SMART_HTTP"] = "0"
    with reference_server(root, env, smart) as reference, zero_server(binary, env) as zero:
        expected_refs = refs(run("git", "ls-remote", reference, env=client_env))
        assert expected_refs == refs(run("git", "ls-remote", zero, env=client_env))
        for name, url in (("reference", reference), ("zero", zero)):
            clone = root / name
            trace_env = dict(client_env, GIT_TRACE_PACKET=str(root / f"{name}-packets.log"))
            # Real Git requests v2; git-zero's v0 advertisement must cause fallback.
            run("git", "-c", "protocol.version=2", "clone", "-q", url, str(clone), env=trace_env)
            assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
            compare_repository(bare, clone, "--all", env)
            verify_checkout(bare, clone, head, env)
            git(clone, "fetch", "origin", env=client_env)
        if smart:
            trace = (root / "zero-packets.log").read_text()
            assert "version 2" in trace and "symref-target:refs/heads/main" in trace
        assert object_manifest(root / "reference", "--all", env) == object_manifest(root / "zero", "--all", env)


def upstream_case(name, root, env, binary):
    scenario, protocol = name.removeprefix("upstream-").rsplit("-", 1)
    smart = protocol == "smart"
    if scenario == "snapshot":
        return snapshot_case(root, env, binary, smart)
    if scenario == "empty":
        bare = root / "repo.git"
        run("git", "init", "-q", "--bare", "--object-format=sha1", "-b", "main", str(bare), env=env)
        git(bare, "update-server-info", env=env)
        head = None
    else:
        bare, head = rich_fixture(root, env)
    if scenario in ("packed", "corrupt-pack", "corrupt-index"):
        pack_fixture(bare, env)
    if scenario == "detached":
        # Pick a commit different from every default ref to test raw HEAD lookup.
        head = git(bare, "rev-parse", "HEAD~2", env=env).strip().decode()
        git(bare, "update-ref", "--no-deref", "HEAD", head, env=env)
        git(bare, "update-server-info", env=env)
    if scenario.startswith("corrupt-"):
        # These are Git reference baselines for planned dumb pack/index imports.
        extension = "pack" if scenario == "corrupt-pack" else "idx"
        path = next((bare / "objects/pack").glob(f"*.{extension}"))
        payload = bytearray(path.read_bytes())
        payload[0] ^= 0x80
        path.chmod(0o644)
        path.write_bytes(payload)
    requests = []
    client_env = dict(env)
    if not smart:
        client_env["GIT_SMART_HTTP"] = "0"
    with reference_server(root, env, smart, requests) as url:
        expected_refs = refs(run("git", "-c", "protocol.version=0", "ls-remote", url, env=client_env))
        actual_refs = refs(run(binary, "ls-remote", url, env=env))
        assert actual_refs == expected_refs, f"refs differ: actual={actual_refs!r}, expected={expected_refs!r}"
        if scenario == "refs":
            return
        clone = root / "clone"
        if scenario.startswith("corrupt-") and not smart:
            result = command("git", "clone", "-q", url, str(clone), env=client_env)
            assert result.returncode != 0, "real Git must reject the corrupted dumb pack/index"
            assert not clone.exists(), "failed clone must remove its partial destination"
        else:
            run("git", "clone", "-q", url, str(clone), env=client_env)
            if head is None:
                assert not git(clone, "for-each-ref", env=env)
                assert git(clone, "rev-parse", "--show-object-format", env=env).strip() == b"sha1"
            else:
                assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
                compare_repository(bare, clone, "--all", env)
                verify_checkout(bare, clone, head, env)
        if head is None or (not smart and scenario in ("packed", "corrupt-pack", "corrupt-index")):
            result = command(binary, "fetch", url, env=env)
            assert result.returncode != 0 and not result.stdout
            assert b"ObjectNotFound" in result.stderr, result.stderr
        else:
            verify_zero_fetch(root, bare, head, env, binary, url, smart, expect_deltas=scenario == "packed")
        if scenario == "incremental":
            source = root / "source"
            (source / "incremental.txt").write_bytes(b"changed remote\n")
            git(source, "add", "incremental.txt", env=env)
            git(source, "commit", "-qm", "incremental update", env=env)
            git(source, "push", str(bare), "z-default", env=env)
            git(bare, "update-server-info", env=env)
            changed = git(bare, "rev-parse", "HEAD", env=env).strip().decode()
            assert changed != head
            git(clone, "fetch", "origin", env=client_env)
            assert git(clone, "rev-parse", "origin/z-default", env=env).strip().decode() == changed
            compare_repository(bare, clone, "--all", env)
            git(clone, "reset", "--hard", "origin/z-default", env=env)
            verify_checkout(bare, clone, changed, env)
            verify_zero_fetch(root, bare, changed, env, binary, url, smart, "-updated")
        if smart and head is not None:
            assert any(method == "POST" and path.endswith("/git-upload-pack") for method, path, _ in requests)
        if not smart:
            assert all(method == "GET" and not path.endswith("/git-upload-pack") for method, path, _ in requests)
            if scenario == "packed":
                assert any(path.endswith(".pack") and status == 200 for _, path, status in requests)
                assert any(path.endswith(".idx") and status == 200 for _, path, status in requests)
