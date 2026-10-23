#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "../../../tests/common/streamed.rs"]
mod streamed;
use git_zero::{
    Error, Repository,
    http::{
        Request,
        dumb::{LooseObjects, Publication},
    },
    pack::IndexEntry,
};
use git_zero_rustix::respond_published;
use rustix::{
    fd::OwnedFd,
    net::{self, sockopt},
};
use std::{cell::Cell, time::Duration};

fn pair() -> (OwnedFd, OwnedFd) {
    let pair = net::socketpair(
        net::AddressFamily::UNIX,
        net::SocketType::STREAM,
        net::SocketFlags::empty(),
        None,
    )
    .unwrap();
    for socket in [&pair.0, &pair.1] {
        sockopt::set_socket_timeout(socket, sockopt::Timeout::Recv, Some(Duration::from_secs(2)))
            .unwrap();
        sockopt::set_socket_timeout(socket, sockopt::Timeout::Send, Some(Duration::from_secs(2)))
            .unwrap();
    }
    pair
}
fn request(path: &str) -> Request<'_> {
    Request {
        method: "GET",
        path,
        query: None,
        body: b"",
        git_protocol: None,
        content_type: None,
    }
}
fn receive(socket: OwnedFd) -> Vec<u8> {
    let mut result = Vec::new();
    let mut storage = [0; 4096];
    loop {
        let (count, _) = net::recv(&socket, &mut storage, net::RecvFlags::empty()).unwrap();
        if count == 0 {
            return result;
        }
        result.extend_from_slice(&storage[..count]);
    }
}
fn split(bytes: &[u8]) -> (&str, &[u8]) {
    let end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    (std::str::from_utf8(&bytes[..end]).unwrap(), &bytes[end..])
}

#[test]
fn publication_socket_framing_matches_exact_lengths_with_one_byte_staging() {
    let repository = streamed::GeneratedRepository::new(66001, 7);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 17],
        LooseObjects::Disabled,
    )
    .unwrap();
    for extension in ["pack", "idx"] {
        let target = format!("objects/pack/pack-{}.{extension}", publication.checksum());
        let (reader, writer) = pair();
        let received = std::thread::spawn(move || receive(reader));
        let mut cursor = publication.prepare(request(&target)).unwrap().unwrap();
        let expected = cursor.size();
        allocations::without_allocations(|| respond_published(&writer, &mut cursor, &mut [0; 1]))
            .unwrap();
        drop(writer);
        let bytes = received.join().unwrap();
        let (headers, body) = split(&bytes);
        assert!(headers.starts_with("HTTP/1.0 200 Response\r\n"));
        assert!(headers.contains(&format!("Content-Length: {expected}\r\n")));
        assert!(headers.contains("Connection: close\r\n"));
        assert_eq!(body.len() as u64, expected);
        if extension == "pack" {
            git_zero::pack::validate(body).unwrap();
        } else {
            assert_eq!(&body[..8], b"\xfftOc\0\0\0\x02");
        }
    }
}

#[test]
fn failed_socket_writes_cancel_the_cursor_and_empty_staging_writes_no_header() {
    let repository = streamed::GeneratedRepository::new(71, 3);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 17],
        LooseObjects::Disabled,
    )
    .unwrap();
    for target in [
        "objects/info/packs".to_owned(),
        format!("objects/pack/pack-{}.pack", publication.checksum()),
        format!("objects/pack/pack-{}.idx", publication.checksum()),
    ] {
        let (reader, writer) = pair();
        let mut cursor = publication.prepare(request(&target)).unwrap().unwrap();
        assert_eq!(
            respond_published(&writer, &mut cursor, &mut []),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(cursor.read(&mut [0; 1]), Err(Error::InvalidRequest));
        let mut guard = [0xa7];
        assert!(matches!(
            net::recv(&reader, &mut guard, net::RecvFlags::DONTWAIT),
            Err(rustix::io::Errno::AGAIN)
        ));
        assert_eq!(guard, [0xa7]);
        drop(reader);
        let mut cursor = publication.prepare(request(&target)).unwrap().unwrap();
        assert_eq!(
            allocations::without_allocations(|| respond_published(
                &writer,
                &mut cursor,
                &mut [0; 7]
            )),
            Err(Error::Backend)
        );
        assert_eq!(cursor.read(&mut [0; 7]), Err(Error::InvalidRequest));
    }
}

struct FailAfterPublication<'a> {
    source: &'a streamed::GeneratedRepository,
    fail: Cell<bool>,
}
impl Repository for FailAfterPublication<'_> {
    fn head(&self) -> git_zero::ObjectId {
        self.source.head()
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object_id(&self, index: usize) -> Option<git_zero::ObjectId> {
        self.source.object_id(index)
    }
    fn object_info(&self, index: usize) -> git_zero::Result<git_zero::ObjectInfo> {
        self.source.object_info(index)
    }
    fn read_object(
        &self,
        index: usize,
        offset: u64,
        storage: &mut [u8],
    ) -> git_zero::Result<usize> {
        if self.fail.get() {
            Err(Error::Backend)
        } else {
            self.source.read_object(index, offset, storage)
        }
    }
}
#[test]
fn failed_stream_is_truncated_without_a_second_status_or_successful_completion() {
    let source = streamed::GeneratedRepository::new(71, 3);
    let repository = FailAfterPublication {
        source: &source,
        fail: Cell::new(false),
    };
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 17],
        LooseObjects::Disabled,
    )
    .unwrap();
    let target = format!("objects/pack/pack-{}.pack", publication.checksum());
    repository.fail.set(true);
    let mut cursor = publication.prepare(request(&target)).unwrap().unwrap();
    let expected = cursor.size();
    let (reader, writer) = pair();
    let received = std::thread::spawn(move || receive(reader));
    assert_eq!(
        respond_published(&writer, &mut cursor, &mut [0; 7]),
        Err(Error::Backend)
    );
    assert_eq!(cursor.read(&mut [0; 7]), Err(Error::InvalidRequest));
    drop(writer);
    let bytes = received.join().unwrap();
    let (header, body) = split(&bytes);
    assert!(header.contains(&format!("Content-Length: {expected}\r\n")));
    assert!(body.len() < expected as usize);
    assert!(!body.windows(8).any(|bytes| bytes == b"HTTP/1.0"));
    assert!(git_zero::pack::validate(body).is_err());
}
