"""Real-Git server comparisons for deep, binary and aggressively packed fixtures.

Fixtures and assertions are independently authored; no upstream GPL tests are
copied. Git's packed source is exported as raw objects for git-zero, so these
cases establish object parity rather than compressed-pack reuse or generation.
"""
import hashlib

from interop import reference_server, run
from server_fixture import export_fixture
from upstream_http import git, object_manifest, verify_checkout


HISTORY_LENGTH = 32
TREE_DEPTH = 40


def prepare(root, object_format, env):
    source = root / "source"
    run("git", "init", "-q", "--object-format=" + object_format, "-b", "z-main", str(source), env=env)
    nested = source.joinpath(*("d" + str(i) for i in range(TREE_DEPTH)))
    nested.mkdir(parents=True)
    (nested / "binary.bin").write_bytes(b"".join(hashlib.sha256(str(i).encode()).digest() for i in range(12288)))
    (source / "empty").write_bytes(b"")
    script = source / "run.sh"
    script.write_bytes(b"#!/bin/sh\nprintf 'rich fixture\\n'\n")
    script.chmod(0o755)
    (source / "link").symlink_to("/".join(nested.relative_to(source).parts) + "/binary.bin")
    git(source, "add", ".", env=env)
    git(source, "commit", "-qm", "root", env=env)
    base = git(source, "rev-parse", "HEAD", env=env).strip().decode()
    git(source, "branch", "history/base", base, env=env)
    for revision in range(HISTORY_LENGTH):
        (source / "history.txt").write_bytes(b"similar historical content\n" * 640 + str(revision).encode() + b"\n")
        git(source, "add", "history.txt", env=env)
        git(source, "commit", "-qm", f"history {revision}", env=env)
    tree = git(source, "rev-parse", "HEAD^{tree}", env=env).strip().decode()
    parent = git(source, "rev-parse", "HEAD", env=env).strip().decode()
    side = git(source, "commit-tree", tree, "-p", base, env=env, data=b"side\n").strip().decode()
    head = git(source, "commit-tree", tree, "-p", parent, "-p", side,
               env=env, data=b"merge deep history\n").strip().decode()
    for name, oid in (("refs/heads/z-main", head), ("refs/heads/releases/current#%é+", side),
                      ("refs/meta/build", base), ("refs/tags/light", head)):
        git(source, "update-ref", name, oid, env=env)
    git(source, "tag", "-a", "-m", "annotation", "annotated", side, env=env)
    git(source, "tag", "-a", "-m", "nested annotation", "nested", "annotated", env=env)
    bare = root / "repo.git"
    run("git", "clone", "-q", "--mirror", "--no-hardlinks", str(source), str(bare), env=env)
    git(bare, "repack", "-a", "-d", "-f", "--window=64", "--depth=32", env=env)
    git(bare, "prune-packed", env=env)
    git(bare, "update-server-info", env=env)
    git(bare, "fsck", "--strict", env=env)
    indexes = sorted((bare / "objects/pack").glob("*.idx"))
    assert indexes
    assert any(len(line.split()) == 7 for index in indexes
               for line in git(bare, "verify-pack", "-v", str(index), env=env).splitlines()), "packed oracle must contain deltas"
    manifest = export_fixture(root, bare, env)
    assert not any((bare / "objects" / oid[:2].decode() / oid[2:].decode()).exists() for oid in manifest)
    assert len(git(bare, "rev-list", "--first-parent", "HEAD", env=env).splitlines()) == HISTORY_LENGTH + 2
    return bare, head


def compare_clone(root, bare, name, url, label, version, env):
    clone = root / f"{name}-{label}-clone"
    run("git", "-c", f"protocol.version={version}", "clone", "-q", url, str(clone), env=env)
    git(clone, "fsck", "--strict", env=env)
    assert object_manifest(clone, "--all", env) == object_manifest(bare, "--all", env)
    head = git(bare, "rev-parse", "HEAD", env=env).strip().decode()
    assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
    assert git(clone, "ls-tree", "-rtz", "HEAD", env=env) == git(bare, "ls-tree", "-rtz", "HEAD", env=env)
    verify_checkout(bare, clone, head, env)
    return git(clone, "for-each-ref", "--format=%(refname) %(objectname)", env=env)


def compare_incremental(root, bare, name, url, label, version, env):
    clone = root / f"{name}-{label}-incremental"
    run("git", "-c", f"protocol.version={version}", "clone", "-q", "--single-branch", "--no-tags",
        "--branch", "history/base", url, str(clone), env=env)
    assert git(clone, "rev-parse", "HEAD", env=env) != git(bare, "rev-parse", "HEAD", env=env)
    git(clone, "-c", f"protocol.version={version}", "fetch", "--no-tags", "origin",
        "+refs/heads/z-main:refs/remotes/origin/z-main", "+refs/tags/nested:refs/tags/nested", env=env)
    tip = git(bare, "rev-parse", "HEAD", env=env).strip().decode()
    assert git(clone, "rev-parse", "origin/z-main", env=env).strip().decode() == tip
    assert object_manifest(clone, "--all", env) == object_manifest(bare, "--all", env)
    git(clone, "checkout", "-q", "--detach", tip, env=env)
    verify_checkout(bare, clone, tip, env)
    git(clone, "fsck", "--strict", env=env)
    return git(clone, "for-each-ref", "--format=%(refname) %(objectname)", env=env)


def compare(root, zero, env):
    bare = root / "repo.git"
    for version in (0, 1, 2, None):
        smart = version is not None
        label = f"v{version}" if smart else "dumb"
        client_env = dict(env, GIT_TRACE_PACKET=str(root / f"{label}-packets.log"),
                          GIT_TRACE_CURL=str(root / f"{label}-http.log"), GIT_TRACE_CURL_NO_DATA="1")
        if not smart:
            client_env["GIT_SMART_HTTP"] = "0"
        with reference_server(root, env, smart) as reference:
            states = []
            listings = []
            for name, url in (("reference", reference), ("zero", zero)):
                listings.append(sorted(run("git", "-c", f"protocol.version={version or 0}", "ls-remote", "--symref", url,
                                           env=client_env).splitlines()))
                states.append((compare_clone(root, bare, name, url, label, version or 0, client_env),
                               compare_incremental(root, bare, name, url, label, version or 0, client_env)))
            assert listings[0] == listings[1]
            assert states[0] == states[1]
