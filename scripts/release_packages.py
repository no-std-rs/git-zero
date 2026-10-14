"""Public crates in dependency order; examples and fuzz harnesses never publish."""

import re

VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"

PACKAGES = {
    "git-zero-core": "crates/git-zero-core/Cargo.toml",
    "git-zero-client": "crates/git-zero-client/Cargo.toml",
    "git-zero-server": "crates/git-zero-server/Cargo.toml",
    "git-zero": "Cargo.toml",
    "git-zero-rustix": "crates/git-zero-rustix/Cargo.toml",
}


def require(condition, message):
    """Reject invalid release metadata with the same error across build helpers."""
    if not condition:
        raise ValueError(message)


def release_version(value, *, tag=False):
    """Parse the shared stable version/tag contract used by release helpers."""
    prefix = "v" if tag else ""
    require(re.fullmatch(prefix + VERSION, value), "expected a stable release version or vMAJOR.MINOR.PATCH tag")
    return value[len(prefix):]
