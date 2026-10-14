//! Small C ABI for the Linux example; Linux owns every buffer and all I/O.
#![cfg_attr(feature = "kernel", no_std)]

use git_zero::{Error, ObjectId, client, zlib::InflateLimits};

const EINVAL: i32 = -22;
const EFBIG: i32 = -27;
const EBADMSG: i32 = -74;

#[repr(C)]
pub struct Decoded {
    pub offset: usize,
    pub length: usize,
    pub kind: u32,
}

#[repr(C)]
pub struct TreeEntry {
    pub name: *const u8,
    pub name_length: usize,
    pub mode: u32,
    pub id: [u8; 20],
}

/// Decode and hash-check one complete loose object before exposing its body.
///
/// # Safety
/// All pointers must be nonnull, aligned, valid for their specified lengths,
/// and exclusive for this call. `id` has 20 bytes, `window` has 32768 bytes;
/// input, output, window and result must not overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_decode_loose(
    wire: *const u8,
    wire_length: usize,
    id: *const u8,
    output: *mut u8,
    capacity: usize,
    window: *mut u8,
    result: *mut Decoded,
) -> i32 {
    // SAFETY: The module supplies disjoint allocations of these exact sizes.
    let (wire, id, output, window) = unsafe {
        (
            core::slice::from_raw_parts(wire, wire_length),
            ObjectId(*id.cast::<[u8; 20]>()),
            core::slice::from_raw_parts_mut(output, capacity),
            &mut *window.cast::<[u8; 32768]>(),
        )
    };
    let limits = InflateLimits {
        max_output: capacity as u64,
        max_ratio: 1024,
        ratio_slack: 65536,
    };
    let output_address = output.as_ptr() as usize;
    match client::decode_loose(wire, id, output, window, limits) {
        Ok(object) => {
            if object.kind == git_zero::ObjectKind::Tree && validate_tree(object.data).is_err() {
                return EBADMSG;
            }
            // SAFETY: `result` is a valid exclusive output struct.
            unsafe {
                *result = Decoded {
                    offset: object.data.as_ptr() as usize - output_address,
                    length: object.data.len(),
                    kind: object.kind as u32,
                };
            }
            0
        }
        Err(Error::BufferTooSmall | Error::LimitExceeded) => EFBIG,
        Err(_) => EBADMSG,
    }
}

/// Extract the root tree from a commit (tag peeling is deliberately unsupported).
///
/// # Safety
/// `data` must be valid for `length` bytes; `id` is a disjoint 20-byte output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_commit_tree(data: *const u8, length: usize, id: *mut u8) -> i32 {
    // SAFETY: The module supplies a verified commit and a valid output array.
    let data = unsafe { core::slice::from_raw_parts(data, length) };
    match commit_tree(data) {
        Ok(tree) => {
            // SAFETY: `id` points to 20 writable bytes and is disjoint from data.
            unsafe { core::ptr::copy_nonoverlapping(tree.0.as_ptr(), id, 20) };
            0
        }
        Err(()) => EBADMSG,
    }
}

fn commit_tree(data: &[u8]) -> Result<ObjectId, ()> {
    let line = data.get(..46).ok_or(())?;
    if !line.starts_with(b"tree ") || line[45] != b'\n' {
        return Err(());
    }
    ObjectId::from_hex(&line[5..45]).map_err(|_| ())
}

struct Entry<'a> {
    name: &'a [u8],
    mode: u32,
    id: &'a [u8; 20],
}

fn tree_entry<'a>(data: &'a [u8], offset: &mut usize) -> Result<Option<Entry<'a>>, ()> {
    let tail = data.get(*offset..).ok_or(())?;
    if tail.is_empty() {
        return Ok(None);
    }
    let space = tail.iter().position(|&b| b == b' ').ok_or(())?;
    let mode = match &tail[..space] {
        b"40000" => 0o040000,
        b"100644" => 0o100644,
        b"100755" => 0o100755,
        b"120000" => 0o120000,
        b"160000" => 0o160000,
        _ => return Err(()),
    };
    let rest = &tail[space + 1..];
    let nul = rest.iter().position(|&b| b == 0).ok_or(())?;
    let name = &rest[..nul];
    if name.is_empty() || name.len() > 255 || name == b"." || name == b".." || name.contains(&b'/')
    {
        return Err(());
    }
    let id = rest
        .get(nul + 1..nul + 21)
        .ok_or(())?
        .try_into()
        .map_err(|_| ())?;
    *offset += space + 1 + nul + 21;
    Ok(Some(Entry { name, mode, id }))
}

fn validate_tree(data: &[u8]) -> Result<(), ()> {
    let mut offset = 0;
    let mut previous: Option<Entry<'_>> = None;
    while let Some(entry) = tree_entry(data, &mut offset)? {
        if let Some(previous) = &previous {
            let left = previous
                .name
                .iter()
                .copied()
                .chain((previous.mode == 0o040000).then_some(b'/'));
            let right = entry
                .name
                .iter()
                .copied()
                .chain((entry.mode == 0o040000).then_some(b'/'));
            if left.cmp(right) != core::cmp::Ordering::Less || previous.name == entry.name {
                return Err(());
            }
        }
        previous = Some(entry);
    }
    Ok(())
}

/// Iterate a verified tree. Names are byte strings borrowed from `data`.
/// Returns 1 for an entry, 0 for EOF, or a negative errno.
///
/// # Safety
/// `data` must be valid for `length`, and `offset` and `result` must be valid,
/// exclusive, aligned output pointers disjoint from data and each other.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_tree_next(
    data: *const u8,
    length: usize,
    offset: *mut usize,
    result: *mut TreeEntry,
) -> i32 {
    // SAFETY: These pointers refer to the module's live cache and local outputs.
    let (data, offset) = unsafe { (core::slice::from_raw_parts(data, length), &mut *offset) };
    match tree_entry(data, offset) {
        Ok(Some(entry)) => {
            // SAFETY: `result` is a valid exclusive struct output pointer.
            unsafe {
                *result = TreeEntry {
                    name: entry.name.as_ptr(),
                    name_length: entry.name.len(),
                    mode: entry.mode,
                    id: *entry.id,
                };
            }
            1
        }
        Ok(None) => 0,
        Err(()) => EINVAL,
    }
}

#[cfg(feature = "kernel")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe extern "C" {
        fn gitfs_codec_panic() -> !;
    }
    // SAFETY: The linked C layer terminates through Linux's BUG handler.
    unsafe { gitfs_codec_panic() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_zero::{Object, ObjectKind, io::SliceWriter, zlib};

    fn encoded_tree(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (mode, name) in entries {
            bytes.extend_from_slice(mode);
            bytes.push(b' ');
            bytes.extend_from_slice(name);
            bytes.push(0);
            bytes.extend_from_slice(&[7; 20]);
        }
        bytes
    }

    #[test]
    fn git_order_modes_and_non_utf8_names() {
        let tree = encoded_tree(&[
            (b"100644", b"a.txt"),
            (b"40000", b"a"),
            (b"100755", b"bin"),
            (b"120000", b"link"),
            (b"160000", b"submodule"),
            (b"100644", b"\xff"),
        ]);
        validate_tree(&tree).unwrap();
        let mut offset = 0;
        let mut count = 0;
        while tree_entry(&tree, &mut offset).unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 6);
        assert_eq!(offset, tree.len());
    }

    #[test]
    fn malformed_trees_never_expose_entries() {
        for entries in [
            vec![(b"100644".as_slice(), b"..".as_slice())],
            vec![(b"100644".as_slice(), b"a/b".as_slice())],
            vec![(b"100644".as_slice(), b"".as_slice())],
            vec![(b"0100644".as_slice(), b"a".as_slice())],
            vec![(b"100644".as_slice(), b"z".as_slice()), (b"40000", b"a")],
            vec![(b"100644".as_slice(), b"a".as_slice()), (b"100755", b"a")],
        ] {
            assert!(validate_tree(&encoded_tree(&entries)).is_err());
        }
        let valid = encoded_tree(&[(b"100644", b"a")]);
        for end in 1..valid.len() {
            assert!(validate_tree(&valid[..end]).is_err());
        }
        assert!(tree_entry(&valid, &mut (valid.len() + 1)).is_err());
    }

    #[test]
    fn commit_requires_exact_root_line() {
        let id = ObjectId([9; 20]);
        let commit = format!("tree {id}\nauthor test\n\nmessage\n");
        assert_eq!(commit_tree(commit.as_bytes()), Ok(id));
        for bad in [
            b"tree 123\n".as_slice(),
            b"tree zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n",
            b"blob x",
        ] {
            assert!(commit_tree(bad).is_err());
        }
    }

    #[test]
    fn ffi_validates_hash_framing_and_budget() {
        let object = Object {
            kind: ObjectKind::Blob,
            data: b"hello\n",
        };
        let mut header = [0; 32];
        let header = object.header(&mut header).unwrap();
        let mut wire = [0; 128];
        let mut writer = SliceWriter::new(&mut wire);
        zlib::write_parts(&mut writer, &[header, object.data]).unwrap();
        let wire = writer.finish();
        let mut output = [0; 128];
        let mut window = [0; 32768];
        let mut result = Decoded {
            offset: 0,
            length: 0,
            kind: 0,
        };
        let mut run = |wire: &[u8], id: ObjectId, capacity| {
            // SAFETY: All buffers are live, disjoint and have the documented sizes.
            unsafe {
                gitfs_decode_loose(
                    wire.as_ptr(),
                    wire.len(),
                    id.0.as_ptr(),
                    output.as_mut_ptr(),
                    capacity,
                    window.as_mut_ptr(),
                    &mut result,
                )
            }
        };
        assert_eq!(run(wire, object.id(), 128), 0);
        assert_eq!(run(wire, ObjectId([0; 20]), 128), EBADMSG);
        assert_eq!(run(wire, object.id(), 4), EFBIG);
        assert_eq!(run(&wire[..wire.len() - 1], object.id(), 128), EBADMSG);
        assert_eq!(result.kind, ObjectKind::Blob as u32);
        assert_eq!(
            &output[result.offset..result.offset + result.length],
            object.data
        );
    }
}
