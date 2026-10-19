//! Merge previously validated Git packs without inflating their objects.
//!
//! The native receive path runs Git's strict object checks before publication.
//! Stored inputs are subsequently authenticated by the blob store. This module
//! checks their framing, index, checksums, and delta graph again; it deliberately
//! does not replace validation of untrusted object contents.
use git_zero_core::pack::crc32;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, hash_map};
use std::io;

const MAX_PACK_BYTES: usize = 128 * 1024 * 1024;
type Id = [u8; 20];

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(
        bytes[at..at + 4]
            .try_into()
            .expect("checked table boundary"),
    )
}

fn digest(bytes: &[u8]) -> Id {
    Sha1::digest(bytes).into()
}

#[derive(Clone, Copy)]
struct Indexed {
    id: Id,
    offset: usize,
    crc: u32,
}

struct Entry<'a> {
    id: Id,
    bytes: &'a [u8],
    header_len: usize,
    payload_at: usize,
    base: Option<Id>,
    ofs_delta: bool,
    crc: u32,
}

/// Read a v2 index into pack order, validating each table before slicing it.
fn indexed(pack: &[u8], index: &[u8]) -> io::Result<Vec<Indexed>> {
    const FIXED: usize = 8 + 256 * 4 + 40;
    if pack.len() < 32
        || pack.len() > MAX_PACK_BYTES
        || &pack[..4] != b"PACK"
        || !matches!(be32(pack, 4), 2 | 3)
        || index.len() < FIXED
        || &index[..4] != b"\xfftOc"
        || be32(index, 4) != 2
    {
        return Err(invalid("invalid pack or v2 index header"));
    }
    let count = be32(index, 8 + 255 * 4) as usize;
    if count != be32(pack, 8) as usize || count > (pack.len() - 32) / 2 {
        return Err(invalid("invalid pack object count"));
    }
    let table_end = FIXED
        .checked_add(
            count
                .checked_mul(28)
                .ok_or_else(|| invalid("index too large"))?,
        )
        .ok_or_else(|| invalid("index too large"))?;
    if index.len() < table_end || (index.len() - table_end) % 8 != 0 {
        return Err(invalid("invalid index table length"));
    }
    let pack_hash = &pack[pack.len() - 20..];
    if digest(&pack[..pack.len() - 20]) != pack_hash
        || pack_hash != &index[index.len() - 40..index.len() - 20]
        || digest(&index[..index.len() - 20]) != index[index.len() - 20..]
    {
        return Err(invalid("pack or index checksum mismatch"));
    }
    let names_at = 8 + 256 * 4;
    let crcs_at = names_at + count * 20;
    let offsets_at = crcs_at + count * 4;
    let large_at = offsets_at + count * 4;
    let large_count = (index.len() - table_end) / 8;
    let mut seen_large = vec![false; large_count];
    let mut fanout = [0_u32; 256];
    let mut previous = None;
    let mut result = Vec::with_capacity(count);
    for n in 0..count {
        let id: Id = index[names_at + n * 20..names_at + (n + 1) * 20]
            .try_into()
            .expect("checked object table");
        if previous.is_some_and(|previous| previous >= id) {
            return Err(invalid("index object IDs are not strictly sorted"));
        }
        previous = Some(id);
        fanout[usize::from(id[0])] += 1;
        let offset = be32(index, offsets_at + n * 4);
        let offset = if offset & 0x8000_0000 == 0 {
            offset as usize
        } else {
            let slot = (offset & 0x7fff_ffff) as usize;
            if slot >= large_count || seen_large[slot] {
                return Err(invalid("invalid large-offset table reference"));
            }
            seen_large[slot] = true;
            usize::try_from(u64::from_be_bytes(
                index[large_at + slot * 8..large_at + (slot + 1) * 8]
                    .try_into()
                    .expect("checked large-offset table"),
            ))
            .map_err(|_| invalid("pack offset is too large"))?
        };
        if !(12..pack.len() - 20).contains(&offset) {
            return Err(invalid("pack object offset is outside the pack"));
        }
        result.push(Indexed {
            id,
            offset,
            crc: be32(index, crcs_at + n * 4),
        });
    }
    if seen_large.iter().any(|seen| !seen) {
        return Err(invalid("unused large-offset table entry"));
    }
    let mut cumulative = 0;
    for (slot, objects) in fanout.iter().enumerate() {
        cumulative += objects;
        if be32(index, 8 + slot * 4) != cumulative {
            return Err(invalid("invalid index fanout table"));
        }
    }
    result.sort_unstable_by_key(|entry| entry.offset);
    if result.first().map_or(pack.len() - 20, |entry| entry.offset) != 12
        || result
            .windows(2)
            .any(|pair| pair[0].offset == pair[1].offset)
    {
        return Err(invalid("invalid or duplicate pack offsets"));
    }
    Ok(result)
}

fn entries<'a>(pack: &'a [u8], index: &[u8]) -> io::Result<Vec<Entry<'a>>> {
    let objects = indexed(pack, index)?;
    let mut result = Vec::with_capacity(objects.len());
    for (n, object) in objects.iter().enumerate() {
        let end = objects
            .get(n + 1)
            .map_or(pack.len() - 20, |entry| entry.offset);
        let bytes = &pack[object.offset..end];
        result.push(decode_entry(bytes, object, &objects)?);
    }
    Ok(result)
}

fn decode_entry<'a>(
    bytes: &'a [u8],
    object: &Indexed,
    objects: &[Indexed],
) -> io::Result<Entry<'a>> {
    if crc32(bytes) != object.crc {
        return Err(invalid("pack entry CRC mismatch"));
    }
    let kind = (bytes[0] >> 4) & 7;
    if !matches!(kind, 1..=4 | 6 | 7) {
        return Err(invalid("invalid pack entry kind"));
    }
    let mut cursor = 1;
    let mut previous = bytes[0];
    let mut shift = 4;
    let mut size = u64::from(previous & 15);
    while previous & 128 != 0 {
        previous = *bytes
            .get(cursor)
            .ok_or_else(|| invalid("truncated entry header"))?;
        if shift >= 64 || u64::from(previous & 127) > (u64::MAX >> shift) {
            return Err(invalid("pack entry size overflow"));
        }
        size |= u64::from(previous & 127) << shift;
        shift += 7;
        cursor += 1;
    }
    let _ = size; // Object sizes were checked by strict receive validation.
    let header_len = cursor;
    let base = match kind {
        6 => {
            previous = *bytes
                .get(cursor)
                .ok_or_else(|| invalid("truncated delta offset"))?;
            cursor += 1;
            let mut distance = usize::from(previous & 127);
            while previous & 128 != 0 {
                previous = *bytes
                    .get(cursor)
                    .ok_or_else(|| invalid("truncated delta offset"))?;
                cursor += 1;
                distance = distance
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(128))
                    .and_then(|n| n.checked_add(usize::from(previous & 127)))
                    .ok_or_else(|| invalid("delta offset overflow"))?;
            }
            if distance == 0 || distance > object.offset {
                return Err(invalid("invalid delta base offset"));
            }
            let base_offset = object.offset - distance;
            let base = objects
                .binary_search_by_key(&base_offset, |entry| entry.offset)
                .map_err(|_| invalid("delta base does not point to an entry"))?;
            Some(objects[base].id)
        }
        7 => {
            let end = cursor + 20;
            let id = bytes
                .get(cursor..end)
                .ok_or_else(|| invalid("truncated delta base ID"))?
                .try_into()
                .expect("20-byte base ID");
            cursor = end;
            Some(id)
        }
        _ => None,
    };
    if cursor >= bytes.len() {
        return Err(invalid("missing compressed object payload"));
    }
    Ok(Entry {
        id: object.id,
        bytes,
        header_len,
        payload_at: cursor,
        base,
        ofs_delta: kind == 6,
        crc: object.crc,
    })
}

fn validate_delta_graph(parents: &[Option<usize>]) -> io::Result<()> {
    // Every object has at most one dependency. Walk paths without recursive
    // stack growth, including adversarial cyclic REF-delta graphs.
    let mut colors = vec![0_u8; parents.len()];
    let mut path = Vec::new();
    for start in 0..parents.len() {
        if colors[start] != 0 {
            continue;
        }
        let mut cursor = Some(start);
        while let Some(current) = cursor {
            match colors[current] {
                1 => return Err(invalid("cyclic delta dependencies")),
                2 => break,
                _ => {
                    colors[current] = 1;
                    path.push(current);
                    cursor = parents[current];
                }
            }
        }
        for current in path.drain(..) {
            colors[current] = 2;
        }
    }
    Ok(())
}

/// Merge trusted SHA-1 Git packs by copying their compressed object streams.
/// Duplicate object IDs retain the first input's representation. OFS deltas
/// become REF deltas, so relocating or deduplicating entries preserves bases.
/// Pack records keep source order for locality; only the index is ID-sorted.
pub(crate) fn merge_packs(inputs: &[(&[u8], &[u8])]) -> io::Result<(String, Vec<u8>, Vec<u8>)> {
    let mut ids = HashMap::new();
    let mut objects = Vec::new();
    for &(pack, index) in inputs {
        for entry in entries(pack, index)? {
            if let hash_map::Entry::Vacant(slot) = ids.entry(entry.id) {
                slot.insert(objects.len());
                objects.push(entry);
            }
        }
    }
    let mut parents = Vec::with_capacity(objects.len());
    let mut output_bytes = 32_usize;
    for object in &objects {
        let parent = object
            .base
            .map(|id| {
                ids.get(&id)
                    .copied()
                    .ok_or_else(|| invalid("merged pack is missing a delta base"))
            })
            .transpose()?;
        parents.push(parent);
        let len = if object.ofs_delta {
            object.header_len + 20 + object.bytes.len() - object.payload_at
        } else {
            object.bytes.len()
        };
        output_bytes = output_bytes
            .checked_add(len)
            .ok_or_else(|| invalid("merged pack is too large"))?;
    }
    if output_bytes > MAX_PACK_BYTES || objects.len() > u32::MAX as usize {
        return Err(invalid("merged pack exceeds prototype limit"));
    }
    validate_delta_graph(&parents)?;

    let mut pack = Vec::with_capacity(output_bytes);
    pack.extend_from_slice(b"PACK\0\0\0\x02");
    pack.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    let mut offsets = Vec::with_capacity(objects.len());
    let mut crcs = Vec::with_capacity(objects.len());
    for object in &objects {
        let start = pack.len();
        offsets.push(start as u32);
        if object.ofs_delta {
            pack.push((object.bytes[0] & !0x70) | 0x70);
            pack.extend_from_slice(&object.bytes[1..object.header_len]);
            pack.extend_from_slice(&object.base.expect("OFS delta has a base"));
            pack.extend_from_slice(&object.bytes[object.payload_at..]);
            crcs.push(crc32(&pack[start..]));
        } else {
            pack.extend_from_slice(object.bytes);
            crcs.push(object.crc);
        }
    }
    let pack_hash = digest(&pack);
    pack.extend_from_slice(&pack_hash);

    let mut index = Vec::with_capacity(8 + 1024 + objects.len() * 28 + 40);
    index.extend_from_slice(b"\xfftOc\0\0\0\x02");
    let mut fanout = [0_u32; 256];
    for object in &objects {
        fanout[usize::from(object.id[0])] += 1;
    }
    let mut cumulative = 0;
    for count in fanout {
        cumulative += count;
        index.extend_from_slice(&cumulative.to_be_bytes());
    }
    let mut index_order: Vec<_> = (0..objects.len()).collect();
    index_order.sort_unstable_by_key(|&n| objects[n].id);
    for &n in &index_order {
        index.extend_from_slice(&objects[n].id);
    }
    for &n in &index_order {
        index.extend_from_slice(&crcs[n].to_be_bytes());
    }
    for &n in &index_order {
        index.extend_from_slice(&offsets[n].to_be_bytes());
    }
    index.extend_from_slice(&pack_hash);
    index.extend_from_slice(&digest(&index));
    let mut id = String::with_capacity(40);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in pack_hash {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok((id, pack, index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "git-zero-pack-merge-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let output = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output().unwrap();
            writer.join().unwrap().unwrap();
            output
        });
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn fixture(root: &Path, name: &str, ids: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
        let pack = git(
            root,
            &[
                "pack-objects",
                "--stdout",
                "--window=50",
                "--depth=20",
                "--delta-base-offset",
            ],
            &ids.concat(),
        );
        let pack_path = root.join(format!("{name}.pack"));
        let index_path = root.join(format!("{name}.idx"));
        fs::write(&pack_path, &pack).unwrap();
        git(
            root,
            &[
                "index-pack",
                "--strict",
                "--index-version=2",
                "-o",
                index_path.to_str().unwrap(),
                pack_path.to_str().unwrap(),
            ],
            &[],
        );
        (pack, fs::read(index_path).unwrap())
    }

    fn repair_index_checksum(index: &mut [u8]) {
        let end = index.len() - 20;
        let checksum = digest(&index[..end]);
        index[end..].copy_from_slice(&checksum);
    }

    fn synthetic(objects: &[(Id, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let mut pack = b"PACK\0\0\0\x02".to_vec();
        pack.extend_from_slice(&(objects.len() as u32).to_be_bytes());
        let mut sorted = BTreeMap::new();
        for (id, bytes) in objects {
            sorted.insert(*id, (pack.len() as u32, crc32(bytes)));
            pack.extend_from_slice(bytes);
        }
        let checksum = digest(&pack);
        pack.extend_from_slice(&checksum);
        let mut index = b"\xfftOc\0\0\0\x02".to_vec();
        for byte in 0..256 {
            index.extend_from_slice(
                &(sorted
                    .keys()
                    .filter(|id| usize::from(id[0]) <= byte)
                    .count() as u32)
                    .to_be_bytes(),
            );
        }
        for id in sorted.keys() {
            index.extend_from_slice(id);
        }
        for (_, crc) in sorted.values() {
            index.extend_from_slice(&crc.to_be_bytes());
        }
        for (offset, _) in sorted.values() {
            index.extend_from_slice(&offset.to_be_bytes());
        }
        index.extend_from_slice(&checksum);
        index.extend_from_slice(&digest(&index));
        (pack, index)
    }

    #[test]
    fn compressed_merge_deduplicates_and_git_strictly_validates_it() {
        let temp = Temp::new();
        git(
            &temp.0,
            &["init", "--bare", "--quiet", "--object-format=sha1"],
            &[],
        );
        let mut base = vec![0; 64 * 1024];
        let mut state = 37_u32;
        for byte in &mut base {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        let mut ids = Vec::new();
        for n in 0..12 {
            let mut blob = base.clone();
            blob[n * 1024..n * 1024 + 100].fill(n as u8);
            ids.push(git(&temp.0, &["hash-object", "-w", "--stdin"], &blob));
        }
        let one = fixture(&temp.0, "one", &ids[..8]);
        let two = fixture(&temp.0, "two", &ids[5..]);
        assert!(
            entries(&one.0, &one.1)
                .unwrap()
                .iter()
                .any(|entry| entry.ofs_delta),
            "Git fixture must exercise OFS deltas"
        );
        let mut expected_order = Vec::new();
        let mut expected_payloads = HashMap::new();
        for source in [&one, &two] {
            for entry in entries(&source.0, &source.1).unwrap() {
                if let hash_map::Entry::Vacant(slot) = expected_payloads.entry(entry.id) {
                    expected_order.push(entry.id);
                    slot.insert(&entry.bytes[entry.payload_at..]);
                }
            }
        }
        let (id, pack, index) = merge_packs(&[(&one.0, &one.1), (&two.0, &two.1)]).unwrap();
        assert_eq!(be32(&pack, 8), 12);
        assert_eq!(id, format!("{:x}", Sha1::digest(&pack[..pack.len() - 20])));
        let merged_entries = entries(&pack, &index).unwrap();
        assert_eq!(
            merged_entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            expected_order,
            "retain source physical order after removing duplicate IDs"
        );
        for entry in &merged_entries {
            assert!(!entry.ofs_delta);
            assert_eq!(
                &entry.bytes[entry.payload_at..],
                expected_payloads[&entry.id],
                "copy the first representation's compressed payload unchanged"
            );
        }
        let pack_path = temp.0.join("merged.pack");
        let index_path = temp.0.join("merged.idx");
        fs::write(&pack_path, &pack).unwrap();
        fs::write(&index_path, &index).unwrap();
        git(&temp.0, &["verify-pack", index_path.to_str().unwrap()], &[]);
        // Generate a fresh index independently while resolving and validating
        // every object; compare it with the index emitted by the Rust merger.
        let checked_path = temp.0.join("checked.idx");
        git(
            &temp.0,
            &[
                "index-pack",
                "--strict",
                "--index-version=2",
                "-o",
                checked_path.to_str().unwrap(),
                pack_path.to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(index, fs::read(checked_path).unwrap());
        let again = merge_packs(&[(&pack, &index), (&one.0, &one.1)]).unwrap();
        assert_eq!(pack, again.1);
        assert_eq!(index, again.2);

        let mut corrupt = one.0.clone();
        corrupt[13] ^= 1;
        assert!(merge_packs(&[(&corrupt, &one.1)]).is_err());
        let mut corrupt = one.1.clone();
        corrupt[8] ^= 1;
        repair_index_checksum(&mut corrupt);
        assert!(merge_packs(&[(&one.0, &corrupt)]).is_err());
        let mut corrupt = one.1.clone();
        let crc_at = 8 + 1024 + 8 * 20;
        corrupt[crc_at] ^= 1;
        repair_index_checksum(&mut corrupt);
        assert!(merge_packs(&[(&one.0, &corrupt)]).is_err());
        let mut corrupt = one.1.clone();
        let offsets_at = 8 + 1024 + 8 * 24;
        corrupt[offsets_at..offsets_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        repair_index_checksum(&mut corrupt);
        assert!(merge_packs(&[(&one.0, &corrupt)]).is_err());
    }

    #[test]
    fn empty_pack_and_truncated_inputs_are_checked() {
        let (id, pack, index) = merge_packs(&[]).unwrap();
        assert_eq!(pack.len(), 32);
        assert_eq!(index.len(), 8 + 1024 + 40);
        assert_eq!(id.len(), 40);
        assert_eq!(merge_packs(&[(&pack, &index)]).unwrap().1, pack);
        for length in 0..pack.len() {
            assert!(merge_packs(&[(&pack[..length], &index)]).is_err());
        }
        for length in 0..index.len() {
            assert!(merge_packs(&[(&pack, &index[..length])]).is_err());
        }
    }

    #[test]
    fn delta_graph_requires_existing_bases_and_no_cycles() {
        let one = [1; 20];
        let two = [2; 20];
        let ref_delta = |base: Id| {
            let mut bytes = vec![0x70];
            bytes.extend_from_slice(&base);
            bytes.extend_from_slice(b"\x78\x9c\x03\0\0\0\0\x01");
            bytes
        };
        let missing = synthetic(&[(one, ref_delta(two))]);
        assert!(
            merge_packs(&[(&missing.0, &missing.1)])
                .unwrap_err()
                .to_string()
                .contains("missing a delta base")
        );
        let cycle = synthetic(&[(one, ref_delta(two)), (two, ref_delta(one))]);
        assert!(
            merge_packs(&[(&cycle.0, &cycle.1)])
                .unwrap_err()
                .to_string()
                .contains("cyclic")
        );
        let zero_offset = synthetic(&[(one, vec![0x60, 0, 0x78, 0x9c])]);
        assert!(
            merge_packs(&[(&zero_offset.0, &zero_offset.1)])
                .unwrap_err()
                .to_string()
                .contains("base offset")
        );

        // Duplicate IDs must use the same first representation for emission
        // and dependency validation. Later duplicates cannot mask its cycle.
        let first = synthetic(&[(one, ref_delta(two))]);
        let later = synthetic(&[
            (one, vec![0x30, 0x78, 0x9c, 3, 0, 0, 0, 0, 1]),
            (two, ref_delta(one)),
        ]);
        assert!(
            merge_packs(&[(&first.0, &first.1), (&later.0, &later.1)])
                .unwrap_err()
                .to_string()
                .contains("cyclic")
        );
        let selected_full = merge_packs(&[(&later.0, &later.1), (&first.0, &first.1)]).unwrap();
        let selected = entries(&selected_full.1, &selected_full.2).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].base, None);
        assert_eq!(selected[1].base, Some(one));
    }
}
