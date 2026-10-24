use super::allocations::without_allocations;
use git_zero::{
    Error, File, ObjectId, ObjectInfo, Repository, Result, Snapshot,
    http::{
        Request,
        dumb::{LooseObjects, Publication},
        stream::ResponseEncoder,
    },
};
use git_zero_rustix::{
    AcceptedSocketTimeouts, ResponseDeadlines, accept_with_timeouts, listen, local_address,
    respond_published_with_deadlines, respond_stream_with_deadlines,
};
use rustix::net::sockopt::{self, Timeout};
use std::{
    cell::Cell,
    io::Read,
    net::{Shutdown, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn pair() -> (rustix::fd::OwnedFd, TcpStream) {
    let listener = listen(0).unwrap();
    let peer = TcpStream::connect(local_address(&listener).unwrap()).unwrap();
    let socket = accept_with_timeouts(
        &listener,
        AcceptedSocketTimeouts {
            read: Some(Duration::from_secs(2)),
            write: None,
        },
    )
    .unwrap();
    sockopt::set_socket_send_buffer_size(&socket, 4096).unwrap();
    (socket, peer)
}

fn with_repository(run: impl FnOnce(&Snapshot<'_>)) {
    let body = vec![0xa7; 4 * 1024 * 1024];
    let files = [File {
        name: "large",
        contents: &body,
    }];
    let mut tree = [0; 256];
    let mut commit = [0; 256];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    run(&repository);
}

fn request(path: &str) -> Request<'_> {
    Request {
        method: "GET",
        path,
        query: None,
        content_type: None,
        git_protocol: None,
        body: &[],
    }
}

fn watchdog(peer: TcpStream, run: impl FnOnce() -> Result<()>) -> Result<()> {
    let (done, wait) = mpsc::channel();
    let task = thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(3)).is_err() {
            peer.shutdown(Shutdown::Both).unwrap();
            true
        } else {
            false
        }
    });
    let result = run();
    done.send(()).ok();
    assert!(
        !task.join().unwrap(),
        "response deadline required watchdog intervention"
    );
    result
}

#[test]
fn idle_backpressure_expires_and_cancels_without_a_socket_write_timeout() {
    with_repository(check_idle_backpressure);
}

#[test]
fn total_response_deadline_survives_continuous_progress_and_slow_provider_reads() {
    with_repository(check_total_response_deadline);
}

#[test]
fn deadline_configuration_failure_cancels_before_headers_and_empty_staging_is_rejected() {
    with_repository(check_invalid_deadlines);
}

#[test]
fn complete_stream_and_known_length_publication_responses_preserve_wire_and_socket_options() {
    with_repository(check_complete_responses);
}

#[test]
fn known_length_pack_backpressure_cancels_its_publication_cursor() {
    with_repository(check_publication_backpressure);
}

#[test]
fn publication_length_mismatch_is_rejected_and_cancels_with_or_without_deadlines() {
    with_repository(check_publication_length_mismatch);
}

fn check_publication_length_mismatch(repository: &Snapshot<'_>) {
    let mut entries = [git_zero::pack::IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        repository,
        &mut entries,
        &mut [0; 4096],
        LooseObjects::Disabled,
    )
    .unwrap();
    for deadlines in [
        ResponseDeadlines::default(),
        ResponseDeadlines {
            total: Some(Duration::from_secs(2)),
            idle: None,
        },
    ] {
        let mut encoder = publication
            .prepare(request("objects/info/packs"))
            .unwrap()
            .unwrap();
        assert_eq!(encoder.read(&mut [0; 1]).unwrap(), 1);
        let (socket, _peer) = pair();
        assert_eq!(
            respond_published_with_deadlines(&socket, &mut encoder, &mut [0; 17], deadlines,),
            Err(Error::Backend)
        );
        assert_eq!(encoder.read(&mut [0; 1]), Err(Error::InvalidRequest));
    }
}

struct Slow<'a, R> {
    repository: &'a R,
    reads: Cell<usize>,
}
impl<R: Repository> Repository for Slow<'_, R> {
    fn head(&self) -> ObjectId {
        self.repository.head()
    }
    fn object_count(&self) -> usize {
        self.repository.object_count()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.repository.object_id(index)
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.repository.object_info(index)
    }
    fn read_object(&self, index: usize, offset: u64, output: &mut [u8]) -> Result<usize> {
        self.reads.set(self.reads.get() + 1);
        thread::sleep(Duration::from_millis(25));
        self.repository.read_object(index, offset, output)
    }
}

fn check_idle_backpressure(repository: &Snapshot<'_>) {
    let (socket, peer) = pair();
    let id = repository.object_id(2).unwrap().to_string();
    let path = format!("objects/{}/{}", &id[..2], &id[2..]);
    let mut encoder = ResponseEncoder::new(repository, request(&path), &mut []).unwrap();
    let start = Instant::now();
    let result = watchdog(peer, || {
        without_allocations(|| {
            respond_stream_with_deadlines(
                &socket,
                repository,
                &mut encoder,
                &mut [0; 1024],
                ResponseDeadlines {
                    total: None,
                    idle: Some(Duration::from_millis(100)),
                },
            )
        })
    });
    assert_eq!(result, Err(Error::LimitExceeded));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(
        encoder.read(repository, &mut [0; 1]),
        Err(Error::InvalidRequest)
    );
    assert_eq!(
        sockopt::socket_timeout(&socket, Timeout::Send).unwrap(),
        None
    );
    assert_eq!(
        sockopt::socket_timeout(&socket, Timeout::Recv).unwrap(),
        Some(Duration::from_secs(2))
    );
}

fn check_total_response_deadline(repository: &Snapshot<'_>) {
    let slow = Slow {
        repository,
        reads: Cell::new(0),
    };
    let (socket, mut peer) = pair();
    let guard = peer.try_clone().unwrap();
    let receiver = thread::spawn(move || {
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let id = repository.object_id(2).unwrap().to_string();
    let path = format!("objects/{}/{}", &id[..2], &id[2..]);
    let mut encoder = ResponseEncoder::new(&slow, request(&path), &mut []).unwrap();
    let result = watchdog(guard, || {
        without_allocations(|| {
            respond_stream_with_deadlines(
                &socket,
                &slow,
                &mut encoder,
                &mut [0; 1024],
                ResponseDeadlines {
                    total: Some(Duration::from_millis(120)),
                    idle: Some(Duration::from_secs(1)),
                },
            )
        })
    });
    assert_eq!(result, Err(Error::LimitExceeded));
    assert!(slow.reads.get() > 1);
    assert_eq!(encoder.read(&slow, &mut [0; 1]), Err(Error::InvalidRequest));
    drop(socket);
    let partial = receiver.join().unwrap();
    assert!(partial.starts_with(b"HTTP/1.0 200"));
    assert!(partial.len() < 4 * 1024 * 1024);
}

fn check_invalid_deadlines(repository: &Snapshot<'_>) {
    for (deadlines, expected) in [
        (
            ResponseDeadlines {
                total: Some(Duration::ZERO),
                idle: None,
            },
            Error::InvalidRequest,
        ),
        (
            ResponseDeadlines {
                total: None,
                idle: Some(Duration::ZERO),
            },
            Error::InvalidRequest,
        ),
        (
            ResponseDeadlines {
                total: Some(Duration::MAX),
                idle: None,
            },
            Error::InvalidRequest,
        ),
    ] {
        let (socket, mut peer) = pair();
        let mut control = [0; 128];
        let mut encoder = ResponseEncoder::new(repository, request("HEAD"), &mut control).unwrap();
        assert_eq!(
            without_allocations(|| respond_stream_with_deadlines(
                &socket,
                repository,
                &mut encoder,
                &mut [0; 128],
                deadlines
            )),
            Err(expected)
        );
        assert_eq!(
            encoder.read(repository, &mut [0; 1]),
            Err(Error::InvalidRequest)
        );
        drop(socket);
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }
    let (socket, _peer) = pair();
    let mut control = [0; 128];
    let mut encoder = ResponseEncoder::new(repository, request("HEAD"), &mut control).unwrap();
    assert_eq!(
        respond_stream_with_deadlines(
            &socket,
            repository,
            &mut encoder,
            &mut [],
            ResponseDeadlines {
                total: Some(Duration::from_secs(1)),
                idle: None
            }
        ),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(
        encoder.read(repository, &mut [0; 1]),
        Err(Error::InvalidRequest)
    );
}

fn check_complete_responses(repository: &Snapshot<'_>) {
    for deadlines in [
        ResponseDeadlines::default(),
        ResponseDeadlines {
            total: Some(Duration::from_secs(2)),
            idle: Some(Duration::from_secs(1)),
        },
    ] {
        let (socket, mut peer) = pair();
        let mut control = [0; 128];
        let mut encoder = ResponseEncoder::new(repository, request("HEAD"), &mut control).unwrap();
        respond_stream_with_deadlines(&socket, repository, &mut encoder, &mut [0; 17], deadlines)
            .unwrap();
        drop(socket);
        let mut bytes = String::new();
        peer.read_to_string(&mut bytes).unwrap();
        assert!(bytes.starts_with("HTTP/1.0 200 Response\r\nContent-Type: text/plain"));
        assert!(bytes.ends_with("\r\n\r\nref: refs/heads/main\n"));
    }
    let mut entries = [git_zero::pack::IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        repository,
        &mut entries,
        &mut [0; 4096],
        LooseObjects::Disabled,
    )
    .unwrap();
    let mut encoder = publication
        .prepare(request("objects/info/packs"))
        .unwrap()
        .unwrap();
    let expected_size = encoder.size();
    let (socket, mut peer) = pair();
    without_allocations(|| {
        respond_published_with_deadlines(
            &socket,
            &mut encoder,
            &mut [0; 17],
            ResponseDeadlines {
                total: Some(Duration::from_secs(2)),
                idle: None,
            },
        )
    })
    .unwrap();
    drop(socket);
    let mut bytes = String::new();
    peer.read_to_string(&mut bytes).unwrap();
    assert!(bytes.contains(&format!("Content-Length: {expected_size}\r\n")));
    assert!(bytes.contains("\r\n\r\nP pack-"));
}

fn check_publication_backpressure(repository: &Snapshot<'_>) {
    let mut entries = [git_zero::pack::IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        repository,
        &mut entries,
        &mut [0; 4096],
        LooseObjects::Disabled,
    )
    .unwrap();
    let path = format!("objects/pack/pack-{}.pack", publication.checksum());
    let mut encoder = publication.prepare(request(&path)).unwrap().unwrap();
    let (socket, peer) = pair();
    assert_eq!(
        watchdog(peer, || without_allocations(|| {
            respond_published_with_deadlines(
                &socket,
                &mut encoder,
                &mut [0; 1024],
                ResponseDeadlines {
                    total: Some(Duration::from_millis(100)),
                    idle: None,
                },
            )
        })),
        Err(Error::LimitExceeded)
    );
    assert_eq!(encoder.read(&mut [0; 1]), Err(Error::InvalidRequest));
}
