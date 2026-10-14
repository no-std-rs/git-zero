#!/usr/bin/env python3
"""Validate public crate metadata and the Runnerless dependency upload order."""

import argparse
import json
from pathlib import Path
import subprocess
import tomllib
from release_packages import PACKAGES, require

ROOT = Path(__file__).resolve().parents[1]


def check(release=False):
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=ROOT
    ))
    public = {package["name"]: package for package in metadata["packages"]
              if package["publish"] != []}
    require(public.keys() == PACKAGES.keys(), "public workspace packages differ from the release package list")
    versions = {package["version"] for package in public.values()}
    require(len(versions) == 1, "public packages must share one version")
    version = versions.pop()
    if release:
        require(version == ROOT.joinpath("version.txt").read_text().strip(),
                "prepared package versions differ from version.txt")
    earlier = set()
    for name, relative in PACKAGES.items():
        package = public[name]
        check_metadata(package, relative)
        check_dependencies(package, earlier, version)
        earlier.add(name)
    print(f"Public package metadata verified: {', '.join(PACKAGES)} ({version})")


def check_metadata(package, relative):
    name = package["name"]
    require(Path(package["manifest_path"]) == ROOT / relative, f"unexpected manifest path for {name}")
    for field in ("description", "license", "repository", "readme"):
        require(package[field], f"missing {field} for {name}")
    directory = Path(package["manifest_path"]).parent
    for license_name in ("LICENSE-MIT", "LICENSE-APACHE"):
        require((directory / license_name).is_file(), f"missing {license_name} for {name}")
    manifest = tomllib.loads((ROOT / relative).read_text())
    require("include" in manifest["package"], f"missing package file allowlist for {name}")


def check_dependencies(package, earlier, version):
    for dependency in package["dependencies"]:
        name = dependency["name"]
        if name not in PACKAGES:
            require(not dependency.get("path"), f"{package['name']} depends on an unpublished local package")
            continue
        require(name in earlier, f"{package['name']}'s dependencies must be uploaded first")
        require(dependency["req"] == "^" + version, f"{package['name']}'s dependency version differs from the release")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="require the prepared version.txt version")
    try:
        check(parser.parse_args().release)
    except ValueError as error:
        raise SystemExit(str(error)) from None
