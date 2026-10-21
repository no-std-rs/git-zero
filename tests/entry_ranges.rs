#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, Object, ObjectFormat, ObjectKind,
    io::Write,
    pack::{
        ByteRange, CompressedEntry, CompressedKind, EntryRangeDecoder, EntryRangeStatus,
        PackComposer,
    },
    zlib::{self, InflateLimits, WINDOW_SIZE},
};

const LIMITS: InflateLimits = InflateLimits {
    max_output: 1 << 20,
    max_ratio: 65536,
    ratio_slack: 65536,
};
struct Bytes(Vec<u8>);
impl Write for Bytes {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

fn packed(format: ObjectFormat, body: &[u8], delta: bool) -> Vec<u8> {
    let mut compressed = Bytes(Vec::new());
    zlib::write_parts(&mut compressed, &[body]).unwrap();
    let mut output = Bytes(Vec::new());
    let object = Object {
        kind: ObjectKind::Blob,
        data: body,
    };
    let mut composer = PackComposer::new(format, 1, 1 << 24, &mut output).unwrap();
    let kind = if delta {
        CompressedKind::Delta {
            base_id: object.id_with_format(format),
        }
    } else {
        CompressedKind::Object(ObjectKind::Blob)
    };
    composer
        .start_entry(
            CompressedEntry {
                id: object.id_with_format(format),
                kind,
                inflated_size: body.len() as u64,
                compressed_size: compressed.0.len() as u64,
            },
            &mut output,
        )
        .unwrap();
    composer
        .write_compressed(&compressed.0, &mut output)
        .unwrap();
    composer.finish_entry().unwrap();
    composer.finish(&mut output).unwrap();
    output.0[12..].to_vec()
}

fn decode(
    bytes: &[u8],
    format: ObjectFormat,
    range: ByteRange,
    input_chunk: usize,
    output_chunk: usize,
    scratch_size: usize,
) -> git_zero::Result<Vec<u8>> {
    let mut window = [0; WINDOW_SIZE];
    let mut scratch = [0xa5; 65];
    let mut output = [0xa5; 65];
    let mut decoder = EntryRangeDecoder::new(
        format,
        1_u64 << 40,
        &mut window,
        1 << 24,
        LIMITS,
        range,
        &mut scratch[..scratch_size],
    )?;
    let (mut offset, mut result) = (0, Vec::new());
    for _ in 0..1_000_000 {
        let end = (offset + input_chunk).min(bytes.len());
        if range.length != 0 {
            let blocked = decoder.update(&bytes[offset..end], &mut [], end == bytes.len());
            assert_eq!(blocked.unwrap().status, EntryRangeStatus::NeedOutput);
        }
        let progress = match decoder.update(
            &bytes[offset..end],
            &mut output[..output_chunk],
            end == bytes.len(),
        ) {
            Ok(step) => step,
            Err(error) => {
                assert_eq!(decoder.finish(), Err(error));
                assert_eq!(decoder.update(&[], &mut output, true), Err(error));
                return Err(error);
            }
        };
        offset += progress.consumed;
        result.extend_from_slice(&output[..progress.written]);
        assert_eq!(output[64], 0xa5);
        assert_eq!(decoder.position(), result.len() as u64);
        assert_eq!(decoder.wire_position(), (1_u64 << 40) + offset as u64);
        if progress.status == EntryRangeStatus::Finished {
            decoder.finish().unwrap();
            let repeat = decoder.update(&bytes[offset..], &mut output, true).unwrap();
            assert_eq!(
                (repeat.consumed, repeat.written, repeat.status),
                (0, 0, EntryRangeStatus::Finished)
            );
            assert_eq!(scratch[64], 0xa5);
            return Ok(result);
        }
    }
    panic!("range inflater failed to make bounded internal progress")
}

#[test]
fn fragmented_plain_and_delta_representation_ranges_preserve_bytes() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let body: Vec<_> = (0..1000).map(|index| (index % 251) as u8).collect();
        for delta in [false, true] {
            let bytes = packed(format, &body, delta);
            for input in [1, 17, bytes.len()] {
                for output in [1, 7, 64] {
                    for scratch in [1, 3, 64] {
                        for (offset, length) in [(0, body.len()), (500, 17), (999, 1)] {
                            let range = ByteRange {
                                offset: offset as u64,
                                length: length as u64,
                            };
                            assert_eq!(
                                decode(&bytes, format, range, input, output, scratch).unwrap(),
                                &body[offset..offset + length]
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn empty_ranges_check_the_header_then_preserve_following_bytes() {
    for body in [b"".as_slice(), b"body"] {
        let bytes = packed(ObjectFormat::Sha1, body, false);
        let mut window = [0; WINDOW_SIZE];
        let mut decoder = EntryRangeDecoder::new(
            ObjectFormat::Sha1,
            12,
            &mut window,
            bytes.len() as u64,
            LIMITS,
            ByteRange {
                offset: body.len() as u64,
                length: 0,
            },
            &mut [],
        )
        .unwrap();
        let step = decoder.update(&bytes, &mut [], true).unwrap();
        assert_eq!(step.status, EntryRangeStatus::Finished);
        assert!(step.consumed < bytes.len());
        assert_eq!(step.written, 0);
        decoder.finish().unwrap();
    }
}

#[test]
fn truncated_sources_bad_ranges_and_wire_limits_are_terminal() {
    let format = ObjectFormat::Sha1;
    let bytes = packed(format, b"hello", false);
    let all = ByteRange {
        offset: 0,
        length: 5,
    };
    // Partial reads intentionally need not consume Adler/trailer bytes. Cut
    // before the final requested body byte to test actual early source EOF.
    for end in 0..13 {
        assert_eq!(
            decode(&bytes[..end], format, all, 1, 1, 3),
            Err(Error::CorruptPack)
        );
    }
    assert_eq!(
        decode(
            &bytes,
            format,
            ByteRange {
                offset: 5,
                length: 1
            },
            1,
            1,
            3
        ),
        Err(Error::InvalidObject)
    );
    let mut window = [0; WINDOW_SIZE];
    assert!(matches!(
        EntryRangeDecoder::new(
            format,
            12,
            &mut window,
            100,
            LIMITS,
            ByteRange {
                offset: u64::MAX,
                length: 1
            },
            &mut [0; 1]
        ),
        Err(Error::InvalidObject)
    ));
    assert!(matches!(
        EntryRangeDecoder::new(format, 12, &mut window, 100, LIMITS, all, &mut []),
        Err(Error::BufferTooSmall)
    ));
    let mut scratch = [0; 3];
    let mut decoder =
        EntryRangeDecoder::new(format, 12, &mut window, 1, LIMITS, all, &mut scratch).unwrap();
    decoder.update(&bytes, &mut [0; 5], false).unwrap();
    assert_eq!(
        decoder.update(&bytes[1..], &mut [0; 5], true),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn prefix_inflation_and_pending_output_use_only_caller_storage() {
    let bytes = packed(ObjectFormat::Sha1, &[b'a'; 65536], false);
    let mut window = [0; WINDOW_SIZE];
    let mut scratch = [0; 31];
    let mut output = [0; 2];
    allocations::without_allocations(|| {
        let mut decoder = EntryRangeDecoder::new(
            ObjectFormat::Sha1,
            12,
            &mut window,
            bytes.len() as u64,
            LIMITS,
            ByteRange {
                offset: 65530,
                length: 6,
            },
            &mut scratch,
        )
        .unwrap();
        assert!(core::mem::size_of_val(&decoder) < 4096);
        let (mut offset, mut written) = (0, 0);
        loop {
            let step = decoder.update(&bytes[offset..], &mut output, true).unwrap();
            offset += step.consumed;
            written += step.written;
            assert!(output[..step.written].iter().all(|byte| *byte == b'a'));
            if step.status == EntryRangeStatus::Finished {
                break;
            }
        }
        assert_eq!(written, 6);
        assert_eq!(decoder.inflated_position(), 65536);
        decoder.finish().unwrap();
    });
}
