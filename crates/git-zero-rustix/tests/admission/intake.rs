use super::{allocations::without_allocations, http};
use git_zero::{Error, io::Write as _};
use git_zero_rustix::{RequestLimits, SocketWriter, read_request_with_limits};
use rustix::{fd::AsFd, net};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn receive(
    wire: &[u8],
    width: usize,
    storage: &mut [u8],
    limits: RequestLimits,
) -> Result<http::Parsed, Error> {
    http::receive_using(wire, width, storage, |socket, storage| {
        without_allocations(|| read_request_with_limits(socket, storage, limits))
    })
}

fn request(body: &[u8], encoding: Option<&str>) -> Vec<u8> {
    let mut wire = format!("POST /repo.git/git-upload-pack?service=git-upload-pack HTTP/1.1\r\nContent-Length: {}\r\nContent-Type: application/x-git-upload-pack-request\r\nGit-Protocol: version=2\r\n", body.len()).into_bytes();
    if let Some(encoding) = encoding {
        wire.extend_from_slice(format!("Content-Encoding: {encoding}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(body);
    wire
}

fn quotas(wire: Option<usize>, body: Option<usize>) -> RequestLimits {
    RequestLimits {
        max_wire_bytes: wire,
        max_body_bytes: body,
    }
}

#[test]
fn unset_policy_preserves_default_results_and_borrowed_binary_metadata() {
    assert_eq!(RequestLimits::default(), quotas(None, None));
    let valid = request(b"0000\0\xff\r\n\r\npayload", Some("IdEnTiTy"));
    let cases: &[&[u8]] = &[
        &valid,
        b"GET /HEAD HTTP/1.0\r\n\r\n",
        b"GET / HTTP/1.0\r\n",
        b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc",
        b"POST / HTTP/1.0\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
        b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"POST / HTTP/1.0\r\nContent-Encoding: br\r\n\r\n",
    ];
    for wire in cases {
        for width in [1, 3, 31, wire.len()] {
            assert_eq!(
                receive(wire, width, &mut [0; 1024], RequestLimits::default()),
                http::receive(wire, width, &mut [0; 1024])
            );
        }
    }
    let parsed = receive(
        &valid,
        1,
        &mut [0; 1024],
        quotas(Some(valid.len()), Some(19)),
    )
    .unwrap();
    assert_eq!(parsed.method, "POST");
    assert_eq!(parsed.path, "/repo.git/git-upload-pack");
    assert_eq!(parsed.query.as_deref(), Some("service=git-upload-pack"));
    assert_eq!(
        parsed.content_type.as_deref(),
        Some("application/x-git-upload-pack-request")
    );
    assert_eq!(parsed.git_protocol.as_deref(), Some("version=2"));
    assert_eq!(parsed.body, b"0000\0\xff\r\n\r\npayload");
}

#[test]
fn inclusive_wire_and_identity_quotas_distinguish_physical_capacity() {
    let body = b"\0\xffbody";
    for encoding in [None, Some("identity"), Some("IDENTITY")] {
        let wire = request(body, encoding);
        for width in [1, 7, wire.len()] {
            for extra in [0, 1] {
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut vec![0; wire.len()],
                        quotas(Some(wire.len() + extra), Some(body.len() + extra))
                    )
                    .unwrap()
                    .body,
                    body
                );
            }
            assert_eq!(
                receive(
                    &wire,
                    width,
                    &mut [0; 1024],
                    quotas(Some(wire.len() - 1), None)
                ),
                Err(Error::LimitExceeded)
            );
            assert_eq!(
                receive(
                    &wire,
                    width,
                    &mut [0; 1024],
                    quotas(None, Some(body.len() - 1))
                ),
                Err(Error::LimitExceeded)
            );
            // A known body quota rejection precedes the otherwise insufficient physical body storage.
            let header = wire.len() - body.len();
            assert_eq!(
                receive(
                    &wire,
                    width,
                    &mut vec![0; header],
                    quotas(None, Some(body.len() - 1))
                ),
                Err(Error::LimitExceeded)
            );
            assert_eq!(
                receive(&wire, width, &mut vec![0; header], quotas(None, None)),
                Err(Error::BufferTooSmall)
            );
        }
    }
    let header = b"GET /HEAD HTTP/1.0\r\n\r\n";
    for width in [1, 4, header.len()] {
        assert!(
            receive(
                header,
                width,
                &mut vec![0; header.len()],
                quotas(Some(header.len()), Some(0))
            )
            .is_ok()
        );
        assert_eq!(
            receive(
                header,
                width,
                &mut [0; 256],
                quotas(Some(header.len() - 1), None)
            ),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            receive(
                header,
                width,
                &mut vec![0; header.len() - 1],
                quotas(Some(header.len()), None)
            ),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(
            receive(
                header,
                width,
                &mut vec![0; header.len() - 1],
                quotas(Some(header.len() - 1), None)
            ),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            receive(header, width, &mut [], quotas(Some(0), None)),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            receive(header, width, &mut [], quotas(None, None)),
            Err(Error::BufferTooSmall)
        );
    }
    assert_eq!(
        receive(
            &request(b"x", None),
            1,
            &mut [0; 512],
            quotas(None, Some(0))
        ),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn malformed_truncated_and_overflow_cases_preserve_error_order_and_recovery() {
    let max = format!("POST / HTTP/1.0\r\nContent-Length: {}\r\n\r\n", usize::MAX);
    let overflow = format!("POST / HTTP/1.0\r\nContent-Length: {}0\r\n\r\n", usize::MAX);
    let cases: &[(&[u8], RequestLimits, Error)] = &[
        (max.as_bytes(), quotas(None, None), Error::BufferTooSmall),
        (
            max.as_bytes(),
            quotas(Some(usize::MAX), None),
            Error::LimitExceeded,
        ),
        (
            overflow.as_bytes(),
            quotas(None, None),
            Error::InvalidRequest,
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Length: -1\r\n\r\n",
            quotas(None, Some(0)),
            Error::InvalidRequest,
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Length: 999\r\nContent-Length: 999\r\n\r\n",
            quotas(None, Some(0)),
            Error::InvalidRequest,
        ),
        (
            b"POST / HTTP/1.0\r\nTransfer-Encoding: chunked\r\nContent-Length: 999\r\n\r\n",
            quotas(None, Some(0)),
            Error::Unsupported,
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc",
            quotas(None, Some(4)),
            Error::InvalidRequest,
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc",
            quotas(None, Some(3)),
            Error::LimitExceeded,
        ),
        (
            b"GET / HTTP/1.0\r\n",
            quotas(Some(100), None),
            Error::InvalidRequest,
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Encoding: br\r\nContent-Length: 4\r\n\r\nabc",
            quotas(None, Some(0)),
            Error::InvalidRequest,
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Encoding: br\r\nContent-Length: 4\r\n\r\nabcd",
            quotas(None, Some(0)),
            Error::Unsupported,
        ),
    ];
    let valid = request(b"0000", None);
    let mut storage = [0xa5; 1024];
    for (wire, limits, error) in cases {
        for width in [1, 7, wire.len()] {
            assert_eq!(
                receive(wire, width, &mut storage, *limits),
                Err(*error),
                "wire={wire:?}"
            );
            assert_eq!(
                receive(
                    &valid,
                    width,
                    &mut storage,
                    quotas(Some(valid.len()), Some(4))
                )
                .unwrap()
                .body,
                b"0000"
            );
        }
    }
}

#[test]
fn header_policy_never_receives_beyond_its_wire_quota() {
    let wire = b"GET /HEAD HTTP/1.0\r\n\r\nexcess";
    for quota in [0, 1, 19, 21] {
        let (reader, writer) = net::socketpair(
            net::AddressFamily::UNIX,
            net::SocketType::STREAM,
            net::SocketFlags::empty(),
            None,
        )
        .unwrap();
        SocketWriter(writer.as_fd()).write_all(wire).unwrap();
        let mut storage = [0xa5; 128];
        assert!(matches!(
            without_allocations(|| read_request_with_limits(
                &reader,
                &mut storage,
                quotas(Some(quota), None)
            )),
            Err(Error::LimitExceeded)
        ));
        assert!(storage[quota..].iter().all(|&byte| byte == 0xa5));
        let mut remaining = [0; 128];
        let (count, _) = net::recv(&reader, &mut remaining, net::RecvFlags::DONTWAIT).unwrap();
        assert_eq!(&remaining[..count], &wire[quota..]);
    }
}

fn gzip(body: &[u8]) -> Vec<u8> {
    let mut child = Command::new("python3").args(["-c", "import gzip,sys; sys.stdout.buffer.write(gzip.compress(sys.stdin.buffer.read(),mtime=0))"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(body).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    output.stdout
}

#[test]
fn gzip_exact_output_quota_and_physical_capacity_are_independent() {
    for body in [Vec::new(), b"\0\xffbinary\r\n".to_vec(), vec![b'z'; 8192]] {
        let compressed = gzip(&body);
        let wire = request(&compressed, Some("gzip"));
        for width in [1, 7, wire.len()] {
            for extra in [0, 1, 37] {
                let mut storage = vec![0xa5; wire.len() + body.len() + extra];
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut storage,
                        quotas(Some(wire.len()), Some(body.len()))
                    )
                    .unwrap()
                    .body,
                    body
                );
                assert!(
                    storage[wire.len() + body.len()..]
                        .iter()
                        .all(|&byte| byte == 0xa5)
                );
            }
            if !body.is_empty() {
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut vec![0; wire.len() + body.len() + 17],
                        quotas(None, Some(body.len() - 1))
                    ),
                    Err(Error::LimitExceeded)
                );
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut vec![0; wire.len() + body.len() - 1],
                        quotas(None, Some(body.len()))
                    ),
                    Err(Error::BufferTooSmall)
                );
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut vec![0; wire.len() + body.len() - 1],
                        quotas(None, Some(body.len() - 1))
                    ),
                    Err(Error::LimitExceeded)
                );
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut vec![0; wire.len() + body.len() - 1],
                        quotas(None, None)
                    ),
                    Err(Error::BufferTooSmall)
                );
            }
            assert_eq!(
                receive(
                    &wire,
                    width,
                    &mut vec![0; wire.len() + body.len()],
                    quotas(Some(wire.len() - 1), Some(body.len()))
                ),
                Err(Error::LimitExceeded)
            );
        }
    }
}

#[test]
fn malformed_gzip_at_exact_and_zero_quotas_keeps_checksum_errors() {
    for body in [b"".as_slice(), b"checksum at quota".as_slice()] {
        let valid = gzip(body);
        let mut crc = valid.clone();
        let tail = crc.len() - 8;
        crc[tail] ^= 1;
        let mut size = valid.clone();
        let tail = size.len() - 4;
        size[tail] ^= 1;
        let mut header = valid.clone();
        header[2] = 0;
        let mut deflate = valid.clone();
        deflate[10] = 0x07; // Reserved DEFLATE block type.
        let mut trailing = valid.clone();
        trailing.push(0);
        let mut concatenated = valid.clone();
        concatenated.extend_from_slice(&valid);
        for bad in [
            crc,
            size,
            header,
            deflate,
            trailing,
            concatenated,
            valid[..valid.len() - 1].to_vec(),
        ] {
            let wire = request(&bad, Some("gzip"));
            for width in [1, wire.len()] {
                for spare in [0, 19] {
                    assert_eq!(
                        receive(
                            &wire,
                            width,
                            &mut vec![0; wire.len() + body.len() + spare],
                            quotas(Some(wire.len()), Some(body.len()))
                        ),
                        Err(Error::InvalidZlib)
                    );
                }
                assert_eq!(
                    receive(
                        &wire,
                        width,
                        &mut vec![0; wire.len() + body.len()],
                        quotas(None, None)
                    ),
                    Err(Error::InvalidZlib)
                );
            }
        }
    }
}
