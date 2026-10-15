"""Independent v0 ACK/sideband and incremental-pack comparisons with Git."""
import struct
import urllib.error
import urllib.request

from interop import run
from upstream_http import git, object_manifest, verify_checkout
from server_references import all_object_ids, packet, wanted_manifest


def post(url, body):
    request = urllib.request.Request(url + "/git-upload-pack", data=body,
                                    headers={"Content-Type": "application/x-git-upload-pack-request"})
    with urllib.request.urlopen(request, timeout=10) as response:
        assert response.headers.get_content_type() == "application/x-git-upload-pack-result"
        return response.read()


def upload_body(wants, haves, capabilities, done):
    lines = [b"want " + oid for oid in wants]
    if capabilities:
        lines[0] += b" " + capabilities
    body = b"".join(packet(line + b"\n") for line in lines) + b"0000"
    body += b"".join(packet(b"have " + oid + b"\n") for oid in haves)
    return body + (packet(b"done\n") if done else b"0000")


def decode(data, sideband):
    acks, packed = [], bytearray()
    while data:
        if not sideband and data.startswith(b"PACK"):
            return acks, data
        length = int(data[:4], 16)
        if length == 0:
            assert sideband and not data[4:]
            return acks, bytes(packed)
        assert 4 <= length <= min(len(data), 1000 if sideband == b"side-band" else 65520)
        line, data = data[4:length], data[length:]
        if line.startswith((b"ACK ", b"NAK")):
            acks.append(line.rstrip(b"\n"))
        else:
            assert sideband and line[0] == 1, line
            packed.extend(line[1:])
    assert not packed, "sideband terminator missing"
    return acks, b""


def imported_pack(root, bare, label, packed, wants, haves, env):
    expected = wanted_manifest(bare, wants, env)
    common = wanted_manifest(bare, haves, env)
    missing = set(expected) - set(common)
    assert struct.unpack(">I", packed[8:12])[0] == len(missing), (label, missing)
    (root / (label + ".pack")).write_bytes(packed)
    imported = root / (label + ".git")
    object_format = "sha256" if len(wants[0]) == 64 else "sha1"
    run("git", "init", "-q", "--bare", "--object-format=" + object_format, str(imported), env=env)
    if haves:
        # Seed the receiver's history independently, using Git's own pack writer.
        seed = git(bare, "pack-objects", "--stdout", "--revs", env=env, data=b"\n".join(haves) + b"\n")
        git(imported, "index-pack", "--strict", "--stdin", env=env, data=seed)
    git(imported, "index-pack", "--strict", "--stdin", env=env, data=packed)
    assert all_object_ids(imported, env) == set(expected) | set(common)
    assert wanted_manifest(imported, wants, env) == expected
    git(imported, "fsck", "--strict", env=env)


def check_acks(name, acks, known, unknown, mode, done):
    if not known:
        assert acks == [b"NAK"], (name, acks)
        return
    if not mode:
        assert acks == [b"ACK " + known[0]], (name, acks)
        return
    suffix = b" continue" if mode == b"multi_ack" else b" common"
    assert all(b"ACK " + oid + suffix in acks for oid in known), (name, acks)
    assert acks[-1] == (b"ACK " + known[-1] if done else b"NAK"), (name, acks)
    assert all(unknown not in line for line in acks)


def compare_round(root, bare, urls, tip, unknown, fmt, index, round_, env):
    mode, band, haves, done = round_
    caps = b" ".join(part for part in (fmt, mode, band, b"no-progress") if part)
    request = upload_body((tip,), haves, caps, done)
    known = tuple(oid for oid in haves if oid != unknown)
    for name, url in urls:
        raw = post(url, request)
        label = f"{name}-negotiation-{index}"
        (root / (label + ".response")).write_bytes(raw)
        acks, packed = decode(raw, band)
        check_acks(name, acks, known, unknown, mode, done)
        if done:
            imported_pack(root, bare, label, packed, (tip,), known, env)
        else:
            assert not packed


def check_incremental_fetch(root, bare, name, url, tip, env):
    clone = root / (name + "-negotiated-fetch")
    run("git", "-c", "protocol.version=0", "clone", "-q", "--single-branch", "--no-tags",
        "--branch", "z-main", url, str(clone), env=env)
    git(clone, "-c", "protocol.version=0", "fetch", "--no-tags", "origin", "refs/tags/light", env=env)
    assert git(clone, "rev-parse", "FETCH_HEAD", env=env).strip() == tip
    assert object_manifest(clone, tip.decode(), env) == object_manifest(bare, tip.decode(), env)
    git(clone, "checkout", "-q", "--detach", tip.decode(), env=env)
    verify_checkout(bare, clone, tip.decode(), env)
    git(clone, "fsck", "--strict", env=env)


def compare_negotiation(root, bare, urls, env):
    tip = git(bare, "rev-parse", "refs/tags/light", env=env).strip()
    main = git(bare, "rev-parse", "z-main", env=env).strip()
    side = git(bare, "rev-parse", "a-side", env=env).strip()
    base = git(bare, "rev-parse", "z-main^", env=env).strip()
    unknown = b"f" * len(tip)
    fmt = b"object-format=sha256" if len(tip) == 64 else b""
    rounds = [
        (b"", b"", (), True),
        (b"", b"", (base,), True),
        (b"multi_ack", b"", (unknown, base), False),
        (b"multi_ack", b"side-band", (main, side), True),
        (b"multi_ack_detailed", b"side-band-64k", (unknown, base), False),
        (b"multi_ack_detailed", b"side-band-64k", (main, side), True),
        (b"multi_ack_detailed", b"side-band-64k", (), True),
    ]
    for index, round_ in enumerate(rounds):
        compare_round(root, bare, urls, tip, unknown, fmt, index, round_, env)

    # The same immutable server supports a real client's incremental fetch from
    # one branch into the merge tip, with existing common history at the client.
    for name, url in urls:
        check_incremental_fetch(root, bare, name, url, tip, env)

    # Buffered hosts replace a rejected request with a protocol ERR response.
    zero = urls[1][1]
    try:
        post(zero, upload_body((unknown,), (), fmt, True))
    except urllib.error.HTTPError as error:
        raw = error.read()
        assert error.code == 400 and raw[4:].startswith(b"ERR ") and b"PACK" not in raw
    else:
        raise AssertionError("unauthorized want succeeded")
