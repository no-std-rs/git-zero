//! Named checkout selection and ordinary-HTTP symbolic-reference resolution.
use crate::Transport;
use git_zero_core::{
    Error, ObjectId, Result,
    http::Request,
    io::{SliceWriter, Write},
    repository::validate_ref_name,
};

/// Normalize HEAD, a full ref, or an unqualified branch into caller storage.
pub fn reference_name<'a>(name: &str, storage: &'a mut [u8]) -> Result<&'a str> {
    let mut output = SliceWriter::new(storage);
    if name != "HEAD" && !name.starts_with("refs/") {
        output.write_all(b"refs/heads/")?;
    }
    output.write_all(name.as_bytes())?;
    let bytes = output.finish();
    let name = core::str::from_utf8(bytes).map_err(|_| Error::InvalidReference)?;
    if name != "HEAD" {
        validate_ref_name(name)?;
    }
    Ok(name)
}

fn encode_path<'a>(name: &str, storage: &'a mut [u8]) -> Result<&'a str> {
    let mut output = SliceWriter::new(storage);
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"/._-".contains(&byte) {
            output.write_all(&[byte])?;
        } else {
            const HEX: &[u8] = b"0123456789ABCDEF";
            output.write_all(&[b'%', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])?;
        }
    }
    core::str::from_utf8(output.finish()).map_err(|_| Error::InvalidReference)
}

/// Resolve a normalized ref using bounded GETs, without discovery. `name` is
/// HEAD or a validated fully qualified ref. Only symbolic refs are followed;
/// object traversal and tag peeling belong to the shared checkout layer.
pub fn resolve_reference(
    transport: &mut impl Transport,
    name: &str,
    response: &mut [u8],
    path: &mut [u8],
    max_reads: u32,
) -> Result<ObjectId> {
    if name != "HEAD" {
        validate_ref_name(name)?;
    }
    let mut request_path = encode_path(name, path)?;
    for _ in 0..max_reads {
        let (status, length) = transport.request(
            Request {
                content_type: None,
                git_protocol: None,
                method: "GET",
                path: request_path,
                query: None,
                body: &[],
            },
            response,
        )?;
        if status == 404 {
            return Err(Error::ObjectNotFound);
        }
        if status != 200 {
            return Err(Error::Backend);
        }
        let line = response.get(..length).ok_or(Error::BufferTooSmall)?;
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        if let Some(target) = line.strip_prefix(b"ref: ") {
            let target = core::str::from_utf8(target).map_err(|_| Error::InvalidReference)?;
            validate_ref_name(target)?;
            request_path = encode_path(target, path)?;
        } else {
            return ObjectId::from_hex(line);
        }
    }
    Err(Error::LimitExceeded)
}
