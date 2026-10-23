use super::allocations::without_allocations;
use git_zero::Error;
use git_zero_rustix::{
    AcceptedSocketTimeouts, RequestDeadlines, RequestPolicy, accept_with_timeouts, listen_on,
    local_address, read_request_with_deadlines,
};
use rustix::{
    fd::AsFd,
    io::{FdFlags, fcntl_getfd, fcntl_setfd},
    net::sockopt::{self, Timeout},
};
use std::{
    io::Write,
    net::{Shutdown, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn pair(address: &str, timeouts: AcceptedSocketTimeouts) -> (rustix::fd::OwnedFd, TcpStream) {
    let listener = listen_on(address.parse().unwrap()).unwrap();
    let peer = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
    let child = accept_with_timeouts(&listener, timeouts).unwrap();
    (child, peer)
}

fn disabled() -> AcceptedSocketTimeouts {
    AcceptedSocketTimeouts {
        read: None,
        write: None,
    }
}

// The watchdog makes an accidentally blocking implementation fail within the
// test deadline. It also releases both peers and joins every helper thread.
fn watchdog(
    peer: TcpStream,
    operation: impl FnOnce() -> Result<(), Error>,
) -> (Result<(), Error>, Duration) {
    let (done, wait) = mpsc::channel();
    let task = thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(3)).is_err() {
            peer.shutdown(Shutdown::Both).unwrap();
            true
        } else {
            false
        }
    });
    let start = Instant::now();
    let result = operation();
    let elapsed = start.elapsed();
    done.send(()).ok();
    assert!(
        !task.join().unwrap(),
        "watchdog released unbounded request intake"
    );
    (result, elapsed)
}

#[test]
fn total_deadline_bounds_continuously_progressing_header_and_body_drips_in_both_families() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        for prefix in [
            b"GET /HEAD HTTP/1.0\r\nX-Drip: ".as_slice(),
            b"POST / HTTP/1.0\r\nContent-Length: 1024\r\n\r\n".as_slice(),
        ] {
            let (child, mut peer) = pair(address, disabled());
            peer.write_all(prefix).unwrap();
            let guard = peer.try_clone().unwrap();
            let (stop, stopped) = mpsc::channel();
            let feeder = thread::spawn(move || {
                loop {
                    if stopped.recv_timeout(Duration::from_millis(20)).is_ok() {
                        break;
                    }
                    if peer.write_all(b"a").is_err() {
                        break;
                    }
                }
            });
            let (result, elapsed) = watchdog(guard, || {
                without_allocations(|| {
                    read_request_with_deadlines(
                        &child,
                        &mut [0; 2048],
                        RequestPolicy::default(),
                        RequestDeadlines {
                            total: Some(Duration::from_millis(180)),
                            idle: Some(Duration::from_secs(1)),
                        },
                    )
                    .map(|_| ())
                })
            });
            stop.send(()).ok();
            feeder.join().unwrap();
            assert_eq!(result, Err(Error::LimitExceeded));
            assert!(elapsed >= Duration::from_millis(150));
            assert!(elapsed < Duration::from_secs(2));
        }
    }
}

#[test]
fn idle_deadline_expires_on_incomplete_headers_and_body_without_os_timeouts() {
    for prefix in [
        b"GET /HEAD HTTP/1.0\r\n".as_slice(),
        b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc".as_slice(),
    ] {
        let (child, mut peer) = pair("127.0.0.1:0", disabled());
        peer.write_all(prefix).unwrap();
        let (result, elapsed) = watchdog(peer, || {
            without_allocations(|| {
                read_request_with_deadlines(
                    &child,
                    &mut [0; 256],
                    RequestPolicy::default(),
                    RequestDeadlines {
                        total: Some(Duration::from_secs(2)),
                        idle: Some(Duration::from_millis(100)),
                    },
                )
                .map(|_| ())
            })
        });
        assert_eq!(result, Err(Error::LimitExceeded));
        assert!(elapsed >= Duration::from_millis(80));
        assert!(elapsed < Duration::from_secs(1));
    }
}

#[test]
fn progress_resets_idle_but_never_mutates_descriptor_settings() {
    let timeouts = AcceptedSocketTimeouts {
        read: Some(Duration::from_secs(2)),
        write: Some(Duration::from_secs(3)),
    };
    let (child, mut peer) = pair("127.0.0.1:0", timeouts);
    // fcntl getfd is part of the ownership contract, independent of poll state.
    let flags = fcntl_getfd(&child).unwrap();
    assert!(flags.contains(FdFlags::CLOEXEC));
    fcntl_setfd(&child, flags).unwrap();
    let feeder = thread::spawn(move || {
        for bytes in [
            b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\n".as_slice(),
            b"a",
            b"b",
            b"c",
            b"d",
        ] {
            peer.write_all(bytes).unwrap();
            thread::sleep(Duration::from_millis(40));
        }
    });
    let mut storage = [0; 256];
    let request = without_allocations(|| {
        read_request_with_deadlines(
            &child,
            &mut storage,
            RequestPolicy::default(),
            RequestDeadlines {
                total: Some(Duration::from_secs(2)),
                idle: Some(Duration::from_millis(500)),
            },
        )
    })
    .unwrap();
    assert_eq!(request.body, b"abcd");
    assert_eq!(request.method, "POST");
    assert_eq!(
        sockopt::socket_timeout(&child, Timeout::Recv).unwrap(),
        timeouts.read
    );
    assert_eq!(
        sockopt::socket_timeout(&child, Timeout::Send).unwrap(),
        timeouts.write
    );
    assert_eq!(fcntl_getfd(&child).unwrap(), flags);
    feeder.join().unwrap();
}

#[test]
fn shorter_accepted_socket_timeout_retains_backend_error_and_longer_deadlines_remain_absolute() {
    let (child, peer) = pair(
        "127.0.0.1:0",
        AcceptedSocketTimeouts {
            read: Some(Duration::from_millis(100)),
            write: None,
        },
    );
    let (result, elapsed) = watchdog(peer, || {
        without_allocations(|| {
            read_request_with_deadlines(
                &child,
                &mut [0; 256],
                RequestPolicy::default(),
                RequestDeadlines {
                    total: Some(Duration::from_secs(2)),
                    idle: Some(Duration::from_secs(1)),
                },
            )
            .map(|_| ())
        })
    });
    assert_eq!(result, Err(Error::Backend));
    assert!(elapsed < Duration::from_secs(1));
    assert_eq!(
        sockopt::socket_timeout(&child, Timeout::Recv).unwrap(),
        Some(Duration::from_millis(100))
    );
}

#[test]
fn invalid_deadlines_reject_before_consuming_any_bytes_and_clean_retry_reuses_storage() {
    let (child, mut peer) = pair("127.0.0.1:0", disabled());
    peer.write_all(b"GET /HEAD HTTP/1.0\r\n\r\n").unwrap();
    let mut storage = [0xa7; 256];
    for deadlines in [
        RequestDeadlines {
            total: Some(Duration::ZERO),
            idle: None,
        },
        RequestDeadlines {
            total: None,
            idle: Some(Duration::ZERO),
        },
        RequestDeadlines {
            total: Some(Duration::MAX),
            idle: None,
        },
        RequestDeadlines {
            total: None,
            idle: Some(Duration::MAX),
        },
    ] {
        assert_eq!(
            without_allocations(|| read_request_with_deadlines(
                &child,
                &mut storage,
                RequestPolicy::default(),
                deadlines
            ))
            .err()
            .unwrap(),
            Error::InvalidRequest
        );
        assert!(storage.iter().all(|&byte| byte == 0xa7));
    }
    let request = read_request_with_deadlines(
        &child,
        &mut storage,
        RequestPolicy::default(),
        RequestDeadlines {
            total: Some(Duration::from_secs(1)),
            idle: None,
        },
    )
    .unwrap();
    assert_eq!(request.path, "/HEAD");
}

#[test]
fn eof_syntax_quotas_and_disabled_defaults_preserve_existing_error_contracts() {
    for (wire, expected) in [
        (b"GET /HEAD HTTP/1.0\r\n\r\n".as_slice(), Ok(())),
        (
            b"GET /HEAD HTTP/1.0\r\nX-Bad\r\n\r\n".as_slice(),
            Err(Error::InvalidRequest),
        ),
        (
            b"GET /HEAD HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(),
            Err(Error::Unsupported),
        ),
        (
            b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc".as_slice(),
            Err(Error::InvalidRequest),
        ),
    ] {
        for deadlines in [
            RequestDeadlines::default(),
            RequestDeadlines {
                total: Some(Duration::from_secs(1)),
                idle: Some(Duration::from_secs(1)),
            },
        ] {
            let (child, mut peer) = pair("127.0.0.1:0", disabled());
            peer.write_all(wire).unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
            let result = without_allocations(|| {
                read_request_with_deadlines(
                    &child,
                    &mut [0; 256],
                    RequestPolicy::default(),
                    deadlines,
                )
                .map(|_| ())
            });
            assert_eq!(result, expected);
        }
    }
    let (child, mut peer) = pair("127.0.0.1:0", disabled());
    peer.write_all(b"GET /HEAD HTTP/1.0\r\n\r\n").unwrap();
    let mut policy = RequestPolicy::default();
    policy.headers.max_bytes = Some(5);
    assert_eq!(
        read_request_with_deadlines(
            &child,
            &mut [0; 256],
            policy,
            RequestDeadlines {
                total: Some(Duration::from_secs(1)),
                idle: None
            }
        )
        .err()
        .unwrap(),
        Error::LimitExceeded
    );
}

#[test]
fn timed_out_connection_does_not_poison_an_independent_accepted_connection() {
    let listener = listen_on("127.0.0.1:0".parse().unwrap()).unwrap();
    let _stalled = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
    let stalled = accept_with_timeouts(&listener, disabled()).unwrap();
    let mut good = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
    good.write_all(b"GET /HEAD HTTP/1.0\r\n\r\n").unwrap();
    let good = accept_with_timeouts(&listener, disabled()).unwrap();
    let mut storage = [0; 256];
    let deadlines = RequestDeadlines {
        total: Some(Duration::from_millis(60)),
        idle: None,
    };
    assert_eq!(
        read_request_with_deadlines(&stalled, &mut storage, RequestPolicy::default(), deadlines)
            .err()
            .unwrap(),
        Error::LimitExceeded
    );
    let request =
        read_request_with_deadlines(&good, &mut storage, RequestPolicy::default(), deadlines)
            .unwrap();
    assert_eq!(request.path, "/HEAD");
    // Both settings remain disabled, including after the error return.
    for socket in [stalled.as_fd(), good.as_fd()] {
        assert_eq!(
            sockopt::socket_timeout(socket, Timeout::Recv).unwrap(),
            None
        );
    }
}
