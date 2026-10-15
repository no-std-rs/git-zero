//! C ABI and Linux name limits for the shared Git client and pack importer.
use crate::{Decoded, errno, sha1_bytes, validate_tree_names};
use git_zero::{
    Error, ObjectFormat, ObjectId, ObjectKind, client, io::SliceWriter, pack, zlib::InflateLimits,
};

pub type PackObject = pack::MaterializedEntry;

/// Select a normalized ref through the shared advertisement parser.
/// # Safety
/// Inputs are live and ID is a disjoint writable 20-byte range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_resolve_ref(
    data: *const u8,
    length: usize,
    name: *const u8,
    name_length: usize,
    id: *mut u8,
) -> i32 {
    let (data, name) = unsafe {
        (
            core::slice::from_raw_parts(data, length),
            core::slice::from_raw_parts(name, name_length),
        )
    };
    let result = (|| {
        let name = core::str::from_utf8(name).map_err(|_| Error::InvalidReference)?;
        sha1_bytes(client::SmartAdvertisement::parse(data)?.resolve_reference(name)?)
    })();
    match result {
        Ok(value) => {
            unsafe { core::ptr::copy_nonoverlapping(value.as_ptr(), id, 20) };
            0
        }
        Err(error) => errno(error),
    }
}

#[repr(C)]
pub struct RefEntry {
    name: *const u8,
    name_length: usize,
    id: [u8; 20],
}

fn vfs_ref_name(name: &str) -> Result<(), Error> {
    if name
        .strip_suffix("^{}")
        .unwrap_or(name)
        .split('/')
        .any(|part| part.len() > 255)
    {
        Err(Error::InvalidReference)
    } else {
        Ok(())
    }
}

/// Validate discovery. Bits 0-2 report shallow/OFS/no-progress; bit 3 reports
/// advertised HEAD. Missing HEAD is valid and leaves the output untouched.
/// # Safety
/// Data is valid for length bytes; head is a disjoint 20-byte output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_discover(
    data: *const u8,
    length: usize,
    head: *mut u8,
) -> i32 {
    // SAFETY: C owns the complete response and exclusive output.
    let data = unsafe { core::slice::from_raw_parts(data, length) };
    let parsed = (|| {
        let advertisement = client::SmartAdvertisement::parse(data)?;
        if advertisement
            .object_format()
            .is_some_and(|format| format != ObjectFormat::Sha1)
        {
            return Err(Error::Unsupported);
        }
        for reference in advertisement.references() {
            vfs_ref_name(reference?.name)?;
        }
        let options = advertisement.fetch_options(Some(1));
        let capabilities = i32::from(options.shallow_depth.is_some())
            | (i32::from(options.ofs_delta) << 1)
            | (i32::from(options.no_progress) << 2);
        Ok((advertisement.head().ok(), capabilities))
    })();
    match parsed {
        Ok((id, capabilities)) => {
            if let Some(id) = id {
                let Ok(id) = sha1_bytes(id) else {
                    return -95;
                };
                // SAFETY: Head has 20 writable bytes disjoint from the response.
                unsafe { core::ptr::copy_nonoverlapping(id.as_ptr(), head, 20) };
                capabilities | 8
            } else {
                capabilities
            }
        }
        Err(e) => errno(e),
    }
}

/// Check an explicit want against discovery before any upload-pack request.
/// # Safety
/// Data is live for length bytes and ID is a disjoint live 20-byte input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_validate_want(
    data: *const u8,
    length: usize,
    id: *const u8,
) -> i32 {
    // SAFETY: C keeps both immutable inputs live.
    let (data, id) = unsafe {
        (
            core::slice::from_raw_parts(data, length),
            ObjectId::Sha1(*id.cast::<[u8; 20]>()),
        )
    };
    match client::SmartAdvertisement::parse(data)
        .and_then(|advertisement| advertisement.validate_want(id))
    {
        Ok(()) => 0,
        Err(e) => errno(e),
    }
}

/// Iterate validated discovery, omitting peeled tag pseudo-refs.
/// # Safety
/// Data is live for length bytes; offset and entry are aligned, disjoint outputs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_ref_next(
    data: *const u8,
    length: usize,
    offset: *mut usize,
    entry: *mut RefEntry,
) -> i32 {
    // SAFETY: C keeps every buffer live for this synchronous call.
    let (data, offset) = unsafe { (core::slice::from_raw_parts(data, length), &mut *offset) };
    loop {
        match client::read_smart_reference(data, offset) {
            Ok(Some(reference)) => {
                if reference.name.ends_with("^{}") {
                    continue;
                }
                if vfs_ref_name(reference.name).is_err() {
                    return -74;
                }
                let Ok(id) = sha1_bytes(reference.id) else {
                    return -95;
                };
                // SAFETY: The name remains borrowed from C's live response.
                unsafe {
                    *entry = RefEntry {
                        name: reference.name.as_ptr(),
                        name_length: reference.name.len(),
                        id,
                    }
                };
                return 1;
            }
            Ok(None) => return 0,
            Err(e) => return errno(e),
        }
    }
}

/// Encode deduplicated wants with the shared protocol-v0 request builder.
/// # Safety
/// IDs contains count consecutive 20-byte IDs. Output has capacity exclusive
/// writable bytes; length is an aligned output. Every range is nonnull/disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_request(
    ids: *const u8,
    count: usize,
    capabilities: u32,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    // SAFETY: C supplies consecutive 20-byte arrays aligned to one. Construct
    // format-tagged Rust IDs through the iterator; their layout is not a C ABI.
    let (ids, output) = unsafe {
        (
            core::slice::from_raw_parts(ids.cast::<[u8; 20]>(), count),
            core::slice::from_raw_parts_mut(output, capacity),
        )
    };
    let mut writer = SliceWriter::new(output);
    let options = client::FetchOptions {
        shallow_depth: (capabilities & 1 != 0).then_some(1),
        ofs_delta: capabilities & 2 != 0,
        no_progress: capabilities & 4 != 0,
    };
    match client::write_fetch_request_iter(
        ids.iter().copied().map(ObjectId::Sha1),
        options,
        &mut writer,
    ) {
        Ok(()) => {
            // SAFETY: C owns the exclusive aligned output.
            unsafe { *length = writer.len() };
            0
        }
        Err(e) => errno(e),
    }
}

/// Materialize a pack through git-zero, then apply Linux component limits.
/// # Safety
/// All ranges are live/disjoint, including index_count initialized entries,
/// arena_length bytes, scratch_length bytes, a 32768-byte window and used output.
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
    // SAFETY: Linux owns each allocation for the full duration of this call.
    let (pack, index, arena, scratch, window) = unsafe {
        (
            core::slice::from_raw_parts(pack, pack_length),
            core::slice::from_raw_parts_mut(index, index_count),
            core::slice::from_raw_parts_mut(arena, arena_length),
            core::slice::from_raw_parts_mut(scratch, scratch_length),
            &mut *window.cast::<[u8; 32768]>(),
        )
    };
    let limits = pack::MaterializeLimits {
        inflate: InflateLimits {
            max_output: object_limit as u64,
            max_ratio: 0,
            ratio_slack: object_limit as u64,
        },
        delta: pack::DeltaLimits {
            max_output: object_limit as u64,
            max_instructions: object_limit as u64,
            max_depth: 64,
        },
        max_passes: 65,
        max_decoded_bytes_per_pass: (arena_length as u64).saturating_mul(8),
        max_base_probes: 16 * 1024 * 1024,
        max_tree_validation_bytes: 64 * 1024 * 1024,
    };
    let result = (|| {
        let store = pack::materialize(pack, index, arena, scratch, window, limits)?;
        for i in 0..store.entries().len() {
            let object = store.object(i).ok_or(Error::InvalidObject)?;
            if object.kind == ObjectKind::Tree {
                validate_tree_names(object.data)?;
            }
        }
        Ok(store.bytes_used())
    })();
    match result {
        Ok(length) => {
            // SAFETY: The used output is exclusive and disjoint from all storage.
            unsafe { *used = length };
            0
        }
        Err(e) => errno(e),
    }
}

/// Lookup metadata through the shared, imported ID index.
/// # Safety
/// Index is initialized and sorted by import; ID has 20 bytes; result is disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_pack_find(
    index: *const PackObject,
    count: usize,
    id: *const u8,
    result: *mut Decoded,
) -> i32 {
    // SAFETY: C retains immutable index storage and supplies a live ID/output.
    let (index, id) = unsafe {
        (
            core::slice::from_raw_parts(index, count),
            ObjectId::Sha1(*id.cast::<[u8; 20]>()),
        )
    };
    let Some(entry) = pack::find_materialized(index, id) else {
        return -2;
    };
    let Ok(kind) = entry.kind() else {
        return -74;
    };
    // SAFETY: Result is an exclusive aligned struct output.
    unsafe {
        *result = Decoded {
            offset: entry.offset(),
            length: entry.length(),
            kind: kind as u32,
        }
    };
    0
}

/// Expose a raw pack after shared shallow-response and checksum validation.
/// # Safety
/// Data is live for length bytes; result is a disjoint aligned output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_smart_pack(
    data: *const u8,
    length: usize,
    result: *mut Decoded,
) -> i32 {
    // SAFETY: C supplies a complete live HTTP response body.
    let data = unsafe { core::slice::from_raw_parts(data, length) };
    match client::read_pack_response(data) {
        Ok(pack) => {
            // SAFETY: Result is writable; pack is a subslice of data.
            unsafe {
                *result = Decoded {
                    offset: length - pack.len(),
                    length: pack.len(),
                    kind: 0,
                }
            };
            0
        }
        Err(e) => errno(e),
    }
}
