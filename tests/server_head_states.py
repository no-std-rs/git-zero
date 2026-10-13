"""Compare git-zero server HEAD states with independently created Git remotes."""
import argparse
import os
from pathlib import Path
import urllib.request

from interop import command, environment, reference_server, run
from upstream_http import git, object_manifest, snapshot_fixture, verify_checkout


def oracle_fixture(root, state, env):
    bare = root / "repo.git"
    if state.startswith("unborn"):
        branch = "main" if state == "unborn-main" else "new/default"
        run("git", "init", "-q", "--bare", "--object-format=sha1", "-b", branch, str(bare), env=env)
        head = None
    else:
        bare, head = snapshot_fixture(root, env)
        git(bare, "update-ref", "--no-deref", "HEAD", head, env=env)
        if state == "detached-only":
            git(bare, "update-ref", "-d", "refs/heads/main", env=env)
    git(bare, "update-server-info", env=env)
    return bare, head


def raw_head(url):
    with urllib.request.urlopen(url + "/HEAD", timeout=10) as response:
        return response.read()


def compare_discovery(bare, reference, zero_url, smart, head, env):
    actual_head = raw_head(zero_url)
    # http-backend emits an empty /HEAD body for an unborn ref;
    # the static bare repository retains its symbolic HEAD file.
    assert actual_head == (bare / "HEAD").read_bytes()
    assert raw_head(reference) == (b"" if smart and head is None else actual_head)
    expected = run("git", "-c", "protocol.version=0", "ls-remote", "--symref", reference, env=env)
    actual = run("git", "-c", "protocol.version=0", "ls-remote", "--symref", zero_url, env=env)
    assert sorted(actual.splitlines()) == sorted(expected.splitlines()), (actual, expected)


def verify_clone(bare, clone, url, head, env):
    run("git", "-c", "protocol.version=0", "clone", "-q", url, str(clone), env=env)
    git(clone, "fsck", "--strict", env=env)
    if head is None:
        assert not git(clone, "for-each-ref", env=env)
        assert command("git", "-C", str(clone), "rev-parse", "--verify", "HEAD", env=env).returncode != 0
        assert git(clone, "count-objects", "-v", env=env).startswith(b"count: 0\n")
        assert git(clone, "rev-parse", "--show-object-format", env=env).strip() == b"sha1"
    else:
        assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
        assert object_manifest(bare, "HEAD", env) == object_manifest(clone, "HEAD", env)
        verify_checkout(bare, clone, head, env)
        git(clone, "fetch", "origin", "HEAD", env=env)
        assert git(clone, "rev-parse", "FETCH_HEAD", env=env).strip().decode() == head


def clone_refs(clone, env):
    head = command("git", "-C", str(clone), "symbolic-ref", "-q", "HEAD", env=env)
    refs = git(clone, "for-each-ref", "--format=%(refname) %(objectname)", env=env)
    return head.returncode, head.stdout, refs


def compare_protocol(root, bare, head, zero_url, smart, env):
    client_env = dict(env, GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="init.defaultBranch", GIT_CONFIG_VALUE_0="main")
    if not smart:
        client_env["GIT_SMART_HTTP"] = "0"
    protocol = "smart" if smart else "dumb"
    with reference_server(root, env, smart) as reference:
        compare_discovery(bare, reference, zero_url, smart, head, client_env)
        expected = root / f"reference-{protocol}"
        actual = root / f"zero-{protocol}"
        verify_clone(bare, expected, reference, head, client_env)
        verify_clone(bare, actual, zero_url, head, client_env)
        assert clone_refs(expected, client_env) == clone_refs(actual, client_env)


def compare(root, zero_url, state):
    env = environment(git=os.environ.get("GIT_ZERO_TEST_GIT", "git"), root=root)
    bare, head = oracle_fixture(root, state, env)
    for smart in (True, False):
        compare_protocol(root, bare, head, zero_url, smart, env)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--zero-url", required=True)
    parser.add_argument("--state", choices=("unborn-main", "unborn-custom", "detached-alias", "detached-only"), required=True)
    args = parser.parse_args()
    compare(args.root, args.zero_url, args.state)
