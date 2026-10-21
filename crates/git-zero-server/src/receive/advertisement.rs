use super::Capabilities;
use git_zero_core::{
    Error, ObjectFormat, ObjectId, Result,
    io::{SliceWriter, Write},
    pktline,
    repository::validate_ref_name_bytes,
};

/// Incremental advertisement for hosts whose stable ref projection lives in
/// external storage. `previous` retains only the last name, never the ref set.
pub struct Advertisement<'a> {
    format: ObjectFormat,
    capabilities: Capabilities,
    previous: &'a mut [u8],
    length: usize,
    started: bool,
    finished: bool,
}

impl<'a> Advertisement<'a> {
    pub fn new(format: ObjectFormat, capabilities: Capabilities, previous: &'a mut [u8]) -> Self {
        Self {
            format,
            capabilities,
            previous,
            length: 0,
            started: false,
            finished: false,
        }
    }

    pub fn start(&mut self, output: &mut impl Write) -> Result<()> {
        if self.started || self.finished {
            return Err(Error::InvalidRequest);
        }
        self.started = true;
        pktline::write(output, b"# service=git-receive-pack\n")?;
        output.write_all(b"0000")
    }

    pub fn reference(&mut self, id: ObjectId, name: &[u8], output: &mut impl Write) -> Result<()> {
        if !self.started || self.finished {
            return Err(Error::InvalidRequest);
        }
        if name.len() > self.previous.len() {
            return Err(Error::BufferTooSmall);
        }
        let previous = (self.length != 0).then_some(&self.previous[..self.length]);
        reference(self.format, self.capabilities, id, name, previous, output)?;
        self.previous[..name.len()].copy_from_slice(name);
        self.length = name.len();
        Ok(())
    }

    pub fn finish(&mut self, output: &mut impl Write) -> Result<()> {
        if !self.started || self.finished {
            return Err(Error::InvalidRequest);
        }
        self.finished = true;
        finish(self.format, self.capabilities, self.length == 0, output)
    }
}

/// Write a smart HTTP receive-pack advertisement from a stable, byte-sorted
/// ref projection. Names are raw Git bytes. The host must discard partial
/// output on an invalid ref or sink failure. Only opt into host-backed features.
pub fn write_advertisement<'a>(
    format: ObjectFormat,
    capabilities: Capabilities,
    refs: impl IntoIterator<Item = (ObjectId, &'a [u8])>,
    output: &mut impl Write,
) -> Result<()> {
    pktline::write(output, b"# service=git-receive-pack\n")?;
    output.write_all(b"0000")?;
    let mut previous: Option<&[u8]> = None;
    for (id, name) in refs {
        reference(format, capabilities, id, name, previous, output)?;
        previous = Some(name);
    }
    finish(format, capabilities, previous.is_none(), output)
}

fn reference(
    format: ObjectFormat,
    capabilities: Capabilities,
    id: ObjectId,
    name: &[u8],
    previous: Option<&[u8]>,
    output: &mut impl Write,
) -> Result<()> {
    validate_ref_name_bytes(name)?;
    if id.format() != format || id.is_zero() || previous.is_some_and(|previous| previous >= name) {
        return Err(Error::InvalidReference);
    }
    if previous.is_none() {
        first(format, capabilities, id, name, output)
    } else {
        pktline::write_parts(output, &[id.to_hex().as_bytes(), b" ", name, b"\n"])
    }
}

fn first(
    format: ObjectFormat,
    capabilities: Capabilities,
    id: ObjectId,
    name: &[u8],
    output: &mut impl Write,
) -> Result<()> {
    let mut storage = [0; 256];
    let mut caps = SliceWriter::new(&mut storage);
    capabilities.write(format, &mut caps)?;
    pktline::write_parts(
        output,
        &[
            id.to_hex().as_bytes(),
            b" ",
            name,
            b"\0",
            caps.finish(),
            b"\n",
        ],
    )
}

pub(super) fn validate_first_name(
    format: ObjectFormat,
    capabilities: Capabilities,
    name: &[u8],
) -> Result<()> {
    // Reuse the actual encoder's packet budget. A created ref may sort first
    // later, even when its incoming command did not carry any capabilities.
    struct Discard;
    impl Write for Discard {
        fn write_all(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }
    first(format, capabilities, format.zero_id(), name, &mut Discard).map_err(|error| {
        if error == Error::InvalidPacket {
            Error::InvalidReference
        } else {
            error
        }
    })
}

fn finish(
    format: ObjectFormat,
    capabilities: Capabilities,
    empty: bool,
    output: &mut impl Write,
) -> Result<()> {
    if empty {
        first(
            format,
            capabilities,
            format.zero_id(),
            b"capabilities^{}",
            output,
        )?;
    }
    output.write_all(b"0000")
}
