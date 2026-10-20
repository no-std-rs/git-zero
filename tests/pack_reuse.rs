#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, Object, ObjectFormat, ObjectKind,
    io::{SliceWriter, Write},
    pack::{self, CompressedEntry, CompressedKind, PackComposer},
};

const ZLIB: &[u8] = b"\x78\x01\x01\x00\x00\xff\xff\x00\x00\x00\x01";

fn entry(format: ObjectFormat) -> CompressedEntry {
    CompressedEntry {
        id: Object {
            kind: ObjectKind::Blob,
            data: b"",
        }
        .id_with_format(format),
        kind: CompressedKind::Object(ObjectKind::Blob),
        inflated_size: 0,
        compressed_size: ZLIB.len() as u64,
    }
}

#[test]
fn bounded_composition_has_correct_crcs_checksums_and_no_allocations() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for chunk in 1..=ZLIB.len() {
            let mut storage = [0; 128];
            allocations::without_allocations(|| {
                let mut output = SliceWriter::new(&mut storage);
                let mut composer = PackComposer::new(format, 1, 128, &mut output).unwrap();
                assert!(core::mem::size_of_val(&composer) < 512);
                assert_eq!(composer.checksum(), None);
                composer.start_entry(entry(format), &mut output).unwrap();
                for chunk in ZLIB.chunks(chunk) {
                    composer.write_compressed(chunk, &mut output).unwrap();
                }
                let metadata = composer.finish_entry().unwrap();
                assert_eq!(metadata.offset, 12);
                assert_eq!(metadata.id, entry(format).id);
                assert_eq!(metadata.crc32, pack::crc32(&output.as_bytes()[12..]));
                let id = composer.finish(&mut output).unwrap();
                assert_eq!(composer.finish(&mut output), Ok(id));
                assert_eq!(composer.checksum(), Some(id));
                assert_eq!(composer.position(), output.len() as u64);
                pack::validate_with_format(output.as_bytes(), format).unwrap();
                assert_eq!(&output.as_bytes()[13..13 + ZLIB.len()], ZLIB);
            });
        }
    }
}

#[test]
fn empty_pack_and_exact_wire_limit() {
    let mut storage = [0; 64];
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut output = SliceWriter::new(&mut storage);
        let length = 12 + format.id_length() as u64;
        let mut composer = PackComposer::new(format, 0, length, &mut output).unwrap();
        composer.finish(&mut output).unwrap();
        assert_eq!(output.len() as u64, length);
        pack::validate_with_format(output.as_bytes(), format).unwrap();
        let mut output = SliceWriter::new(&mut storage);
        assert!(matches!(
            PackComposer::new(format, 0, length - 1, &mut output),
            Err(Error::LimitExceeded)
        ));
        assert!(output.is_empty());
    }
    let format = ObjectFormat::Sha1;
    for slack in [0, 1] {
        let mut output = SliceWriter::new(&mut storage);
        let length = 12 + 1 + ZLIB.len() as u64 + 20;
        let mut composer = PackComposer::new(format, 1, length - slack, &mut output).unwrap();
        let result = composer.start_entry(entry(format), &mut output);
        if slack == 1 {
            assert_eq!(result, Err(Error::LimitExceeded));
            assert_eq!(output.len(), 12);
            assert_eq!(composer.finish(&mut output), Err(Error::LimitExceeded));
        } else {
            result.unwrap();
            composer.write_compressed(ZLIB, &mut output).unwrap();
            composer.finish_entry().unwrap();
            composer.finish(&mut output).unwrap();
            assert_eq!(composer.position(), length);
        }
    }
}

#[test]
fn reference_deltas_can_precede_their_bases_in_both_object_formats() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let base = Object {
            kind: ObjectKind::Blob,
            data: b"hello",
        };
        let target = Object {
            kind: ObjectKind::Blob,
            data: b"hello!",
        };
        let instructions = [5, 6, 0x90, 5, 1, b'!'];
        let mut base_storage = [0; 64];
        let mut delta_storage = [0; 64];
        let mut base_output = SliceWriter::new(&mut base_storage);
        let mut delta_output = SliceWriter::new(&mut delta_storage);
        git_zero::zlib::write_parts(&mut base_output, &[base.data]).unwrap();
        git_zero::zlib::write_parts(&mut delta_output, &[&instructions]).unwrap();
        let base_bytes = base_output.finish();
        let delta_bytes = delta_output.finish();
        let mut storage = [0; 256];
        let mut output = SliceWriter::new(&mut storage);
        let mut composer = PackComposer::new(format, 2, 256, &mut output).unwrap();
        for (id, kind, size, bytes) in [
            (
                target.id_with_format(format),
                CompressedKind::Delta {
                    base_id: base.id_with_format(format),
                },
                instructions.len(),
                delta_bytes,
            ),
            (
                base.id_with_format(format),
                CompressedKind::Object(ObjectKind::Blob),
                base.data.len(),
                base_bytes,
            ),
        ] {
            composer
                .start_entry(
                    CompressedEntry {
                        id,
                        kind,
                        inflated_size: size as u64,
                        compressed_size: bytes.len() as u64,
                    },
                    &mut output,
                )
                .unwrap();
            for byte in bytes.chunks(1) {
                composer.write_compressed(byte, &mut output).unwrap();
            }
            composer.finish_entry().unwrap();
        }
        composer.finish(&mut output).unwrap();
        let limits = git_zero::zlib::InflateLimits {
            max_output: 64,
            max_ratio: 64,
            ratio_slack: 64,
        };
        let mut window = [0; git_zero::zlib::WINDOW_SIZE];
        let (mut delta_body, mut base_body, mut result) = ([0; 64], [0; 64], [0; 64]);
        let mut reader = pack::Reader::new_with_format(output.as_bytes(), format).unwrap();
        let delta = reader
            .read_entry(&mut delta_body, &mut window, limits)
            .unwrap()
            .unwrap();
        let base = reader
            .read_entry(&mut base_body, &mut window, limits)
            .unwrap()
            .unwrap()
            .object()
            .unwrap();
        let object = delta
            .resolve_delta(
                &base,
                &mut result,
                pack::DeltaLimits {
                    max_output: 64,
                    max_instructions: 64,
                    max_depth: 1,
                },
            )
            .unwrap();
        assert_eq!(object.object().data, target.data);
        assert_eq!(object.id(), target.id_with_format(format));
        reader.finish().unwrap();
    }
}

#[test]
fn invalid_metadata_and_partial_entries_fail_terminally() {
    let format = ObjectFormat::Sha1;
    let mut storage = [0; 128];
    let cases = [
        (
            CompressedEntry {
                compressed_size: 7,
                ..entry(format)
            },
            Error::InvalidObject,
        ),
        (
            CompressedEntry {
                compressed_size: u64::MAX,
                ..entry(format)
            },
            Error::LimitExceeded,
        ),
        (
            CompressedEntry {
                id: entry(ObjectFormat::Sha256).id,
                ..entry(format)
            },
            Error::InvalidObjectId,
        ),
        (
            CompressedEntry {
                kind: CompressedKind::Delta {
                    base_id: entry(ObjectFormat::Sha256).id,
                },
                ..entry(format)
            },
            Error::InvalidObjectId,
        ),
    ];
    for (entry, expected) in cases {
        let mut output = SliceWriter::new(&mut storage);
        let mut composer = PackComposer::new(format, 1, u64::MAX, &mut output).unwrap();
        assert_eq!(composer.start_entry(entry, &mut output), Err(expected));
        assert_eq!(output.len(), 12);
        assert_eq!(composer.finish_entry(), Err(expected));
        assert_eq!(composer.finish(&mut output), Err(expected));
        assert_eq!(composer.write_compressed(ZLIB, &mut output), Err(expected));
        assert_eq!(composer.checksum(), None);
    }
    for overrun in [false, true] {
        let mut output = SliceWriter::new(&mut storage);
        let mut composer = PackComposer::new(format, 1, 128, &mut output).unwrap();
        composer.start_entry(entry(format), &mut output).unwrap();
        if overrun {
            assert_eq!(
                composer.write_compressed(&[0; 12], &mut output),
                Err(Error::InvalidObject)
            );
        } else {
            assert_eq!(composer.finish_entry(), Err(Error::InvalidObject));
        }
        assert_eq!(composer.finish(&mut output), Err(Error::InvalidObject));
    }
}

struct Failing {
    remaining: usize,
}
impl Write for Failing {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        if bytes.len() > self.remaining {
            self.remaining = 0;
            Err(Error::Backend)
        } else {
            self.remaining -= bytes.len();
            Ok(())
        }
    }
}

#[test]
fn sink_failures_in_headers_bodies_and_trailers_never_publish_a_checksum() {
    for capacity in 0..45 {
        let mut output = Failing {
            remaining: capacity,
        };
        let Ok(mut composer) = PackComposer::new(ObjectFormat::Sha1, 1, 128, &mut output) else {
            continue;
        };
        let result = composer
            .start_entry(entry(ObjectFormat::Sha1), &mut output)
            .and_then(|()| composer.write_compressed(ZLIB, &mut output))
            .and_then(|()| composer.finish_entry().map(|_| ()))
            .and_then(|()| composer.finish(&mut output).map(|_| ()));
        if capacity < 44 {
            assert_eq!(result, Err(Error::Backend));
            assert_eq!(composer.checksum(), None);
            assert_eq!(composer.finish(&mut output), Err(Error::Backend));
        } else {
            result.unwrap();
        }
    }
}
