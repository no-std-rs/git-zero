use super::{allocations::without_allocations, http};
use git_zero::Error;
use git_zero_rustix::{
    ChunkedLimits, RequestDeadlines, RequestPolicy, read_request_chunked, read_request_from,
    read_request_from_chunked,
};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn wire(body: &[u8], width: usize, encoding: Option<&str>) -> Vec<u8> {
    let mut wire = b"POST /repo.git/git-upload-pack HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Type: application/x-git-upload-pack-request\r\nGit-Protocol: version=2\r\n".to_vec();
    if let Some(encoding) = encoding {
        wire.extend_from_slice(format!("Content-Encoding: {encoding}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    for chunk in body.chunks(width) {
        wire.extend_from_slice(format!("{:X}\r\n", chunk.len()).as_bytes());
        wire.extend_from_slice(chunk);
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"0\r\n\r\n");
    wire
}

fn parse(
    bytes: &[u8],
    width: usize,
    storage: &mut [u8],
    policy: RequestPolicy,
    chunks: ChunkedLimits,
) -> Result<http::Parsed, Error> {
    let mut position = 0;
    without_allocations(|| {
        read_request_from_chunked(
            &mut |output| {
                let count = output.len().min(width).min(bytes.len() - position);
                output[..count].copy_from_slice(&bytes[position..position + count]);
                position += count;
                Ok(count)
            },
            storage,
            policy,
            chunks,
        )
    })
    .map(http::Parsed::from)
}

#[test]
fn chunk_and_transport_boundaries_preserve_binary_body_and_git_metadata_without_allocation() {
    let body = b"0000\0\xff\r\n\r\nbinary request";
    for chunk in [1, 2, 7, 32, 1024] {
        let wire = wire(body, chunk, None);
        for width in [1, 2, 3, 7, 31, 4096] {
            let parsed = parse(
                &wire,
                width,
                &mut [0; 1024],
                RequestPolicy::default(),
                ChunkedLimits::default(),
            )
            .unwrap();
            assert_eq!(parsed.body, body);
            assert_eq!(parsed.method, "POST");
            assert_eq!(parsed.path, "/repo.git/git-upload-pack");
            assert_eq!(
                parsed.content_type.as_deref(),
                Some("application/x-git-upload-pack-request")
            );
            assert_eq!(parsed.git_protocol.as_deref(), Some("version=2"));
        }
    }
}

#[test]
fn wire_decoded_body_chunk_count_line_and_capacity_limits_are_inclusive_and_independent() {
    let wire = wire(b"1234567890", 3, None);
    let mut policy = RequestPolicy::default();
    policy.request.max_wire_bytes = Some(wire.len());
    policy.request.max_body_bytes = Some(10);
    let chunks = ChunkedLimits {
        max_chunks: 4,
        max_line_bytes: 3,
    };
    for width in [1, 7, wire.len()] {
        assert_eq!(
            parse(&wire, width, &mut vec![0; wire.len()], policy, chunks)
                .unwrap()
                .body,
            b"1234567890"
        );
        let mut small_wire = policy;
        small_wire.request.max_wire_bytes = Some(wire.len() - 1);
        assert_eq!(
            parse(&wire, width, &mut [0; 1024], small_wire, chunks),
            Err(Error::LimitExceeded)
        );
        let mut small_body = policy;
        small_body.request.max_body_bytes = Some(9);
        assert_eq!(
            parse(&wire, width, &mut [0; 1024], small_body, chunks),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            parse(
                &wire,
                width,
                &mut [0; 1024],
                policy,
                ChunkedLimits {
                    max_chunks: 3,
                    ..chunks
                }
            ),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            parse(
                &wire,
                width,
                &mut [0; 1024],
                policy,
                ChunkedLimits {
                    max_line_bytes: 2,
                    ..chunks
                }
            ),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            parse(
                &wire,
                width,
                &mut vec![0; wire.len() - 1],
                RequestPolicy::default(),
                chunks
            ),
            Err(Error::BufferTooSmall)
        );
    }
    let empty = wire_for(b"0\r\n\r\n");
    let mut zero_body = RequestPolicy::default();
    zero_body.request.max_body_bytes = Some(0);
    assert_eq!(
        parse(
            &empty,
            1,
            &mut [0; 512],
            zero_body,
            ChunkedLimits {
                max_chunks: 0,
                max_line_bytes: 3
            }
        )
        .unwrap()
        .body,
        b""
    );
}

fn wire_for(body: &[u8]) -> Vec<u8> {
    let mut bytes = b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn malformed_chunks_extensions_trailers_and_ambiguous_framing_fail_and_storage_recovers() {
    let cases: &[(&[u8], Error)] = &[
        (b"\r\n", Error::InvalidRequest),
        (b"+1\r\na\r\n0\r\n\r\n", Error::InvalidRequest),
        (b" 1\r\na\r\n0\r\n\r\n", Error::InvalidRequest),
        (b"x\r\n", Error::InvalidRequest),
        (b"1\na\r\n0\r\n\r\n", Error::InvalidRequest),
        (b"1\r\naX\n0\r\n\r\n", Error::InvalidRequest),
        (b"1\r\n", Error::InvalidRequest),
        (b"1\r\na\r\n", Error::InvalidRequest),
        (b"0\r\n", Error::InvalidRequest),
        (b"0\r\n\rX", Error::InvalidRequest),
        (b"1;ext=value\r\na\r\n0\r\n\r\n", Error::Unsupported),
        (b"0\r\nX-Trailer: ignored\r\n\r\n", Error::Unsupported),
        (b"0\r\nContent-Length: 9\r\n\r\n", Error::Unsupported),
        (
            b"0\r\n\r\nGET /next HTTP/1.1\r\n\r\n",
            Error::InvalidRequest,
        ),
        (b"fffffffffffffffff\r\n", Error::InvalidRequest),
    ];
    let valid = wire_for(b"1\r\na\r\n0\r\n\r\n");
    let mut storage = [0; 1024];
    for &(body, expected) in cases {
        let wire = wire_for(body);
        // Pipeline bytes are rejected when present in the retained input. The
        // one-request host closes after the terminator, so later bytes are never
        // interpreted as another request, independent of transport fragmentation.
        let widths: &[usize] = if body.starts_with(b"0\r\n\r\nGET") {
            &[1024]
        } else {
            &[1, 7, 1024]
        };
        for &width in widths {
            assert_eq!(
                parse(
                    &wire,
                    width,
                    &mut storage,
                    RequestPolicy::default(),
                    ChunkedLimits::default()
                ),
                Err(expected),
                "wire={wire:?}, width={width}"
            );
            assert_eq!(
                parse(
                    &valid,
                    width,
                    &mut storage,
                    RequestPolicy::default(),
                    ChunkedLimits::default()
                )
                .unwrap()
                .body,
                b"a"
            );
        }
    }
    for (headers, expected) in [
        (
            "Content-Length: 0\r\nTransfer-Encoding: chunked\r\n",
            Error::InvalidRequest,
        ),
        (
            "Transfer-Encoding: chunked\r\nContent-Length: 0\r\n",
            Error::InvalidRequest,
        ),
        (
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n",
            Error::InvalidRequest,
        ),
        ("Transfer-Encoding: gzip, chunked\r\n", Error::Unsupported),
        (
            "Transfer-Encoding: chunked, identity\r\n",
            Error::Unsupported,
        ),
        ("Transfer-Encoding: identity\r\n", Error::Unsupported),
    ] {
        let bytes = format!("POST / HTTP/1.1\r\n{headers}\r\n0\r\n\r\n");
        assert_eq!(
            parse(
                bytes.as_bytes(),
                1,
                &mut storage,
                RequestPolicy::default(),
                ChunkedLimits::default()
            ),
            Err(expected)
        );
    }
    let old = b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
    assert_eq!(
        parse(
            old,
            1,
            &mut storage,
            RequestPolicy::default(),
            ChunkedLimits::default()
        ),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn gzip_after_transfer_decoding_enforces_decoded_limits_and_integrity() {
    let body = b"Git request body\0\xff with multiple transfer chunks";
    let mut process = Command::new("python3").args(["-c", "import gzip,sys;sys.stdout.buffer.write(gzip.compress(sys.stdin.buffer.read(),mtime=0))"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    process.stdin.take().unwrap().write_all(body).unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(output.status.success());
    for chunk in [1, 7, 1024] {
        let mut wire = wire(&output.stdout, chunk, Some("GZip"));
        let mut policy = RequestPolicy::default();
        policy.request.max_body_bytes = Some(body.len());
        assert_eq!(
            parse(&wire, 1, &mut [0; 4096], policy, ChunkedLimits::default())
                .unwrap()
                .body,
            body
        );
        policy.request.max_body_bytes = Some(body.len() - 1);
        assert_eq!(
            parse(&wire, 3, &mut [0; 4096], policy, ChunkedLimits::default()),
            Err(Error::LimitExceeded)
        );
        // Corrupt the first gzip byte without changing chunk framing.
        let start = wire
            .windows(4)
            .position(|value| value == b"\r\n\r\n")
            .unwrap()
            + 4;
        let offset = wire[start..]
            .windows(2)
            .position(|value| value == b"\r\n")
            .unwrap()
            + start
            + 2;
        wire[offset] ^= 1;
        assert_eq!(
            parse(
                &wire,
                7,
                &mut [0; 4096],
                RequestPolicy::default(),
                ChunkedLimits::default()
            ),
            Err(Error::InvalidZlib)
        );
    }
}

#[test]
fn socket_entry_uses_the_same_framing_with_and_without_deadlines() {
    let bytes = wire(b"0000\0\xffrequest", 3, None);
    for deadlines in [
        RequestDeadlines::default(),
        RequestDeadlines {
            total: Some(std::time::Duration::from_secs(1)),
            idle: None,
        },
    ] {
        for width in [1, 7, bytes.len()] {
            let parsed = http::receive_using(&bytes, width, &mut [0; 1024], |socket, storage| {
                without_allocations(|| {
                    read_request_chunked(
                        socket,
                        storage,
                        RequestPolicy::default(),
                        deadlines,
                        ChunkedLimits::default(),
                    )
                })
            })
            .unwrap();
            assert_eq!(parsed.body, b"0000\0\xffrequest");
        }
    }
}

#[test]
fn host_transport_boundary_bounds_callback_counts_and_preserves_legacy_default() {
    assert_eq!(
        read_request_from(
            &mut |bytes| Ok(bytes.len() + 1),
            &mut [0; 256],
            RequestPolicy::default()
        )
        .err()
        .unwrap(),
        Error::Backend
    );
    let bytes = wire(b"0000", 1, None);
    let mut position = 0;
    let mut storage = [0; 1024];
    let result = read_request_from(
        &mut |output| {
            let count = output.len().min(bytes.len() - position);
            output[..count].copy_from_slice(&bytes[position..position + count]);
            position += count;
            Ok(count)
        },
        &mut storage,
        RequestPolicy::default(),
    );
    assert_eq!(result.err().unwrap(), Error::Unsupported);
}
