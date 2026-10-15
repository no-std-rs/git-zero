"""Real-Git shallow/partial clone and absolute-deepening oracle comparisons."""
from itertools import product
from interop import run
from upstream_http import git, object_manifest, verify_checkout
from server_references import all_object_ids


def shallow_ids(repo):
    path = repo / ".git" / "shallow"
    return set(path.read_bytes().splitlines()) if path.exists() else set()


def clone_one(root, bare, name, url, label, env, version, branch, depth, filter_spec):
    clone = root / f"{name}-{label}"
    args = ["git", "-c", f"protocol.version={version}", "clone", "-q", "--single-branch", "--no-tags", "--branch", branch]
    if depth:
        args += [f"--depth={depth}"]
    if filter_spec:
        args += [f"--filter={filter_spec}", "--no-checkout"]
    run(*args, url, str(clone), env=env)
    git(clone, "fsck", "--strict", env=env)
    if not filter_spec:
        verify_checkout(bare, clone, git(clone, "rev-parse", "HEAD", env=env).strip().decode(), env)
    return clone


def delivered(repo, env):
    # Git v0 advertises include-tag and may append annotations even with
    # --no-tags. Explicit include-tag has separate matched requests.
    return {oid for oid in all_object_ids(repo, env)
            if git(repo, "cat-file", "-t", oid.decode(), env=env).strip() != b"tag"}


def compare_delivered_bodies(left, right, objects, env):
    # Read only delivered objects; no promised-object fetch.
    for oid in objects:
        kind = git(left, "cat-file", "-t", oid.decode(), env=env).strip().decode()
        assert git(left, "cat-file", kind, oid.decode(), env=env) == git(right, "cat-file", kind, oid.decode(), env=env)


def compare_clone(root, bare, urls, env, version, branch, depth, filter_spec=None):
    label = f"bounded-v{version}-{branch}-{depth}-{filter_spec or 'all'}".replace(":", "-").replace("=", "-")
    clones = [clone_one(root, bare, name, url, label, env, version, branch, depth, filter_spec)
              for name, url in urls]
    left, right = clones
    assert shallow_ids(left) == shallow_ids(right), (label, shallow_ids(left), shallow_ids(right))
    left_objects, right_objects = delivered(left, env), delivered(right, env)
    assert left_objects == right_objects, (label, left_objects ^ right_objects)
    if filter_spec:
        compare_delivered_bodies(left, right, left_objects, env)
        return
    assert object_manifest(left, "HEAD", env) == object_manifest(right, "HEAD", env)
    if depth != 1:
        return
    # An unchanged shallow remote, absolute depth increase, then full history.
    # --deepen is deliberately absent: it requests the unsupported relative form.
    for options in ((), ("--depth=2",), ("--unshallow",)):
        for clone in clones:
            git(clone, "-c", f"protocol.version={version}", "fetch", "--no-tags", *options, "origin", env=env)
            git(clone, "fsck", "--strict", env=env)
        assert shallow_ids(left) == shallow_ids(right), (label, options)
        assert object_manifest(left, "HEAD", env) == object_manifest(right, "HEAD", env), (label, options)


def compare_bounded_fetch(root, bare, urls, env):
    git(bare, "config", "uploadpack.allowFilter", "true", env=env)
    for version, branch, depth in product((0, 1, 2), ("z-main", "light"), (1, 2, 8)):
        compare_clone(root, bare, urls, env, version, branch, depth)
    for version, filter_spec in product((0, 1, 2), ("blob:none", "blob:limit=5", "blob:limit=1k")):
        compare_clone(root, bare, urls, env, version, "light", None, filter_spec)
    for version in (0, 1, 2):
        compare_clone(root, bare, urls, env, version, "light", 1, "blob:none")
