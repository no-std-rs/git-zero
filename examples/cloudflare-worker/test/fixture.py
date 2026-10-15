"""Independently generate a bounded raw repository using Git, never git-zero.

This project-authored fixture invokes Git and the unchanged project exporter;
it copies no upstream test bodies. License: MIT OR Apache-2.0.
"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tests"))
from interop import environment, run
from server_fixture import export_fixture
from upstream_http import object_manifest, verify_checkout


def freeze(root, fmt, binary):
    root.mkdir()
    repo = root / "source.git"
    env = environment(binary)
    env.update({"GIT_AUTHOR_NAME": "Scaled fixture", "GIT_COMMITTER_NAME": "Scaled fixture",
                "GIT_AUTHOR_EMAIL": "fixture@example.invalid", "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
                "GIT_AUTHOR_DATE": "2001-02-03T04:05:06+0000", "GIT_COMMITTER_DATE": "2001-02-03T04:05:06+0000"})
    run(binary, "init", "-q", "--bare", "--object-format=" + fmt, str(repo), env=env)

    def git(*args, data=None):
        return run(binary, "-C", str(repo), *args, data=data, env=env)

    def blob(body):
        return git("hash-object", "-t", "blob", "-w", "--stdin", data=body).strip().decode()

    def tree(entries):
        return git("mktree", data="".join(f"{mode} {kind} {oid}\t{name}\n"
                    for mode, kind, oid, name in entries).encode()).strip().decode()

    def commit(tree_id, message, parents=()):
        args = ["commit-tree", tree_id]
        for parent in parents:
            args.extend(["-p", parent])
        return git(*args, data=(message + "\n").encode()).strip().decode()

    binary_body = b"".join(hashlib.sha256(f"scaled-fixture-{i}".encode()).digest() for i in range(3072))
    empty_tree = tree([])
    nested = tree([("100644", "blob", blob(binary_body), "binary.bin"),
                   ("100644", "blob", blob(b""), "empty-file"),
                   ("100755", "blob", blob(b"#!/bin/sh\nprintf 'scaled fixture\\n'\n"), "run.sh"),
                   ("120000", "blob", blob(b"binary.bin"), "link")])
    for depth in range(7, 0, -1):
        entries = [("040000", "tree", nested, f"d{depth}")]
        if depth == 1:
            entries.append(("040000", "tree", empty_tree, "empty-dir"))
        nested = tree(entries)
    history, commits = [], []
    for revision in range(8):
        history.append(blob(f"history revision {revision}\n".encode()))
        root_tree = tree([("040000", "tree", nested, "nested"),
                          ("100644", "blob", history[-1], "history.txt")])
        commits.append(commit(root_tree, f"main revision {revision}", commits[-1:]))
    side_body = blob(b"independent side branch\n")

    def with_side(history_id):
        return tree([("040000", "tree", nested, "nested"),
                     ("100644", "blob", history_id, "history.txt"),
                     ("100644", "blob", side_body, "side.txt")])

    side = commit(with_side(history[2]), "side history", [commits[2]])
    merge = commit(with_side(history[-1]), "two-parent merge", [commits[-1], side])
    for name, oid in [("z-default", merge), ("main-history", commits[-1]), ("side", side)]:
        git("update-ref", "refs/heads/" + name, oid)
    git("symbolic-ref", "refs/heads/current", "refs/heads/z-default")
    git("symbolic-ref", "HEAD", "refs/heads/z-default")
    git("tag", "-am", "inner annotated tag", "inner", merge)
    git("tag", "-am", "nested annotated tag", "nested", "inner")
    git("tag", "lightweight", commits[-1])
    git("fsck", "--strict", "--no-dangling")
    exported = export_fixture(root / "export", repo, env)
    assert exported == object_manifest(repo, "--all", env)
    inventory = {line.split()[0] for line in git("cat-file", "--batch-all-objects", "--batch-check=%(objectname)").splitlines()}
    assert inventory == set(exported)
    counts = Counter(kind.decode() for kind, _ in exported.values())
    assert counts == Counter(commit=10, tree=19, blob=13, tag=2)
    total = sum(len(body) for _, body in exported.values())
    assert total == {"sha1": 102426, "sha256": 103362}[fmt]
    checkout = root / "checkout"
    run(binary, "clone", "-q", str(repo), str(checkout), env=env)
    verify_checkout(repo, checkout, merge, env)
    return {"format": fmt, "objects": len(exported), "kinds": dict(counts), "rawBytes": total,
            "largestBody": max(len(body) for _, body in exported.values()),
            "head": merge, "headTarget": "refs/heads/z-default", "mainHistory": commits[-1],
            "directoryComponents": 8, "treeLevels": 9}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--git", default=os.environ.get("GIT_ZERO_TEST_GIT", "git"))
    args = parser.parse_args()
    args.root.mkdir()
    results = [freeze(args.root / fmt, fmt, args.git) for fmt in ("sha1", "sha256")]
    (args.root / "report.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results))


if __name__ == "__main__":
    main()
