#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, ObjectId,
    client::{self, FetchOptions, SmartAdvertisement},
    io::{SliceWriter, Write},
    pktline::{self, Packet},
};
use sha1::{Digest, Sha1};

fn advertisement(names: &[&str], shallow: bool) -> Vec<u8> {
    advertisement_with_caps(names, if shallow { "shallow ofs-delta" } else { "" })
}

fn advertisement_with_caps(names: &[&str], capabilities: &str) -> Vec<u8> {
    let mut storage = [0; 4096];
    let mut writer = SliceWriter::new(&mut storage);
    pktline::write(&mut writer, b"# service=git-upload-pack\n").unwrap();
    writer.write_all(b"0000").unwrap();
    for (i, name) in names.iter().enumerate() {
        let caps = if i == 0 {
            format!("\0{capabilities}")
        } else {
            String::new()
        };
        pktline::write(
            &mut writer,
            format!("{} {name}{caps}\n", ObjectId([7; 20])).as_bytes(),
        )
        .unwrap();
    }
    writer.write_all(b"0000").unwrap();
    writer.as_bytes().to_vec()
}

#[test]
fn negotiated_options_and_explicit_wants_follow_the_advertisement() {
    let historical = ObjectId([8; 20]);
    for caps in [
        "",
        "shallow",
        "ofs-delta no-progress",
        "shallow ofs-delta no-progress",
    ] {
        let bytes = advertisement_with_caps(&["HEAD"], caps);
        allocations::without_allocations(|| {
            let advertisement = SmartAdvertisement::parse(&bytes).unwrap();
            let options = advertisement.fetch_options(Some(1));
            assert_eq!(options.shallow_depth.is_some(), caps.contains("shallow"));
            assert_eq!(options.ofs_delta, caps.contains("ofs-delta"));
            assert_eq!(options.no_progress, caps.contains("no-progress"));
            assert_eq!(advertisement.validate_want(ObjectId([7; 20])), Ok(()));
            assert_eq!(
                advertisement.validate_want(historical),
                Err(Error::InvalidRequest)
            );
        });
    }
    for cap in ["allow-tip-sha1-in-want", "allow-reachable-sha1-in-want"] {
        let bytes = advertisement_with_caps(&["HEAD"], cap);
        assert_eq!(
            SmartAdvertisement::parse(&bytes)
                .unwrap()
                .validate_want(historical),
            Ok(())
        );
    }
    let bytes = advertisement_with_caps(&["HEAD"], "no-progress-extra shallow-extra");
    let advertisement = SmartAdvertisement::parse(&bytes).unwrap();
    assert!(!advertisement.supports("no-progress"));
    assert!(!advertisement.supports_shallow());
}

#[test]
fn loose_wire_budgets_cover_large_incompressible_objects_and_reject_overflow() {
    for size in [
        0_usize,
        65535,
        16 * 1024 * 1024,
        32 * 1024 * 1024,
        64 * 1024 * 1024,
    ] {
        let capacity = client::loose_wire_capacity(size).unwrap();
        let mut header = [0; 32];
        let info = git_zero::ObjectInfo {
            kind: git_zero::ObjectKind::Commit,
            size: size as u64,
        };
        let header_length = info.header(&mut header).unwrap().len();
        let stored = size + header_length + (1 + size.div_ceil(65535)) * 5 + 11;
        assert!(capacity >= stored);
    }
    assert!(client::loose_wire_capacity(64 * 1024 * 1024).unwrap() > 64 * 1024 * 1024 + 4096);
    assert_eq!(
        client::loose_wire_capacity(usize::MAX),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        git_zero::zlib::compress_bound(u64::MAX),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn discovery_is_borrowed_resumable_and_preserves_peeled_refs() {
    let names = [
        "HEAD",
        "refs/heads/topic/demo",
        "refs/tags/v1",
        "refs/tags/v1^{}",
    ];
    let bytes = advertisement(&names, true);
    allocations::without_allocations(|| {
        let parsed = SmartAdvertisement::parse(&bytes).unwrap();
        assert_eq!(parsed.head(), Ok(ObjectId([7; 20])));
        assert!(parsed.supports_shallow());
        let mut offset = 0;
        for (actual, expected) in parsed.references().zip(names) {
            let actual = actual.unwrap();
            assert_eq!(actual.name, expected);
            let resumed = client::read_smart_reference(&bytes, &mut offset)
                .unwrap()
                .unwrap();
            assert_eq!(resumed.name, expected);
            assert_eq!(resumed.id, actual.id);
        }
        assert!(
            client::read_smart_reference(&bytes, &mut offset)
                .unwrap()
                .is_none()
        );
        assert_eq!(offset, bytes.len());
        assert!(
            client::read_smart_reference(&bytes, &mut offset)
                .unwrap()
                .is_none()
        );
    });
    let without_head = advertisement(&["refs/heads/main"], false);
    assert_eq!(
        SmartAdvertisement::parse(&without_head).unwrap().head(),
        Err(Error::ObjectNotFound)
    );
    assert_eq!(
        client::Discovery::parse(&without_head).unwrap().head(),
        Ok(ObjectId([7; 20]))
    );
}

#[test]
fn discovery_requires_exact_closure_and_valid_reference_names() {
    let bytes = advertisement(&["HEAD"], true);
    assert!(SmartAdvertisement::parse(&bytes[..bytes.len() - 4]).is_err());
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(b"0000");
    assert!(SmartAdvertisement::parse(&trailing).is_err());
    assert!(SmartAdvertisement::parse(&advertisement(&["HEAD", "HEAD"], false)).is_err());
    for bad in [
        "refs/heads/../bad",
        "refs/heads/bad\nname",
        "refs/heads/.hidden",
    ] {
        assert!(SmartAdvertisement::parse(&advertisement(&["HEAD", bad], false)).is_err());
    }
    // Git ref parsing has no filesystem-specific 255-byte component limit.
    let name = format!("refs/heads/{}", "x".repeat(300));
    assert!(SmartAdvertisement::parse(&advertisement(&["HEAD", &name], false)).is_ok());
}

#[test]
fn fetch_requests_deduplicate_wants_and_encode_shallow_options_without_allocating() {
    let first = ObjectId([1; 20]);
    let second = ObjectId([2; 20]);
    let mut storage = [0; 512];
    let expected_first = format!("want {first} ofs-delta no-progress shallow\n");
    let expected_second = format!("want {second}\n");
    allocations::without_allocations(|| {
        let mut writer = SliceWriter::new(&mut storage);
        client::write_fetch_request(
            &[first, first, second, first],
            FetchOptions {
                shallow_depth: Some(1),
                ofs_delta: true,
                no_progress: true,
            },
            &mut writer,
        )
        .unwrap();
        let mut bytes = writer.as_bytes();
        assert_eq!(
            pktline::read(&mut bytes),
            Ok(Some(Packet::Data(expected_first.as_bytes())))
        );
        assert_eq!(
            pktline::read(&mut bytes),
            Ok(Some(Packet::Data(expected_second.as_bytes())))
        );
        assert_eq!(
            pktline::read(&mut bytes),
            Ok(Some(Packet::Data(b"deepen 1\n")))
        );
        assert_eq!(pktline::read(&mut bytes), Ok(Some(Packet::Flush)));
        assert_eq!(pktline::read(&mut bytes), Ok(Some(Packet::Data(b"done\n"))));
        assert!(bytes.is_empty());
    });
    let mut writer = SliceWriter::new(&mut storage);
    assert_eq!(
        client::write_fetch_request(&[], FetchOptions::default(), &mut writer),
        Err(Error::InvalidRequest)
    );
    assert_eq!(
        client::write_fetch_request(
            &[first],
            FetchOptions {
                shallow_depth: Some(0),
                ..FetchOptions::default()
            },
            &mut writer
        ),
        Err(Error::InvalidRequest)
    );
    assert!(writer.is_empty());
    assert_eq!(
        client::write_fetch_request(
            &[first],
            FetchOptions::default(),
            &mut SliceWriter::new(&mut [0; 4])
        ),
        Err(Error::BufferTooSmall)
    );
}

#[test]
fn shallow_responses_require_complete_negotiation_and_pack_checksums() {
    let mut pack = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
    pack.extend_from_slice(&Sha1::digest(&pack));
    let mut storage = [0; 512];
    let mut writer = SliceWriter::new(&mut storage);
    pktline::write(
        &mut writer,
        format!("shallow {}\n", ObjectId([7; 20])).as_bytes(),
    )
    .unwrap();
    writer.write_all(b"0000").unwrap();
    pktline::write(&mut writer, b"NAK\n").unwrap();
    writer.write_all(&pack).unwrap();
    allocations::without_allocations(|| {
        assert_eq!(
            client::read_pack_response(writer.as_bytes()),
            Ok(pack.as_slice())
        );
    });
    let mut corrupt = writer.as_bytes().to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    assert_eq!(
        client::read_pack_response(&corrupt),
        Err(Error::CorruptPack)
    );
    for bad in [
        b"0008NAK".as_slice(),
        b"000dshallow \n",
        b"0000",
        b"0008ACK\n",
    ] {
        assert!(client::read_pack_response(bad).is_err());
    }
}
