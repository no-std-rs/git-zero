"""Real-Git shallow/partial clone and absolute/relative-deepening oracle comparisons."""
from itertools import product
import re
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
    # An unchanged shallow remote, relative and absolute depth increases, then full history.
    for options in ((), ("--deepen=1",), ("--depth=2",), ("--unshallow",)):
        for clone in clones:
            git(clone, "-c", f"protocol.version={version}", "fetch", "--no-tags", *options, "origin", env=env)
            git(clone, "fsck", "--strict", env=env)
        assert shallow_ids(left) == shallow_ids(right), (label, options)
        assert object_manifest(left, "HEAD", env) == object_manifest(right, "HEAD", env), (label, options)


def compare_relative_boundaries(root, bare, urls, env, version):
    # Since Git 2.54, only boundaries in the requested history influence the
    # relative cut. Pin that policy and retain the older oracle's difference.
    git_version = tuple(map(int, re.search(rb"(\d+)\.(\d+)", run("git", "version", env=env)).groups()))
    side = git(bare, "rev-parse", "a-side", env=env).strip()
    base = git(bare, "rev-parse", "z-main^", env=env).strip()
    for selected_branch in (True, False):
        clones = []
        for name, url in urls:
            clone = root / f"{name}-relative-all-v{version}-{selected_branch}"
            run("git", "-c", f"protocol.version={version}", "clone", "-q", "--depth=1",
                "--no-single-branch", "--no-tags", url, str(clone), env=env)
            clones.append(clone)
        for generation, options in enumerate((("--deepen=1",), ("--deepen=2",))):
            for clone in clones:
                git(clone, "-c", f"protocol.version={version}", "fetch", "--no-tags", *options,
                    "origin", *(("z-main",) if selected_branch else ()), env=env)
                git(clone, "fsck", "--strict", env=env)
                verify_checkout(bare, clone, git(clone, "rev-parse", "HEAD", env=env).strip().decode(), env)
            expected = ({base} if generation == 0 else set()) | ({side} if selected_branch else set())
            reference_expected = expected - ({side} if selected_branch and git_version < (2, 54) else set())
            assert shallow_ids(clones[0]) == reference_expected, (version, options, "reference", git_version)
            assert shallow_ids(clones[1]) == expected, (version, options, "zero")
            assert object_manifest(clones[0], "--all", env) == object_manifest(clones[1], "--all", env)


def compare_relative_unrelated_tip(root, bare, urls, env, version):
    # Git 2.55 returns no new boundary when the wanted history has no client
    # boundary. This deliberately differs from 2.53's global-boundary walk.
    git_version = tuple(map(int, re.search(rb"(\d+)\.(\d+)", run("git", "version", env=env)).groups()))
    main = git(bare, "rev-parse", "z-main", env=env).strip()
    side = git(bare, "rev-parse", "a-side", env=env).strip()
    base = git(bare, "rev-parse", "z-main^", env=env).strip()
    clones = []
    for name, url in urls:
        clone = root / f"{name}-relative-unrelated-v{version}"
        run("git", "-c", f"protocol.version={version}", "clone", "-q", "--depth=1",
            "--single-branch", "--branch=z-main", "--no-tags", url, str(clone), env=env)
        git(clone, "-c", f"protocol.version={version}", "fetch", "--deepen=1", "--no-tags",
            "origin", "a-side", env=env)
        git(clone, "fsck", "--strict", env=env)
        clones.append(clone)
    assert shallow_ids(clones[1]) == {main}, (version, "zero unrelated boundary")
    expected_reference = {main} if git_version >= (2, 55) else ({main, side} if git_version >= (2, 54) else {base})
    assert shallow_ids(clones[0]) == expected_reference, (version, "reference unrelated boundary", git_version)
    assert object_manifest(clones[1], "FETCH_HEAD", env) == object_manifest(bare, "a-side", env)
    objects = delivered(clones[0], env)
    compare_delivered_bodies(clones[0], bare, objects, env)
    if git_version != (2, 54):
        assert object_manifest(clones[0], "FETCH_HEAD", env) == object_manifest(clones[1], "FETCH_HEAD", env)
        assert objects == delivered(clones[1], env)
    else:
        # Git 2.54 alone falls back to absolute depth N for this case; 2.55
        # explicitly removed that fallback. Its side tip is still a boundary.
        assert side in shallow_ids(clones[0])


def compare_bounded_fetch(root, bare, urls, env):
    git(bare, "config", "uploadpack.allowFilter", "true", env=env)
    for version, branch, depth in product((0, 1, 2), ("z-main", "light"), (1, 2, 8)):
        compare_clone(root, bare, urls, env, version, branch, depth)
    for version, filter_spec in product((0, 1, 2), ("blob:none", "blob:limit=5", "blob:limit=1k")):
        compare_clone(root, bare, urls, env, version, "light", None, filter_spec)
    for version in (0, 1, 2):
        compare_relative_boundaries(root, bare, urls, env, version)
        compare_relative_unrelated_tip(root, bare, urls, env, version)
        compare_clone(root, bare, urls, env, version, "light", 1, "blob:none")
