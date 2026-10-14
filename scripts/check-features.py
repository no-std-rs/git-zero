#!/usr/bin/env python3
"""Check feature combinations as separate Cargo builds to avoid workspace unification."""

from itertools import combinations
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def run(*arguments):
    return subprocess.run(["cargo", *arguments], cwd=ROOT, check=True)


def main():
    matrices = {
        "git-zero-core": ("read", "write"),
        "git-zero": ("read", "write", "client", "server"),
        "git-zero-rustix": ("client", "server", "storage", "cli"),
        "git-zero-client": (),
        "git-zero-server": ("read",),
    }
    for package, features in matrices.items():
        for size in range(len(features) + 1):
            for selected in combinations(features, size):
                arguments = ["check", "-p", package, "--all-targets", "--no-default-features", "--locked"]
                if selected:
                    arguments += ["--features", ",".join(selected)]
                run(*arguments)
    # An isolated client must not link the server, nor a server the client.
    for role, excluded in (("client", "git-zero-server"), ("server", "git-zero-client")):
        dependencies = subprocess.check_output([
            "cargo", "tree", "-p", "git-zero", "--no-default-features", "--features", role,
            "--edges", "normal", "--prefix", "none", "--format", "{p}|{f}", "--locked"
        ], cwd=ROOT, text=True)
        core = next(line for line in dependencies.splitlines() if line.startswith("git-zero-core "))
        expected = "read" if role == "client" else "write"
        if core.split("|")[1] != expected:
            raise RuntimeError(f"{role}-only facade enabled unexpected core features: {core}")
        if excluded in dependencies:
            raise RuntimeError(f"{role}-only facade unexpectedly depends on {excluded}")
    print("All 39 isolated feature combinations passed; client/server dependencies are separate.")


if __name__ == "__main__":
    main()
