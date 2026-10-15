#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, ObjectFormat, ObjectId,
    client::{self, FetchOptions, SmartAdvertisement},
    io::{SliceWriter, Write},
    pktline::{self, Packet},
};
use sha1::{Digest, Sha1};
use sha2::Sha256;

fn advertisement(names: &[&str], shallow: bool) -> Vec<u8> {
    advertisement_with_caps(names, if shallow { "shallow ofs-delta" } else { "" })
}

fn advertisement_with_caps(names: &[&str], capabilities: &str) -> Vec<u8> {
    advertisement_with_id(names, capabilities, ObjectId::Sha1([7; 20]))
}

fn advertisement_with_id(names: &[&str], capabilities: &str, id: ObjectId) -> Vec<u8> {
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
        pktline::write(&mut writer, format!("{id} {name}{caps}\n").as_bytes()).unwrap();
    }
    writer.write_all(b"0000").unwrap();
    writer.as_bytes().to_vec()
}

#[test]
fn named_checkouts_use_exact_advertised_refs_in_both_formats() {
    for id in [ObjectId::Sha1([7; 20]), ObjectId::Sha256([8; 32])] {
        let caps = if id.format() == ObjectFormat::Sha256 {
            "object-format=sha256"
        } else {
            ""
        };
        let bytes = advertisement_with_id(
            &[
                "HEAD",
                "refs/heads/main",
                "refs/heads/topic/demo",
                "refs/tags/v1",
                "refs/tags/v1^{}",
            ],
            caps,
            id,
        );
        allocations::without_allocations(|| {
            let advertisement = SmartAdvertisement::parse(&bytes).unwrap();
            for name in [
                "HEAD",
                "refs/heads/main",
                "refs/heads/topic/demo",
                "refs/tags/v1",
            ] {
                assert_eq!(advertisement.resolve_reference(name), Ok(id));
            }
            assert_eq!(
                advertisement.resolve_reference("refs/heads/missing"),
                Err(Error::ObjectNotFound)
            );
            assert_eq!(
                advertisement.resolve_reference("main"),
                Err(Error::InvalidReference)
            );
            assert_eq!(
                advertisement.resolve_reference("refs/tags/v1^{}"),
                Err(Error::InvalidReference)
            );
        });
        let bytes = advertisement_with_id(&["refs/heads/main"], caps, id);
        assert_eq!(
            SmartAdvertisement::parse(&bytes)
                .unwrap()
                .resolve_reference("HEAD"),
            Err(Error::ObjectNotFound)
        );
        let bytes = advertisement_with_id(&["refs/heads/main", "refs/heads/main"], caps, id);
        assert_eq!(
            SmartAdvertisement::parse(&bytes)
                .unwrap()
                .resolve_reference("refs/heads/main"),
            Err(Error::InvalidReference)
        );
    }
}

#[test]
fn sha256_smart_fetch_preserves_ids_and_rejects_mixed_formats_before_output() {
    let first = ObjectId::Sha256([7; 32]);
    let second = ObjectId::Sha256([8; 32]);
    let bytes = advertisement_with_id(
        &["HEAD", "refs/heads/main"],
        "object-format=sha256 shallow allow-tip-sha1-in-want",
        first,
    );
    let expected_first = format!("want {first} object-format=sha256 shallow\n");
    let expected_second = format!("want {second}\n");
    let mut storage = [0; 512];
    allocations::without_allocations(|| {
        let advertisement = SmartAdvertisement::parse(&bytes).unwrap();
        assert_eq!(advertisement.object_format(), Some(ObjectFormat::Sha256));
        assert_eq!(advertisement.head(), Ok(first));
        assert_eq!(advertisement.validate_want(second), Ok(()));
        assert_eq!(
            advertisement.validate_want(ObjectId::Sha1([7; 20])),
            Err(Error::InvalidRequest)
        );
        let mut offset = 0;
        for reference in advertisement.references() {
            let reference = reference.unwrap();
            assert_eq!(reference.id, first);
            let resumed = client::read_smart_reference(&bytes, &mut offset)
                .unwrap()
                .unwrap();
            assert_eq!(resumed.id, reference.id);
            assert_eq!(resumed.name, reference.name);
        }
        assert!(
            client::read_smart_reference(&bytes, &mut offset)
                .unwrap()
                .is_none()
        );
        let mut output = SliceWriter::new(&mut storage);
        client::write_fetch_request_iter(
            [first, second, first].into_iter(),
            advertisement.fetch_options(Some(1)),
            &mut output,
        )
        .unwrap();
        let mut encoded = output.as_bytes();
        assert_eq!(
            pktline::read(&mut encoded),
            Ok(Some(Packet::Data(expected_first.as_bytes())))
        );
        assert_eq!(
            pktline::read(&mut encoded),
            Ok(Some(Packet::Data(expected_second.as_bytes())))
        );
        assert_eq!(
            pktline::read(&mut encoded),
            Ok(Some(Packet::Data(b"deepen 1\n")))
        );
        assert_eq!(pktline::read(&mut encoded), Ok(Some(Packet::Flush)));
        assert_eq!(
            pktline::read(&mut encoded),
            Ok(Some(Packet::Data(b"done\n")))
        );
        assert!(encoded.is_empty());
        let mut output = SliceWriter::new(&mut storage);
        assert_eq!(
            client::write_fetch_request(
                &[first, ObjectId::Sha1([7; 20])],
                FetchOptions::default(),
                &mut output
            ),
            Err(Error::InvalidRequest)
        );
        assert!(output.is_empty());
    });
}

#[test]
fn sha256_shallow_responses_validate_boundary_width_and_full_checksum() {
    let mut pack = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
    pack.extend_from_slice(&Sha256::digest(&pack));
    for id in [ObjectId::Sha256([7; 32]), ObjectId::Sha1([7; 20])] {
        let mut storage = [0; 512];
        let mut writer = SliceWriter::new(&mut storage);
        pktline::write(&mut writer, format!("shallow {id}\n").as_bytes()).unwrap();
        writer.write_all(b"0000").unwrap();
        pktline::write(&mut writer, b"NAK\n").unwrap();
        writer.write_all(&pack).unwrap();
        allocations::without_allocations(|| {
            let parsed =
                client::read_pack_response_with_format(writer.as_bytes(), ObjectFormat::Sha256);
            if id.format() == ObjectFormat::Sha256 {
                assert_eq!(parsed, Ok(pack.as_slice()));
            } else {
                assert_eq!(parsed, Err(Error::InvalidRequest));
            }
            assert!(client::read_pack_response(writer.as_bytes()).is_err());
        });
        if id.format() == ObjectFormat::Sha256 {
            let mut corrupt = writer.as_bytes().to_vec();
            *corrupt.last_mut().unwrap() ^= 1;
            assert_eq!(
                client::read_pack_response_with_format(&corrupt, ObjectFormat::Sha256),
                Err(Error::CorruptPack)
            );
        }
    }
}

#[test]
fn negotiated_options_and_explicit_wants_follow_the_advertisement() {
    let historical = ObjectId::Sha1([8; 20]);
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
            assert_eq!(advertisement.validate_want(ObjectId::Sha1([7; 20])), Ok(()));
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
        assert_eq!(parsed.head(), Ok(ObjectId::Sha1([7; 20])));
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
        Ok(ObjectId::Sha1([7; 20]))
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
    let first = ObjectId::Sha1([1; 20]);
    let second = ObjectId::Sha1([2; 20]);
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
        format!("shallow {}\n", ObjectId::Sha1([7; 20])).as_bytes(),
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
