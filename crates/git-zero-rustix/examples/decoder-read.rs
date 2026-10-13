//! Local object-read benchmark host. Allocations/I/O are part of the measurement;
//! the core decoder uses caller buffers. Emit Git cat-file --batch wire output.
use git_zero::{
    Object, ObjectId, client,
    pack::{DeltaLimits, EntryKind, Reader, ResolvedObject},
    zlib::{InflateLimits, WINDOW_SIZE},
};
use std::{io::Write, path::Path};

const MAX_OBJECTS: usize = 8192;
const MAX_RAW_BYTES: usize = 64 * 1024 * 1024;
const MAX_STAGING_BYTES: usize = 128 * 1024 * 1024;

fn core_error(error: git_zero::Error) -> std::io::Error {
    std::io::Error::other(format!("{error:?}"))
}

fn emit(output: &mut impl Write, id: ObjectId, object: Object<'_>) -> std::io::Result<()> {
    writeln!(output, "{id} {} {}", object.kind.name(), object.data.len())?;
    output.write_all(object.data)?;
    output.write_all(b"\n")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || !matches!(args[1].as_str(), "loose" | "pack") {
        return Err("usage: decoder-read loose|pack REPOSITORY_OR_PACK OBJECTS_TSV".into());
    }
    let manifest = std::fs::read_to_string(&args[3])?;
    let mut ids = Vec::new();
    let (mut largest, mut total) = (0_usize, 0_usize);
    for line in manifest.lines().skip(1) {
        let columns: Vec<_> = line.split('\t').collect();
        if columns.len() != 4 || ids.len() >= MAX_OBJECTS {
            return Err("invalid or oversized manifest".into());
        }
        ids.push(ObjectId::from_hex(columns[0].as_bytes()).map_err(core_error)?);
        let size: usize = columns[3].parse()?;
        total = total.checked_add(size).ok_or("size overflow")?;
        largest = largest.max(size);
    }
    if ids.is_empty() || total > MAX_RAW_BYTES || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("invalid manifest order or raw-byte budget".into());
    }
    let capacity = largest
        .checked_mul(2)
        .and_then(|size| size.checked_add(64))
        .ok_or("staging size overflow")?;
    let limits = InflateLimits {
        max_output: capacity as u64,
        max_ratio: 1032,
        ratio_slack: 32768,
    };
    let mut window = [0; WINDOW_SIZE];
    let mut output = std::io::BufWriter::with_capacity(64 * 1024, std::io::stdout().lock());
    if args[1] == "loose" {
        let mut storage = vec![0; capacity];
        for id in ids {
            let hex = id.to_string();
            let path = Path::new(&args[2])
                .join("objects")
                .join(&hex[..2])
                .join(&hex[2..]);
            let compressed = std::fs::read(path)?;
            let object = client::decode_loose(&compressed, id, &mut storage, &mut window, limits)
                .map_err(core_error)?;
            emit(&mut output, id, object)?;
        }
    } else {
        if ids
            .len()
            .checked_mul(capacity)
            .and_then(|size| size.checked_mul(2))
            .ok_or("staging budget overflow")?
            > MAX_STAGING_BYTES
        {
            return Err("pack staging exceeds 128 MiB host budget".into());
        }
        let bytes = std::fs::read(&args[2])?;
        let mut reader = Reader::new(&bytes).map_err(core_error)?;
        if reader.object_count() as usize != ids.len() {
            return Err("pack count differs from manifest".into());
        }
        // Host-owned slots keep each base/body alive; they are deliberately
        // included in process RSS rather than attributed to the no_std core.
        let mut staging: Vec<_> = ids.iter().map(|_| Vec::new()).collect();
        let mut entries = Vec::new();
        for storage in &mut staging {
            let size = reader
                .next_entry_size()
                .map_err(core_error)?
                .ok_or("missing entry")?;
            if size > capacity as u64 {
                return Err("entry exceeds staging capacity".into());
            }
            storage.resize(size as usize, 0);
            entries.push(
                reader
                    .read_entry(storage, &mut window, limits)
                    .map_err(core_error)?
                    .ok_or("missing entry")?,
            );
        }
        reader.finish().map_err(core_error)?;
        let mut reconstructed: Vec<_> = entries
            .iter()
            .map(|entry| match entry.kind() {
                EntryKind::Object(_) => None,
                _ => Some(vec![0; capacity]),
            })
            .collect();
        let mut resolved: Vec<_> = entries.iter().map(|entry| entry.object()).collect();
        let mut by_id = std::collections::BTreeMap::new();
        for (index, object) in resolved.iter().enumerate() {
            if let Some(object) = object {
                by_id.insert(object.id(), index);
            }
        }
        let mut buffers: Vec<_> = reconstructed
            .iter_mut()
            .map(|buffer| buffer.as_deref_mut())
            .collect();
        let delta_limits = DeltaLimits {
            max_output: largest as u64,
            max_instructions: capacity as u64,
            max_depth: 4095,
        };
        loop {
            let mut progress = false;
            for (index, entry) in entries.iter().enumerate() {
                if resolved[index].is_some() {
                    continue;
                }
                let base = match entry.kind() {
                    EntryKind::OffsetDelta { base_offset } => entries
                        .binary_search_by_key(&base_offset, |entry| entry.offset())
                        .ok(),
                    EntryKind::ReferenceDelta { base_id } => by_id.get(&base_id).copied(),
                    EntryKind::Object(_) => return Err("unresolved plain entry".into()),
                };
                let Some(base) = base.and_then(|index| resolved[index].as_ref()) else {
                    continue;
                };
                let object = entry
                    .resolve_delta(
                        base,
                        buffers[index].take().ok_or("delta buffer already used")?,
                        delta_limits,
                    )
                    .map_err(core_error)?;
                by_id.insert(object.id(), index);
                resolved[index] = Some(object);
                progress = true;
            }
            if resolved.iter().all(Option::is_some) {
                break;
            }
            if !progress {
                return Err("missing or cyclic delta base".into());
            }
        }
        let mut objects: Vec<_> = resolved.into_iter().flatten().collect();
        objects.sort_by_key(ResolvedObject::id);
        for (id, object) in ids.into_iter().zip(objects) {
            object.verify_id(id).map_err(core_error)?;
            emit(&mut output, id, object.object())?;
        }
    }
    output.flush()?;
    Ok(())
}
