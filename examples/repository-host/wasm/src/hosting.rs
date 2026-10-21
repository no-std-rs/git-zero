//! Streaming hosting ABI. Each HTTP operation owns a fresh Wasm instance.
//! Calls are synchronous, single-threaded and non-reentrant. Hosts copy output
//! before the next call and never mutate input during a call. No session retains
//! INPUT/OUTPUT references; packet, inflate and graph scratch are disjoint.
use core::cell::UnsafeCell;
use git_zero::{
    Error, ObjectFormat, ObjectId, ObjectKind, Result, http::receive::Capabilities, pack::ObjectKey,
};

mod budget;
mod compose;
mod graph;
mod ranges;
mod receive;
mod source;
mod upload;
mod wire;

pub(super) const INPUT_BYTES: usize = 65536;
pub(super) const OUTPUT_BYTES: usize = 131072;
pub(super) const FRAME_BYTES: usize = 128;

pub(super) struct Shared<T>(pub UnsafeCell<T>);
// SAFETY: This module is compiled only for the synchronous, single-threaded
// Wasm ABI above. Each request has its own instance and callers never reenter.
unsafe impl<T> Sync for Shared<T> {}
static INPUT: Shared<[u8; INPUT_BYTES]> = Shared(UnsafeCell::new([0; INPUT_BYTES]));
static OUTPUT: Shared<[u8; OUTPUT_BYTES]> = Shared(UnsafeCell::new([0; OUTPUT_BYTES]));

#[unsafe(no_mangle)]
pub extern "C" fn host_input_ptr() -> *mut u8 {
    INPUT.0.get().cast()
}
#[unsafe(no_mangle)]
pub extern "C" fn host_input_capacity() -> usize {
    INPUT_BYTES
}
#[unsafe(no_mangle)]
pub extern "C" fn host_output_ptr() -> *const u8 {
    OUTPUT.0.get().cast()
}
#[unsafe(no_mangle)]
pub extern "C" fn host_output_capacity() -> usize {
    OUTPUT_BYTES
}

pub(super) fn input(length: usize) -> Result<&'static [u8]> {
    if length > INPUT_BYTES {
        return Err(Error::BufferTooSmall);
    }
    // SAFETY: Read-only for the duration of one exclusive ABI call. INPUT is
    // never retained by a session, and JS cannot modify it during the call.
    Ok(unsafe { &(&*INPUT.0.get())[..length] })
}

pub(super) fn output() -> &'static mut [u8] {
    // SAFETY: One exclusive ABI call obtains OUTPUT once, and returns before
    // another call or JS access. No session retains a reference to this arena.
    unsafe { &mut *OUTPUT.0.get() }
}

pub(super) fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(Error::InvalidRequest)?
            .try_into()
            .map_err(|_| Error::InvalidRequest)?,
    ))
}
pub(super) fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(Error::InvalidRequest)?
            .try_into()
            .map_err(|_| Error::InvalidRequest)?,
    ))
}
pub(super) fn format(value: u32) -> Result<ObjectFormat> {
    match value {
        0 => Ok(ObjectFormat::Sha1),
        1 => Ok(ObjectFormat::Sha256),
        _ => Err(Error::InvalidRequest),
    }
}

pub(super) fn kind(value: u32) -> Result<ObjectKind> {
    match value {
        1 => Ok(ObjectKind::Commit),
        2 => Ok(ObjectKind::Tree),
        3 => Ok(ObjectKind::Blob),
        4 => Ok(ObjectKind::Tag),
        _ => Err(Error::InvalidObject),
    }
}

pub(super) fn id_at(bytes: &[u8], offset: usize, format: ObjectFormat) -> Result<ObjectId> {
    ObjectId::from_bytes(
        format,
        bytes
            .get(offset..offset + format.id_length())
            .ok_or(Error::InvalidRequest)?,
    )
}

pub(super) fn key_at(
    bytes: &[u8],
    tag: usize,
    staged: usize,
    id: usize,
    format: ObjectFormat,
) -> Result<ObjectKey> {
    match u32_at(bytes, tag)? {
        0 => Ok(ObjectKey::Id(id_at(bytes, id, format)?)),
        1 => Ok(ObjectKey::Staged(u64_at(bytes, staged)?)),
        _ => Err(Error::InvalidRequest),
    }
}

pub(super) const CAPS: [Capabilities; 8] = [
    Capabilities::REPORT_STATUS,
    Capabilities::REPORT_STATUS_V2,
    Capabilities::DELETE_REFS,
    Capabilities::OFS_DELTA,
    Capabilities::ATOMIC,
    Capabilities::PUSH_OPTIONS,
    Capabilities::SIDE_BAND_64K,
    Capabilities::QUIET,
];
pub(super) fn capabilities(flags: u32) -> Result<Capabilities> {
    if flags > 255 {
        return Err(Error::InvalidRequest);
    }
    let mut caps = Capabilities::default();
    for (index, flag) in CAPS.iter().enumerate() {
        if flags & (1 << index) != 0 {
            caps = caps | *flag;
        }
    }
    Ok(caps)
}
pub(super) fn capability_bits(caps: Capabilities) -> u32 {
    CAPS.iter().enumerate().fold(0, |bits, (index, flag)| {
        bits | if caps.contains(*flag) { 1 << index } else { 0 }
    })
}

pub(super) fn code(error: Error) -> i32 {
    -match error {
        Error::BufferTooSmall => 1,
        Error::InvalidPacket => 2,
        Error::InvalidRequest => 3,
        Error::CorruptPack => 4,
        Error::InvalidZlib => 5,
        Error::InvalidObjectId => 6,
        Error::InvalidObject => 7,
        Error::InvalidReference => 8,
        Error::InvalidFileName => 9,
        Error::UnsortedFiles => 10,
        Error::ObjectNotFound => 11,
        Error::Unsupported => 12,
        Error::Backend => 13,
        Error::LimitExceeded => 14,
    }
}

/// Fixed little-endian event envelope followed by payload. Event-specific
/// fields occupy offsets 12..48; two padded binary IDs occupy 48..112.
pub(super) struct Frame<'a> {
    bytes: &'a mut [u8],
}
impl<'a> Frame<'a> {
    pub(super) fn new(bytes: &'a mut [u8]) -> Self {
        bytes[..FRAME_BYTES].fill(0);
        Self { bytes }
    }
    pub(super) fn word(&mut self, offset: usize, value: u32) {
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    pub(super) fn wide(&mut self, offset: usize, value: u64) {
        self.bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    pub(super) fn id(&mut self, slot: usize, id: ObjectId) {
        let start = 48 + slot * 32;
        self.bytes[start..start + id.as_bytes().len()].copy_from_slice(id.as_bytes());
    }
    pub(super) fn key(&mut self, key: ObjectKey) {
        match key {
            ObjectKey::Id(id) => {
                self.word(112, 0);
                self.id(0, id);
            }
            ObjectKey::Staged(id) => {
                self.word(112, 1);
                self.wide(40, id);
            }
        }
    }
    pub(super) fn payload(&mut self) -> &mut [u8] {
        &mut self.bytes[FRAME_BYTES..FRAME_BYTES + INPUT_BYTES]
    }
    pub(super) fn data(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes
            .get_mut(FRAME_BYTES..FRAME_BYTES + bytes.len())
            .ok_or(Error::BufferTooSmall)?
            .copy_from_slice(bytes);
        self.word(8, bytes.len() as u32);
        Ok(())
    }
    pub(super) fn finish(mut self, tag: u32, consumed: usize, written: usize) -> i32 {
        self.word(0, tag);
        self.word(4, consumed as u32);
        self.word(8, written as u32);
        (FRAME_BYTES + written) as i32
    }
}

// Typed operations have returned all arena borrows before failure cleanup.
pub(super) fn finish(result: Result<i32>, cancel: impl FnOnce()) -> i32 {
    match result {
        Ok(value) => value,
        Err(error) => {
            cancel();
            code(error)
        }
    }
}

// Build the bounded payload before translating the result and releasing state.
pub(super) fn emit(
    operation: impl FnOnce(&mut git_zero::io::SliceWriter<'_>) -> Result<()>,
    cancel: impl FnOnce(),
) -> i32 {
    let bytes = output();
    let result = {
        let mut writer = git_zero::io::SliceWriter::new(&mut bytes[FRAME_BYTES..]);
        operation(&mut writer).map(|()| writer.as_bytes().len())
    };
    finish(
        result.map(|length| Frame::new(bytes).finish(60, 0, length)),
        cancel,
    )
}
