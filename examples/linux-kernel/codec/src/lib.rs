//! Small C ABI for the Linux example; Linux owns every buffer and all I/O.
#![cfg_attr(feature = "kernel", no_std)]

use git_zero::{
    Error, ObjectId, client,
    object::{Commit, Tag, Tree},
    zlib::InflateLimits,
};

mod smart;
mod tls;

const EINVAL: i32 = -22;
const EFBIG: i32 = -27;
const EBADMSG: i32 = -74;

fn errno(error: Error) -> i32 {
    match error {
        Error::BufferTooSmall | Error::LimitExceeded => EFBIG,
        Error::ObjectNotFound => -2,
        Error::InvalidRequest => EINVAL,
        Error::Unsupported => -95,
        _ => EBADMSG,
    }
}

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

/// Select a default-zlib wire budget including the canonical object header.
#[unsafe(no_mangle)]
pub extern "C" fn gitfs_loose_wire_size(body_limit: usize) -> usize {
    client::loose_wire_capacity(body_limit).unwrap_or(0)
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
        max_ratio: 0,
        ratio_slack: capacity as u64,
    };
    let output_address = output.as_ptr() as usize;
    match client::decode_loose(wire, id, output, window, limits) {
        Ok(object) => {
            if object.kind == git_zero::ObjectKind::Tree {
                if let Err(error) = validate_tree(object.data) {
                    return errno(error);
                }
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
    match Commit::root_tree(data) {
        Ok(tree) => {
            // SAFETY: `id` points to 20 writable bytes and is disjoint from data.
            unsafe { core::ptr::copy_nonoverlapping(tree.0.as_ptr(), id, 20) };
            0
        }
        Err(_) => EBADMSG,
    }
}

/// Locate the exact commit message, preserving arbitrary bytes and newlines.
/// # Safety
/// Data is live for length bytes and result is a disjoint, aligned output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_commit_message(
    data: *const u8,
    length: usize,
    result: *mut Decoded,
) -> i32 {
    // SAFETY: C supplies a live object and exclusive output.
    let data = unsafe { core::slice::from_raw_parts(data, length) };
    let Ok(commit) = Commit::parse(data) else {
        return EBADMSG;
    };
    // SAFETY: The caller owns the output; offset is inside the commit body.
    unsafe {
        *result = Decoded {
            offset: commit.message_offset(),
            length: commit.message().len(),
            kind: git_zero::ObjectKind::Commit as u32,
        };
    }
    0
}

fn validate_tree(data: &[u8]) -> Result<(), Error> {
    Tree::parse(data)?;
    validate_tree_names(data)
}

// Pack materialization has already checked ordering and aliases. The adapter
// only adds Linux's component limit, without repeating alias validation.
fn validate_tree_names(data: &[u8]) -> Result<(), Error> {
    let mut offset = 0;
    while let Some(entry) = git_zero::object::TreeEntry::read(data, &mut offset)? {
        if entry.name.len() > 255 {
            return Err(Error::InvalidFileName);
        }
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
    match git_zero::object::TreeEntry::read(data, offset) {
        Ok(Some(entry)) => {
            if entry.name.len() > 255 {
                return EINVAL;
            }
            // SAFETY: `result` is a valid exclusive struct output pointer.
            unsafe {
                *result = TreeEntry {
                    name: entry.name.as_ptr(),
                    name_length: entry.name.len(),
                    mode: entry.mode,
                    id: entry.id.0,
                };
            }
            1
        }
        Ok(None) => 0,
        Err(_) => EINVAL,
    }
}

/// Read an annotated tag target using the shared object parser.
/// # Safety
/// Data is valid for length bytes; id and kind are disjoint writable outputs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_tag_target(
    data: *const u8,
    length: usize,
    id: *mut u8,
    kind: *mut u32,
) -> i32 {
    // SAFETY: C owns a live object and both exclusive output ranges.
    let data = unsafe { core::slice::from_raw_parts(data, length) };
    let Ok(tag) = Tag::parse(data) else {
        return EBADMSG;
    };
    // SAFETY: Outputs have the documented size and alignment.
    unsafe {
        core::ptr::copy_nonoverlapping(tag.target.0.as_ptr(), id, 20);
        *kind = tag.target_kind as u32;
    }
    0
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

    #[test]
    fn smart_discovery_reports_capabilities_without_requiring_head() {
        for name in ["HEAD", "refs/heads/main"] {
            let mut storage = [0; 512];
            let mut wire = SliceWriter::new(&mut storage);
            git_zero::pktline::write(&mut wire, b"# service=git-upload-pack\n").unwrap();
            git_zero::io::Write::write_all(&mut wire, b"0000").unwrap();
            let reference = format!(
                "{} {name}\0shallow ofs-delta no-progress\n",
                ObjectId([7; 20])
            );
            git_zero::pktline::write(&mut wire, reference.as_bytes()).unwrap();
            git_zero::io::Write::write_all(&mut wire, b"0000").unwrap();
            let mut head = [5; 20];
            let bytes = wire.as_bytes();
            let result = unsafe {
                smart::gitfs_smart_discover(bytes.as_ptr(), bytes.len(), head.as_mut_ptr())
            };
            assert_eq!(result, if name == "HEAD" { 15 } else { 7 });
            assert_eq!(head, if name == "HEAD" { [7; 20] } else { [5; 20] });
        }
    }

    #[test]
    fn commit_messages_preserve_exact_bytes_and_require_header_separator() {
        for message in [
            &b""[..],
            &b"subject\n\nbody\n"[..],
            &b"binary\0message\xff\n"[..],
        ] {
            let mut commit =
                b"tree 0123456789abcdef0123456789abcdef01234567\nauthor example\n\n".to_vec();
            let start = commit.len();
            commit.extend_from_slice(message);
            let mut result = Decoded {
                offset: 0,
                length: 0,
                kind: 0,
            };
            assert_eq!(
                unsafe { gitfs_commit_message(commit.as_ptr(), commit.len(), &mut result) },
                0
            );
            assert_eq!(result.offset, start);
            assert_eq!(result.length, message.len());
            assert_eq!(
                &commit[result.offset..result.offset + result.length],
                message
            );
        }
        let malformed = b"tree 0123456789abcdef0123456789abcdef01234567\nno separator";
        let mut result = Decoded {
            offset: 0,
            length: 0,
            kind: 0,
        };
        assert_eq!(
            unsafe { gitfs_commit_message(malformed.as_ptr(), malformed.len(), &mut result) },
            EBADMSG
        );
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
