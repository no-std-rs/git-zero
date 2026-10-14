#!/usr/bin/env python3
"""Build a real-Git fixture and boot gitfs in an isolated, unprivileged QEMU VM."""
import argparse
import gzip
import lzma
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
from https_origin import fixture as https_fixture, origin as https_origin

EXAMPLE = Path(__file__).resolve().parents[1]


def run(*command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def fixture(root):
    repo = root / "fixture/repo.git"
    repo.parent.mkdir()
    run("git", "init", "--bare", "--initial-branch=main", str(repo),
        stdout=subprocess.DEVNULL)
    env = {**os.environ, "GIT_AUTHOR_NAME": "gitfs test",
           "GIT_AUTHOR_EMAIL": "test@example.invalid",
           "GIT_COMMITTER_NAME": "gitfs test",
           "GIT_COMMITTER_EMAIL": "test@example.invalid"}

    def git(*args, data=None):
        return run("git", "--git-dir", str(repo), *args, input=data,
                   stdout=subprocess.PIPE, env=env).stdout.strip()

    def blob(data):
        return git("hash-object", "-w", "--stdin", data=data)

    def tree(entries):
        return git("mktree", "-z", "--missing", data=b"".join(
            mode + b" " + kind + b" " + oid + b"\t" + name + b"\0"
            for mode, kind, oid, name in entries))

    hello = blob(b"hello from kernel gitfs\n")
    nested = tree([(b"100644", b"blob", blob(b"nested\n"), b"note.txt")])
    big = [blob(bytes((i + seed) % 251 for i in range(700000)))
           for seed in range(3)]
    parallel = blob(b"concurrent\n")
    root_tree = tree([
        (b"100644", b"blob", hello, b"README.md"),
        (b"100644", b"blob", hello, b"alias.txt"),
        *((b"100644", b"blob", oid, f"big-{chr(97+i)}".encode())
          for i, oid in enumerate(big)),
        (b"040000", b"tree", nested, b"dir"),
        (b"040000", b"tree", tree([]), b"empty"),
        (b"120000", b"blob", blob(b"README.md"), b"link"),
        (b"100644", b"blob", parallel, b"parallel-a"),
        (b"100644", b"blob", parallel, b"parallel-b"),
        (b"100755", b"blob", blob(b"#!/bin/sh\n"), b"run"),
        (b"160000", b"commit", b"1" * 40, b"submodule"),
        (b"100644", b"blob", b"2" * 40, b"unavailable.txt"),
    ])
    revision = git("commit-tree", root_tree.decode(), data=b"kernel mount test\n")
    git("update-ref", "refs/heads/main", revision.decode())
    (root / "fixture/revision").write_bytes(revision)


def initramfs(root, destination):
    """Write newc directly; no root, cpio program, or device creation required."""
    archive = bytearray()
    inode = 0

    def entry(name, mode, data=b"", major=0, minor=0):
        nonlocal inode
        inode += 1
        filename = name.encode() + b"\0"
        fields = [inode, mode, 0, 0, 1, 0, len(data), 0, 0, major, minor,
                  len(filename), 0]
        archive.extend(b"070701" + "".join(f"{f:08x}" for f in fields).encode())
        archive.extend(filename)
        archive.extend(b"\0" * (-len(archive) % 4))
        archive.extend(data)
        archive.extend(b"\0" * (-len(archive) % 4))

    for path in sorted(root.rglob("*")):
        if path.is_dir():
            entry(str(path.relative_to(root)), stat.S_IFDIR | 0o755)
        elif path.is_file():
            entry(str(path.relative_to(root)), path.stat().st_mode,
                  path.read_bytes())
    entry("dev/console", stat.S_IFCHR | 0o600, major=5, minor=1)
    entry("TRAILER!!!", 0)
    destination.write_bytes(gzip.compress(archive, mtime=0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kernel", type=Path,
                        default=Path("/lib/modules") / os.uname().release / "vmlinuz")
    parser.add_argument("--module", type=Path, default=EXAMPLE / "gitfs.ko")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--log", type=Path, default=EXAMPLE / "build/qemu.log")
    parser.add_argument("--github", action="store_true",
                        help="also mount public GitHub directly from the guest kernel")
    args = parser.parse_args()
    args.log.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="gitfs-qemu-") as temporary:
        work = Path(temporary)
        root = work / "root"
        root.mkdir()
        (root / "dev").mkdir()
        fixture(root)
        certs = https_fixture(root)
        network_module = Path(run("modinfo", "-n", "e1000", stdout=subprocess.PIPE,
                                  text=True).stdout.strip())
        data = network_module.read_bytes()
        if network_module.suffix == ".xz":
            data = lzma.decompress(data)
        elif network_module.suffix == ".zst":
            data = run("zstd", "-d", "-c", str(network_module), stdout=subprocess.PIPE).stdout
        (root / "e1000.ko").write_bytes(data)
        if args.github:
            (root / "fixture/test-github").touch()
        run("cc", "-O2", "-Wall", "-Wextra", "-Werror",
            str(EXAMPLE / "tests/init.c"), "-o", str(root / "init"))
        # Include precisely the loader/libraries needed by the test's PID 1.
        dependencies = run("ldd", str(root / "init"),
                           stdout=subprocess.PIPE, text=True).stdout
        for name in re.findall(r"(/[^\s()]+)", dependencies):
            library = Path(name)
            target = root / library.relative_to("/")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(library, target)
            target.chmod(0o755)
        shutil.copyfile(args.module, root / "gitfs.ko")
        image = work / "initramfs.gz"
        with https_origin(root, certs) as port:
            (root / "fixture/tls-port").write_text(str(port))
            initramfs(root, image)
            command = ["qemu-system-x86_64", "-accel", "tcg", "-m", "512M",
                       "-smp", "2", "-nographic", "-no-reboot",
                       "-nic", "user,model=e1000",
                       "-kernel", str(args.kernel), "-initrd", str(image),
                       "-append", "console=ttyS0 rdinit=/init panic=-1 nokaslr quiet"]
            print("Booting gitfs test VM; log:", args.log, flush=True)
            with args.log.open("w") as log:
                try:
                    completed = subprocess.run(command, stdout=log,
                                               stderr=subprocess.STDOUT,
                                               timeout=args.timeout)
                except subprocess.TimeoutExpired:
                    raise SystemExit(f"VM timed out; inspect {args.log}")
        output = args.log.read_text(errors="replace")
        print("\n".join(line for line in output.splitlines()
                        if line.startswith(("ok:", "GITFS_TEST_", "gitfs:", "entry:", "git-zero main commit:"))))
        if completed.returncode or "GITFS_TEST_PASS" not in output:
            raise SystemExit(f"Kernel smoke test failed; inspect {args.log}")
        if re.search(r"BUG:|WARNING:|Oops:|Kernel panic|stack frame", output):
            raise SystemExit(f"Kernel diagnostics during test; inspect {args.log}")


if __name__ == "__main__":
    main()
