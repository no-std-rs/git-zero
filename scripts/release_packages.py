"""Public crates in dependency order; examples and fuzz harnesses never publish."""

PACKAGES = {
    "git-zero-core": "crates/git-zero-core/Cargo.toml",
    "git-zero-client": "crates/git-zero-client/Cargo.toml",
    "git-zero-server": "crates/git-zero-server/Cargo.toml",
    "git-zero": "Cargo.toml",
    "git-zero-rustix": "crates/git-zero-rustix/Cargo.toml",
}
