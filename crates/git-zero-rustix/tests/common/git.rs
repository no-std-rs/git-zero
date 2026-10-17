//! Isolated Git child commands for native acceptance tests.
use std::{
    ffi::OsStr,
    process::{Command, Output},
};

pub fn run(program: impl AsRef<OsStr>, args: &[&str], dumb: bool) -> Output {
    let mut command = Command::new(program);
    for (name, _) in
        std::env::vars_os().filter(|(name, _)| name.as_encoded_bytes().starts_with(b"GIT_"))
    {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    if dumb {
        command.env("GIT_SMART_HTTP", "0");
    }
    let result = command.args(args).output().unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}
