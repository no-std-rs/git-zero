#!/usr/bin/env python3
"""Align public manifests, dependency requirements and lock entries for a release."""

from functools import partial
from pathlib import Path
import re
import sys
import tomllib
from release_packages import PACKAGES, release_version, require

ROOT = Path(__file__).resolve().parents[1]


def replace_one(source, pattern, replacement, label):
    updated, count = re.subn(pattern, replacement, source)
    require(count == 1, f"expected one {label} version entry")
    return updated


def dependency_version(match, version):
    name, body = match[1], match[2]
    fields = tomllib.loads("dependency = {" + body + "}")["dependency"]
    if fields.get("package", name) in PACKAGES and "path" in fields and "version" in fields:
        body = re.sub(r'version\s*=\s*"[^"\n]+"', f'version = "{version}"', body)
    return name + " = {" + body + "}"


def manifest_version(relative, version):
    source = (ROOT / relative).read_text()
    package = tomllib.loads(source)["package"]["name"]
    require(PACKAGES.get(package) == relative, f"unexpected package name in {relative}")
    source = replace_one(source, r'(?m)^version\s*=\s*"[^"\n]+"', f'version = "{version}"', relative)
    return re.sub(r'(?m)^([A-Za-z0-9_-]+)\s*=\s*\{([^\n]+)\}',
                  partial(dependency_version, version=version), source)


def lock_version(version):
    source = (ROOT / "Cargo.lock").read_text()
    for name in PACKAGES:
        pattern = r'(\[\[package\]\]\nname = "' + re.escape(name) + r'"\n)version = "[^"\n]+"'
        source = replace_one(source, pattern, lambda match: match[1] + f'version = "{version}"', name)
    return source


def prepare(tag):
    version = release_version(tag, tag=True)
    require(ROOT.joinpath("version.txt").read_text().strip() == version, "tag differs from version.txt")
    updates = {ROOT / relative: manifest_version(relative, version) for relative in PACKAGES.values()}
    updates[ROOT / "Cargo.lock"] = lock_version(version)
    # Validate every manifest and lock entry before writing any file.
    for path, source in updates.items():
        path.write_text(source)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: prepare-release.py vMAJOR.MINOR.PATCH")
    try:
        prepare(sys.argv[1])
    except ValueError as error:
        raise SystemExit(str(error)) from None
