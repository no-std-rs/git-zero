use super::{Frame, INPUT_BYTES, Shared, code, format, input, output, u32_at, u64_at};
use core::cell::UnsafeCell;
use git_zero::{
    Error,
    pack::{
        ByteRange, DeltaDecoder, DeltaLimits, DeltaStatus, EntryRangeDecoder, EntryRangeStatus,
    },
    zlib::{InflateLimits, WINDOW_SIZE},
};

static WINDOW: Shared<[u8; WINDOW_SIZE]> = Shared(UnsafeCell::new([0; WINDOW_SIZE]));
static SCRATCH: Shared<[u8; INPUT_BYTES]> = Shared(UnsafeCell::new([0; INPUT_BYTES]));
static SOURCE: Shared<Option<EntryRangeDecoder<'static, 'static>>> = Shared(UnsafeCell::new(None));

#[unsafe(no_mangle)]
pub extern "C" fn host_source_cancel() {
    // SAFETY: Exclusive call releases WINDOW/SCRATCH before reuse.
    unsafe {
        *SOURCE.0.get() = None;
    }
}

/// Init (64 bytes): format u32@0; entry offset/wire budget/inflate max/ratio/
/// slack/range offset/range length u64@8/16/24/32/40/48/56.
#[unsafe(no_mangle)]
pub extern "C" fn host_source_init(length: usize) -> i32 {
    host_source_cancel();
    let result = (|| {
        let bytes = input(length)?;
        if bytes.len() != 64 {
            return Err(Error::InvalidRequest);
        }
        let format = format(u32_at(bytes, 0)?)?;
        let inflate = InflateLimits {
            max_output: u64_at(bytes, 24)?,
            max_ratio: u32::try_from(u64_at(bytes, 32)?).map_err(|_| Error::InvalidRequest)?,
            ratio_slack: u64_at(bytes, 40)?,
        };
        let range = ByteRange {
            offset: u64_at(bytes, 48)?,
            length: u64_at(bytes, 56)?,
        };
        // SAFETY: cancel dropped old borrows. The private arenas are disjoint
        // from every other session and from the host-facing input/output.
        unsafe {
            *SOURCE.0.get() = Some(EntryRangeDecoder::new(
                format,
                u64_at(bytes, 8)?,
                &mut *WINDOW.0.get(),
                u64_at(bytes, 16)?,
                inflate,
                range,
                &mut *SCRATCH.0.get(),
            )?);
        }
        Ok(0)
    })();
    result.unwrap_or_else(code)
}

/// Tag 40=progress, 41=range finished. Flags: 0 continue, 1 need input,
/// 2 need output, 3 finished. Positions @16/24/32: packed/inflated/emitted.
#[unsafe(no_mangle)]
pub extern "C" fn host_source_step(length: usize, end_of_input: u32, capacity: usize) -> i32 {
    let result = (|| {
        if capacity > INPUT_BYTES {
            return Err(Error::BufferTooSmall);
        }
        if end_of_input > 1 {
            return Err(Error::InvalidRequest);
        }
        let bytes = input(length)?;
        // SAFETY: Exclusive session access. Source retains neither INPUT nor
        // OUTPUT; inflation and pending bytes use its private borrowed arenas.
        let session = unsafe {
            (&mut *SOURCE.0.get())
                .as_mut()
                .ok_or(Error::InvalidRequest)?
        };
        let mut frame = Frame::new(output());
        let step = session.update(bytes, &mut frame.payload()[..capacity], end_of_input != 0)?;
        let flag = match step.status {
            EntryRangeStatus::Continue => 0,
            EntryRangeStatus::NeedInput => 1,
            EntryRangeStatus::NeedOutput => 2,
            EntryRangeStatus::Finished => 3,
        };
        frame.word(12, flag);
        frame.wide(16, session.wire_position());
        frame.wide(24, session.inflated_position());
        frame.wide(32, session.position());
        Ok(frame.finish(if flag == 3 { 41 } else { 40 }, step.consumed, step.written))
    })();
    match result {
        Ok(value) => value,
        Err(error) => {
            host_source_cancel();
            code(error)
        }
    }
}

/// Parse the bounded size-header prefix of a staged delta. Tag 50 carries
/// base/result sizes @16/24 and resolved depth @32. No instructions execute.
#[unsafe(no_mangle)]
pub extern "C" fn host_delta_header(
    length: usize,
    base_size: u64,
    base_depth: u32,
    max_output: u64,
    max_depth: u32,
) -> i32 {
    let result = (|| {
        let bytes = input(length)?;
        let mut decoder = DeltaDecoder::new(
            base_size,
            base_depth,
            DeltaLimits {
                max_output,
                max_instructions: 0,
                max_depth,
            },
        )?;
        let step = decoder.update(bytes, true)?;
        let DeltaStatus::Header {
            base_size,
            result_size,
        } = step.status
        else {
            return Err(Error::CorruptPack);
        };
        let mut frame = Frame::new(output());
        frame.wide(16, base_size);
        frame.wide(24, result_size);
        frame.wide(32, u64::from(decoder.depth()));
        Ok(frame.finish(50, step.consumed, 0))
    })();
    result.unwrap_or_else(code)
}
