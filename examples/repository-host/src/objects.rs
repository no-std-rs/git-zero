//! Observer-only Git inventory. These scans are excluded from request timings.
use crate::native::command;
use crate::storage::Manifest;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
};

const MAX_OBJECTS: usize = 250_000;
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[derive(Default)]
struct Entry {
    kind: String,
    logical: u64,
    compressed: u64,
    representations: u64,
}

#[derive(Default, Serialize)]
struct TypeSummary {
    unique_objects: u64,
    logical_bytes: u64,
    reachable_objects: u64,
    encoded_representations: u64,
}

#[derive(Serialize)]
struct ObjectSummary<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    logical_bytes: u64,
    minimum_encoded_bytes: u64,
    representations: u64,
    reachable: bool,
}

pub fn inventory(repo: &Path, manifest: &Manifest) -> io::Result<Value> {
    let mut objects = BTreeMap::<String, Entry>::new();
    let (mut entries, mut deltas, mut encoded_bytes, mut max_depth) = (0u64, 0u64, 0u64, 0u64);
    let mut delta_bases = BTreeSet::new();
    for pack in &manifest.packs {
        let index = repo.join(format!("objects/pack/pack-{}.idx", pack.id));
        let mut process = Command::new("git");
        process
            .args(["verify-pack", "-v"])
            .arg(index)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = ChildGuard(process.spawn()?);
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing inventory output"))?;
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            if line.len() > 4096 {
                return Err(io::Error::other("object inventory row too large"));
            }
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 5
                || fields[0].len() != 40
                || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
            {
                continue;
            }
            let number = |index: usize| -> io::Result<u64> {
                fields[index].parse().map_err(io::Error::other)
            };
            let _stored_entry_size = number(2)?;
            let compressed = number(3)?;
            entries += 1;
            encoded_bytes += compressed;
            if fields.len() == 7 {
                deltas += 1;
                max_depth = max_depth.max(number(5)?);
                delta_bases.insert(fields[6].to_owned());
            }
            let entry = objects.entry(fields[0].into()).or_insert_with(|| Entry {
                kind: fields[1].into(),
                logical: 0,
                compressed,
                representations: 0,
            });
            if entry.kind != fields[1] {
                return Err(io::Error::other("inconsistent duplicate object"));
            }
            entry.representations += 1;
            entry.compressed = entry.compressed.min(compressed);
            if objects.len() > MAX_OBJECTS {
                return Err(io::Error::other(
                    "observer object inventory exceeds250000 objects",
                ));
            }
        }
        if !child.0.wait()?.success() {
            return Err(io::Error::other("Git object inventory verification failed"));
        }
    }
    // verify-pack's size for a delta is its instruction stream size. Resolve
    // canonical object sizes independently so delta-heavy histories are honest.
    if !objects.is_empty() {
        let ids = objects
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut process = Command::new("git");
        process.arg("-C").arg(repo).args([
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ]);
        let checked = command(process, ids.as_bytes())?.stdout;
        for line in std::str::from_utf8(&checked)
            .map_err(io::Error::other)?
            .lines()
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() != 3 {
                return Err(io::Error::other("invalid canonical object inventory"));
            }
            let entry = objects
                .get_mut(fields[0])
                .ok_or_else(|| io::Error::other("unknown inventory object"))?;
            if entry.kind != fields[1] {
                return Err(io::Error::other("inconsistent inventory type"));
            }
            entry.logical = fields[2].parse().map_err(io::Error::other)?;
        }
    }
    let mut process = Command::new("git");
    process
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--objects", "--all", "--no-object-names"]);
    let reachable = command(process, &[])?.stdout;
    let reachable: BTreeSet<_> = std::str::from_utf8(&reachable)
        .map_err(io::Error::other)?
        .lines()
        .collect();
    let mut by_type = BTreeMap::<&str, TypeSummary>::new();
    let mut logical_bytes = 0u64;
    let mut minimum_encoded_bytes = 0u64;
    for (id, entry) in &objects {
        logical_bytes += entry.logical;
        minimum_encoded_bytes += entry.compressed;
        let row = by_type.entry(&entry.kind).or_default();
        row.unique_objects += 1;
        row.logical_bytes += entry.logical;
        row.reachable_objects += u64::from(reachable.contains(id.as_str()));
        row.encoded_representations += entry.representations;
    }
    let mut largest: Vec<_> = objects.iter().collect();
    largest.sort_unstable_by(|a, b| b.1.logical.cmp(&a.1.logical).then_with(|| a.0.cmp(b.0)));
    let largest: Vec<_> = largest
        .into_iter()
        .take(10)
        .map(|(id, entry)| ObjectSummary {
            id,
            kind: &entry.kind,
            logical_bytes: entry.logical,
            minimum_encoded_bytes: entry.compressed,
            representations: entry.representations,
            reachable: reachable.contains(id.as_str()),
        })
        .collect();
    Ok(json!({
        "scope":"observer-only native Git scan; inventory, not per-object access frequency",
        "unique_objects":objects.len(),"reachable_objects":reachable.len(),
        "unreachable_objects":objects.keys().filter(|id|!reachable.contains(id.as_str())).count(),
        "logical_object_bytes":logical_bytes,"encoded_representations":entries,
        "duplicate_representations":entries.saturating_sub(objects.len() as u64),
        "encoded_entry_bytes":encoded_bytes,"minimum_encoded_entry_bytes":minimum_encoded_bytes,
        "delta_representations":deltas,"distinct_delta_bases":delta_bases.len(),"maximum_delta_depth":max_depth,
        "compressed_pack_bytes":manifest.packs.iter().map(|p|p.pack_bytes).sum::<u64>(),
        "pack_index_bytes":manifest.packs.iter().map(|p|p.index_bytes).sum::<u64>(),
        "pack_count":manifest.packs.len(),"ref_count":manifest.refs.len(),
        "by_type":by_type,"largest_objects":largest,"observer_limit_objects":MAX_OBJECTS
    }))
}
