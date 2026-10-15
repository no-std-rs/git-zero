//! Validate routing and HTTP metadata before repository reads or output.
use crate::{Request, Response};
use git_zero_core::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Version {
    V0,
    V1,
    V2,
}

pub(crate) fn version(header: Option<&str>) -> Result<Version> {
    let mut version = None;
    for field in header.into_iter().flat_map(|value| value.split(':')) {
        if field.is_empty() || !field.bytes().all(|byte| (33..=126).contains(&byte)) {
            return Err(Error::InvalidRequest);
        }
        if let Some(value) = field.strip_prefix("version=") {
            if version.is_some() {
                return Err(Error::InvalidRequest);
            }
            version = Some(match value {
                "0" => Version::V0,
                "1" => Version::V1,
                "2" => Version::V2,
                _ => return Err(Error::Unsupported),
            });
        }
    }
    Ok(version.unwrap_or(Version::V0))
}

pub(crate) fn path(path: &str) -> Result<&str> {
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.bytes().any(|byte| matches!(byte, b'?' | b'#')) {
        return Err(Error::InvalidRequest);
    }
    for segment in path.split('/') {
        let mut dots = 0;
        let mut length = 0;
        for byte in decoded(segment) {
            let byte = byte?;
            if byte <= 32 || matches!(byte, 127 | b'/' | b'\\') {
                return Err(Error::InvalidRequest);
            }
            length += 1;
            dots += usize::from(byte == b'.');
        }
        if length == 0 || (dots == length && length <= 2) {
            return Err(Error::InvalidRequest);
        }
    }
    Ok(path)
}

/// Decode exactly once, shared by route validation and ref-name matching.
pub(crate) fn decoded(path: &str) -> impl Iterator<Item = Result<u8>> + '_ {
    let mut bytes = path.bytes();
    core::iter::from_fn(move || {
        let byte = bytes.next()?;
        Some(if byte == b'%' {
            escape(&mut bytes)
        } else {
            Ok(byte)
        })
    })
}

fn escape(bytes: &mut impl Iterator<Item = u8>) -> Result<u8> {
    let high = bytes
        .next()
        .and_then(|b| (b as char).to_digit(16))
        .ok_or(Error::InvalidRequest)?;
    let low = bytes
        .next()
        .and_then(|b| (b as char).to_digit(16))
        .ok_or(Error::InvalidRequest)?;
    Ok(((high << 4) | low) as u8)
}

pub(crate) fn service(query: Option<&str>) -> Result<Option<&str>> {
    let mut service = None;
    for field in query.into_iter().flat_map(|query| query.split('&')) {
        if let Some(value) = field.strip_prefix("service=") {
            if service.replace(value).is_some() {
                return Err(Error::InvalidRequest);
            }
        }
        let name = field.split_once('=').map_or(field, |(name, _)| name);
        if field == "service" || name.contains('%') || field.bytes().any(|b| b <= 32 || b == 127) {
            return Err(Error::InvalidRequest);
        }
    }
    Ok(service)
}

pub(crate) fn validate(request: Request<'_>) -> Result<Option<Response>> {
    let path = path(request.path)?;
    version(request.git_protocol)?;
    let service = service(request.query)?;
    let status = match (request.method, path) {
        ("POST", "git-upload-pack") => upload_status(request.content_type),
        (_, "git-upload-pack") => 405,
        ("GET", "info/refs") if service.is_some_and(|s| s != "git-upload-pack") => 403,
        ("GET", _) => 200,
        ("POST", "HEAD" | "info/refs") => 405,
        ("POST", path) if path.starts_with("refs/") || path.starts_with("objects/") => 405,
        ("POST", _) => 200,
        _ => 405,
    };
    if status == 200 {
        return Ok(None);
    }
    Ok(Some(Response {
        status,
        content_type: "text/plain; charset=utf-8",
    }))
}

fn upload_status(content_type: Option<&str>) -> u16 {
    match content_type {
        Some(value)
            if value
                .trim()
                .eq_ignore_ascii_case("application/x-git-upload-pack-request") =>
        {
            200
        }
        _ => 415,
    }
}
