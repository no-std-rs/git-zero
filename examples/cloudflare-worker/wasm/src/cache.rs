// Included in abi: all accesses obey its exclusive, synchronous call contract.
// The async host retains the instance; no imported callback or Rust future runs.
const CACHE_CAPACITY: usize = 147456;

/// Select and authorize an imported request, then suspend before storage I/O.
/// 1=selected pack, 0=ordinary response (no storage), negative=start error.
/// Host policies are explicit scalars, copied before any backend await.
#[unsafe(no_mangle)]
pub extern "C" fn cache_prepare(
    input_length: usize,
    method: u32,
    policy: u32,
    max_want_lines: u32,
    revisions: u32,
    max_exclusions: u32,
    graph_read_limit: u32,
) -> i32 {
    stream_cancel();
    let revision = match revisions {
        0 => None,
        1 => match RevisionConfig::new(max_exclusions, graph_read_limit) {
            Ok(value) => Some(value),
            Err(_) => return -2,
        },
        _ => return -2,
    };
    let result = start_repository_frame(
        input_length, method, policy, max_want_lines as usize, revision,
    );
    if result < 0 { return result; }
    // SAFETY: Exclusive entry, moving the session releases its unique encoder.
    let session = unsafe { (&mut *SESSION.0.get()).take() };
    let Some(Session::Imported { repository, encoder }) = session else { return -2; };
    let mut ids = [ObjectFormat::Sha1.zero_id(); raw::OBJECTS];
    let preparation = match http::stream::preparation::PreparedResponse::new(
        encoder, repository, &mut ids,
    ) {
        Ok(value) => value,
        Err(_) => return -2,
    };
    let (session, result) = if preparation.key().is_some() {
        (Session::Prepared { repository, preparation }, 1)
    } else {
        let Ok(encoder) = preparation.without_pack() else { return -2; };
        (Session::Imported { repository, encoder }, 0)
    };
    unsafe { *SESSION.0.get() = Some(session); }
    result
}

/// Copy the versioned 48-byte raw-pack key into OUTPUT. Not a Git filename.
#[unsafe(no_mangle)]
pub extern "C" fn cache_key() -> i32 {
    let Some(Session::Prepared { preparation, .. }) = (unsafe { &*SESSION.0.get() }) else { return -2; };
    let Some(key) = preparation.key() else { return -2; };
    unsafe { (&mut *OUTPUT.0.get())[..48].copy_from_slice(key.as_bytes()); }
    48
}

#[unsafe(no_mangle)]
pub extern "C" fn cache_capacity() -> usize { CACHE_CAPACITY }

/// Writable cache staging exists only in Prepared. After attachment this returns
/// null so a conforming host cannot overwrite bytes borrowed by an active cursor.
/// The pointer is valid until the next ABI call; copy synchronously, never await
/// with a writable view. Cancellation invalidates every previously returned view.
#[unsafe(no_mangle)]
pub extern "C" fn cache_ptr() -> *mut u8 {
    if matches!(unsafe { &*SESSION.0.get() }, Some(Session::Prepared { .. })) {
        OUTPUT.0.get().cast()
    } else { core::ptr::null_mut() }
}

/// Generate raw miss bytes into OUTPUT. Ordinary stream reads remain disabled.
/// Invalid capacities preserve preparation; provider errors cancel it.
#[unsafe(no_mangle)]
pub extern "C" fn cache_read(capacity: usize) -> i32 {
    if capacity == 0 || capacity > OUTPUT_CAPACITY { return -1; }
    let Some(Session::Prepared { repository, preparation }) = (unsafe { &mut *SESSION.0.get() }) else { return -2; };
    let result = preparation.read_pack(*repository, unsafe { &mut (&mut *OUTPUT.0.get())[..capacity] });
    match result {
        Ok(count) => count as i32,
        Err(_) => { stream_cancel(); -2 }
    }
}

/// Authenticate staged hit/miss bytes before any storage publication or protocol
/// output. Errors terminate preparation; a retry starts with fresh authorization.
/// On success the ordinary response cursor owns a read-only borrow of CACHE.
#[unsafe(no_mangle)]
pub extern "C" fn cache_attach(length: usize, byte_limit: usize) -> i32 {
    let session = unsafe { (&mut *SESSION.0.get()).take() };
    unsafe { *CACHED_ACTIVE.0.get() = false; }
    let Some(Session::Prepared { repository, preparation }) = session else { return -2; };
    if length > CACHE_CAPACITY || byte_limit > CACHE_CAPACITY { return -1; }
    let mut ids = [ObjectFormat::Sha1.zero_id(); raw::OBJECTS];
    match preparation.attach(unsafe { &(&*OUTPUT.0.get())[..length] }, &mut ids, byte_limit) {
        Ok(encoder) => {
            // Preparation retained no INPUT references, so it is now output staging.
            // OUTPUT stays immutable until this cached response is cancelled.
            response_header(encoder.response(), unsafe { &mut *INPUT.0.get() });
            unsafe {
                *SESSION.0.get() = Some(Session::Imported { repository, encoder });
                *CACHED_ACTIVE.0.get() = true;
            }
            3
        }
        Err(Error::BufferTooSmall | Error::LimitExceeded) => -1,
        Err(_) => -2,
    }
}
