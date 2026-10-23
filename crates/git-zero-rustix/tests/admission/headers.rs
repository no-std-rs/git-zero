use super::{allocations::without_allocations, http};
use git_zero::{Error, io::Write as _};
use git_zero_rustix::{
    HeaderLimits, RequestLimits, RequestPolicy, SocketWriter, read_request_with_policy,
};
use rustix::{fd::AsFd, net};

fn receive(
    wire: &[u8],
    width: usize,
    capacity: usize,
    policy: RequestPolicy,
) -> Result<http::Parsed, Error> {
    http::receive_using(wire, width, &mut vec![0; capacity], |socket, storage| {
        without_allocations(|| read_request_with_policy(socket, storage, policy))
    })
}
fn policy(headers: HeaderLimits) -> RequestPolicy {
    RequestPolicy {
        request: RequestLimits::default(),
        headers,
    }
}

#[test]
fn exact_header_byte_field_name_and_value_budgets_are_inclusive_under_fragmentation() {
    let wire = b"POST /repo.git/git-upload-pack HTTP/1.1\r\nX-Unknown:  abc\t\r\nX-Unknown: def\r\nContent-Length: 4\r\n\r\n\0\xff\r\n";
    let end = wire.len() - 4;
    let exact = HeaderLimits {
        max_bytes: Some(end),
        max_fields: Some(3),
        max_name_bytes: Some(14),
        max_value_bytes: Some(6),
    };
    for width in [1, 2, 3, 17, wire.len()] {
        let parsed = receive(wire, width, 512, policy(exact)).unwrap();
        assert_eq!(parsed.body, b"\0\xff\r\n");
        for headers in [
            HeaderLimits {
                max_bytes: Some(end - 1),
                ..exact
            },
            HeaderLimits {
                max_fields: Some(2),
                ..exact
            },
            HeaderLimits {
                max_name_bytes: Some(13),
                ..exact
            },
            HeaderLimits {
                max_value_bytes: Some(5),
                ..exact
            },
        ] {
            assert_eq!(
                receive(wire, width, 512, policy(headers)),
                Err(Error::LimitExceeded)
            );
        }
        assert_eq!(
            receive(wire, width, end - 1, policy(exact)),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(
            receive(
                wire,
                width,
                end - 1,
                policy(HeaderLimits {
                    max_bytes: Some(end - 1),
                    ..exact
                })
            ),
            Err(Error::LimitExceeded)
        );
    }
}

#[test]
fn zero_and_unset_header_budgets_preserve_empty_fields_and_count_wire_octets() {
    let empty = b"GET /HEAD HTTP/1.0\r\n\r\n";
    let zero_fields = HeaderLimits {
        max_fields: Some(0),
        max_name_bytes: Some(0),
        max_value_bytes: Some(0),
        ..HeaderLimits::default()
    };
    for width in [1, 17, 512] {
        assert!(receive(empty, width, 512, policy(zero_fields)).is_ok());
        assert_eq!(
            receive(
                empty,
                width,
                512,
                policy(HeaderLimits {
                    max_bytes: Some(0),
                    ..zero_fields
                })
            ),
            Err(Error::LimitExceeded)
        );
        assert!(
            receive(
                b"GET /HEAD HTTP/1.0\r\nX:\r\n\r\n",
                width,
                512,
                policy(HeaderLimits {
                    max_value_bytes: Some(0),
                    ..HeaderLimits::default()
                })
            )
            .is_ok()
        );
        assert_eq!(
            receive(
                b"GET /HEAD HTTP/1.0\r\nX: \r\n\r\n",
                width,
                512,
                policy(HeaderLimits {
                    max_value_bytes: Some(0),
                    ..HeaderLimits::default()
                })
            ),
            Err(Error::LimitExceeded)
        );
        let unicode = "GET /HEAD HTTP/1.0\r\nX:é\r\n\r\n".as_bytes();
        assert!(
            receive(
                unicode,
                width,
                512,
                policy(HeaderLimits {
                    max_value_bytes: Some(2),
                    ..HeaderLimits::default()
                })
            )
            .is_ok()
        );
        assert_eq!(
            receive(
                unicode,
                width,
                512,
                policy(HeaderLimits {
                    max_value_bytes: Some(1),
                    ..HeaderLimits::default()
                })
            ),
            Err(Error::LimitExceeded)
        );
    }
    let many = format!("GET /HEAD HTTP/1.0\r\n{}\r\n", "X:a\r\n".repeat(128));
    assert!(receive(many.as_bytes(), 7, 1024, RequestPolicy::default()).is_ok());
    assert_eq!(
        receive(
            many.as_bytes(),
            7,
            1024,
            policy(HeaderLimits {
                max_fields: Some(127),
                ..HeaderLimits::default()
            })
        ),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn malformed_field_syntax_precedes_its_policy_and_rejected_headers_never_wait_for_the_body() {
    for wire in [
        b"GET /HEAD HTTP/1.0\r\nBad Name: value\r\n\r\n".as_slice(),
        b"GET /HEAD HTTP/1.0\r\n: value\r\n\r\n",
        b"GET /HEAD HTTP/1.0\r\nX: bad\x01value\r\n\r\n",
        b"GET /HEAD HTTP/1.0\r\nno-colon\r\n\r\n",
    ] {
        assert_eq!(
            receive(
                wire,
                1,
                512,
                policy(HeaderLimits {
                    max_fields: Some(0),
                    ..HeaderLimits::default()
                })
            ),
            Err(Error::InvalidRequest)
        );
    }
    let incomplete = b"POST /HEAD HTTP/1.0\r\nX: oversized\r\nContent-Length: 1000\r\n\r\n";
    // There are no body bytes. Field admission must fail rather than continuing
    // into EOF or allocating/inflating a declared body that will never arrive.
    assert_eq!(
        receive(
            incomplete,
            1,
            2048,
            policy(HeaderLimits {
                max_value_bytes: Some(2),
                ..HeaderLimits::default()
            })
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        receive(incomplete, 1, 2048, RequestPolicy::default()),
        Err(Error::InvalidRequest)
    );
    let duplicate = b"POST /HEAD HTTP/1.0\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(
        receive(
            duplicate,
            7,
            512,
            policy(HeaderLimits {
                max_fields: Some(1),
                ..HeaderLimits::default()
            })
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        receive(duplicate, 7, 512, RequestPolicy::default()),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn header_and_whole_wire_caps_stop_without_consuming_the_first_excess_byte() {
    for header_cap in [Some(19), None] {
        let (reader, writer) = net::socketpair(
            net::AddressFamily::UNIX,
            net::SocketType::STREAM,
            net::SocketFlags::empty(),
            None,
        )
        .unwrap();
        let bytes = b"GET /HEAD HTTP/1.0\r\nX: never complete";
        SocketWriter(writer.as_fd()).write_all(bytes).unwrap();
        let selected = RequestPolicy {
            request: RequestLimits {
                max_wire_bytes: Some(19),
                max_body_bytes: None,
            },
            headers: HeaderLimits {
                max_bytes: header_cap,
                ..HeaderLimits::default()
            },
        };
        assert!(matches!(
            without_allocations(
                || read_request_with_policy(&reader, &mut [0; 512], selected).map(|_| ())
            ),
            Err(Error::LimitExceeded)
        ));
        let mut byte = [0];
        let (count, _) = net::recv(&reader, &mut byte, net::RecvFlags::DONTWAIT).unwrap();
        assert_eq!(count, 1);
        assert_eq!(byte[0], bytes[19]);
    }
}

#[test]
fn body_and_header_quotas_are_independent_and_storage_is_reusable_after_rejection() {
    let wire = b"POST /HEAD HTTP/1.0\r\nContent-Length: 4\r\n\r\nbody";
    let headers = HeaderLimits {
        max_bytes: Some(wire.len() - 4),
        max_fields: Some(1),
        max_name_bytes: Some(14),
        max_value_bytes: Some(2),
    };
    for width in [1, 2, 31, 512] {
        let selected = RequestPolicy {
            request: RequestLimits {
                max_wire_bytes: Some(wire.len()),
                max_body_bytes: Some(4),
            },
            headers,
        };
        assert_eq!(receive(wire, width, 512, selected).unwrap().body, b"body");
        assert_eq!(
            receive(
                wire,
                width,
                512,
                RequestPolicy {
                    request: RequestLimits {
                        max_body_bytes: Some(3),
                        ..selected.request
                    },
                    ..selected
                }
            ),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            receive(
                wire,
                width,
                512,
                RequestPolicy {
                    request: RequestLimits {
                        max_wire_bytes: Some(wire.len() - 1),
                        ..selected.request
                    },
                    ..selected
                }
            ),
            Err(Error::LimitExceeded)
        );
    }
    let mut reused = [0xa7; 512];
    for _ in 0..16 {
        assert_eq!(
            http::receive_using(wire, 1, &mut reused, |socket, storage| {
                read_request_with_policy(
                    socket,
                    storage,
                    policy(HeaderLimits {
                        max_fields: Some(0),
                        ..headers
                    }),
                )
            }),
            Err(Error::LimitExceeded)
        );
        let parsed = http::receive_using(wire, 1, &mut reused, |socket, storage| {
            without_allocations(|| {
                read_request_with_policy(socket, storage, RequestPolicy::default())
            })
        })
        .unwrap();
        assert_eq!(parsed.body, b"body");
    }
}
