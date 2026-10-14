//! Bounded smart-HTTP discovery and materialized SHA-1 packs.
use crate::validate_tree;
use git_zero::{
    Error, Object, ObjectId, ObjectKind,
    pack::{self, EntryKind},
    pktline::{self, Packet},
    zlib::InflateLimits,
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PackObject {
    pub id: [u8; 20],
    pub kind: u32,
    pub pack_offset: u64,
    pub offset: usize,
    pub length: usize,
    pub depth: u32,
}

fn errno(error: Error) -> i32 {
    match error {
        Error::BufferTooSmall | Error::LimitExceeded => -27,
        _ => -74,
    }
}

fn discovery(mut bytes: &[u8], head: &mut [u8; 20]) -> Result<bool, Error> {
    match pktline::read(&mut bytes)? {
        Some(Packet::Data(b"# service=git-upload-pack\n")) => {}
        _ => return Err(Error::InvalidPacket),
    }
    if !matches!(pktline::read(&mut bytes)?, Some(Packet::Flush)) {
        return Err(Error::InvalidPacket);
    }
    let mut found = false;
    let mut shallow = false;
    let mut finished = false;
    while let Some(packet) = pktline::read(&mut bytes)? {
        match packet {
            Packet::Flush => {
                if !bytes.is_empty() {
                    return Err(Error::InvalidPacket);
                }
                finished = true;
                break;
            }
            Packet::Data(line) => {
                let mut split = line.splitn(2, |&b| b == 0);
                let reference = split.next().ok_or(Error::InvalidReference)?;
                if let Some(capabilities) = split.next() {
                    shallow = capabilities
                        .split(|b| b.is_ascii_whitespace())
                        .any(|c| c == b"shallow");
                }
                let reference = reference.strip_suffix(b"\n").unwrap_or(reference);
                if reference.len() < 42 || reference[40] != b' ' {
                    return Err(Error::InvalidReference);
                }
                let id = ObjectId::from_hex(&reference[..40])?;
                if &reference[41..] == b"HEAD" {
                    *head = id.0;
                    found = true;
                }
            }
        }
    }
    if !finished {
        return Err(Error::InvalidPacket);
    }
    if !found {
        return Err(Error::ObjectNotFound);
    }
    Ok(shallow)
}

/// Read a protocol-v0 smart advertisement, returning 1 if shallow is supported.
/// # Safety
/// Data is valid for length bytes; head is a disjoint 20-byte output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_head(data: *const u8, length: usize, head: *mut u8) -> i32 {
    // SAFETY: C owns both live, disjoint buffers.
    let (data, head) = unsafe {
        (
            core::slice::from_raw_parts(data, length),
            &mut *head.cast::<[u8; 20]>(),
        )
    };
    match discovery(data, head) {
        Ok(shallow) => i32::from(shallow),
        Err(Error::ObjectNotFound) => -2,
        Err(e) => errno(e),
    }
}

fn import(
    pack: &[u8],
    index: &mut [PackObject],
    arena: &mut [u8],
    scratch: &mut [u8],
    window: &mut [u8; 32768],
    object_limit: usize,
) -> Result<usize, Error> {
    let count = pack::Reader::new(pack)?.object_count() as usize;
    if count != index.len() {
        return Err(Error::BufferTooSmall);
    }
    index.fill(PackObject::default());
    let mut used = 0;
    let mut remaining = count;
    for _ in 0..65 {
        let mut reader = pack::Reader::new(pack)?;
        let mut progress = false;
        let mut decoded_bytes = 0_u64;
        for i in 0..count {
            decoded_bytes =
                decoded_bytes.saturating_add(reader.next_entry_size()?.ok_or(Error::CorruptPack)?);
            if decoded_bytes > (arena.len() as u64).saturating_mul(8) {
                return Err(Error::LimitExceeded);
            }
            let entry = reader
                .read_entry(
                    scratch,
                    window,
                    InflateLimits {
                        max_output: object_limit as u64,
                        max_ratio: 1024,
                        ratio_slack: 65536,
                    },
                )?
                .ok_or(Error::CorruptPack)?;
            if index[i].kind != 0 {
                continue;
            }
            let base = match entry.kind() {
                EntryKind::Object(_) => None,
                EntryKind::OffsetDelta { base_offset } => index
                    .iter()
                    .find(|o| o.kind != 0 && o.pack_offset == base_offset)
                    .copied(),
                EntryKind::ReferenceDelta { base_id } => index
                    .iter()
                    .find(|o| o.kind != 0 && o.id == base_id.0)
                    .copied(),
            };
            if !matches!(entry.kind(), EntryKind::Object(_)) && base.is_none() {
                continue;
            }
            let (previous, output) = arena.split_at_mut(used);
            let output_len = output.len().min(object_limit);
            let (object, depth) = if let Some(base) = base {
                if base.depth >= 64 {
                    return Err(Error::LimitExceeded);
                }
                let kind = match base.kind {
                    1 => ObjectKind::Commit,
                    2 => ObjectKind::Tree,
                    3 => ObjectKind::Blob,
                    4 => ObjectKind::Tag,
                    _ => return Err(Error::CorruptPack),
                };
                let body = previous
                    .get(base.offset..base.offset + base.length)
                    .ok_or(Error::CorruptPack)?;
                (
                    pack::apply_delta(
                        Object { kind, data: body },
                        entry.data(),
                        &mut output[..output_len],
                        pack::DeltaLimits {
                            max_output: object_limit as u64,
                            max_instructions: object_limit as u64,
                            max_depth: 64,
                        },
                    )?,
                    base.depth + 1,
                )
            } else {
                let EntryKind::Object(kind) = entry.kind() else {
                    return Err(Error::CorruptPack);
                };
                let output = output
                    .get_mut(..entry.data().len())
                    .ok_or(Error::BufferTooSmall)?;
                output.copy_from_slice(entry.data());
                (Object { kind, data: output }, 0)
            };
            if object.kind == ObjectKind::Tree && validate_tree(object.data).is_err() {
                return Err(Error::InvalidObject);
            }
            index[i] = PackObject {
                id: object.id().0,
                kind: object.kind as u32,
                pack_offset: entry.offset(),
                offset: used,
                length: object.data.len(),
                depth,
            };
            used += object.data.len();
            remaining -= 1;
            progress = true;
        }
        reader.finish()?;
        if remaining == 0 {
            index.sort_unstable_by_key(|o| o.id);
            return Ok(used);
        }
        if !progress {
            return Err(Error::CorruptPack);
        }
    }
    Err(Error::LimitExceeded)
}

/// Materialize a checksummed pack, including bounded offset/reference deltas.
/// # Safety
/// All ranges are live and disjoint, including index_count initialized entries,
/// a 32768-byte window, scratch_length bytes and an aligned used output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_pack_import(
    pack: *const u8,
    pack_length: usize,
    index: *mut PackObject,
    index_count: usize,
    arena: *mut u8,
    arena_length: usize,
    scratch: *mut u8,
    scratch_length: usize,
    window: *mut u8,
    object_limit: usize,
    used: *mut usize,
) -> i32 {
    // SAFETY: C allocates every range and keeps it live through this synchronous call.
    let result = unsafe {
        import(
            core::slice::from_raw_parts(pack, pack_length),
            core::slice::from_raw_parts_mut(index, index_count),
            core::slice::from_raw_parts_mut(arena, arena_length),
            core::slice::from_raw_parts_mut(scratch, scratch_length),
            &mut *window.cast::<[u8; 32768]>(),
            object_limit,
        )
    };
    match result {
        Ok(length) => {
            unsafe {
                *used = length;
            }
            0
        }
        Err(e) => errno(e),
    }
}

/// Find an object in an imported, sorted snapshot index.
/// # Safety
/// Index is a validated immutable index; id has 20 bytes; result is disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_pack_find(
    index: *const PackObject,
    count: usize,
    id: *const u8,
    result: *mut crate::Decoded,
) -> i32 {
    // SAFETY: C retains the validated index and supplies live outputs.
    let (index, id) = unsafe {
        (
            core::slice::from_raw_parts(index, count),
            &*id.cast::<[u8; 20]>(),
        )
    };
    let Ok(position) = index.binary_search_by_key(id, |o| o.id) else {
        return -2;
    };
    let object = index[position];
    // SAFETY: result is an exclusive output struct.
    unsafe {
        *result = crate::Decoded {
            offset: object.offset,
            length: object.length,
            kind: object.kind,
        };
    }
    0
}

/// Strip shallow negotiation from a non-sideband upload-pack response.
/// # Safety
/// Data is a complete HTTP body; result is a disjoint output struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_pack(
    data: *const u8,
    length: usize,
    result: *mut crate::Decoded,
) -> i32 {
    // SAFETY: The caller supplies a live complete body.
    let mut bytes = unsafe { core::slice::from_raw_parts(data, length) };
    let parsed = (|| {
        loop {
            match pktline::read(&mut bytes)? {
                Some(Packet::Data(b"NAK\n")) => break,
                Some(Packet::Data(line)) if line.starts_with(b"shallow ") => {
                    ObjectId::from_hex(
                        line.strip_suffix(b"\n")
                            .unwrap_or(line)
                            .get(8..)
                            .ok_or(Error::InvalidPacket)?,
                    )?;
                }
                Some(Packet::Flush) => {}
                _ => return Err(Error::InvalidPacket),
            }
        }
        pack::validate(bytes)?;
        Ok(())
    })();
    match parsed {
        Ok(()) => {
            unsafe {
                *result = crate::Decoded {
                    offset: length - bytes.len(),
                    length: bytes.len(),
                    kind: 0,
                };
            }
            0
        }
        Err(e) => errno(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_zero::{io::SliceWriter, zlib};
    use sha1::{Digest, Sha1};

    fn append_zlib(bytes: &mut Vec<u8>, body: &[u8]) {
        let mut storage = [0; 256];
        let mut writer = SliceWriter::new(&mut storage);
        zlib::write_parts(&mut writer, &[body]).unwrap();
        bytes.extend_from_slice(writer.as_bytes());
    }

    fn delta_pack(forward: bool) -> Vec<u8> {
        let base = Object {
            kind: ObjectKind::Blob,
            data: b"hello",
        };
        let mut bytes = b"PACK\0\0\0\x02\0\0\0\x02".to_vec();
        let delta = b"\x05\x05\x05world";
        if forward {
            bytes.push(0x78);
            bytes.extend_from_slice(&base.id().0);
            append_zlib(&mut bytes, delta);
            bytes.push(0x35);
            append_zlib(&mut bytes, base.data);
        } else {
            bytes.push(0x35);
            append_zlib(&mut bytes, base.data);
            let distance = bytes.len() - 12;
            assert!(distance < 128);
            bytes.extend_from_slice(&[0x68, distance as u8]);
            append_zlib(&mut bytes, delta);
        }
        let checksum = Sha1::digest(&bytes);
        bytes.extend_from_slice(&checksum);
        bytes
    }

    #[test]
    fn forward_reference_and_offset_deltas_materialize_identically() {
        for forward in [true, false] {
            let pack = delta_pack(forward);
            let mut index = [PackObject::default(); 2];
            let mut arena = [0; 128];
            let mut scratch = [0; 128];
            let mut window = [0; 32768];
            assert_eq!(
                import(
                    &pack,
                    &mut index,
                    &mut arena,
                    &mut scratch,
                    &mut window,
                    128
                ),
                Ok(10)
            );
            let object = Object {
                kind: ObjectKind::Blob,
                data: b"world",
            };
            let entry = index
                .iter()
                .find(|entry| entry.id == object.id().0)
                .unwrap();
            assert_eq!(entry.depth, 1);
            assert_eq!(
                &arena[entry.offset..entry.offset + entry.length],
                object.data
            );
            assert!(index[0].id < index[1].id);
        }
    }

    #[test]
    fn pack_corruption_and_memory_limits_fail_closed() {
        let mut pack = delta_pack(false);
        let mut index = [PackObject::default(); 2];
        let mut arena = [0; 2];
        let mut scratch = [0; 128];
        let mut window = [0; 32768];
        assert_eq!(
            import(
                &pack,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                128
            ),
            Err(Error::BufferTooSmall)
        );
        pack[12] ^= 1;
        assert_eq!(
            import(
                &pack,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                128
            ),
            Err(Error::CorruptPack)
        );
    }

    #[test]
    fn discovery_requires_head_and_final_flush() {
        let id = ObjectId([7; 20]);
        let mut storage = [0; 512];
        let mut writer = SliceWriter::new(&mut storage);
        pktline::write(&mut writer, b"# service=git-upload-pack\n").unwrap();
        use git_zero::io::Write;
        writer.write_all(b"0000").unwrap();
        let reference = format!("{id} HEAD\0shallow ofs-delta\n");
        pktline::write(&mut writer, reference.as_bytes()).unwrap();
        let mut head = [0; 20];
        assert!(discovery(writer.as_bytes(), &mut head).is_err());
        writer.write_all(b"0000").unwrap();
        assert_eq!(discovery(writer.as_bytes(), &mut head), Ok(true));
        assert_eq!(head, id.0);
    }
}
