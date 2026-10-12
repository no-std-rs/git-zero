import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from benchmark_fixture import verify
from benchmark_source import create, read
from interop import environment, run
from upstream_http import git, object_manifest


def source_fixture(root, env):
    source = root / "source"
    run("git", "init", "-q", "--object-format=sha1", "-b", "caller", str(source), env=env)
    (source / "nested").mkdir()
    (source / "nested/binary.bin").write_bytes(bytes(range(256)) * 256)
    (source / "large.txt").write_bytes(b"delta candidate\n" * 10000)
    script = source / "run.sh"
    script.write_bytes(b"#!/bin/sh\nexit 0\n")
    script.chmod(0o755)
    (source / "link").symlink_to("nested/binary.bin")
    (source / "space \tline\nname").write_bytes(b"path bytes\n")
    if sys.platform == "linux":
        with open(os.fsencode(source) + b"/binary-\xff-name", "wb") as file:
            file.write(b"non-UTF-8 path\n")
    git(source, "add", ".", env=env)
    git(source, "update-index", "--add", "--cacheinfo", "160000," + "f" * 40 + ",external", env=env)
    git(source, "commit", "-qm", "first", env=env)
    first = git(source, "rev-parse", "HEAD", env=env).strip().decode()
    with (source / "large.txt").open("ab") as file:
        file.write(b"second revision\n")
    git(source, "add", "large.txt", env=env)
    git(source, "commit", "-qm", "second", env=env)
    second = git(source, "rev-parse", "HEAD", env=env).strip().decode()
    tree = git(source, "rev-parse", "HEAD^{tree}", env=env).strip().decode()
    side = git(source, "commit-tree", tree, "-p", first, env=env, data=b"side\n").strip().decode()
    head = git(source, "commit-tree", tree, "-p", second, "-p", side,
               env=env, data=b"merge\n").strip().decode()
    git(source, "update-ref", "refs/heads/caller", head, env=env)
    git(source, "tag", "-am", "not part of the pinned commit graph", "extra-tag", head, env=env)
    git(source, "repack", "-a", "-d", "-f", "--window=64", "--depth=32", env=env)
    git(source, "prune-packed", env=env)
    indexes = sorted((source / ".git/objects/pack").glob("*.idx"))
    assert any(len(line.split()) == 7 for index in indexes
               for line in git(source, "verify-pack", "-v", str(index), env=env).splitlines())
    return source, head


def fingerprint(root):
    return {str(path.relative_to(root)): ("link", os.readlink(path)) if path.is_symlink() else
            ("file", hashlib.sha256(path.read_bytes()).hexdigest())
            for path in root.rglob("*") if path.is_symlink() or path.is_file()}


class BenchmarkSourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="git-zero-source-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = environment()
        self.source, self.head = source_fixture(self.root, self.env)
        self.expected = object_manifest(self.source, self.head, self.env)
        self.bytes = sum(len(body) for _, body in self.expected.values())

    def load(self, revision=None, max_objects=None, max_bytes=None):
        return read(self.source, self.head if revision is None else revision, self.env,
                    len(self.expected) if max_objects is None else max_objects,
                    self.bytes if max_bytes is None else max_bytes)

    def test_packed_merge_graph_and_checkout_survive_export_without_source_changes(self):
        (self.source / "untracked").write_bytes(b"caller-owned state\n")
        (self.source / "nested/binary.bin").write_bytes(b"dirty working copy\n")
        before, original_env = fingerprint(self.source), dict(self.env)
        manifest, description = self.load()
        self.assertEqual(manifest, self.expected)
        output = self.root / "export"
        output.mkdir()
        bare, expected, metadata = create(output, self.env, self.head, manifest, description, 0)
        target = self.root / "checkout"
        run("git", "clone", "-q", "--no-hardlinks", str(bare), str(target), env=self.env)
        verify(bare, target, expected, self.env)
        self.assertEqual(metadata["pinned_revision"], self.head)
        self.assertEqual(metadata["history_commits"], 4)
        self.assertEqual(metadata["source_limits"]["raw_object_bytes"], self.bytes)
        self.assertEqual(json.loads((output / "fixture.json").read_text()), metadata)
        self.assertEqual(git(bare, "for-each-ref", "--format=%(refname)", env=self.env),
                         b"refs/heads/main\n")
        self.assertEqual(fingerprint(self.source), before)
        self.assertEqual(self.env, original_env)

    def test_pin_is_independent_of_later_head_and_replacement_refs(self):
        first = git(self.source, "rev-list", "--max-parents=0", self.head, env=self.env).strip().decode()
        git(self.source, "replace", self.head, first, env=self.env)
        git(self.source, "update-ref", "refs/heads/caller", first, env=self.env)
        before = fingerprint(self.source)
        manifest, _ = self.load()
        self.assertEqual(manifest, self.expected)
        self.assertEqual(fingerprint(self.source), before)

    def test_destinations_cannot_change_source_or_shared_git_state(self):
        manifest, description = self.load()
        alias = self.root / "alias"
        alias.symlink_to(self.source, target_is_directory=True)
        for destination in (self.source / "report.json", self.source / ".git/config",
                            alias / "fixture"):
            with self.subTest(destination=destination), self.assertRaisesRegex(ValueError, "outside"):
                create(destination, self.env, self.head, manifest, description, 0)
            with patch("benchmark_source.object_manifest", side_effect=AssertionError("body read too early")):
                with self.subTest(destination=destination), self.assertRaisesRegex(ValueError, "outside"):
                    read(self.source, self.head, self.env, 100, 1000000, (destination,))
        linked = self.root / "linked"
        git(self.source, "worktree", "add", "-q", "--detach", str(linked), self.head, env=self.env)
        before = fingerprint(self.source)
        with self.assertRaisesRegex(ValueError, "outside"):
            read(linked, self.head, self.env, 100, 1000000, (self.source / ".git/config",))
        self.assertEqual(fingerprint(self.source), before)

    def test_exact_limits_pass_and_excess_fails_before_reading_bodies(self):
        self.assertEqual(self.load()[0], self.expected)
        with patch("benchmark_source.object_manifest", side_effect=AssertionError("body read too early")):
            with self.assertRaisesRegex(ValueError, "max-source-objects"):
                self.load(max_objects=len(self.expected) - 1)
            with self.assertRaisesRegex(ValueError, "max-source-bytes"):
                self.load(max_bytes=self.bytes - 1)
        for maximum in (0, -1):
            with self.subTest(maximum=maximum), self.assertRaisesRegex(ValueError, "positive"):
                self.load(max_objects=maximum)
            with self.subTest(maximum=maximum), self.assertRaisesRegex(ValueError, "positive"):
                self.load(max_bytes=maximum)

    def test_shared_and_transitive_alternates_are_rejected_before_writes(self):
        shared, transitive = self.root / "shared", self.root / "transitive"
        run("git", "clone", "-q", "--shared", str(self.source), str(shared), env=self.env)
        run("git", "clone", "-q", "--shared", str(shared), str(transitive), env=self.env)
        destination = self.source / ".git/objects/report.json"
        before = fingerprint(self.root)
        with patch("benchmark_source.object_ids", side_effect=AssertionError("enumeration too early")):
            for source in (shared, transitive):
                with self.subTest(source=source), self.assertRaisesRegex(ValueError, "alternates"):
                    read(source, self.head, self.env, 100, 1000000, (destination,))
        self.assertFalse(destination.exists())
        self.assertEqual(fingerprint(self.root), before)

    def test_grafts_in_common_worktree_metadata_are_rejected_before_enumeration(self):
        linked = self.root / "linked"
        git(self.source, "worktree", "add", "-q", "--detach", str(linked), self.head, env=self.env)
        (self.source / ".git/info/grafts").write_text(self.head + "\n")
        before = fingerprint(self.root)
        with patch("benchmark_source.object_ids", side_effect=AssertionError("enumeration too early")):
            for source in (self.source, linked):
                with self.subTest(source=source), self.assertRaisesRegex(ValueError, "graft"):
                    read(source, self.head, self.env, 100, 1000000)
        self.assertEqual(fingerprint(self.root), before)

    def test_unpinned_refs_and_noncommit_objects_are_rejected(self):
        for revision in ("HEAD", self.head[:12], self.head.upper(), "--all", "0" * 39):
            with self.subTest(revision=revision), self.assertRaisesRegex(ValueError, "full lowercase"):
                self.load(revision=revision)
        blob = next(oid.decode() for oid, (kind, _) in self.expected.items() if kind == b"blob")
        with self.assertRaisesRegex(ValueError, "identify a commit"):
            self.load(revision=blob)

    def test_incomplete_history_and_wrong_object_format_are_rejected(self):
        (self.source / ".git/shallow").write_text(self.head + "\n")
        with self.assertRaisesRegex(ValueError, "shallow"):
            self.load()
        other = self.root / "sha256.git"
        run("git", "init", "-q", "--bare", "--object-format=sha256", str(other), env=self.env)
        with self.assertRaisesRegex(ValueError, "SHA-1"):
            read(other, self.head, self.env, 100, 1000000)

    def test_missing_promised_body_fails_without_fetching_or_writing_source(self):
        # Mark one absent loose object as promised. The unreachable URL must not
        # be contacted, and the importer must fail before preparing any fixture.
        bare = self.root / "missing.git"
        run("git", "init", "-q", "--bare", "--object-format=sha1", str(bare), env=self.env)
        blob = git(bare, "hash-object", "-w", "--stdin", env=self.env, data=b"missing\n").strip().decode()
        tree = git(bare, "mktree", env=self.env, data=f"100644 blob {blob}\tfile\n".encode()).strip().decode()
        head = git(bare, "commit-tree", tree, env=self.env, data=b"missing fixture\n").strip().decode()
        (bare / "objects" / blob[:2] / blob[2:]).unlink()
        git(bare, "config", "remote.origin.url", "https://example.invalid/unreachable.git", env=self.env)
        git(bare, "config", "remote.origin.promisor", "true", env=self.env)
        git(bare, "config", "extensions.partialClone", "origin", env=self.env)
        before = fingerprint(bare)
        with self.assertRaises((ValueError, AssertionError)):
            read(bare, head, self.env, 100, 1000000)
        self.assertEqual(fingerprint(bare), before)

    def test_cli_rejects_incomplete_source_selection_and_synthetic_dimensions(self):
        runner = Path(__file__).with_name("benchmark.py")
        for arguments, message in [
            (["--source-repo", str(self.source)], "supplied together"),
            (["--revision", self.head], "supplied together"),
            (["--source-repo", str(self.source), "--revision", self.head, "--files", "1"],
             "synthetic dimensions"),
        ]:
            with self.subTest(arguments=arguments):
                output = self.root / "invalid.json"
                result = subprocess.run([sys.executable, str(runner), *arguments, "--output", str(output)],
                                        env=self.env, capture_output=True, timeout=30)
                self.assertEqual(result.returncode, 2)
                self.assertIn(message.encode(), result.stderr)
                self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
