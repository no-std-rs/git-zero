"""Export independent Git fixtures for the native file-backed server harness.

This adapter is independently authored under MIT OR Apache-2.0. It does not
copy Git's GPL test bodies or replace the Apache-specific upstream harness.
"""
import argparse
import os
from pathlib import Path

from interop import command, environment
from upstream_http import git, object_manifest


def export_fixture(root, bare, env):
    """Freeze reachable objects, refs and HEAD without changing the Git source."""
    if root.resolve().is_relative_to(bare.resolve()):
        raise ValueError("export root must be outside the source repository")
    root.mkdir(parents=True, exist_ok=True)
    object_format = git(bare, "rev-parse", "--show-object-format", env=env).strip().decode()
    resolved = command("git", "-C", str(bare), "rev-parse", "--verify", "HEAD", env=env)
    head = resolved.stdout.strip().decode() if resolved.returncode == 0 else None
    manifest = object_manifest(bare, "--all", env)
    objects = root / "raw"
    objects.mkdir()
    rows = []
    for oid, (kind, body) in sorted(manifest.items()):
        (objects / oid.decode()).write_bytes(body)
        rows.append(oid.decode() + "\t" + kind.decode())
    (root / "objects.txt").write_text("".join(row + "\n" for row in rows))
    refs = []
    for line in git(bare, "for-each-ref", "--format=%(refname)\t%(objectname)", env=env).splitlines():
        name, oid = line.decode().split("\t")
        symbolic = command("git", "-C", str(bare), "symbolic-ref", "-q", "--no-recurse", name, env=env)
        target = symbolic.stdout.strip().decode() if symbolic.returncode == 0 else "-"
        kind, _ = manifest[oid.encode()]
        peeled = git(bare, "rev-parse", name + "^{}", env=env).strip().decode() if kind == b"tag" else "-"
        refs.append("\t".join((name, oid, target, peeled)))
    (root / "refs.txt").write_text("".join(row + "\n" for row in refs))
    symbolic = command("git", "-C", str(bare), "symbolic-ref", "-q", "--no-recurse", "HEAD", env=env)
    target = symbolic.stdout.strip().decode() if symbolic.returncode == 0 else ""
    (root / "head.txt").write_text((head or "0" * (64 if object_format == "sha256" else 40)) + "\n" + target + "\n")
    (root / "format.txt").write_text(object_format)
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--root", required=True, type=Path)
    args = parser.parse_args()
    export_fixture(args.root, args.repo, environment(os.environ.get("GIT_ZERO_TEST_GIT", "git")))
