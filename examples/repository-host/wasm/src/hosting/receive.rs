use super::{
    Frame, Shared, capabilities, capability_bits, code, format, input, output, u32_at, u64_at,
};
use core::cell::UnsafeCell;
use git_zero::{
    Error, ObjectFormat, ObjectKind, Result,
    http::receive::{Capabilities, ControlDecoder, ControlEvent, Limits},
    pack::{DecodeLimits, DecodeStatus, Decoder, EntryKind},
    zlib::{InflateLimits, WINDOW_SIZE},
};

static PACKET: Shared<[u8; 65520]> = Shared(UnsafeCell::new([0; 65520]));
static WINDOW: Shared<[u8; WINDOW_SIZE]> = Shared(UnsafeCell::new([0; WINDOW_SIZE]));
static RECEIVE: Shared<Option<Receive>> = Shared(UnsafeCell::new(None));

#[derive(Clone, Copy)]
enum Phase {
    Controls,
    Pack,
    Eof,
    Finished,
    Failed(Error),
}
struct Receive {
    controls: Option<ControlDecoder<'static>>,
    pack: Option<Decoder<'static>>,
    format: ObjectFormat,
    limits: DecodeLimits,
    phase: Phase,
    selected: Capabilities,
    commands: u32,
    blob: bool,
}

/// Init frame (72 bytes): format u32@0; control-byte limit u64@8;
/// pack-byte limit u64@16; max objects u32@24; inflate size/ratio/slack
/// u64@32/40/48; command/shallow/option limits u32@56/60/64;
/// offered capability mask u32@68. Only opt into implemented host features.
#[unsafe(no_mangle)]
pub extern "C" fn host_receive_init(length: usize) -> i32 {
    host_receive_cancel();
    let result = initialize(length);
    match result {
        Ok(()) => 0,
        Err(error) => code(error),
    }
}

fn initialize(length: usize) -> Result<()> {
    let bytes = input(length)?;
    if bytes.len() != 72 {
        return Err(Error::InvalidRequest);
    }
    let format = format(u32_at(bytes, 0)?)?;
    let offered = capabilities(u32_at(bytes, 68)?)?;
    let controls = Limits {
        max_bytes: u64_at(bytes, 8)?,
        max_commands: u32_at(bytes, 56)?,
        max_shallow: u32_at(bytes, 60)?,
        max_options: u32_at(bytes, 64)?,
    };
    let limits = DecodeLimits {
        max_pack_bytes: u64_at(bytes, 16)?,
        max_objects: u32_at(bytes, 24)?,
        inflate: InflateLimits {
            max_output: u64_at(bytes, 32)?,
            max_ratio: u32::try_from(u64_at(bytes, 40)?).map_err(|_| Error::InvalidRequest)?,
            ratio_slack: u64_at(bytes, 48)?,
        },
    };
    // SAFETY: Drop all old packet/window borrows before reusing scratch. The
    // exclusive ABI contract prevents concurrent calls or access by another job.
    unsafe {
        *RECEIVE.0.get() = None;
        let controls = ControlDecoder::new(&mut *PACKET.0.get(), format, offered, controls)?;
        *RECEIVE.0.get() = Some(Receive {
            controls: Some(controls),
            pack: None,
            format,
            limits,
            phase: Phase::Controls,
            selected: Capabilities::default(),
            commands: 0,
            blob: false,
        });
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn host_receive_cancel() {
    // SAFETY: Exclusive ABI call; releases every borrowed scratch arena.
    unsafe {
        *RECEIVE.0.get() = None;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn host_receive_capabilities() -> u32 {
    // SAFETY: Read only during this exclusive synchronous call.
    unsafe {
        (&*RECEIVE.0.get())
            .as_ref()
            .map_or(0, |session| capability_bits(session.selected))
    }
}

/// Return an event envelope and payload length, or a negative stable error code.
/// Tags: 1 need input; 2 shallow; 3 command; 4 options; 5 option; 6 control
/// end; 7 pack header; 8 entry header; 9 body; 10 entry end; 11 pack checksum;
/// 12 transport EOF. All payload bytes and consumed packed bytes are staged
/// privately by the host. No event asserts ref publication or push success.
#[unsafe(no_mangle)]
pub extern "C" fn host_receive_step(length: usize, end_of_input: u32) -> i32 {
    let result = input(length).and_then(|bytes| {
        if end_of_input > 1 {
            return Err(Error::InvalidRequest);
        }
        // SAFETY: Session scratch is disjoint from INPUT/OUTPUT. Output is
        // obtained exactly once; no references to either arena survive this call.
        let session = unsafe {
            (&mut *RECEIVE.0.get())
                .as_mut()
                .ok_or(Error::InvalidRequest)?
        };
        let result = session.step(bytes, end_of_input != 0, Frame::new(output()));
        if let Err(error) = result {
            session.phase = Phase::Failed(error);
        }
        result
    });
    match result {
        Ok(length) => length,
        Err(error) => {
            // SAFETY: The failed exclusive call has returned all temporary
            // borrows. Preserve a terminal failure until cancel/reinitialize.
            unsafe {
                if let Some(session) = (&mut *RECEIVE.0.get()).as_mut() {
                    session.phase = Phase::Failed(error);
                }
            }
            code(error)
        }
    }
}

impl Receive {
    fn step(&mut self, bytes: &[u8], eof: bool, mut frame: Frame<'_>) -> Result<i32> {
        match self.phase {
            Phase::Failed(error) => Err(error),
            Phase::Controls => self.control(bytes, eof, frame),
            Phase::Pack => self.pack(bytes, eof, frame),
            Phase::Eof | Phase::Finished => {
                if !bytes.is_empty() {
                    return Err(Error::InvalidRequest);
                }
                if eof {
                    self.phase = Phase::Finished;
                }
                frame.wide(16, u64::from(self.commands));
                frame.word(24, capability_bits(self.selected));
                Ok(frame.finish(if eof { 12 } else { 1 }, 0, 0))
            }
        }
    }

    fn control(&mut self, bytes: &[u8], eof: bool, mut frame: Frame<'_>) -> Result<i32> {
        let decoder = self.controls.as_mut().ok_or(Error::InvalidRequest)?;
        let progress = decoder.read(bytes, eof)?;
        let consumed = progress.consumed;
        let required = matches!(
            progress.event,
            ControlEvent::End {
                pack_required: true
            }
        );
        let (tag, written) = match progress.event {
            ControlEvent::NeedInput => (1, 0),
            ControlEvent::Shallow(id) => {
                frame.id(0, id);
                (2, 0)
            }
            ControlEvent::Command(command) => {
                frame.id(0, command.old);
                frame.id(1, command.new);
                frame.word(
                    12,
                    u32::from(command.name.starts_with(b"refs/heads/"))
                        | (u32::from(command.old.is_zero()) << 1)
                        | (u32::from(command.new.is_zero()) << 2),
                );
                frame.data(command.name)?;
                self.commands += 1;
                (3, command.name.len())
            }
            ControlEvent::Options => (4, 0),
            ControlEvent::PushOption(option) => {
                frame.data(option)?;
                (5, option.len())
            }
            ControlEvent::End { pack_required } => {
                frame.word(12, u32::from(pack_required));
                frame.wide(16, u64::from(self.commands));
                (6, 0)
            }
        };
        self.selected = decoder.capabilities();
        frame.word(24, capability_bits(self.selected));
        if tag == 6 {
            self.controls = None;
            if required {
                // SAFETY: WINDOW has no live borrow until entering Pack. The
                // previous Receive was dropped by initialize before any reuse.
                self.pack = Some(Decoder::new(
                    self.format,
                    unsafe { &mut *WINDOW.0.get() },
                    self.limits,
                ));
                self.phase = Phase::Pack;
            } else {
                self.phase = Phase::Eof;
            }
        }
        Ok(frame.finish(tag, consumed, written))
    }

    fn pack(&mut self, bytes: &[u8], eof: bool, mut frame: Frame<'_>) -> Result<i32> {
        let decoder = self.pack.as_mut().ok_or(Error::InvalidRequest)?;
        let progress = decoder.update(bytes, frame.payload(), eof)?;
        let tag = match progress.status {
            DecodeStatus::NeedInput => {
                frame.word(12, 1);
                9
            }
            DecodeStatus::NeedOutput => {
                frame.word(12, 2);
                9
            }
            DecodeStatus::Header { version, objects } => {
                frame.word(12, version);
                frame.wide(16, u64::from(objects));
                7
            }
            DecodeStatus::EntryStart(header) => {
                self.blob = matches!(header.kind, EntryKind::Object(ObjectKind::Blob));
                frame.wide(16, header.offset);
                frame.wide(24, header.size);
                frame.wide(32, decoder.position());
                let kind = match header.kind {
                    EntryKind::Object(kind) => kind as u32,
                    EntryKind::OffsetDelta { base_offset } => {
                        frame.wide(40, base_offset);
                        6
                    }
                    EntryKind::ReferenceDelta { base_id } => {
                        frame.id(0, base_id);
                        7
                    }
                };
                frame.word(12, kind);
                8
            }
            DecodeStatus::EntryEnd {
                end_offset,
                crc32,
                object_id,
            } => {
                frame.wide(16, end_offset);
                frame.word(24, crc32);
                if let Some(id) = object_id {
                    frame.id(0, id);
                    // A verified plain blob has no links to scan. This remains
                    // provisional until the complete pack and transport EOF.
                    frame.word(12, 1 | (u32::from(self.blob) << 1));
                }
                10
            }
            DecodeStatus::Finished(id) => {
                frame.id(0, id);
                self.phase = Phase::Eof;
                11
            }
        };
        Ok(frame.finish(tag, progress.consumed, progress.written))
    }
}
