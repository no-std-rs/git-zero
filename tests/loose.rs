#![cfg(all(feature = "client", feature = "write"))]

#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, Object, ObjectId, ObjectInfo, ObjectKind, Result,
    client::{self, Transport},
    http::Request,
    io::SliceWriter,
    pack,
    zlib::{self, InflateLimits, WINDOW_SIZE},
};
use sha1::{Digest, Sha1};

const LIMITS: InflateLimits = InflateLimits {
    max_output: 4096,
    max_ratio: 1024,
    ratio_slack: 32,
};

fn encoded(bytes: &[u8]) -> Vec<u8> {
    let mut storage = vec![0; bytes.len() + 64];
    let mut output = SliceWriter::new(&mut storage);
    zlib::write_parts(&mut output, &[bytes]).unwrap();
    output.as_bytes().to_vec()
}

#[test]
fn canonical_headers_round_trip_all_kinds_and_64_bit_sizes() {
    for kind in [
        ObjectKind::Commit,
        ObjectKind::Tree,
        ObjectKind::Blob,
        ObjectKind::Tag,
    ] {
        for size in [0, 1, 4096, u64::MAX] {
            let info = ObjectInfo { kind, size };
            let mut storage = [0; 32];
            assert_eq!(
                ObjectInfo::from_header(info.header(&mut storage).unwrap()),
                Ok(info)
            );
        }
    }
    for header in [
        b"".as_slice(),
        b"blob 0",
        b"Blob 0\0",
        b"unknown 0\0",
        b"blob\0",
        b"blob \0",
        b"blob 00\0",
        b"blob 01\0",
        b"blob +1\0",
        b"blob -1\0",
        b"blob 1 \0",
        b"blob  1\0",
        b"blob 1\n\0",
        b"blob 1\0\0",
        b"blob 18446744073709551616\0",
    ] {
        assert_eq!(ObjectInfo::from_header(header), Err(Error::InvalidObject));
    }
}

#[test]
fn loose_decoder_returns_only_validated_bodies_with_exact_staging_capacity() {
    for kind in [
        ObjectKind::Commit,
        ObjectKind::Tree,
        ObjectKind::Blob,
        ObjectKind::Tag,
    ] {
        for data in [b"".as_slice(), b"binary\0\xffbody"] {
            let object = Object { kind, data };
            let mut wire = [0; 256];
            let mut output = SliceWriter::new(&mut wire);
            pack::write_loose(object, &mut output).unwrap();
            let mut header = [0; 32];
            let length = object.header(&mut header).unwrap().len() + data.len();
            let mut storage = [0xcc; 256];
            let mut window = [0; WINDOW_SIZE];
            allocations::without_allocations(|| {
                let decoded = client::decode_loose(
                    output.as_bytes(),
                    object.id(),
                    &mut storage[..length],
                    &mut window,
                    LIMITS,
                )
                .unwrap();
                assert_eq!(decoded.kind, kind);
                assert_eq!(decoded.data, data);
                assert_eq!(decoded.id(), object.id());
            });
            assert!(storage[length..].iter().all(|&byte| byte == 0xcc));
            assert!(matches!(
                client::decode_loose(
                    output.as_bytes(),
                    object.id(),
                    &mut storage[..length - 1],
                    &mut window,
                    LIMITS
                ),
                Err(Error::BufferTooSmall)
            ));
        }
    }
}

#[test]
fn even_matching_ids_cannot_accept_bad_headers_or_body_lengths() {
    for raw in [
        b"blob 4\0abc".as_slice(),
        b"blob 2\0abc",
        b"blob 03\0abc",
        b"blob +3\0abc",
        b"blob 3\0abc\0",
        b"tag 0\0x",
        b"blob 0",
        b"unknown 0\0",
        b"blob 18446744073709551616\0",
        b"blob 0000000000000000000000000000000000000000\0",
    ] {
        let expected = ObjectId(Sha1::digest(raw).into());
        assert!(matches!(
            Object::from_loose(raw, expected),
            Err(Error::InvalidObject)
        ));
        let mut storage = [0; 256];
        let mut window = [0; WINDOW_SIZE];
        assert!(matches!(
            client::decode_loose(&encoded(raw), expected, &mut storage, &mut window, LIMITS),
            Err(Error::InvalidObject)
        ));
    }
}

#[test]
fn wrong_ids_trailing_streams_corruption_and_limits_fail() {
    let object = Object {
        kind: ObjectKind::Blob,
        data: b"abc",
    };
    let wire = encoded(b"blob 3\0abc");
    let mut storage = [0; 256];
    let mut window = [0; WINDOW_SIZE];
    assert!(matches!(
        client::decode_loose(&wire, ObjectId([0; 20]), &mut storage, &mut window, LIMITS),
        Err(Error::InvalidObject)
    ));
    let mut trailing = wire.clone();
    trailing.extend_from_slice(&wire);
    assert!(matches!(
        client::decode_loose(&trailing, object.id(), &mut storage, &mut window, LIMITS),
        Err(Error::InvalidObject)
    ));
    let mut corrupt = wire.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(matches!(
        client::decode_loose(&corrupt, object.id(), &mut storage, &mut window, LIMITS),
        Err(Error::InvalidZlib)
    ));
    for limits in [
        InflateLimits {
            max_output: 3,
            ..LIMITS
        },
        InflateLimits {
            max_ratio: 0,
            ratio_slack: 1,
            ..LIMITS
        },
    ] {
        assert!(matches!(
            client::decode_loose(&wire, object.id(), &mut storage, &mut window, limits),
            Err(Error::LimitExceeded)
        ));
    }
}

struct Reply<'a> {
    bytes: &'a [u8],
    status: u16,
    extra: usize,
}
impl Transport for Reply<'_> {
    fn request(&mut self, request: Request<'_>, response: &mut [u8]) -> Result<(u16, usize)> {
        assert_eq!(request.method, "GET");
        assert!(request.path.starts_with("objects/"));
        response
            .get_mut(..self.bytes.len())
            .ok_or(Error::BufferTooSmall)?
            .copy_from_slice(self.bytes);
        Ok((self.status, self.bytes.len() + self.extra))
    }
}

#[test]
fn dumb_fetch_validates_both_transport_counts_and_object_identity_without_allocating() {
    let object = Object {
        kind: ObjectKind::Blob,
        data: b"abc",
    };
    let wire = encoded(b"blob 3\0abc");
    let mut response = [0; 256];
    let mut staging = [0; 256];
    let mut window = [0; WINDOW_SIZE];
    let mut transport = Reply {
        bytes: &wire,
        status: 200,
        extra: 0,
    };
    allocations::without_allocations(|| {
        let decoded = client::fetch_loose_object(
            &mut transport,
            object.id(),
            &mut response,
            &mut staging,
            &mut window,
            LIMITS,
        )
        .unwrap();
        assert_eq!(decoded.data, object.data);
    });
    for (status, extra, error) in [
        (404, 0, Error::ObjectNotFound),
        (500, 0, Error::Backend),
        (200, 1024, Error::BufferTooSmall),
    ] {
        transport.status = status;
        transport.extra = extra;
        assert!(
            matches!(client::fetch_loose_object(&mut transport, object.id(), &mut response, &mut staging, &mut window, LIMITS), Err(actual) if actual == error)
        );
    }
}
