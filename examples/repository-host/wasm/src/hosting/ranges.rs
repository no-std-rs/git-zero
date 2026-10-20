use super::{
    Frame, INPUT_BYTES, Shared, code, finish, format, input, key_at, kind, output, u32_at, u64_at,
};
use core::cell::UnsafeCell;
use git_zero::{
    Error, ObjectFormat, ObjectInfo, Result,
    pack::{
        ObjectRange, RangeFrame, RangeInput, RangeLimits, RangeObject, RangeRepresentation,
        RangeRequest, RangeResolver,
    },
};

const DEPTH: u32 = 128;
static FRAMES: Shared<[RangeFrame; DEPTH as usize + 1]> = Shared(UnsafeCell::new(
    [const { RangeFrame::empty() }; DEPTH as usize + 1],
));
static RANGE: Shared<Option<Session>> = Shared(UnsafeCell::new(None));
struct Session {
    resolver: RangeResolver<'static>,
    format: ObjectFormat,
}

#[unsafe(no_mangle)]
pub extern "C" fn host_range_cancel() {
    // SAFETY: Exclusive ABI call releases every FRAMES borrow before reuse.
    unsafe {
        *RANGE.0.get() = None;
    }
}

/// Init (104 bytes): format@0, key tag@4, staged key u64@8, binary ID@16;
/// range tag@48 (0=All, 1=Bytes), offset/length u64@56/64;
/// output/work/instruction limits u64@72/80/88; depth/read limits u32@96/100.
#[unsafe(no_mangle)]
pub extern "C" fn host_range_init(length: usize) -> i32 {
    host_range_cancel();
    let result = initialize(length);
    result.unwrap_or_else(code)
}

fn object(bytes: &[u8], format: ObjectFormat) -> Result<RangeObject> {
    if bytes.len() != 96 {
        return Err(Error::InvalidRequest);
    }
    let info = ObjectInfo {
        kind: kind(u32_at(bytes, 0)?)?,
        size: u64_at(bytes, 8)?,
    };
    let representation = match u32_at(bytes, 16)? {
        0 => RangeRepresentation::Plain,
        1 => RangeRepresentation::Delta {
            base: key_at(bytes, 20, 24, 32, format)?,
            base_info: ObjectInfo {
                kind: kind(u32_at(bytes, 64)?)?,
                size: u64_at(bytes, 72)?,
            },
            base_depth: u32_at(bytes, 80)?,
            instructions_size: u64_at(bytes, 88)?,
        },
        _ => return Err(Error::InvalidObject),
    };
    Ok(RangeObject {
        info,
        representation,
    })
}

/// Reply to Describe with the 96-byte RangeObject layout above. The host must
/// keep metadata/cache views stable until this reconstruction finishes.
#[unsafe(no_mangle)]
pub extern "C" fn host_range_describe(length: usize) -> i32 {
    let result = describe(length);
    finish(result, host_range_cancel)
}

fn render(mut frame: Frame<'_>, request: RangeRequest, consumed: usize, written: usize) -> i32 {
    let tag = match request {
        RangeRequest::Describe(key) => {
            frame.key(key);
            20
        }
        RangeRequest::Read {
            object,
            kind,
            offset,
            length,
        } => {
            frame.key(object);
            frame.word(12, u32::from(kind == RangeInput::Delta));
            frame.wide(16, offset);
            frame.wide(24, length as u64);
            21
        }
        RangeRequest::Finished => 22,
        RangeRequest::NeedOutput => 23,
    };
    frame.finish(tag, consumed, written)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_range_request() -> i32 {
    let result = request();
    finish(result, host_range_cancel)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_range_feed(length: usize, capacity: usize) -> i32 {
    let result = feed(length, capacity);
    finish(result, host_range_cancel)
}

fn initialize(length: usize) -> Result<i32> {
    let bytes = input(length)?;
    if bytes.len() != 104 {
        return Err(Error::InvalidRequest);
    }
    let format = format(u32_at(bytes, 0)?)?;
    let key = key_at(bytes, 4, 8, 16, format)?;
    let range = match u32_at(bytes, 48)? {
        0 => ObjectRange::All,
        1 => ObjectRange::Bytes {
            offset: u64_at(bytes, 56)?,
            length: u64_at(bytes, 64)?,
        },
        _ => return Err(Error::InvalidRequest),
    };
    let limits = RangeLimits {
        max_output: u64_at(bytes, 72)?,
        max_work_bytes: u64_at(bytes, 80)?,
        max_instructions: u64_at(bytes, 88)?,
        max_depth: u32_at(bytes, 96)?,
        max_read_bytes: u32_at(bytes, 100)? as usize,
    };
    if limits.max_depth > DEPTH || limits.max_read_bytes > INPUT_BYTES {
        return Err(Error::LimitExceeded);
    }
    // SAFETY: cancel dropped all FRAMES references. Input/output arenas are
    // disjoint; the new resolver retains only this private frame arena.
    unsafe {
        *RANGE.0.get() = Some(Session {
            resolver: RangeResolver::new(format, key, range, &mut *FRAMES.0.get(), limits)?,
            format,
        });
    }
    Ok(0)
}

fn describe(length: usize) -> Result<i32> {
    let bytes = input(length)?;
    // SAFETY: Exclusive session access; descriptor owns values, not INPUT.
    let session = unsafe {
        (&mut *RANGE.0.get())
            .as_mut()
            .ok_or(Error::InvalidRequest)?
    };
    session.resolver.describe(object(bytes, session.format)?)?;
    Ok(0)
}

fn request() -> Result<i32> {
    // SAFETY: Exclusive ABI call; result is copied into disjoint OUTPUT.
    let session = unsafe {
        (&mut *RANGE.0.get())
            .as_mut()
            .ok_or(Error::InvalidRequest)?
    };
    let request = session.resolver.request()?;
    Ok(render(Frame::new(output()), request, 0, 0))
}

fn feed(length: usize, capacity: usize) -> Result<i32> {
    if capacity > INPUT_BYTES {
        return Err(Error::BufferTooSmall);
    }
    let bytes = input(length)?;
    // SAFETY: Resolver retains no input/output references, and sessions use
    // separate scratch. This exclusive call obtains OUTPUT exactly once.
    let session = unsafe {
        (&mut *RANGE.0.get())
            .as_mut()
            .ok_or(Error::InvalidRequest)?
    };
    let mut frame = Frame::new(output());
    let step = session
        .resolver
        .update(bytes, &mut frame.payload()[..capacity])?;
    Ok(render(frame, step.request, step.consumed, step.written))
}
