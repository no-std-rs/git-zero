"""Deterministic Git repository and identical raw file-provider export."""
import hashlib
import json
import os

from interop import run
from upstream_http import git, object_manifest


def initialize(root, env, compression):
    bare = root / "repo.git"
    run("git", "init", "-q", "--bare", "--object-format=sha1", "-b", "main", str(bare), env=env)
    for key, value in (("core.compression", compression), ("core.looseCompression", compression),
                       ("pack.compression", compression),
                       ("pack.window", 0), ("pack.depth", 0), ("http.getanyfile", "true")):
        git(bare, "config", key, str(value), env=env)
    return bare


def create(root, env, files, blob_bytes, commits, compression):
    bare = initialize(root, env, compression)
    entries = {}
    parent = None
    for revision in range(commits):
        changed = range(files) if revision == 0 else ((revision - 1) % files,)
        for index in changed:
            seed = f"file {index}, revision {revision}\n".encode()
            body = ((seed * ((blob_bytes + len(seed) - 1) // len(seed)))[:blob_bytes]
                    if index % 2 == 0 else hashlib.shake_256(seed).digest(blob_bytes))
            oid = git(bare, "hash-object", "-w", "--stdin", env=env, data=body).strip()
            mode = b"100755" if index == 0 else b"100644"
            entries[f"file-{index:05d}.bin".encode()] = mode + b" blob " + oid
        listing = b"".join(metadata + b"\t" + name + b"\n" for name, metadata in sorted(entries.items()))
        tree = git(bare, "mktree", env=env, data=listing).strip().decode()
        parents = ["-p", parent] if parent else []
        parent = git(bare, "commit-tree", tree, *parents, env=env,
                     data=f"synthetic revision {revision}\n".encode()).strip().decode()
    manifest = object_manifest(bare, parent, env)
    return export(root, bare, parent, manifest, env, compression,
                  {"kind": "synthetic", "files": files, "blob_bytes": blob_bytes,
                   "history_commits": commits})


def export(root, bare, head, manifest, env, compression, description):
    """Encode identical bodies for Git and the raw provider in an owned fixture."""
    # mktree does not load default compression config in some Git versions.
    # Rewrite every loose object through hash-object, which does, before any
    # server sees this fixture. Object IDs and graph stay exactly the same.
    loose_digest = hashlib.sha256()
    loose_bytes = 0
    for oid, (kind, body) in sorted(manifest.items()):
        path = bare / "objects" / oid[:2].decode() / oid[2:].decode()
        path.unlink(missing_ok=True)
        assert git(bare, "hash-object", "-w", "-t", kind.decode(), "--stdin", env=env, data=body).strip() == oid
        encoded = path.read_bytes()
        if compression == 0:
            assert (encoded[2] >> 1) & 3 == 0, "expected stored-DEFLATE loose object"
        loose_digest.update(encoded)
        loose_bytes += len(encoded)
    git(bare, "update-ref", "refs/heads/main", head, env=env)
    git(bare, "update-server-info", env=env)
    git(bare, "fsck", "--strict", env=env)
    rows = [head]
    digest = hashlib.sha256()
    with (root / "objects.raw").open("wb") as raw:
        for oid, (kind, body) in sorted(manifest.items()):
            rows.append(f"{oid.decode()}\t{kind.decode()}\t{raw.tell()}\t{len(body)}")
            raw.write(body)
            digest.update(body)
    (root / "objects.tsv").write_text("\n".join(rows) + "\n")
    metadata = {**description, "head": head, "objects": len(manifest),
                "raw_object_bytes": (root / "objects.raw").stat().st_size,
                "raw_sha256": digest.hexdigest(), "git_compression": compression,
                "git_loose_object_bytes": loose_bytes, "git_loose_sha256": loose_digest.hexdigest(),
                "git_delta_window": 0, "git_delta_depth": 0,
                "git_storage": "loose Git objects", "zero_storage": "identical raw bodies in a file; no prepacked cache"}
    (root / "fixture.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return bare, manifest, metadata


def verify(bare, target, expected, env):
    """Check full history and checkout without one Git process per file."""
    git(target, "fsck", "--strict", env=env)
    assert git(target, "rev-parse", "HEAD", env=env) == git(bare, "rev-parse", "HEAD", env=env)
    assert object_manifest(target, "--all", env) == expected, "reachable object graph differs"
    listing = git(bare, "ls-tree", "-rz", "HEAD", env=env)
    assert git(target, "ls-tree", "-rz", "HEAD", env=env) == listing
    for record in listing.split(b"\0"):
        if not record:
            continue
        metadata, name = record.split(b"\t", 1)
        mode, kind, oid = metadata.split()
        path = target / os.fsdecode(name)
        if mode == b"160000":
            assert kind == b"commit" and path.is_dir() and not path.is_symlink()
        elif mode == b"120000":
            assert kind == b"blob" and path.is_symlink()
            assert os.fsencode(os.readlink(path)) == expected[oid][1]
        else:
            assert kind == b"blob" and not path.is_symlink()
            assert path.read_bytes() == expected[oid][1]
            assert bool(path.stat().st_mode & 0o111) == (mode == b"100755")
