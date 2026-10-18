//! Failure-only stderr retention; never reads fixtures, request bodies or env.
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, io, io::Read, path::Path};

const FILE_BYTES: usize = 64 * 1024;
const TOTAL_BYTES: usize = 512 * 1024;
const PATH_BYTES: usize = 256;
const MANIFEST_BYTES: usize = 512 * 1024;

fn bounded_path(path: &Path) -> (String, bool) {
    let text = path.to_string_lossy();
    let mut start = text.len().saturating_sub(PATH_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), start != 0)
}

// Hash the complete stopped file while retaining only its bounded final bytes.
fn hash_and_tail(mut file: fs::File, cap: usize) -> io::Result<(u64, String, Vec<u8>)> {
    let mut hash = Sha256::new();
    let mut tail = vec![0; cap];
    let mut written = 0usize;
    let mut total = 0u64;
    let mut buffer = [0; 16 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        total += n as u64;
        if cap != 0 {
            for part in buffer[..n].chunks(cap) {
                let first = part.len().min(cap - written);
                tail[written..written + first].copy_from_slice(&part[..first]);
                tail[..part.len() - first].copy_from_slice(&part[first..]);
                written = (written + part.len()) % cap;
            }
        }
    }
    if total >= cap as u64 {
        tail.rotate_left(written);
    } else {
        tail.truncate(total as usize);
    }
    Ok((total, format!("{:x}", hash.finalize()), tail))
}

pub(super) fn export(
    fixtures: &Path,
    destination: &Path,
    samples: u64,
    prototype: &str,
) -> io::Result<()> {
    // Same bounds as the runner, including at most 202 expected trial paths.
    if !(2..=100).contains(&samples) || !["prototype", "cloudflare-local"].contains(&prototype) {
        return Err(io::Error::other("invalid failure log trial selection"));
    }
    fs::create_dir(destination)?; // A unique run path; never overwrite evidence.
    let mut remaining = TOTAL_BYTES;
    let mut entries = Vec::new();
    // Preserve the latest (possibly failed) trial before spending bytes on old
    // successful stderr. No fixture-directory recursion or arbitrary globbing.
    for sample in (0..samples as i64).rev().chain(std::iter::once(-1)) {
        for backend in [prototype, "git"] {
            let trial = format!("{sample}-{backend}");
            let directory = fixtures.join(&trial);
            let metadata = match fs::symlink_metadata(&directory) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                result => result,
            };
            let source = directory.join("host.log");
            let (path, path_truncated) = bounded_path(&source);
            let mut entry = json!({"trial":trial,"original_path":path,
                "original_path_truncated":path_truncated,
                "original_bytes":null,"original_sha256":null,
                "retained_path":null,"retained_bytes":0,"retained_sha256":null,
                "truncated":null,"truncation_reason":null,"missing_reason":null});
            let copied = (|| -> io::Result<()> {
                if !metadata?.is_dir() || !fs::symlink_metadata(&source)?.is_file() {
                    return Err(io::Error::other(
                        "refusing nonregular host log or trial directory",
                    ));
                }
                let (bytes, hash, tail) =
                    hash_and_tail(fs::File::open(&source)?, FILE_BYTES.min(remaining))?;
                let name = format!("{trial}.host.log");
                fs::write(destination.join(&name), &tail)?;
                remaining -= tail.len();
                entry["original_bytes"] = json!(bytes);
                entry["original_sha256"] = json!(hash);
                entry["retained_path"] = json!(name);
                entry["retained_bytes"] = json!(tail.len());
                entry["retained_sha256"] = json!(format!("{:x}", Sha256::digest(&tail)));
                entry["truncated"] = json!(bytes > tail.len() as u64);
                if bytes > tail.len() as u64 {
                    entry["truncation_reason"] = json!(if tail.len() < FILE_BYTES {
                        "total-log-budget"
                    } else {
                        "per-file-budget"
                    });
                }
                Ok(())
            })();
            if let Err(error) = copied {
                // Error kinds cannot accidentally include headers, env or bodies.
                entry["missing_reason"] = json!(error.kind().to_string());
            }
            entries.push(entry);
        }
    }
    let manifest = json!({"version":1,"scope":"Failure-only stopped Native/reference stderr tails; original fixtures remain retained. Missing/unretained bytes are not empty evidence.",
        "per_file_budget_bytes":FILE_BYTES,"total_log_budget_bytes":TOTAL_BYTES,
        "retained_log_bytes":TOTAL_BYTES-remaining,"path_budget_bytes":PATH_BYTES,
        "original_path_encoding":"lossy UTF-8; oversized paths retain the final path bytes",
        "files":entries});
    let bytes = serde_json::to_vec(&manifest)?;
    if bytes.len() > MANIFEST_BYTES {
        return Err(io::Error::other("failure log manifest exceeds budget"));
    }
    fs::write(destination.join("manifest.json"), bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "host-failure-log-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn log(&self, trial: &str, bytes: &[u8]) {
            let dir = self.0.join(trial);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("host.log"), bytes).unwrap();
        }
        fn export(&self, samples: u64) -> serde_json::Value {
            export(
                &self.0,
                &self.0.join("retained"),
                samples,
                "cloudflare-local",
            )
            .unwrap();
            serde_json::from_slice(&fs::read(self.0.join("retained/manifest.json")).unwrap())
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn large_binary_log_keeps_exact_tail_and_full_original_hash() {
        let f = Fixture::new();
        let bytes: Vec<_> = (0..FILE_BYTES * 3 + 19)
            .map(|i| (i * 17 % 251) as u8)
            .collect();
        f.log("-1-cloudflare-local", &bytes);
        f.log("-1-git", b"reference stopped\n");
        let m = f.export(2);
        let first = &m["files"][0];
        let kept = fs::read(f.0.join("retained/-1-cloudflare-local.host.log")).unwrap();
        assert_eq!(kept, bytes[bytes.len() - FILE_BYTES..]);
        assert_eq!(first["original_bytes"], bytes.len());
        assert_eq!(first["original_sha256"], git_zero_host::hash(&bytes));
        assert_eq!(first["retained_sha256"], git_zero_host::hash(&kept));
        assert_eq!(first["truncated"], true);
        assert_eq!(m["files"][1]["truncated"], false);
        assert_eq!(m["files"][1]["missing_reason"], serde_json::Value::Null);
        assert!(export(&f.0, &f.0.join("retained"), 2, "cloudflare-local").is_err());
    }

    #[test]
    fn total_budget_prioritizes_latest_trial_and_marks_unretained_older_bytes() {
        let f = Fixture::new();
        for sample in 0..10 {
            f.log(
                &format!("{sample}-cloudflare-local"),
                &vec![b'x'; FILE_BYTES + 1],
            );
        }
        let m = f.export(10);
        assert_eq!(m["retained_log_bytes"], TOTAL_BYTES);
        assert_eq!(m["files"][0]["trial"], "9-cloudflare-local");
        assert_eq!(m["files"][0]["retained_bytes"], FILE_BYTES);
        assert_eq!(m["files"][9]["original_bytes"], FILE_BYTES + 1);
        assert_eq!(m["files"][9]["retained_bytes"], 0);
        assert_eq!(m["files"][9]["truncated"], true);
        assert_eq!(
            m["files"][9]["original_sha256"],
            git_zero_host::hash(&vec![b'x'; FILE_BYTES + 1])
        );
        assert!(
            fs::metadata(f.0.join("retained/manifest.json"))
                .unwrap()
                .len()
                < MANIFEST_BYTES as u64
        );
    }

    #[test]
    fn missing_log_is_distinct_from_empty_log_and_paths_are_utf8_bounded() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("-1-cloudflare-local")).unwrap();
        f.log("-1-git", b"");
        let m = f.export(2);
        assert_eq!(m["files"][0]["missing_reason"], "entity not found");
        assert_eq!(m["files"][0]["original_sha256"], serde_json::Value::Null);
        assert_eq!(m["files"][0]["truncated"], serde_json::Value::Null);
        assert_eq!(
            m["files"][1]["original_sha256"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(m["files"][1]["truncated"], false);
        let long = format!("/{}/host.log", "é".repeat(PATH_BYTES));
        let (path, truncated) = bounded_path(Path::new(&long));
        assert!(truncated && path.len() <= PATH_BYTES && path.ends_with("/host.log"));
    }

    #[test]
    fn maximum_trial_manifest_fits_budget_and_invalid_selection_creates_nothing() {
        let f = Fixture::new();
        for sample in (0..100).chain(std::iter::once(-1)) {
            for backend in ["cloudflare-local", "git"] {
                f.log(&format!("{sample}-{backend}"), b"");
            }
        }
        let m = f.export(100);
        assert_eq!(m["files"].as_array().unwrap().len(), 202);
        assert_eq!(m["retained_log_bytes"], 0);
        assert!(
            fs::metadata(f.0.join("retained/manifest.json"))
                .unwrap()
                .len()
                < MANIFEST_BYTES as u64
        );
        let invalid = f.0.join("invalid");
        assert!(export(&f.0, &invalid, 101, "cloudflare-local").is_err());
        assert!(export(&f.0, &invalid, 2, "../unrelated").is_err());
        assert!(!invalid.exists());
    }

    #[test]
    #[cfg(unix)]
    fn symlinks_cannot_export_unrelated_files_or_directories() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        fs::create_dir(f.0.join("-1-cloudflare-local")).unwrap();
        fs::write(f.0.join("unrelated"), b"must not be copied").unwrap();
        symlink(
            f.0.join("unrelated"),
            f.0.join("-1-cloudflare-local/host.log"),
        )
        .unwrap();
        symlink(f.0.join("-1-cloudflare-local"), f.0.join("-1-git")).unwrap();
        let m = f.export(2);
        for entry in m["files"].as_array().unwrap() {
            assert!(entry["missing_reason"].is_string());
            assert_eq!(entry["original_sha256"], serde_json::Value::Null);
        }
        assert_eq!(m["retained_log_bytes"], 0);
        assert_eq!(fs::read_dir(f.0.join("retained")).unwrap().count(), 1);
    }
}
