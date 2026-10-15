"""Protocol v1/v2 comparisons using real Git clients and Git's HTTP backend."""
import urllib.error
import urllib.request

from interop import run
from server_references import packet, wanted_manifest
from server_negotiation import imported_pack
from upstream_http import git, object_manifest, verify_checkout


def command(name, fmt, args):
    return (packet(b"command=" + name + b"\n") + packet(b"object-format=" + fmt + b"\n")
            + b"0001" + b"".join(packet(arg + b"\n") for arg in args) + b"0000")


def post(url, body):
    request = urllib.request.Request(url + "/git-upload-pack", data=body, headers={
        "Git-Protocol": "version=2", "Content-Type": "application/x-git-upload-pack-request"})
    with urllib.request.urlopen(request, timeout=10) as response:
        assert response.headers.get_content_type() == "application/x-git-upload-pack-result"
        return response.read()


def packets(data):
    lines = []
    while data:
        length = int(data[:4], 16)
        if length == 0:
            assert data == b"0000"
            return lines
        if length == 1:
            lines.append(None)
            data = data[4:]
            continue
        assert 4 <= length <= min(len(data), 65520)
        lines.append(data[4:length])
        data = data[length:]
    raise AssertionError("missing response flush")


def compare_ls_refs(bare, urls, fmt):
    for args in [[], [b"unborn"], [b"symrefs", b"peel", b"unborn"], [b"ref-prefix refs/tags/", b"peel"],
                 [b"ref-prefix HEAD", b"symrefs", b"unborn"], [b"ref-prefix does-not-exist"]]:
        replies = [sorted(packets(post(url, command(b"ls-refs", fmt, args)))) for _, url in urls]
        # Git's ls-refs treats symref-target as a complete resolved ref name.
        assert replies[0] == replies[1], (args, replies)


def compare_client(root, bare, urls, head, version, env):
    traces = dict(env, GIT_TRACE_PACKET=str(root / f"v{version}.packets"),
                  GIT_TRACE_CURL=str(root / f"v{version}.http"), GIT_TRACE_CURL_NO_DATA="1")
    listings = [run("git", "-c", f"protocol.version={version}", "ls-remote", "--symref", url, env=traces)
                for _, url in urls]
    assert sorted(listings[0].splitlines()) == sorted(listings[1].splitlines())
    clone_heads = [clone_repository(root, bare, name, url, head, version, traces)
                   for name, url in urls]
    assert clone_heads[0] == clone_heads[1]
    if git(bare, "for-each-ref", env=env):
        for name, url in urls:
            incremental_fetch(root, bare, name, url, version, traces)


def clone_repository(root, bare, name, url, head, version, env):
    clone = root / f"{name}-v{version}-clone"
    run("git", "-c", f"protocol.version={version}", "clone", "-q", url, str(clone), env=env)
    git(clone, "fsck", "--strict", env=env)
    assert object_manifest(clone, "--all", env) == object_manifest(bare, "--all", env)
    if head:
        assert git(clone, "rev-parse", "HEAD", env=env).strip().decode() == head
        verify_checkout(bare, clone, head, env)
        return head
    actual = git(clone, "symbolic-ref", "HEAD", env=env)
    if version == 2:
        assert actual == git(bare, "symbolic-ref", "HEAD", env=env)
    return actual


def incremental_fetch(root, bare, name, url, version, env):
    branch = root / f"{name}-v{version}-incremental"
    run("git", "-c", f"protocol.version={version}", "clone", "-q", "--single-branch", "--no-tags",
        "--branch", "z-main", url, str(branch), env=env)
    git(branch, "-c", f"protocol.version={version}", "fetch", "--no-tags", "origin", "refs/tags/light", env=env)
    tip = git(bare, "rev-parse", "light", env=env).strip().decode()
    assert git(branch, "rev-parse", "FETCH_HEAD", env=env).strip().decode() == tip
    assert object_manifest(branch, tip, env) == object_manifest(bare, tip, env)
    git(branch, "checkout", "-q", "--detach", tip, env=env)
    verify_checkout(bare, branch, tip, env)
    git(branch, "fsck", "--strict", env=env)


def packfile(lines):
    assert lines[0] == b"packfile\n"
    payload = lines[1:]
    assert all(line[0] == 1 for line in payload)
    return b"".join(line[1:] for line in payload)


def fetch_response(lines, name, haves, unknown, done):
    if done:
        return packfile(lines)
    expected = [b"acknowledgments\n"] + [b"ACK " + oid + b"\n" for oid in haves if oid != unknown]
    if len(expected) == 1:
        expected.append(b"NAK\n")
    if b"ready\n" not in lines:
        assert lines == expected, (name, lines, expected)
        return None
    assert name == "reference" and lines[:len(expected)] == expected
    assert lines[len(expected):len(expected) + 2] == [b"ready\n", None]
    return packfile(lines[len(expected) + 2:])


def compare_fetch(root, bare, urls, fmt, env):
    tip = git(bare, "rev-parse", "light", env=env).strip()
    base = git(bare, "rev-parse", "z-main^", env=env).strip()
    side = git(bare, "rev-parse", "a-side", env=env).strip()
    main = git(bare, "rev-parse", "z-main", env=env).strip()
    unknown = b"f" * len(tip)
    for index, (wants, haves, done) in enumerate([
            ((tip,), (), True), ((tip,), (unknown,), False),
            ((tip,), (base,), False), ((tip,), (base,), True),
            ((tip, main, tip), (main, side), True)]):
        args = [b"no-progress"] + [b"want " + oid for oid in wants] + [b"have " + oid for oid in haves]
        if done:
            args.append(b"done")
        for name, url in urls:
            raw = post(url, command(b"fetch", fmt, args))
            label = f"{name}-v2-fetch-{index}"
            (root / (label + ".response")).write_bytes(raw)
            lines = packets(raw)
            packed = fetch_response(lines, name, haves, unknown, done)
            if packed is not None:
                imported_pack(root, bare, label, packed, wants, haves, env)
    compare_tags(root, bare, urls, fmt, side, env)


def included_tags(bare, sent, env):
    for oid in git(bare, "for-each-ref", "--format=%(objectname)", "refs/tags", env=env).splitlines():
        if git(bare, "cat-file", "-t", oid.decode(), env=env).strip() != b"tag":
            continue
        if git(bare, "rev-parse", oid.decode() + "^{}", env=env).strip() in sent:
            yield oid


def compare_tags(root, bare, urls, fmt, side, env):
    # Exercise nested annotated tags and blob/tree peels through include-tag.
    roots = (side,)
    sent = wanted_manifest(bare, roots, env)
    tags = tuple(included_tags(bare, sent, env))
    for name, url in urls:
        raw = post(url, command(b"fetch", fmt, [b"want " + side, b"include-tag", b"no-progress", b"done"]))
        imported_pack(root, bare, name + "-v2-tags", packfile(packets(raw)), (*roots, *tags), (), env)


def compare_protocol(root, bare, urls, head, env):
    fmt = git(bare, "rev-parse", "--show-object-format", env=env).strip()
    for version in [1, 2]:
        compare_client(root, bare, urls, head, version, env)
        for name, url in urls:
            request = urllib.request.Request(url + "/info/refs?service=git-upload-pack",
                                             headers={"Git-Protocol": f"version={version}"})
            with urllib.request.urlopen(request, timeout=10) as response:
                data = response.read()
            (root / f"{name}-v{version}-discovery").write_bytes(data)
            assert packet(f"version {version}\n".encode()) in data
    compare_ls_refs(bare, urls, fmt)
    if head and git(bare, "for-each-ref", env=env):
        compare_fetch(root, bare, urls, fmt, env)
