#!/usr/bin/env python3
"""Align public manifests, dependency requirements and lock entries for a release."""

from functools import partial
from pathlib import Path
import re
import sys
import tomllib
from release_packages import PACKAGES

ROOT = Path(__file__).resolve().parents[1]
VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"


def replace_one(source, pattern, replacement, label):
    updated, count = re.subn(pattern, replacement, source)
    if count != 1:
        raise ValueError(f"expected one {label} version entry")
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
    if PACKAGES.get(package) != relative:
        raise ValueError(f"unexpected package name in {relative}")
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
    if not re.fullmatch("v" + VERSION, tag):
        raise ValueError("expected a stable vMAJOR.MINOR.PATCH release tag")
    version = tag.removeprefix("v")
    if ROOT.joinpath("version.txt").read_text().strip() != version:
        raise ValueError("tag differs from version.txt")
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
