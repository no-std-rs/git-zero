"""Independent packed-only/mixed dumb HTTP oracle, with no assisted object import."""
import argparse
import hashlib
import json
from pathlib import Path
import urllib.request

from interop import reference_server, run
from server_fixture import export_fixture
from server_references import make_commit, test_environment
from upstream_http import git, object_manifest, verify_checkout


def prepare(root, object_format, state):
    env = test_environment(root)
    bare = root / "repo.git"
    run("git", "init", "-q", "--bare", "--object-format=" + object_format,
        "-b", "main", str(bare), env=env)
    empty = git(bare, "mktree", env=env, data=b"").strip().decode()
    binary = git(bare, "hash-object", "-w", "--stdin", env=env,
                 data=bytes(range(256)) * 257).strip().decode()
    script = git(bare, "hash-object", "-w", "--stdin", env=env,
                 data=b"#!/bin/sh\nprintf 'packed fixture\\n'\n").strip().decode()
    link = git(bare, "hash-object", "-w", "--stdin", env=env, data=b"binary").strip().decode()
    tree = git(bare, "mktree", env=env, data=(
        f"100644 blob {binary}\tbinary\n040000 tree {empty}\tempty-directory\n"
        f"120000 blob {link}\tlink\n100755 blob {script}\trun.sh\n").encode()).strip().decode()
    base = make_commit(bare, tree, (), "base with referenced empty tree", env)
    main = base
    for revision in range(8):
        main = make_commit(bare, tree, (main,), f"history {revision}", env)
    side = make_commit(bare, tree, (base,), "side", env)
    merged = make_commit(bare, tree, (main, side), "merge", env)
    for name, oid in (("main", merged), ("base", base), ("side", side)):
        git(bare, "update-ref", "refs/heads/" + name, oid, env=env)
    git(bare, "symbolic-ref", "refs/heads/alias", "refs/heads/main", env=env)
    git(bare, "tag", "-a", "-m", "annotation", "annotated", side, env=env)
    git(bare, "tag", "-a", "-m", "nested", "nested", "annotated", env=env)
    if state == "detached":
        git(bare, "update-ref", "--no-deref", "HEAD", merged, env=env)
    elif state == "unborn":
        git(bare, "symbolic-ref", "HEAD", "refs/heads/missing", env=env)
    git(bare, "repack", "-a", "-d", env=env)
    git(bare, "prune-packed", env=env)
    git(bare, "update-server-info", env=env)
    git(bare, "fsck", "--strict", env=env)
    manifest = export_fixture(root, bare, env)
    assert empty.encode() in manifest, "export must include the referenced empty tree"
    assert len(manifest) < 64
    (root / "packed-ids.json").write_text(json.dumps({"empty": empty, "main": merged}) + "\n")


def download(url):
    with urllib.request.urlopen(url, timeout=30) as response:
        return response.headers, response.read()


def verify_download(root, bare, url, label, env):
    headers, listing = download(url + "/objects/info/packs")
    assert headers.get_content_type() == "text/plain"
    lines = listing.splitlines()
    assert len(lines) == 2 and lines[-1] == b"", listing
    marker, filename = lines[0].decode().split()
    assert marker == "P" and filename.startswith("pack-") and filename.endswith(".pack")
    object_format = git(bare, "rev-parse", "--show-object-format", env=env).strip().decode()
    width = 20 if object_format == "sha1" else 32
    headers, pack = download(url + "/objects/pack/" + filename)
    assert headers.get_content_type() == "application/x-git-packed-objects"
    assert int(headers["Content-Length"]) == len(pack)
    checksum = hashlib.new(object_format, pack[:-width]).digest()
    assert checksum == pack[-width:]
    assert filename == "pack-" + checksum.hex() + ".pack"
    headers, index = download(url + "/objects/pack/" + filename[:-5] + ".idx")
    assert headers.get_content_type() == "application/x-git-packed-objects-toc"
    assert int(headers["Content-Length"]) == len(index)
    assert index[-2 * width:-width] == checksum
    assert hashlib.new(object_format, index[:-width]).digest() == index[-width:]
    pack_path = root / (label + ".pack")
    index_path = root / (label + ".idx")
    pack_path.write_bytes(pack)
    index_path.write_bytes(index)
    git(bare, "verify-pack", "-v", str(index_path), env=env)
    generated = root / (label + "-git.idx")
    git(bare, "index-pack", "--index-version=2", "-o", str(generated), str(pack_path), env=env)
    assert generated.read_bytes() == index, "Git's independent indexer must produce the exact index"


def inspect_client(bare, path, ids, env, checked_out=True):
    # No fetch, hash-object, or alternate is used to repair the clone. Physical
    # objects must be present before fsck, including the empty tree omitted by
    # some versions of Git's loose-object HTTP walker.
    physical = set(git(path, "cat-file", "--batch-all-objects", "--batch-check=%(objectname)", env=env).splitlines())
    assert physical == set(object_manifest(bare, "--all", env))
    assert ids["empty"].encode() in physical
    git(path, "fsck", "--strict", env=env)
    assert object_manifest(path, "--all", env) == object_manifest(bare, "--all", env)
    if checked_out:
        head = git(path, "rev-parse", "HEAD", env=env).strip().decode()
        verify_checkout(bare, path, head, env)


def compare(root, zero_url, state, files=True):
    env = dict(test_environment(root), GIT_SMART_HTTP="0")
    bare = root / "repo.git"
    ids = json.loads((root / "packed-ids.json").read_text())
    results = []
    with reference_server(root, env, False) as reference:
        urls = [("reference", reference), ("packed", zero_url + "/packed/repo.git"), ("mixed", zero_url + "/mixed/repo.git")]
        if files:
            urls.append(("files", zero_url + "/files/repo.git"))
        for label, url in urls:
            if label != "reference":
                verify_download(root, bare, url, label, env)
            path = root / (label + "-clone")
            trace = dict(env, GIT_TRACE_CURL=str(root / (label + "-http.log")), GIT_TRACE_CURL_NO_DATA="1")
            run("git", "clone", "-q", url, str(path), env=trace)
            inspect_client(bare, path, ids, env, state != "unborn")
            refs = git(path, "for-each-ref", "--format=%(refname) %(objectname)", env=env)
            results.append(refs)
            # Refetch and branch expansion use only the published immutable URL.
            git(path, "fetch", "origin", "refs/heads/main:refs/remotes/origin/main", env=env)
            git(path, "checkout", "-q", "--detach", ids["main"], env=env)
            inspect_client(bare, path, ids, env)
            if label == "packed":
                log = (root / (label + "-http.log")).read_text()
                assert ".pack HTTP/" in log and ".idx HTTP/" in log
                assert "/objects/info/packs" in log
    assert all(row == results[0] for row in results)
    print("packed/mixed dumb clone, refetch, exact graph, empty tree, checkout and fsck passed", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "compare"))
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--object-format", choices=("sha1", "sha256"), default="sha1")
    parser.add_argument("--state", choices=("symbolic", "detached", "unborn"), default="symbolic")
    parser.add_argument("--zero-url")
    parser.add_argument("--no-files", action="store_true")
    args = parser.parse_args()
    if args.phase == "prepare":
        prepare(args.root, args.object_format, args.state)
    else:
        compare(args.root, args.zero_url, args.state, not args.no_files)
