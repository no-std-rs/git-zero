//! Bounded HTTP wire reads shared by the loopback server and service client.
use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Policy {
    Request,
    Response,
}

pub(crate) fn line(reader: &mut impl BufRead, limit: usize, policy: Policy) -> io::Result<String> {
    let mut line = String::new();
    reader.take((limit + 1) as u64).read_line(&mut line)?;
    if line.len() > limit || (policy == Policy::Request && !line.ends_with("\r\n")) {
        return Err(io::Error::other("invalid/oversized HTTP line"));
    }
    Ok(line)
}

pub(crate) fn headers(
    reader: &mut impl BufRead,
    first_line_bytes: usize,
    policy: Policy,
) -> io::Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    let mut bytes = first_line_bytes;
    loop {
        let header = line(reader, 8192, policy)?;
        bytes += header.len();
        if header.is_empty() || bytes > 65536 {
            return Err(io::Error::other("invalid/oversized HTTP headers"));
        }
        if header == "\r\n" {
            return Ok(headers);
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid HTTP header"))?;
        let previous = headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        if policy == Policy::Request && previous.is_some() {
            return Err(io::Error::other("duplicate HTTP header"));
        }
    }
}

fn append_exact(
    reader: &mut impl Read,
    body: &mut Vec<u8>,
    length: usize,
    limit: usize,
) -> io::Result<()> {
    if length > limit.saturating_sub(body.len()) {
        return Err(io::Error::other("HTTP body too large"));
    }
    let start = body.len();
    body.resize(start + length, 0);
    reader.read_exact(&mut body[start..])
}

pub(crate) fn exact(reader: &mut impl Read, length: usize, limit: usize) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    append_exact(reader, &mut body, length, limit)?;
    Ok(body)
}

pub(crate) fn chunked(
    reader: &mut impl BufRead,
    limit: usize,
    policy: Policy,
) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let size = line(reader, 128, policy)?;
        let size = match policy {
            Policy::Request => size.trim(),
            Policy::Response => size.trim().split(';').next().unwrap_or(""),
        };
        let length = usize::from_str_radix(size, 16).map_err(io::Error::other)?;
        if length == 0 {
            if policy == Policy::Request && line(reader, 8192, policy)? != "\r\n" {
                return Err(io::Error::other("trailers unsupported"));
            }
            return Ok(body);
        }
        append_exact(reader, &mut body, length, limit)?;
        let mut ending = [0u8; 2];
        reader.read_exact(&mut ending)?;
        if ending != *b"\r\n" {
            return Err(io::Error::other("invalid HTTP chunk ending"));
        }
    }
}

pub(crate) fn to_end(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    reader.take((limit + 1) as u64).read_to_end(&mut body)?;
    if body.len() > limit {
        return Err(io::Error::other("HTTP body too large"));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn header_policies_preserve_duplicate_and_line_ending_rules() {
        let duplicate = b"X-Value: first\r\nx-value: last\r\n\r\n";
        assert!(headers(&mut Cursor::new(duplicate), 0, Policy::Request).is_err());
        assert_eq!(
            headers(&mut Cursor::new(duplicate), 0, Policy::Response).unwrap()["x-value"],
            "last"
        );
        let bare_lf = b"X-Value: value\n\r\n";
        assert!(headers(&mut Cursor::new(bare_lf), 0, Policy::Request).is_err());
        assert_eq!(
            headers(&mut Cursor::new(bare_lf), 0, Policy::Response).unwrap()["x-value"],
            "value"
        );
        for policy in [Policy::Request, Policy::Response] {
            for invalid in [
                b"X: \xff\r\n\r\n".as_slice(),
                b"X: value\r\n",
                b"invalid\r\n\r\n",
            ] {
                assert!(headers(&mut Cursor::new(invalid), 0, policy).is_err());
            }
        }
    }

    #[test]
    fn headers_bound_each_line_and_the_complete_block() {
        for policy in [Policy::Request, Policy::Response] {
            let maximum_line = format!("X: {}\r\n\r\n", "a".repeat(8187));
            assert!(headers(&mut Cursor::new(maximum_line), 16, policy).is_ok());
            let oversized_line = format!("X: {}\r\n\r\n", "a".repeat(8188));
            assert!(headers(&mut Cursor::new(oversized_line), 16, policy).is_err());
            let mut block = String::new();
            for index in 0..7 {
                block += &format!("X-{index}: {}\r\n", "a".repeat(8185));
            }
            let maximum_block = format!("{block}Tail: {}\r\n\r\n", "a".repeat(8166));
            assert!(headers(&mut Cursor::new(maximum_block), 16, policy).is_ok());
            let oversized_block = format!("{block}Tail: {}\r\n\r\n", "a".repeat(8167));
            assert!(headers(&mut Cursor::new(oversized_block), 16, policy).is_err());
        }
    }

    #[test]
    fn chunked_bodies_enforce_bounds_and_distinct_extension_and_trailer_rules() {
        for policy in [Policy::Request, Policy::Response] {
            let complete = b"2\r\nab\r\n2\r\ncd\r\n0\r\n\r\n";
            assert_eq!(
                chunked(&mut Cursor::new(complete), 4, policy).unwrap(),
                b"abcd"
            );
            assert!(chunked(&mut Cursor::new(complete), 3, policy).is_err());
            for invalid in [
                b"4\r\nabc".as_slice(),
                b"1\r\na",
                b"1\r\naXX0\r\n",
                b"zz\r\n",
            ] {
                assert!(chunked(&mut Cursor::new(invalid), 4, policy).is_err());
            }
        }
        let extension = b"2;name=value\r\nok\r\n0\r\n\r\n";
        assert!(chunked(&mut Cursor::new(extension), 2, Policy::Request).is_err());
        assert_eq!(
            chunked(&mut Cursor::new(extension), 2, Policy::Response).unwrap(),
            b"ok"
        );
        let trailer = b"0\r\nX-Trailer: value\r\n\r\n";
        assert!(chunked(&mut Cursor::new(trailer), 0, Policy::Request).is_err());
        let mut response = Cursor::new(trailer);
        assert!(
            chunked(&mut response, 0, Policy::Response)
                .unwrap()
                .is_empty()
        );
        assert_eq!(response.fill_buf().unwrap(), b"X-Trailer: value\r\n\r\n");
    }

    #[test]
    fn fixed_and_eof_bodies_enforce_limits_and_detect_truncation() {
        let mut reader = Cursor::new(b"datafollowing");
        assert_eq!(exact(&mut reader, 4, 4).unwrap(), b"data");
        assert_eq!(reader.fill_buf().unwrap(), b"following");
        assert!(exact(&mut Cursor::new(b"data"), 5, 4).is_err());
        assert_eq!(
            exact(&mut Cursor::new(b"data"), 5, 5).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(to_end(&mut Cursor::new(b"data"), 4).unwrap(), b"data");
        assert!(to_end(&mut Cursor::new(b"extra"), 4).is_err());
    }
}
