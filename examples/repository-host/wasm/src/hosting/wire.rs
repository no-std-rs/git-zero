//! Bounded smart HTTP receive advertisement and authoritative status encoding.
use super::{FRAME_BYTES, Frame, Shared, capabilities, code, format, id_at, input, output, u32_at};
use core::cell::UnsafeCell;
use git_zero::{
    Error, ObjectFormat, Result,
    http::receive::{Advertisement, Status, write_progress},
    io::SliceWriter,
};

static NAME: Shared<[u8; 65536]> = Shared(UnsafeCell::new([0; 65536]));
static ADVERT: Shared<Option<(ObjectFormat, Advertisement<'static>)>> =
    Shared(UnsafeCell::new(None));
static STATUS: Shared<Option<Status>> = Shared(UnsafeCell::new(None));

fn emit(operation: impl FnOnce(&mut SliceWriter<'_>) -> Result<()>) -> i32 {
    let bytes = output();
    let result = {
        let mut writer = SliceWriter::new(&mut bytes[FRAME_BYTES..]);
        operation(&mut writer).map(|()| writer.as_bytes().len())
    };
    match result {
        Ok(length) => Frame::new(bytes).finish(60, 0, length),
        Err(error) => {
            // SAFETY: Exclusive call; no scratch borrow survives the closure.
            unsafe {
                *ADVERT.0.get() = None;
                *STATUS.0.get() = None;
            }
            code(error)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn host_advert_start(object_format: u32, flags: u32) -> i32 {
    emit(|output| {
        // SAFETY: Drop old NAME borrow before constructing the new encoder.
        unsafe {
            *ADVERT.0.get() = None;
        }
        let format = format(object_format)?;
        let caps = capabilities(flags)?;
        // SAFETY: NAME is private to ADVERT; each call is exclusive.
        let mut advert = Advertisement::new(format, caps, unsafe { &mut *NAME.0.get() });
        advert.start(output)?;
        unsafe {
            *ADVERT.0.get() = Some((format, advert));
        }
        Ok(())
    })
}

/// Padded binary ID at 0..32, followed by an exact raw ref name.
#[unsafe(no_mangle)]
pub extern "C" fn host_advert_ref(length: usize) -> i32 {
    emit(|output| {
        let bytes = input(length)?;
        // SAFETY: Exclusive synchronous access; input and output are disjoint.
        let (format, advert) =
            unsafe { (&mut *ADVERT.0.get()).as_mut() }.ok_or(Error::InvalidRequest)?;
        let name = bytes.get(32..).ok_or(Error::InvalidRequest)?;
        advert.reference(id_at(bytes, 0, *format)?, name, output)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn host_advert_finish() -> i32 {
    emit(|output| {
        // SAFETY: Exclusive call; take drops the retained NAME borrow on exit.
        let (_, mut advert) =
            unsafe { (&mut *ADVERT.0.get()).take() }.ok_or(Error::InvalidRequest)?;
        advert.finish(output)
    })
}

/// Empty input means unpack success; otherwise input is a bounded error reason.
#[unsafe(no_mangle)]
pub extern "C" fn host_status_start(length: usize, flags: u32) -> i32 {
    emit(|output| {
        // SAFETY: Exclusive call; invalidate a previous status even on failure.
        unsafe {
            *STATUS.0.get() = None;
        }
        let bytes = input(length)?;
        let mut status = Status::new(capabilities(flags)?);
        status.unpack(if bytes.is_empty() { Ok(()) } else { Err(bytes) }, output)?;
        unsafe {
            *STATUS.0.get() = Some(status);
        }
        Ok(())
    })
}

/// Name length u32@0, followed by name and optional error reason. No reason
/// means ref success, which the host may assert only after durable publication.
#[unsafe(no_mangle)]
pub extern "C" fn host_status_ref(length: usize) -> i32 {
    emit(|output| {
        let bytes = input(length)?;
        let end = 4usize
            .checked_add(u32_at(bytes, 0)? as usize)
            .ok_or(Error::InvalidRequest)?;
        let name = bytes.get(4..end).ok_or(Error::InvalidRequest)?;
        let reason = bytes.get(end..).ok_or(Error::InvalidRequest)?;
        // SAFETY: Exclusive call, state retains no input/output references.
        let status = unsafe { (&mut *STATUS.0.get()).as_mut() }.ok_or(Error::InvalidRequest)?;
        status.reference(
            name,
            if reason.is_empty() {
                Ok(())
            } else {
                Err(reason)
            },
            output,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn host_status_finish() -> i32 {
    emit(|output| {
        // SAFETY: Exclusive call; consuming state prevents duplicate trailers.
        let mut status = unsafe { (&mut *STATUS.0.get()).take() }.ok_or(Error::InvalidRequest)?;
        status.finish(output)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn host_progress(length: usize, flags: u32) -> i32 {
    emit(|output| write_progress(output, capabilities(flags)?, input(length)?))
}
