//! Authenticated compressed-entry reuse, with a bounded canonical fallback.
use super::{FRAME_BYTES, Frame, Shared, code, format, id_at, input, kind, output, u32_at, u64_at};
use core::cell::UnsafeCell;
use git_zero::{
    Error, ObjectFormat, Result,
    http::stored,
    io::{SliceWriter, Write},
    pack::{CompressedEntry, CompressedKind, PackComposer},
    zlib::StoredEncoder,
};

struct Compose {
    format: ObjectFormat,
    band: u8,
    pack: PackComposer,
    plain: Option<StoredEncoder>,
}
static COMPOSE: Shared<Option<Compose>> = Shared(UnsafeCell::new(None));
struct Band<'a, W> {
    output: &'a mut W,
    band: u8,
}
impl<W: Write> Write for Band<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        stored::pack_chunk(self.band, bytes, self.output)
    }
}
struct Compressed<'a, W> {
    pack: &'a mut PackComposer,
    output: &'a mut W,
}
impl<W: Write> Write for Compressed<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.pack.write_compressed(bytes, self.output)
    }
}
fn session() -> Result<&'static mut Compose> {
    // SAFETY: Exclusive synchronous ABI call, no references escape to JS.
    unsafe { (&mut *COMPOSE.0.get()).as_mut() }.ok_or(Error::InvalidRequest)
}
fn emit(operation: impl FnOnce(&mut SliceWriter<'_>) -> Result<()>) -> i32 {
    let bytes = output();
    let value = {
        let mut writer = SliceWriter::new(&mut bytes[FRAME_BYTES..]);
        operation(&mut writer).map(|()| writer.as_bytes().len())
    };
    match value {
        Ok(length) => Frame::new(bytes).finish(60, 0, length),
        Err(error) => {
            host_pack_cancel();
            code(error)
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn host_pack_cancel() {
    // SAFETY: Exclusive call; state retains no borrowed arenas.
    unsafe {
        *COMPOSE.0.get() = None;
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn host_pack_start(object_format: u32, count: u32, limit: u64, band: u32) -> i32 {
    host_pack_cancel();
    emit(|output| {
        let format = format(object_format)?;
        if band > 2 {
            return Err(Error::InvalidRequest);
        }
        let pack = PackComposer::new(
            format,
            count,
            limit,
            &mut Band {
                output,
                band: band as u8,
            },
        )?;
        // SAFETY: Exclusive initialization after dropping the previous state.
        unsafe {
            *COMPOSE.0.get() = Some(Compose {
                format,
                band: band as u8,
                pack,
                plain: None,
            });
        }
        Ok(())
    })
}

/// Entry frame: object kind@0, fallback mode@4, padded object/base IDs@8/40,
/// inflated representation size@72 and compressed length@80 (88 bytes total).
/// Fallback mode 1 uses canonical kind/size and computes its exact zlib length.
#[unsafe(no_mangle)]
pub extern "C" fn host_pack_entry(length: usize) -> i32 {
    emit(|output| {
        let session = session()?;
        if session.plain.is_some() {
            return Err(Error::InvalidRequest);
        }
        let bytes = input(length)?;
        if bytes.len() != 88 {
            return Err(Error::InvalidRequest);
        }
        let mode = u32_at(bytes, 4)?;
        if mode > 1 {
            return Err(Error::InvalidRequest);
        }
        let wire_kind = u32_at(bytes, 0)?;
        let size = u64_at(bytes, 72)?;
        let kind = if wire_kind == 7 && mode == 0 {
            CompressedKind::Delta {
                base_id: id_at(bytes, 40, session.format)?,
            }
        } else {
            CompressedKind::Object(kind(wire_kind)?)
        };
        let compressed_size = if mode == 1 {
            StoredEncoder::encoded_size(size)?
        } else {
            u64_at(bytes, 80)?
        };
        let entry = CompressedEntry {
            id: id_at(bytes, 8, session.format)?,
            kind,
            inflated_size: size,
            compressed_size,
        };
        let mut output = Band {
            output,
            band: session.band,
        };
        session.pack.start_entry(entry, &mut output)?;
        if mode == 1 {
            session.plain = Some(StoredEncoder::new(
                size,
                &mut Compressed {
                    pack: &mut session.pack,
                    output: &mut output,
                },
            )?);
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn host_pack_feed(length: usize) -> i32 {
    emit(|output| {
        let session = session()?;
        let bytes = input(length)?;
        let mut output = Band {
            output,
            band: session.band,
        };
        if let Some(plain) = session.plain.as_mut() {
            plain.write(
                bytes,
                &mut Compressed {
                    pack: &mut session.pack,
                    output: &mut output,
                },
            )
        } else {
            session.pack.write_compressed(bytes, &mut output)
        }
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn host_pack_entry_finish() -> i32 {
    emit(|output| {
        let session = session()?;
        if let Some(mut plain) = session.plain.take() {
            plain.finish(&mut Compressed {
                pack: &mut session.pack,
                output: &mut Band {
                    output,
                    band: session.band,
                },
            })?;
        }
        session.pack.finish_entry()?;
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn host_pack_finish() -> i32 {
    emit(|output| {
        // SAFETY: Exclusive call consumes state so no duplicate trailer escapes.
        let mut session = unsafe { (&mut *COMPOSE.0.get()).take() }.ok_or(Error::InvalidRequest)?;
        if session.plain.is_some() {
            return Err(Error::InvalidRequest);
        }
        session.pack.finish(&mut Band {
            output,
            band: session.band,
        })?;
        if session.band != 0 {
            output.write_all(b"0000")?;
        }
        Ok(())
    })
}
