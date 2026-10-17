use super::allocations::without_allocations;
use git_zero::{
    Error, File, Repository, Snapshot,
    http::{Request, stream::ResponseEncoder},
};
use git_zero_rustix::{
    AcceptedSocketTimeouts, RequestLimits, accept, accept_with_timeouts, listen, listen_on,
    local_address, read_request_with_limits, respond_stream,
};
use rustix::{
    fd::AsFd,
    io::{FdFlags, fcntl_getfd},
    net::sockopt::{self, Timeout},
};
use std::{
    io::Write,
    net::{Shutdown, TcpStream},
    sync::mpsc,
    thread,
    time::Duration,
};

#[test]
fn accepted_settings_are_independent_and_leave_both_ip_listeners_unchanged() {
    let defaults = AcceptedSocketTimeouts::default();
    assert_eq!(defaults.read, Some(Duration::from_secs(10)));
    assert_eq!(defaults.write, Some(Duration::from_secs(10)));
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let listener = listen_on(address.parse().unwrap()).unwrap();
        sockopt::set_socket_timeout(&listener, Timeout::Recv, Some(Duration::from_secs(4)))
            .unwrap();
        sockopt::set_socket_timeout(&listener, Timeout::Send, Some(Duration::from_secs(5)))
            .unwrap();
        for settings in [
            defaults,
            AcceptedSocketTimeouts {
                read: Some(Duration::from_secs(1)),
                write: Some(Duration::from_secs(2)),
            },
            AcceptedSocketTimeouts {
                read: None,
                write: Some(Duration::from_secs(2)),
            },
            AcceptedSocketTimeouts {
                read: Some(Duration::from_secs(1)),
                write: None,
            },
            AcceptedSocketTimeouts {
                read: None,
                write: None,
            },
        ] {
            let _peer = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
            let child = without_allocations(|| accept_with_timeouts(&listener, settings)).unwrap();
            assert!(fcntl_getfd(&child).unwrap().contains(FdFlags::CLOEXEC));
            check_socket_timeouts(&child, settings);
            check_socket_timeouts(
                &listener,
                AcceptedSocketTimeouts {
                    read: Some(Duration::from_secs(4)),
                    write: Some(Duration::from_secs(5)),
                },
            );
        }
    }
}

fn check_socket_timeouts(socket: &impl AsFd, expected: AcceptedSocketTimeouts) {
    for (option, value) in [
        (Timeout::Recv, expected.read),
        (Timeout::Send, expected.write),
    ] {
        assert_eq!(sockopt::socket_timeout(socket, option).unwrap(), value);
    }
}

#[test]
fn zero_timeouts_reject_before_consuming_queued_connections() {
    let listener = listen(0).unwrap();
    for settings in [
        AcceptedSocketTimeouts {
            read: Some(Duration::ZERO),
            ..Default::default()
        },
        AcceptedSocketTimeouts {
            write: Some(Duration::ZERO),
            ..Default::default()
        },
    ] {
        let mut peer = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
        peer.write_all(b"GET /HEAD HTTP/1.0\r\n\r\n").unwrap();
        assert!(matches!(
            without_allocations(|| accept_with_timeouts(&listener, settings)),
            Err(Error::InvalidRequest)
        ));
        let child = without_allocations(|| accept(&listener)).unwrap();
        let mut storage = [0; 128];
        let request = without_allocations(|| {
            read_request_with_limits(&child, &mut storage, RequestLimits::default())
        })
        .unwrap();
        assert_eq!(request.path, "/HEAD");
    }
}

// A broken timeout implementation must release the blocking operation and join
// rather than hang the suite. The assertion distinguishes timeout from that EOF.
fn bounded_io<T>(peer: TcpStream, operation: impl FnOnce() -> T) -> T {
    let (done, wait) = mpsc::channel();
    let task = thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(3)).is_err() {
            let _ = peer.shutdown(Shutdown::Both);
            true
        } else {
            false
        }
    });
    let result = operation();
    let _ = done.send(());
    assert!(
        !task.join().unwrap(),
        "outer watchdog interrupted unbounded I/O"
    );
    result
}

#[test]
fn incomplete_request_stalls_return_backend_under_the_child_read_timeout() {
    let listener = listen(0).unwrap();
    for prefix in [
        b"GET /HEAD HTTP/1.0\r\n".as_slice(),
        b"POST / HTTP/1.0\r\nContent-Length: 4\r\n\r\nabc".as_slice(),
    ] {
        let mut peer = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
        peer.write_all(prefix).unwrap();
        let child = accept_with_timeouts(
            &listener,
            AcceptedSocketTimeouts {
                read: Some(Duration::from_millis(100)),
                ..Default::default()
            },
        )
        .unwrap();
        let result = bounded_io(peer, || {
            without_allocations(|| {
                read_request_with_limits(&child, &mut [0; 256], RequestLimits::default())
                    .map(|_| ())
            })
        });
        assert_eq!(result, Err(Error::Backend));
    }
}

#[test]
fn write_backpressure_times_out_and_cancels_the_stream() {
    let listener = listen(0).unwrap();
    let peer = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
    let child = accept_with_timeouts(
        &listener,
        AcceptedSocketTimeouts {
            write: Some(Duration::from_millis(100)),
            ..Default::default()
        },
    )
    .unwrap();
    sockopt::set_socket_send_buffer_size(&child, 4096).unwrap();
    let body = vec![0x5a; 8_388_608];
    let files = [File {
        name: "large.bin",
        contents: &body,
    }];
    let mut tree = [0; 128];
    let mut commit = [0; 256];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let id = repository.object_id(2).unwrap().to_string();
    let path = format!("objects/{}/{}", &id[..2], &id[2..]);
    let request = Request {
        method: "GET",
        path: &path,
        query: None,
        content_type: None,
        git_protocol: None,
        body: &[],
    };
    let mut encoder = ResponseEncoder::new(&repository, request, &mut []).unwrap();
    let result = bounded_io(peer, || {
        without_allocations(|| respond_stream(&child, &repository, &mut encoder, &mut [0; 1024]))
    });
    assert_eq!(result, Err(Error::Backend));
    assert_eq!(
        encoder.read(&repository, &mut [0; 8]),
        Err(Error::InvalidRequest)
    );
}
