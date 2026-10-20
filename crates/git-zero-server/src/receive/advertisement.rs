use super::Capabilities;
use git_zero_core::{
    Error, ObjectFormat, ObjectId, Result,
    io::{SliceWriter, Write},
    pktline,
    repository::validate_ref_name_bytes,
};

/// Write a smart HTTP receive-pack advertisement from a stable, byte-sorted
/// ref projection. Names are raw Git bytes. The host must discard partial
/// output on an invalid ref or sink failure. Only opt into host-backed features.
pub fn write_advertisement<'a>(
    format: ObjectFormat,
    capabilities: Capabilities,
    refs: impl IntoIterator<Item = (ObjectId, &'a [u8])>,
    output: &mut impl Write,
) -> Result<()> {
    let mut storage = [0; 256];
    let mut caps = SliceWriter::new(&mut storage);
    capabilities.write(format, &mut caps)?;
    let caps = caps.finish();
    pktline::write(output, b"# service=git-receive-pack\n")?;
    output.write_all(b"0000")?;
    let mut first = true;
    let mut previous: Option<&[u8]> = None;
    for (id, name) in refs {
        validate_ref_name_bytes(name)?;
        if id.format() != format
            || id.is_zero()
            || previous.is_some_and(|previous| previous >= name)
        {
            return Err(Error::InvalidReference);
        }
        let hex = id.to_hex();
        if first {
            pktline::write_parts(output, &[hex.as_bytes(), b" ", name, b"\0", caps, b"\n"])?;
        } else {
            pktline::write_parts(output, &[hex.as_bytes(), b" ", name, b"\n"])?;
        }
        first = false;
        previous = Some(name);
    }
    if first {
        let zero = format.zero_id().to_hex();
        pktline::write_parts(
            output,
            &[zero.as_bytes(), b" capabilities^{}\0", caps, b"\n"],
        )?;
    }
    output.write_all(b"0000")
}
