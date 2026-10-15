//! Independent wire cases through real sockets and the public native reader.
#[path = "common/http.rs"]
mod http;

use git_zero::Error;
use http::{Parsed, receive};

fn upload(body: &[u8]) -> Vec<u8> {
    let mut wire = format!(
        "POST /repo.git/git-upload-pack?service=git-upload-pack HTTP/1.1\r\nContent-Length: {}\r\nContent-Type: application/x-git-upload-pack-request\r\nGit-Protocol: version=2\r\n\r\n",
        body.len()
    ).into_bytes();
    wire.extend_from_slice(body);
    wire
}

fn expected_upload(body: &[u8]) -> Parsed {
    Parsed {
        method: "POST".into(),
        path: "/repo.git/git-upload-pack".into(),
        query: Some("service=git-upload-pack".into()),
        content_type: Some("application/x-git-upload-pack-request".into()),
        git_protocol: Some("version=2".into()),
        body: body.into(),
    }
}

#[test]
fn identity_coding_is_case_insensitive_and_preserves_git_metadata() {
    for encoding in ["identity", "Identity", "IDENTITY", "iDeNtItY"] {
        let wire = format!(
            "POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: 4\r\nContent-Encoding: {encoding}\r\nContent-Type: application/x-git-upload-pack-request\r\nGit-Protocol: version=2\r\n\r\n0000"
        );
        let mut storage = [0; 1024];
        let request = receive(wire.as_bytes(), wire.len(), &mut storage).unwrap();
        assert_eq!(request.body, b"0000");
        assert_eq!(
            request.content_type.as_deref(),
            Some("application/x-git-upload-pack-request")
        );
        assert_eq!(request.git_protocol.as_deref(), Some("version=2"));
    }
}

#[test]
fn fragmented_uploads_preserve_metadata_and_binary_body_bytes() {
    let body = b"0000\0\xff\r\n\r\npayload";
    let wire = upload(body);
    let mut storage = [0xa5; 512];
    for width in [1, 2, 3, 4, 7, 31, 512] {
        assert_eq!(
            receive(&wire, width, &mut storage),
            Ok(expected_upload(body))
        );
    }
}

#[test]
fn malformed_corpus_rejects_and_recovers_with_reused_caller_storage() {
    let cases: &[(&str, &[u8], Error)] = &[
        ("empty", b"", Error::InvalidRequest),
        (
            "truncated header",
            b"GET / HTTP/1.0\r\n",
            Error::InvalidRequest,
        ),
        (
            "LF only",
            b"GET / HTTP/1.0\nX: a\n\n",
            Error::InvalidRequest,
        ),
        (
            "truncated body",
            b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc",
            Error::InvalidRequest,
        ),
        (
            "duplicate length",
            b"POST / HTTP/1.0\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "negative length",
            b"POST / HTTP/1.0\r\nContent-Length: -1\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "nondecimal length",
            b"POST / HTTP/1.0\r\nContent-Length: 0x1\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "missing colon",
            b"GET / HTTP/1.0\r\nBad header\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "duplicate protocol",
            b"GET / HTTP/1.0\r\nGit-Protocol: version=1\r\nGit-Protocol: version=2\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "unsupported version",
            b"GET / HTTP/2\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "relative target",
            b"GET repo.git/HEAD HTTP/1.0\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "control target",
            b"GET /repo.git/\x7f HTTP/1.0\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "invalid UTF-8",
            b"GET / HTTP/1.0\r\nX: \xff\r\n\r\n",
            Error::InvalidRequest,
        ),
        (
            "transfer encoding",
            b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n",
            Error::Unsupported,
        ),
        (
            "unsupported coding",
            b"POST / HTTP/1.0\r\nContent-Encoding: br\r\n\r\n",
            Error::Unsupported,
        ),
        (
            "invalid gzip",
            b"POST / HTTP/1.0\r\nContent-Length: 4\r\nContent-Encoding: gzip\r\n\r\nnope",
            Error::InvalidZlib,
        ),
    ];
    let body = b"0000\0\xff\r\n\r\nafter rejection";
    let valid = upload(body);
    let mut storage = [0xa5; 512];
    for (name, wire, error) in cases {
        for width in [1, 4, 31] {
            assert_eq!(
                receive(wire, width, &mut storage),
                Err(*error),
                "{name}, width={width}"
            );
            assert_eq!(
                receive(&valid, width, &mut storage),
                Ok(expected_upload(body)),
                "after {name}"
            );
        }
    }
}

#[test]
fn exact_header_capacity_succeeds_and_over_budget_requests_recover() {
    let mut storage = [0xa5; 512];
    let prefix = b"GET /repo.git/HEAD HTTP/1.0\r\nX: ";
    let mut exact = vec![b'a'; storage.len()];
    exact[..prefix.len()].copy_from_slice(prefix);
    exact[508..].copy_from_slice(b"\r\n\r\n");
    let expected = Parsed {
        method: "GET".into(),
        path: "/repo.git/HEAD".into(),
        query: None,
        content_type: None,
        git_protocol: None,
        body: vec![],
    };
    assert_eq!(receive(&exact, 3, &mut storage), Ok(expected));
    let mut over = exact[..508].to_vec();
    over.extend_from_slice(b"a\r\n\r\n");
    let oversized_body = upload(&[0; 512]);
    for wire in [&over[..], &oversized_body[..]] {
        assert_eq!(receive(wire, 7, &mut storage), Err(Error::BufferTooSmall));
        assert_eq!(
            receive(&upload(b"after limit"), 7, &mut storage),
            Ok(expected_upload(b"after limit"))
        );
    }
}
