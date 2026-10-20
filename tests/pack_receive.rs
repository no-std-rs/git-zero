#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, Object, ObjectFormat, ObjectId, ObjectKind,
    io::Write,
    pack::{self, DecodeLimits, DecodeStatus, Decoder, EntryHeader, EntryKind},
    zlib::{self, InflateLimits, WINDOW_SIZE},
};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

const LIMITS: DecodeLimits = DecodeLimits {
    max_pack_bytes: 16 * 1024 * 1024,
    max_objects: 1000,
    inflate: InflateLimits {
        max_output: 1024 * 1024,
        max_ratio: 4096,
        ratio_slack: 1024,
    },
};

struct Bytes(Vec<u8>);
impl Write for Bytes {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

fn wire(header: &[u8], data: &[u8]) -> Vec<u8> {
    let mut bytes = Bytes(header.to_vec());
    zlib::write_parts(&mut bytes, &[data]).unwrap();
    bytes.0
}

fn packed(format: ObjectFormat, entries: &[&[u8]]) -> Vec<u8> {
    let mut bytes = b"PACK\0\0\0\x02".to_vec();
    bytes.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for entry in entries {
        bytes.extend_from_slice(entry);
    }
    seal(format, &mut bytes);
    bytes
}

fn seal(format: ObjectFormat, bytes: &mut Vec<u8>) {
    match format {
        ObjectFormat::Sha1 => bytes.extend_from_slice(&Sha1::digest(&*bytes)),
        ObjectFormat::Sha256 => bytes.extend_from_slice(&Sha256::digest(&*bytes)),
    }
}

#[derive(Debug, Default, PartialEq)]
struct Decoded {
    header: Option<(u32, u32)>,
    entries: Vec<DecodedEntry>,
    checksum: Option<ObjectId>,
}

type DecodedEntry = (EntryHeader, Vec<u8>, u64, u32, Option<ObjectId>);

fn decode(
    bytes: &[u8],
    format: ObjectFormat,
    input_chunk: usize,
    output_chunk: usize,
    limits: DecodeLimits,
) -> git_zero::Result<Decoded> {
    let mut window = [0; WINDOW_SIZE];
    let mut decoder = Decoder::new(format, &mut window, limits);
    let mut output = vec![0; output_chunk];
    let mut result = Decoded::default();
    let mut offset = 0;
    loop {
        let end = (offset + input_chunk).min(bytes.len());
        let progress = decoder.update(&bytes[offset..end], &mut output, end == bytes.len())?;
        offset += progress.consumed;
        assert_eq!(decoder.position(), offset as u64);
        if progress.written != 0 {
            result
                .entries
                .last_mut()
                .unwrap()
                .1
                .extend_from_slice(&output[..progress.written]);
        }
        match progress.status {
            DecodeStatus::Header { version, objects } => result.header = Some((version, objects)),
            DecodeStatus::EntryStart(entry) => result.entries.push((entry, vec![], 0, 0, None)),
            DecodeStatus::EntryEnd {
                end_offset,
                crc32,
                object_id,
            } => {
                let entry = result.entries.last_mut().unwrap();
                entry.2 = end_offset;
                entry.3 = crc32;
                entry.4 = object_id;
            }
            DecodeStatus::Finished(id) => {
                assert_eq!(offset, bytes.len());
                assert_eq!(decoder.finish(), Ok(id));
                result.checksum = Some(id);
                return Ok(result);
            }
            DecodeStatus::NeedInput | DecodeStatus::NeedOutput => {
                assert!(progress.consumed != 0 || progress.written != 0);
            }
        }
    }
}

#[test]
fn arbitrary_fragmentation_preserves_plain_and_delta_entries_for_both_formats() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let plain = wire(&[0x35], b"hello");
        let delta = wire(&[0x66, plain.len() as u8], &[5, 6, 0x90, 5, 1, b'!']);
        let id = Object {
            kind: ObjectKind::Blob,
            data: b"hello!",
        }
        .id_with_format(format);
        let mut reference = vec![0x76];
        reference.extend_from_slice(id.as_bytes());
        let reference = wire(&reference, &[6, 7, 0x90, 6, 1, b'?']);
        let pack = packed(format, &[&plain, &delta, &reference]);
        let expected = decode(&pack, format, pack.len(), 128, LIMITS).unwrap();
        assert_eq!(expected.header, Some((2, 3)));
        assert_eq!(expected.entries[0].0.offset, 12);
        assert_eq!(
            expected.entries[0].0.kind,
            EntryKind::Object(ObjectKind::Blob)
        );
        assert_eq!(expected.entries[0].1, b"hello");
        assert_eq!(expected.entries[0].2, (12 + plain.len()) as u64);
        assert_eq!(expected.entries[0].3, pack::crc32(&plain));
        assert_eq!(
            expected.entries[0].4,
            Some(
                Object {
                    kind: ObjectKind::Blob,
                    data: b"hello"
                }
                .id_with_format(format)
            )
        );
        assert_eq!(
            expected.entries[1].0.kind,
            EntryKind::OffsetDelta { base_offset: 12 }
        );
        assert_eq!(expected.entries[1].1, [5, 6, 0x90, 5, 1, b'!']);
        assert_eq!(expected.entries[1].3, pack::crc32(&delta));
        assert_eq!(expected.entries[1].4, None);
        assert_eq!(
            expected.entries[2].0.kind,
            EntryKind::ReferenceDelta { base_id: id }
        );
        assert_eq!(expected.entries[2].3, pack::crc32(&reference));
        for input in 1..=pack.len() {
            for output in [1, 2, 5, 16, 64] {
                assert_eq!(
                    decode(&pack, format, input, output, LIMITS).unwrap(),
                    expected
                );
            }
        }
    }
}

#[test]
fn no_allocations_or_object_sized_storage() {
    let entry = wire(&[0xb0, 0x08], &[b'a'; 128]);
    let bytes = packed(ObjectFormat::Sha1, &[&entry]);
    let mut window = [0; WINDOW_SIZE];
    let mut scratch = [0; 3];
    allocations::without_allocations(|| {
        let mut decoder = Decoder::new(ObjectFormat::Sha1, &mut window, LIMITS);
        assert!(core::mem::size_of_val(&decoder) < 4096);
        let mut offset = 0;
        loop {
            let end = (offset + 2).min(bytes.len());
            let progress = decoder
                .update(&bytes[offset..end], &mut scratch, end == bytes.len())
                .unwrap();
            offset += progress.consumed;
            if let DecodeStatus::Finished(id) = progress.status {
                assert_eq!(decoder.finish(), Ok(id));
                break;
            }
        }
    });
}

#[test]
fn accepts_empty_packs_empty_objects_and_pack_version_three() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let empty = packed(format, &[]);
        let decoded = decode(&empty, format, 1, 1, LIMITS).unwrap();
        assert_eq!(decoded.header, Some((2, 0)));
        assert!(decoded.entries.is_empty());
        let mut version_three = empty[..empty.len() - format.id_length()].to_vec();
        version_three[7] = 3;
        seal(format, &mut version_three);
        assert_eq!(
            decode(&version_three, format, 1, 1, LIMITS).unwrap().header,
            Some((3, 0))
        );
        let entry = wire(&[0x30], b"");
        let pack = packed(format, &[&entry]);
        let decoded = decode(&pack, format, 1, 1, LIMITS).unwrap();
        assert_eq!(
            decoded.entries[0].4,
            Some(
                Object {
                    kind: ObjectKind::Blob,
                    data: b""
                }
                .id_with_format(format)
            )
        );
    }
}

#[test]
fn truncation_at_every_byte_corrupt_trailers_and_trailing_data_are_rejected() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let entry = wire(&[0x35], b"hello");
        let bytes = packed(format, &[&entry]);
        for length in 0..bytes.len() {
            assert_eq!(
                decode(&bytes[..length], format, 1, 1, LIMITS),
                Err(Error::CorruptPack),
                "truncation at {length}"
            );
        }
        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode(&corrupt, format, 1, 1, LIMITS),
            Err(Error::CorruptPack)
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            decode(&trailing, format, trailing.len(), 128, LIMITS),
            Err(Error::CorruptPack)
        );
        let mut window = [0; WINDOW_SIZE];
        let mut decoder = Decoder::new(format, &mut window, LIMITS);
        let mut offset = 0;
        loop {
            let progress = decoder.update(&bytes[offset..], &mut [0; 8], true).unwrap();
            offset += progress.consumed;
            if matches!(progress.status, DecodeStatus::Finished(_)) {
                break;
            }
        }
        assert_eq!(decoder.update(b"x", &mut [], true), Err(Error::CorruptPack));
        assert_eq!(decoder.finish(), Err(Error::CorruptPack));
        assert_eq!(
            decoder.update(&[], &mut [0; 8], true),
            Err(Error::CorruptPack)
        );
    }
}

#[test]
fn malformed_headers_sizes_and_zlib_cannot_be_hidden_by_a_valid_pack_hash() {
    let malformed = [
        wire(&[0x00], b""),
        wire(&[0x50], b""),
        wire(&[0x34], b"hello"),
        wire(&[0x36], b"hello"),
        wire(&[0x60, 0], b""),
        wire(&[0x60, 1], b""),
        wire(&[0x60, 12], b""),
        wire(
            &[0x60, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0],
            b"",
        ),
        wire(&[0xb0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0], b""),
    ];
    for entry in malformed {
        assert!(
            decode(
                &packed(ObjectFormat::Sha1, &[&entry]),
                ObjectFormat::Sha1,
                1,
                1,
                LIMITS
            )
            .is_err()
        );
    }
    let mut entry = wire(&[0x35], b"hello");
    *entry.last_mut().unwrap() ^= 1;
    assert_eq!(
        decode(
            &packed(ObjectFormat::Sha1, &[&entry]),
            ObjectFormat::Sha1,
            1,
            1,
            LIMITS
        ),
        Err(Error::CorruptPack)
    );
    for index in [0, 7, 11] {
        let mut header = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
        header[index] ^= 1;
        if index == 7 {
            header[index] = 4;
        }
        seal(ObjectFormat::Sha1, &mut header);
        assert!(decode(&header, ObjectFormat::Sha1, 1, 1, LIMITS).is_err());
    }
}

#[test]
fn budgets_are_independent_and_failure_is_terminal() {
    let entry = wire(&[0x35], b"hello");
    let bytes = packed(ObjectFormat::Sha1, &[&entry]);
    let exact = DecodeLimits {
        max_pack_bytes: bytes.len() as u64,
        max_objects: 1,
        inflate: InflateLimits {
            max_output: 5,
            ..LIMITS.inflate
        },
    };
    decode(&bytes, ObjectFormat::Sha1, 1, 1, exact).unwrap();
    for limits in [
        DecodeLimits {
            max_pack_bytes: bytes.len() as u64 - 1,
            ..exact
        },
        DecodeLimits {
            max_objects: 0,
            ..exact
        },
        DecodeLimits {
            inflate: InflateLimits {
                max_output: 4,
                ..exact.inflate
            },
            ..exact
        },
        DecodeLimits {
            inflate: InflateLimits {
                max_ratio: 0,
                ratio_slack: 4,
                ..exact.inflate
            },
            ..exact
        },
    ] {
        assert_eq!(
            decode(&bytes, ObjectFormat::Sha1, 1, 1, limits),
            Err(Error::LimitExceeded)
        );
    }
    let mut window = [0; WINDOW_SIZE];
    let mut decoder = Decoder::new(
        ObjectFormat::Sha1,
        &mut window,
        DecodeLimits {
            max_pack_bytes: 0,
            ..LIMITS
        },
    );
    assert_eq!(decoder.finish(), Err(Error::CorruptPack));
    assert_eq!(
        decoder.update(b"P", &mut [], false),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        decoder.update(&bytes, &mut [0; 8], true),
        Err(Error::LimitExceeded)
    );
    assert_eq!(decoder.finish(), Err(Error::LimitExceeded));
}

#[test]
fn empty_fragments_and_output_backpressure_preserve_state() {
    let entry = wire(&[0x35], b"hello");
    let bytes = packed(ObjectFormat::Sha1, &[&entry]);
    let mut window = [0; WINDOW_SIZE];
    let mut decoder = Decoder::new(ObjectFormat::Sha1, &mut window, LIMITS);
    let progress = decoder.update(&[], &mut [], false).unwrap();
    assert_eq!(progress.status, DecodeStatus::NeedInput);
    assert_eq!(progress.consumed, 0);
    assert!(matches!(
        decoder.update(&bytes[..12], &mut [], false).unwrap().status,
        DecodeStatus::Header { .. }
    ));
    assert!(matches!(
        decoder
            .update(&bytes[12..13], &mut [], false)
            .unwrap()
            .status,
        DecodeStatus::EntryStart(_)
    ));
    let progress = decoder.update(&bytes[13..], &mut [], true).unwrap();
    assert_eq!(progress.status, DecodeStatus::NeedOutput);
    assert_eq!(progress.written, 0);
    let mut offset = 13 + progress.consumed;
    let mut body = Vec::new();
    loop {
        let mut output = [0; 1];
        let progress = decoder.update(&bytes[offset..], &mut output, true).unwrap();
        offset += progress.consumed;
        body.extend_from_slice(&output[..progress.written]);
        if matches!(progress.status, DecodeStatus::Finished(_)) {
            break;
        }
    }
    assert_eq!(body, b"hello");
}

#[test]
fn decodes_real_git_compressed_packs_including_deltas() {
    let root = std::env::temp_dir().join(format!("git-zero-pack-receive-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let run = |args: &[&str], input: &[u8]| {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&["init", "--bare", "--object-format=sha1"], b"");
    let mut ids = Vec::new();
    for n in 0..12 {
        let mut data: Vec<u8> = (0..32_768)
            .map(|i| ((i * 17 + i / 97) % 251) as u8)
            .collect();
        data[n * 100] ^= n as u8;
        ids.extend_from_slice(&run(&["hash-object", "-w", "--stdin"], &data));
    }
    for offset_deltas in [false, true] {
        let mut args = vec!["pack-objects", "--stdout", "--window=50", "--depth=20"];
        if offset_deltas {
            args.push("--delta-base-offset");
        }
        let bytes = run(&args, &ids);
        let actual = decode(&bytes, ObjectFormat::Sha1, 7, 31, LIMITS).unwrap();
        let mut reader = pack::Reader::new(&bytes).unwrap();
        let mut output = [0; 65_536];
        let mut window = [0; WINDOW_SIZE];
        let mut deltas = 0;
        for (header, data, end, crc, id) in actual.entries {
            let expected = reader
                .read_entry(&mut output, &mut window, LIMITS.inflate)
                .unwrap()
                .unwrap();
            assert_eq!(header.offset, expected.offset());
            assert_eq!(header.kind, expected.kind());
            assert_eq!(header.size, expected.data().len() as u64);
            assert_eq!(data, expected.data());
            assert_eq!(end, expected.end_offset());
            assert_eq!(crc, expected.crc32());
            assert_eq!(id, expected.object().map(|object| object.id()));
            deltas += usize::from(!matches!(header.kind, EntryKind::Object(_)));
        }
        assert!(deltas > 0);
        reader.finish().unwrap();
    }
}
