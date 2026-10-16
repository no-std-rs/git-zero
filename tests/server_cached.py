"""Real Git oracle for selected HTTP caching across immutable A/B snapshots."""
import argparse
import os
from pathlib import Path

from interop import environment, reference_server, run
from server_fixture import export_fixture
from server_references import make_commit, make_tree
from upstream_http import git, object_manifest, verify_checkout


def prepare(root, object_format):
    env = environment(os.environ.get("GIT_ZERO_TEST_GIT", "git"), root)
    source = root / "source.git"
    run("git", "init", "-q", "--bare", f"--object-format={object_format}", "-b", "main", str(source), env=env)
    blobs = [git(source, "hash-object", "-w", "--stdin", env=env, data=data).strip().decode()
             for data in (b"first\0" + bytes(range(256)) * 64, b"shared\n",
                          b"second\0" + bytes(range(255, -1, -1)) * 96, b"side branch\n")]
    initial_tree = make_tree(source, (("binary", blobs[0]), ("shared", blobs[1])), env)
    base = make_commit(source, initial_tree, (), "initial", env)
    side_tree = make_tree(source, (("shared", blobs[1]), ("side", blobs[3])), env)
    side = make_commit(source, side_tree, (), "independent tip", env)
    git(source, "update-ref", "refs/heads/main", base, env=env)
    git(source, "update-ref", "refs/heads/side", side, env=env)
    for phase in ("A", "B"):
        if phase == "B":
            tree = make_tree(source, (("binary", blobs[2]), ("shared", blobs[1])), env)
            updated = make_commit(source, tree, (base,), "changed generation", env)
            git(source, "update-ref", "refs/heads/main", updated, env=env)
        target = root / phase
        target.mkdir()
        bare = target / "repo.git"
        run("git", "clone", "-q", "--bare", "--no-hardlinks", str(source), str(bare), env=env)
        git(bare, "update-server-info", env=env)
        git(bare, "fsck", "--strict", env=env)
        export_fixture(target, bare, env)


def verify(bare, clone, env):
    git(clone, "fsck", "--strict", env=env)
    expected = object_manifest(bare, "--all", env)
    assert object_manifest(clone, "--all", env) == expected
    head = git(bare, "rev-parse", "HEAD", env=env).strip().decode()
    assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
    verify_checkout(bare, clone, head, env)


def compare(root, phase, version, zero):
    env = environment(os.environ.get("GIT_ZERO_TEST_GIT", "git"), root)
    fixture = root / ("A" if phase == "return" else phase)
    bare = fixture / "repo.git"
    with reference_server(fixture, env, True) as reference:
        if phase == "B":
            for name, url in (("reference", reference), ("zero", zero)):
                clone = root / (name + "-A")
                git(clone, "remote", "set-url", "origin", url, env=env)
                git(clone, "-c", f"protocol.version={version}", "fetch", "--no-tags", "origin", env=env)
                git(clone, "reset", "--hard", "refs/remotes/origin/main", env=env)
                verify(bare, clone, env)
        for name, url in (("reference", reference), ("zero", zero), ("zero-repeat", zero)):
            clone = root / f"{name}-{phase}"
            run("git", "-c", f"protocol.version={version}", "clone", "-q", url, str(clone), env=env)
            verify(bare, clone, env)
        # A separate single-ref request must not reuse the full-union pack.
        for name, url in (("reference", reference), ("zero", zero)):
            clone = root / f"{name}-{phase}-side"
            run("git", "-c", f"protocol.version={version}", "clone", "-q", "--single-branch", "--no-tags", "--branch", "side", url, str(clone), env=env)
            expected = object_manifest(bare, "refs/heads/side", env)
            assert object_manifest(clone, "--all", env) == expected
            physical = {row.split()[0] for row in git(clone, "cat-file", "--batch-all-objects", "--batch-check", env=env).splitlines()}
            assert physical == set(expected), "selected branch imported an unrelated object"
            git(clone, "fsck", "--strict", env=env)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "A", "B", "return"))
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--object-format", choices=("sha1", "sha256"), default="sha1")
    parser.add_argument("--version", type=int, choices=(0, 1, 2), default=0)
    parser.add_argument("--zero-url")
    args = parser.parse_args()
    if args.phase == "prepare":
        prepare(args.root, args.object_format)
    else:
        compare(args.root, args.phase, args.version, args.zero_url)
