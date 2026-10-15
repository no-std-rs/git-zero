"""Independent Git oracle for server refs, tags and exact wanted-object closures."""
import argparse
import os
from pathlib import Path
import struct
import urllib.error
import urllib.parse
import urllib.request

from interop import command, environment, reference_server, run
from upstream_http import git, object_manifest, verify_checkout


def test_environment(root):
    env = environment(os.environ.get("GIT_ZERO_TEST_GIT", "git"), root)
    env.update(GIT_AUTHOR_DATE="@1 +0000", GIT_COMMITTER_DATE="@1 +0000")
    return env


def make_tree(bare, entries, env):
    data = b"".join(f"100644 blob {oid}\t{name}\n".encode() for name, oid in entries)
    return git(bare, "mktree", env=env, data=data).strip().decode()


def make_commit(bare, tree, parents, message, env):
    args = ["commit-tree", tree]
    for parent in parents:
        args.extend(("-p", parent))
    return git(bare, *args, env=env, data=(message + "\n").encode()).strip().decode()


def fixture(root, state, env, object_format="sha1"):
    bare = root / "repo.git"
    branch = "new/default" if state == "empty-custom" else "z-main"
    run("git", "init", "-q", "--bare", "--object-format=" + object_format, "-b", branch, str(bare), env=env)
    if state in ("empty", "empty-custom"):
        git(bare, "update-server-info", env=env)
        git(bare, "fsck", "--strict", env=env)
        return bare, None
    blobs = [git(bare, "hash-object", "-w", "--stdin", env=env, data=data).strip().decode()
             for data in (b"base\0" + bytes(range(256)) * 16,
                          b"main\0" + bytes(range(255, -1, -1)) * 32,
                          b"side\0" + bytes(range(256)) * 12, b"shared\n")]
    base_tree = make_tree(bare, (("file", blobs[0]), ("shared", blobs[3])), env)
    main_tree = make_tree(bare, (("file", blobs[1]), ("shared", blobs[3])), env)
    side_tree = make_tree(bare, (("file", blobs[2]), ("shared", blobs[3])), env)
    merged_tree = make_tree(bare, (("main", blobs[1]), ("shared", blobs[3]), ("side", blobs[2])), env)
    base = make_commit(bare, base_tree, (), "base", env)
    main = make_commit(bare, main_tree, (base,), "main", env)
    side = make_commit(bare, side_tree, (base,), "side", env)
    merged = make_commit(bare, merged_tree, (main, side), "merge", env)
    # Put the merge tip in a lightweight tag. Avoid redundant tips so Git's
    # clone request stays below its 1 KiB gzip threshold for this simple host.
    for name, oid in (("a-side", side), ("z-main", main)):
        git(bare, "update-ref", "refs/heads/" + name, oid, env=env)
    git(bare, "symbolic-ref", "refs/heads/alias-next", "refs/heads/z-main", env=env)
    git(bare, "symbolic-ref", "refs/heads/alias#%é+", "refs/heads/alias-next", env=env)
    for name, oid in (("annotated", side), ("annotated-tree", base_tree), ("annotated-blob", blobs[3])):
        git(bare, "tag", "-a", "-m", "annotation", name, oid, env=env)
    git(bare, "tag", "-a", "-m", "nested annotation", "nested", "annotated", env=env)
    git(bare, "symbolic-ref", "refs/tags/alias-tag", "refs/tags/annotated", env=env)
    for name, oid in (("light", merged), ("tree", base_tree), ("blob", blobs[3])):
        git(bare, "update-ref", "refs/tags/" + name, oid, env=env)
    if state in ("symbolic", "symbolic-tag"):
        if state == "symbolic-tag":
            git(bare, "update-ref", "refs/tags/head", main, env=env)
            git(bare, "symbolic-ref", "refs/heads/alias-next", "refs/tags/head", env=env)
        git(bare, "symbolic-ref", "HEAD", "refs/heads/alias#%é+", env=env)
        head = main
    elif state == "detached":
        head = make_commit(bare, base_tree, (base,), "detached beyond every ref", env)
        git(bare, "update-ref", "--no-deref", "HEAD", head, env=env)
    else:
        git(bare, "symbolic-ref", "HEAD", "refs/heads/new/default", env=env)
        head = None
    git(bare, "update-server-info", env=env)
    git(bare, "fsck", "--strict", env=env)
    return bare, head


def complete_manifest(bare, head, env):
    manifest = object_manifest(bare, "--all", env)
    if head is not None:
        manifest.update(object_manifest(bare, "HEAD", env))
    return manifest


def prepare(root, state, object_format="sha1"):
    env = test_environment(root)
    if state == "rich":
        from server_rich import prepare as prepare_rich
        prepare_rich(root, object_format, env)
        return
    bare, _ = fixture(root, state, env, object_format)
    from server_fixture import export_fixture
    export_fixture(root, bare, env)


def get(url):
    with urllib.request.urlopen(url, timeout=10) as response:
        return response.read()


def compare_discovery(bare, reference, zero, smart, head, env):
    expected = run("git", "-c", "protocol.version=0", "ls-remote", "--symref", reference, env=env)
    actual = run("git", "-c", "protocol.version=0", "ls-remote", "--symref", zero, env=env)
    assert sorted(actual.splitlines()) == sorted(expected.splitlines()), (actual, expected)
    assert get(zero + "/HEAD") == (bare / "HEAD").read_bytes()
    if not smart:
        assert get(reference + "/HEAD") == get(zero + "/HEAD")
        names = ("refs/heads/alias#%é+", "refs/heads/alias-next", "refs/tags/alias-tag") if git(bare, "for-each-ref", env=env) else ()
        for name in names:
            path = urllib.parse.quote(name, safe="/")
            assert get(reference + "/" + path) == get(zero + "/" + path)


def clone_state(clone, env):
    head = command("git", "-C", str(clone), "symbolic-ref", "-q", "HEAD", env=env)
    refs = git(clone, "for-each-ref", "--format=%(refname) %(objectname) %(symref)", env=env)
    return head.returncode, head.stdout, refs


def clone_all(root, bare, head, urls, protocol, env):
    clones = []
    for name, url in urls:
        clone = root / f"{name}-{protocol}"
        run("git", "-c", "protocol.version=0", "clone", "-q", url, str(clone), env=env)
        git(clone, "fsck", "--strict", env=env)
        assert object_manifest(clone, "--all", env) == complete_manifest(bare, head, env)
        if head is not None:
            assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
            verify_checkout(bare, clone, head, env)
        else:
            assert command("git", "-C", str(clone), "rev-parse", "--verify", "HEAD", env=env).returncode != 0
        if git(bare, "for-each-ref", "refs/heads/a-side", env=env):
            git(clone, "fetch", "--no-tags", "origin", "refs/heads/a-side", env=env)
            assert git(clone, "rev-parse", "FETCH_HEAD", env=env) == git(bare, "rev-parse", "refs/heads/a-side", env=env)
        clones.append(clone)
    assert clone_state(clones[0], env) == clone_state(clones[1], env)
    assert git(clones[0], "rev-parse", "--show-object-format", env=env) == git(clones[1], "rev-parse", "--show-object-format", env=env)


def all_object_ids(repo, env):
    return {line.split()[0] for line in git(repo, "cat-file", "--batch-all-objects", "--batch-check", env=env).splitlines()}


def clone_branch(root, bare, urls, protocol, branch, env):
    expected_head = git(bare, "rev-parse", "refs/heads/" + branch, env=env).strip().decode()
    expected = object_manifest(bare, expected_head, env)
    for name, url in urls:
        clone = root / f"{name}-{protocol}-{branch}"
        run("git", "-c", "protocol.version=0", "clone", "-q", "--single-branch", "--no-tags", "--branch", branch, url, str(clone), env=env)
        git(clone, "fsck", "--strict", env=env)
        assert object_manifest(clone, "HEAD", env) == expected
        actual_ids = all_object_ids(clone, env)
        assert set(expected) <= actual_ids
        if name == "reference" and protocol == "smart":
            # Git's client requests include-tag even with --no-tags. It may
            # receive unreferenced tags of selected objects without saving refs.
            for oid in actual_ids - set(expected):
                assert git(clone, "cat-file", "-t", oid.decode(), env=env).strip() == b"tag"
                assert git(clone, "rev-parse", oid.decode() + "^{}", env=env).strip() in expected
        else:
            assert actual_ids == set(expected), "single-branch clone included an unrelated object"
        assert not git(clone, "for-each-ref", "refs/tags", env=env)
        verify_checkout(bare, clone, expected_head, env)


def packet(line):
    return f"{len(line) + 4:04x}".encode() + line


def upload(url, roots):
    capability = b" object-format=sha256" if len(roots[0]) == 64 else b""
    body = b"".join(packet(b"want " + oid + (capability if index == 0 else b"") + b"\n") for index, oid in enumerate(roots)) + b"0000" + packet(b"done\n")
    request = urllib.request.Request(url + "/git-upload-pack", data=body, headers={"Content-Type": "application/x-git-upload-pack-request"})
    with urllib.request.urlopen(request, timeout=10) as response:
        assert response.status == 200
        assert response.headers.get_content_type() == "application/x-git-upload-pack-result"
        result = response.read()
    assert result.startswith(b"0008NAK\nPACK")
    return result[8:]


def wanted_manifest(bare, roots, env):
    expected = {}
    for oid in roots:
        expected.update(object_manifest(bare, oid.decode(), env))
    return expected


def compare_wanted_pack(root, bare, url, label, roots, env):
    expected = wanted_manifest(bare, roots, env)
    data = upload(url, roots)
    assert struct.unpack(">I", data[8:12])[0] == len(expected)
    (root / (label + ".pack")).write_bytes(data)
    imported = root / (label + ".git")
    object_format = "sha256" if len(roots[0]) == 64 else "sha1"
    run("git", "init", "-q", "--bare", "--object-format=" + object_format, str(imported), env=env)
    git(imported, "index-pack", "--strict", "--stdin", env=env, data=data)
    git(imported, "fsck", "--strict", env=env)
    assert all_object_ids(imported, env) == set(expected), "pack object set differs from the wanted closure"
    actual = wanted_manifest(imported, roots, env)
    assert actual == expected


def compare_wants(root, bare, urls, env):
    names = ("refs/heads/z-main", "refs/heads/a-side", "refs/tags/light", "refs/tags/nested", "refs/tags/annotated-tree", "refs/tags/annotated-blob", "refs/tags/blob", "refs/tags/tree")
    roots = [git(bare, "rev-parse", name, env=env).strip() for name in names]
    for name, url in urls:
        for index, oid in enumerate(roots):
            compare_wanted_pack(root, bare, url, f"{name}-wanted-{index}", (oid,), env)
        compare_wanted_pack(root, bare, url, name + "-wanted-multiple", roots[:2], env)
        compare_wanted_pack(root, bare, url, name + "-wanted-duplicate", (roots[0], roots[0]), env)
        compare_wanted_pack(root, bare, url, name + "-wanted-peeled", (git(bare, "rev-parse", "nested^{}", env=env).strip(),), env)
    zero = urls[1][1]
    hidden = git(bare, "rev-parse", "z-main^{tree}", env=env).strip()
    for oid in (b"0" * len(hidden), b"f" * len(hidden), hidden):
        try:
            upload(zero, (oid,))
        except urllib.error.HTTPError as error:
            assert error.code == 400
            assert b"PACK" not in error.read()
        else:
            raise AssertionError("unadvertised want succeeded")


def compare(root, zero, state):
    env = test_environment(root)
    if state == "rich":
        from server_rich import compare as compare_rich
        return compare_rich(root, zero, env)
    bare = root / "repo.git"
    head = None if state in ("unborn", "empty", "empty-custom") else git(bare, "rev-parse", "HEAD", env=env).strip().decode()
    for smart in (True, False):
        client_env = dict(env, GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="init.defaultBranch", GIT_CONFIG_VALUE_0="main")
        if not smart:
            client_env["GIT_SMART_HTTP"] = "0"
        protocol = "smart" if smart else "dumb"
        with reference_server(root, env, smart) as reference:
            urls = (("reference", reference), ("zero", zero))
            if smart:
                from server_protocol import compare_protocol
                compare_protocol(root, bare, urls, head, env)
            compare_discovery(bare, reference, zero, smart, head, client_env)
            clone_all(root, bare, head, urls, protocol, client_env)
            if git(bare, "for-each-ref", env=env):
                clone_branch(root, bare, urls, protocol, "z-main", client_env)
                clone_branch(root, bare, urls, protocol, "a-side", client_env)
                if smart:
                    compare_wants(root, bare, urls, env)
                    if state == "symbolic":
                        from server_negotiation import compare_negotiation
                        compare_negotiation(root, bare, urls, env)
                        from server_shallow import compare_bounded_fetch
                        compare_bounded_fetch(root, bare, urls, env)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "compare"))
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--state", required=True, choices=("symbolic", "symbolic-tag", "detached", "unborn", "empty", "empty-custom", "rich"))
    parser.add_argument("--object-format", default="sha1", choices=("sha1", "sha256"))
    parser.add_argument("--zero-url")
    args = parser.parse_args()
    if args.phase == "prepare":
        prepare(args.root, args.state, args.object_format)
    else:
        compare(args.root, args.zero_url, args.state)
