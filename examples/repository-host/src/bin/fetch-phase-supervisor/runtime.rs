//! Runner-only source/runtime pins. No host-specific ELF identity is assumed.
use super::replay::{executable, stream_hash};
use super::*;
const APPROVED: &str = include_str!("approved-source.sha256");
const CLOSURE_HASH: &str = "4e8283e57be8c2b4e01c35b427de54c23c73ec62e344340aa2b970ce35330632";
const MANIFEST_ENV: &str = "FETCH_PHASE_RUNTIME_MANIFEST";
const MANIFEST_CAP: u64 = 128 * 1024;
const ROLES: [&str; 7] = [
    "native",
    "supervisor",
    "git",
    "node",
    "wrangler",
    "workerd",
    "wrangler_lock",
];
const RUNNER_FILES: [&str; 4] = [
    "src/bin/fetch-phase-supervisor.rs",
    "src/bin/fetch-phase-supervisor/replay.rs",
    "src/bin/fetch-phase-supervisor/runtime.rs",
    "src/bin/fetch-phase-supervisor/approved-source.sha256",
];
pub(super) fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("build workspace exists")
}
fn approved_entries() -> Result<Vec<(&'static str, &'static str)>> {
    if hash(APPROVED.as_bytes()) != CLOSURE_HASH {
        return Err("compiled approved source closure changed".into());
    }
    APPROVED
        .lines()
        .map(|line| {
            let (digest, path) = line.split_once("  ").ok_or("source closure framing")?;
            if digest.len() != 64 || path.starts_with('/') || path.split('/').any(|c| c == "..") {
                return Err("source closure entry".into());
            }
            Ok((digest, path))
        })
        .collect()
}
fn source_files() -> Result<Value> {
    let root = workspace();
    let mut files = Vec::new();
    for (expected, rel) in approved_entries()? {
        let path = root.join(rel);
        if stream_hash(&path)? != expected {
            return Err(format!("approved Native/dependency/Worker source changed: {rel}").into());
        }
        files.push(json!({"path":path,"relative":rel,"sha256":expected}));
    }
    Ok(json!(files))
}
fn runner_files() -> Result<Value> {
    RUNNER_FILES
        .iter()
        .map(|rel| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
            Ok(json!({"relative":rel,"sha256":stream_hash(&path)?}))
        })
        .collect::<Result<Vec<_>>>()
        .map(|v| json!(v))
}
fn bounded_json(path: &Path) -> Result<(Value, String)> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MANIFEST_CAP + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MANIFEST_CAP {
        return Err("runtime manifest exceeds128KiB".into());
    }
    Ok((serde_json::from_slice(&bytes)?, hash(&bytes)))
}
fn validate_manifest(value: &Value, runners: &Value) -> Result<()> {
    if value["schema"] != 1
        || value["approved_native_source"] != STAGE1
        || value["approved_source_closure_sha256"] != CLOSURE_HASH
        || value["runner_source_files"] != *runners
    {
        return Err("runtime manifest does not bind approved source and actual runner".into());
    }
    if !matches!(
        value["build_profile"].as_str(),
        Some("debug" | "release" | "hosting")
    ) {
        return Err("explicit supported build profile required".into());
    }
    let source = value["build_source_commit_at_seal"]
        .as_str()
        .ok_or("build source commit")?;
    if source.len() != 40 || !source.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("explicit40-character build source commit required".into());
    }
    let binaries = value["binaries"]
        .as_object()
        .ok_or("runtime binary roles")?;
    if binaries.len() != ROLES.len() || ROLES.iter().any(|role| !binaries.contains_key(*role)) {
        return Err("runtime manifest requires exact binary roles".into());
    }
    for role in ROLES {
        let entry = &binaries[role];
        let p = entry["path"].as_str().ok_or("runtime absolute path")?;
        let h = entry["sha256"].as_str().ok_or("runtime binary hash")?;
        if p.len() > 4096
            || !Path::new(p).is_absolute()
            || h.len() != 64
            || !h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("runtime path/hash framing".into());
        }
    }
    Ok(())
}
pub(super) fn path(provenance: &Value, role: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        provenance["runtime_manifest"]["binaries"][role]["path"]
            .as_str()
            .ok_or("missing runtime path")?,
    ))
}
pub(super) fn seal(path: &Path) -> Result<()> {
    source_files()?;
    let supervisor = std::env::current_exe()?.canonicalize()?;
    let native = supervisor
        .with_file_name("repository-host")
        .canonicalize()?;
    let wrangler = std::env::var_os("FETCH_PHASE_WRANGLER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("cloudflare/node_modules/wrangler/bin/wrangler.js")
        })
        .canonicalize()?;
    let modules = wrangler
        .ancestors()
        .nth(3)
        .ok_or("Wrangler node_modules path")?;
    let workerd = modules
        .join("@cloudflare/workerd-linux-64/bin/workerd")
        .canonicalize()?;
    let lock = modules
        .parent()
        .ok_or("Wrangler install prefix")?
        .join("package-lock.json")
        .canonicalize()?;
    let mut binaries = serde_json::Map::new();
    for (role, p) in [
        ("native", native),
        ("supervisor", supervisor),
        ("git", executable("git")?),
        ("node", executable("node")?),
        ("wrangler", wrangler),
        ("workerd", workerd),
        ("wrangler_lock", lock),
    ] {
        binaries.insert(role.into(), json!({"path":p,"sha256":stream_hash(&p)?}));
    }
    let value = json!({"schema":1,"approved_native_source":STAGE1,"approved_source_closure_sha256":CLOSURE_HASH,
        "build_source_commit_at_seal":std::env::var("FETCH_PHASE_BUILD_SOURCE_COMMIT")?,
        "build_profile":std::env::var("FETCH_PHASE_BUILD_PROFILE")?,
        "build_provenance_note":"operator records exact build commands/toolchain separately; fresh ELF hashes are identities, not reproducible-build claims",
        "runner_source_files":runner_files()?,"binaries":binaries});
    validate_manifest(&value, &runner_files()?)?;
    verify_value(&value)?;
    let bytes = serde_json::to_vec_pretty(&value)?;
    if bytes.len() as u64 > MANIFEST_CAP {
        return Err("runtime manifest exceeds128KiB".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&bytes)?;
    println!("sealed explicit runtime manifest: {}", path.display());
    Ok(())
}
fn verify_value(manifest: &Value) -> Result<()> {
    validate_manifest(manifest, &runner_files()?)?;
    for role in ROLES {
        let entry = &manifest["binaries"][role];
        let path = Path::new(entry["path"].as_str().ok_or("runtime path")?);
        if path.canonicalize()? != path || stream_hash(path)? != entry["sha256"] {
            return Err(format!("runtime actual identity changed: {role}").into());
        }
    }
    for role in ["git", "node"] {
        if executable(role)?
            != Path::new(
                manifest["binaries"][role]["path"]
                    .as_str()
                    .ok_or("tool path")?,
            )
        {
            return Err(format!("PATH differs from sealed {role}").into());
        }
    }
    if std::env::current_exe()?.canonicalize()?
        != Path::new(
            manifest["binaries"]["supervisor"]["path"]
                .as_str()
                .ok_or("supervisor path")?,
        )
    {
        return Err("actual supervisor path differs from runtime seal".into());
    }
    let lock = workspace().join("examples/repository-host/cloudflare/package-lock.json");
    if manifest["binaries"]["wrangler_lock"]["sha256"] != stream_hash(&lock)? {
        return Err("Wrangler installation lock differs from approved source".into());
    }
    Ok(())
}
pub(super) fn manifest_hash() -> Result<String> {
    let path =
        PathBuf::from(std::env::var_os(MANIFEST_ENV).ok_or("explicit runtime manifest required")?);
    Ok(bounded_json(&path)?.1)
}
pub(super) fn verify() -> Result<Value> {
    let path = PathBuf::from(
        std::env::var_os(MANIFEST_ENV).ok_or("explicit FETCH_PHASE_RUNTIME_MANIFEST required")?,
    );
    let (manifest, digest) = bounded_json(&path)?;
    let files = source_files()?;
    verify_value(&manifest)?;
    Ok(
        json!({"native_source":STAGE1,"approved_source_closure_sha256":CLOSURE_HASH,
        "source_files":files,"runtime_manifest_path":path.canonicalize()?,"runtime_manifest_sha256":digest,
        "runtime_manifest":manifest,"cpu_ms":null}),
    )
}
// No raw environment values are retained. Only an exact expected safe value is
// emitted; unknown/wrong Git values are withheld and qualification fails closed.
fn native_environment(bytes: &[u8], ledger: &Path) -> Value {
    let expected = BTreeMap::from([
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_COUNT", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ZERO_PHASE_LEDGER", ledger.to_str().unwrap_or("")),
    ]);
    let mut seen = BTreeMap::new();
    let mut names = Vec::new();
    let mut invalid = Vec::new();
    let mut duplicate = false;
    for item in bytes.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let Some(at) = item.iter().position(|b| *b == b'=') else {
            invalid.push("malformed environment entry".to_owned());
            continue;
        };
        let Ok(key) = std::str::from_utf8(&item[..at]) else {
            invalid.push("non-UTF8 environment name".to_owned());
            continue;
        };
        if !key.starts_with("GIT_") {
            continue;
        }
        names.push(key.to_owned());
        let safe = expected
            .get(key)
            .filter(|value| value.as_bytes() == &item[at + 1..]);
        if seen.insert(key.to_owned(), safe.copied()).is_some() {
            duplicate = true;
        }
        if safe.is_none() {
            invalid.push(key.to_owned());
        }
    }
    names.sort();
    invalid.sort();
    let missing = expected
        .keys()
        .filter(|key| !seen.contains_key(**key))
        .copied()
        .collect::<Vec<_>>();
    let eligible = !bytes.is_empty() && missing.is_empty() && invalid.is_empty() && !duplicate;
    json!({"raw_environment_bytes":bytes.len(),"raw_environment_sha256":hash(bytes),"git_environment_names":names,
        "safe_git_values":seen,"invalid_git_names":invalid,"missing_canonical_git_names":missing,
        "duplicate_git_names":duplicate,"identity_environment_eligible":eligible,"raw_environment_retained":false})
}
pub(super) fn capture_native(
    root: &Path,
    pid: i32,
    start: u64,
    expected_elf: &str,
) -> Result<Value> {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_default()
    };
    let began = now();
    let before = process(pid);
    let exe_before = stream_hash(Path::new(&format!("/proc/{pid}/exe")));
    let mut bytes = Vec::new();
    let raw = fs::File::open(format!("/proc/{pid}/environ"))
        .and_then(|file| file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes));
    let after = process(pid);
    let exe_after = stream_hash(Path::new(&format!("/proc/{pid}/exe")));
    let group = process(std::process::id() as i32)?.group;
    let matched = before
        .as_ref()
        .ok()
        .is_some_and(|p| p.start == start && p.group == group && p.state != "Z")
        && after
            .as_ref()
            .ok()
            .is_some_and(|p| p.start == start && p.group == group && p.state != "Z")
        && exe_before.as_ref().ok().is_some_and(|h| h == expected_elf)
        && exe_after.as_ref().ok().is_some_and(|h| h == expected_elf);
    let complete = raw.is_ok() && bytes.len() <= 2 * 1024 * 1024;
    let mut value = if complete {
        native_environment(&bytes, &root.join("native-phase.jsonl"))
    } else {
        json!({"raw_environment_bytes":null,"raw_environment_sha256":null,"identity_environment_eligible":false,"raw_environment_retained":false})
    };
    value["source"] = json!(format!("/proc/{pid}/environ"));
    value["read_after_existing_readiness_log"] = json!(true);
    value["capture_begin_unix_ns"] = json!(began);
    value["capture_end_unix_ns"] = json!(now());
    value["process_before"] = before.as_ref().ok().map(process_row).unwrap_or(Value::Null);
    value["process_after"] = after.as_ref().ok().map(process_row).unwrap_or(Value::Null);
    value["actual_exe_sha256_before"] = json!(exe_before.ok());
    value["actual_exe_sha256_after"] = json!(exe_after.ok());
    value["raw_read_error_kind"] = json!(raw.err().map(|e| format!("{:?}", e.kind())));
    value["raw_read_complete"] = json!(complete);
    value["pid_start_pgid_and_elf_match"] = json!(matched);
    value["identity_environment_eligible"] =
        json!(matched && complete && value["identity_environment_eligible"] == true);
    save(root, "native-environment.json", &value)?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn good() -> Vec<u8> {
        b"PATH=/bin\0GIT_CONFIG_NOSYSTEM=1\0GIT_CONFIG_GLOBAL=/dev/null\0GIT_CONFIG_COUNT=0\0GIT_TERMINAL_PROMPT=0\0GIT_ZERO_PHASE_LEDGER=/owned/native-phase.jsonl\0".to_vec()
    }
    #[test]
    fn empty_and_missing_environments_fail_closed() {
        for bytes in [
            Vec::new(),
            b"PATH=/bin\0".to_vec(),
            b"GIT_CONFIG_COUNT=0\0".to_vec(),
        ] {
            assert_eq!(
                native_environment(&bytes, Path::new("/owned/native-phase.jsonl"))["identity_environment_eligible"],
                false
            );
        }
    }
    #[test]
    fn only_exact_safe_values_qualify_and_are_logged() {
        assert_eq!(
            native_environment(&good(), Path::new("/owned/native-phase.jsonl"))["identity_environment_eligible"],
            true
        );
        for extra in [
            "GIT_TRACE2_EVENT=secret-value\0",
            "GIT_EDITOR=secret-value\0",
            "GIT_CONFIG_COUNT=secret-value\0",
        ] {
            let mut bytes = good();
            bytes.extend_from_slice(extra.as_bytes());
            let receipt = native_environment(&bytes, Path::new("/owned/native-phase.jsonl"));
            assert_eq!(receipt["identity_environment_eligible"], false);
            assert!(!receipt.to_string().contains("secret-value"));
        }
        assert_eq!(
            native_environment(&good(), Path::new("/wrong-ledger"))["identity_environment_eligible"],
            false
        );
    }
    #[test]
    fn runtime_manifest_is_bounded_and_rejects_malformed_input() {
        let path = std::env::temp_dir().join(format!(
            "fetch-phase-manifest-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, vec![b'x'; MANIFEST_CAP as usize + 1]).unwrap();
        assert!(
            bounded_json(&path)
                .unwrap_err()
                .to_string()
                .contains("exceeds128KiB")
        );
        fs::write(&path, b"{").unwrap();
        assert!(bounded_json(&path).is_err());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn runtime_manifest_requires_fixed_source_and_every_role() {
        let runners = json!([]);
        let binaries: serde_json::Map<String, Value> = ROLES
            .iter()
            .map(|r| {
                (
                    r.to_string(),
                    json!({"path":"/actual/tool","sha256":"a".repeat(64)}),
                )
            })
            .collect();
        let mut value = json!({"schema":1,"build_profile":"debug","approved_native_source":STAGE1,"approved_source_closure_sha256":CLOSURE_HASH,"runner_source_files":runners,"build_source_commit_at_seal":"e".repeat(40),"binaries":binaries});
        assert!(validate_manifest(&value, &runners).is_ok());
        // A fresh-host ELF hash is allowed only under this explicit sealed manifest.
        value["binaries"]["native"]["sha256"] = json!("b".repeat(64));
        assert!(validate_manifest(&value, &runners).is_ok());
        value["runner_source_files"] = json!([{"changed":true}]);
        assert!(validate_manifest(&value, &runners).is_err());
        value["runner_source_files"] = runners.clone();
        value["approved_source_closure_sha256"] = json!("b".repeat(64));
        assert!(validate_manifest(&value, &runners).is_err());
        value["approved_source_closure_sha256"] = json!(CLOSURE_HASH);
        value["binaries"].as_object_mut().unwrap().remove("native");
        assert!(validate_manifest(&value, &runners).is_err());
    }
}
