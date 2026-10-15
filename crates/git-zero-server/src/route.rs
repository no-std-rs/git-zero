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
        if field.is_empty() || !field.bytes().all(|byte| (33..=126).contains(&byte)) {
            return Err(Error::InvalidRequest);
        }
    }
    Ok(version.unwrap_or(Version::V0))
}

pub(crate) fn path(path: &str) -> Result<&str> {
    let path = path.strip_prefix('/').unwrap_or(path);
    for segment in path.split('/') {
        let mut bytes = segment.bytes();
        let mut dots = 0;
        let mut length = 0;
        while let Some(byte) = bytes.next() {
            if matches!(byte, b'?' | b'#') {
                return Err(Error::InvalidRequest);
            }
            let byte = if byte == b'%' {
                let high = bytes.next().and_then(|b| (b as char).to_digit(16));
                let low = bytes.next().and_then(|b| (b as char).to_digit(16));
                ((high.ok_or(Error::InvalidRequest)? << 4) | low.ok_or(Error::InvalidRequest)?)
                    as u8
            } else {
                byte
            };
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
    let status = if path == "git-upload-pack" && request.method == "POST" {
        if request.content_type.is_some_and(|value| {
            value
                .trim()
                .eq_ignore_ascii_case("application/x-git-upload-pack-request")
        }) {
            return Ok(None);
        }
        415
    } else if (path == "git-upload-pack" && request.method != "POST")
        || (request.method != "GET"
            && (path == "HEAD"
                || path == "info/refs"
                || path.starts_with("refs/")
                || path.starts_with("objects/")))
        || !matches!(request.method, "GET" | "POST")
    {
        405
    } else if path == "info/refs" && service.is_some_and(|s| s != "git-upload-pack") {
        403
    } else {
        return Ok(None);
    };
    Ok(Some(Response {
        status,
        content_type: "text/plain; charset=utf-8",
    }))
}
