#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, Object, ObjectFormat, ObjectInfo, ObjectKind,
    pack::{
        ObjectKey, ObjectRange, RangeFrame, RangeInput, RangeLimits, RangeObject,
        RangeRepresentation, RangeRequest, RangeResolver,
    },
};

const LIMITS: RangeLimits = RangeLimits {
    max_output: 65536,
    max_work_bytes: 65536,
    max_instructions: 1000,
    max_depth: 64,
    max_read_bytes: 64,
};

#[derive(Clone, Copy)]
struct Record<'a> {
    key: ObjectKey,
    object: RangeObject,
    bytes: &'a [u8],
}

fn info(size: u64) -> ObjectInfo {
    ObjectInfo {
        kind: ObjectKind::Blob,
        size,
    }
}

fn fixture(format: ObjectFormat) -> [Record<'static>; 3] {
    let base = ObjectKey::Id(
        Object {
            kind: ObjectKind::Blob,
            data: b"hello",
        }
        .id_with_format(format),
    );
    [
        Record {
            key: base,
            object: RangeObject {
                info: info(5),
                representation: RangeRepresentation::Plain,
            },
            bytes: b"hello",
        },
        Record {
            key: ObjectKey::Staged(1),
            object: RangeObject {
                info: info(6),
                representation: RangeRepresentation::Delta {
                    base,
                    base_info: info(5),
                    base_depth: 0,
                    instructions_size: 6,
                },
            },
            bytes: &[5, 6, 0x90, 5, 1, b'!'],
        },
        Record {
            key: ObjectKey::Staged(2),
            object: RangeObject {
                info: info(8),
                representation: RangeRepresentation::Delta {
                    base: ObjectKey::Staged(1),
                    base_info: info(6),
                    base_depth: 1,
                    instructions_size: 8,
                },
            },
            bytes: &[6, 8, 1, b'?', 0x90, 6, 1, b'?'],
        },
    ]
}

fn run(
    records: &[Record<'_>],
    format: ObjectFormat,
    range: ObjectRange,
    limits: RangeLimits,
    capacity: usize,
    input_chunk: usize,
    output_chunk: usize,
) -> git_zero::Result<Vec<u8>> {
    let mut frames = [const { RangeFrame::empty() }; 8];
    let mut resolver = RangeResolver::new(
        format,
        records.last().unwrap().key,
        range,
        &mut frames[..capacity],
        limits,
    )?;
    let mut output = [0xa5; 65];
    let mut result = Vec::new();
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps < 100_000, "resolver failed to make bounded progress");
        let request = match resolver.request() {
            Ok(request) => request,
            Err(error) => {
                terminal(&mut resolver, error);
                return Err(error);
            }
        };
        assert_eq!(resolver.request(), Ok(request));
        match request {
            RangeRequest::Describe(key) => {
                let record = records.iter().find(|record| record.key == key).unwrap();
                if let Err(error) = resolver.describe(record.object) {
                    terminal(&mut resolver, error);
                    return Err(error);
                }
            }
            RangeRequest::Read {
                object,
                kind,
                offset,
                length,
            } => {
                let record = records.iter().find(|record| record.key == object).unwrap();
                assert_eq!(
                    kind,
                    if matches!(record.object.representation, RangeRepresentation::Plain) {
                        RangeInput::Plain
                    } else {
                        RangeInput::Delta
                    }
                );
                assert!(length > 0 && length <= limits.max_read_bytes);
                let offset = offset as usize;
                let end = (offset + length.min(input_chunk)).min(record.bytes.len());
                let bytes = &record.bytes[offset.min(end)..end];
                let blocked = resolver.update(bytes, &mut []).unwrap();
                assert_eq!(
                    (blocked.consumed, blocked.written, blocked.request),
                    (0, 0, RangeRequest::NeedOutput)
                );
                assert_eq!(resolver.request(), Ok(request));
                let step = match resolver.update(bytes, &mut output[..output_chunk]) {
                    Ok(step) => step,
                    Err(error) => {
                        terminal(&mut resolver, error);
                        return Err(error);
                    }
                };
                assert!(step.consumed <= bytes.len() && step.written <= output_chunk);
                assert!(step.consumed != 0 || step.written != 0);
                assert_eq!(output[64], 0xa5);
                result.extend_from_slice(&output[..step.written]);
            }
            RangeRequest::Finished => {
                resolver.finish().unwrap();
                assert_eq!(resolver.position(), result.len() as u64);
                assert!(resolver.work_bytes() <= limits.max_work_bytes);
                let repeat = resolver.update(&[], &mut output).unwrap();
                assert_eq!(
                    (repeat.consumed, repeat.written, repeat.request),
                    (0, 0, RangeRequest::Finished)
                );
                return Ok(result);
            }
            RangeRequest::NeedOutput => panic!("request() has no output buffer to exhaust"),
        }
    }
}

fn terminal(resolver: &mut RangeResolver<'_>, error: Error) {
    assert_eq!(resolver.finish(), Err(error));
    assert_eq!(resolver.request(), Err(error));
    assert_eq!(resolver.update(&[0; 16], &mut [0; 16]), Err(error));
    assert_eq!(
        resolver.describe(RangeObject {
            info: info(0),
            representation: RangeRepresentation::Plain
        }),
        Err(error)
    );
}

#[test]
fn nested_delta_reconstruction_and_every_partial_range_match() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let records = fixture(format);
        for input in [1, 2, 7, 64] {
            for output in [1, 3, 64] {
                check_all_ranges(&records, format, input, output);
            }
        }
    }
}

fn check_all_ranges(records: &[Record<'_>], format: ObjectFormat, input: usize, output: usize) {
    let expected = b"?hello!?";
    assert_eq!(
        run(records, format, ObjectRange::All, LIMITS, 3, input, output).unwrap(),
        expected
    );
    for offset in 0..=expected.len() {
        for length in 0..=expected.len() - offset {
            let range = ObjectRange::Bytes {
                offset: offset as u64,
                length: length as u64,
            };
            assert_eq!(
                run(records, format, range, LIMITS, 3, input, output).unwrap(),
                &expected[offset..offset + length]
            );
        }
    }
}

#[test]
fn independent_work_output_instruction_depth_and_frame_limits() {
    let format = ObjectFormat::Sha1;
    let records = fixture(format);
    for limits in [
        RangeLimits {
            max_output: 7,
            ..LIMITS
        },
        RangeLimits {
            max_work_bytes: 0,
            ..LIMITS
        },
        RangeLimits {
            max_work_bytes: 5,
            ..LIMITS
        },
        RangeLimits {
            max_instructions: 1,
            ..LIMITS
        },
        RangeLimits {
            max_depth: 1,
            ..LIMITS
        },
    ] {
        assert_eq!(
            run(&records, format, ObjectRange::All, limits, 3, 64, 64),
            Err(Error::LimitExceeded)
        );
    }
    for capacity in [0, 1, 2] {
        assert_eq!(
            run(&records, format, ObjectRange::All, LIMITS, capacity, 64, 64),
            Err(Error::BufferTooSmall)
        );
    }
    let partial = ObjectRange::Bytes {
        offset: 0,
        length: 1,
    };
    assert_eq!(
        run(&records, format, partial, LIMITS, 1, 64, 64).unwrap(),
        b"?"
    );
    assert_eq!(
        run(
            &records,
            format,
            ObjectRange::All,
            RangeLimits {
                max_read_bytes: 0,
                ..LIMITS
            },
            3,
            64,
            64
        ),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn verified_canonical_cache_views_remove_encoded_base_recursion() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut records = fixture(format);
        // A verified canonical cache exposes the resolved base as a plain
        // representation. Keep this view fixed during the query and derive
        // the parent's base depth from it, rather than its original pack depth.
        records[1].object.representation = RangeRepresentation::Plain;
        records[1].bytes = b"hello!";
        if let RangeRepresentation::Delta { base_depth, .. } = &mut records[2].object.representation
        {
            *base_depth = 0;
        }
        assert_eq!(
            run(
                &records,
                format,
                ObjectRange::All,
                RangeLimits {
                    max_depth: 1,
                    ..LIMITS
                },
                2,
                64,
                64
            )
            .unwrap(),
            b"?hello!?"
        );
    }
}

#[test]
fn malformed_metadata_cycles_sizes_and_instructions_are_rejected() {
    let format = ObjectFormat::Sha1;
    let source = fixture(format);
    for representation in [
        RangeRepresentation::Delta {
            base: source[1].key,
            base_info: info(7),
            base_depth: 1,
            instructions_size: 8,
        },
        RangeRepresentation::Delta {
            base: source[1].key,
            base_info: ObjectInfo {
                kind: ObjectKind::Tree,
                size: 6,
            },
            base_depth: 1,
            instructions_size: 8,
        },
        RangeRepresentation::Delta {
            base: source[1].key,
            base_info: info(6),
            base_depth: 0,
            instructions_size: 8,
        },
        RangeRepresentation::Delta {
            base: source[2].key,
            base_info: info(8),
            base_depth: 1,
            instructions_size: 8,
        },
    ] {
        let mut records = source;
        records[2].object.representation = representation;
        // Some inconsistencies are noticed from the delta header before a
        // metadata reply; both paths reject rather than emitting success.
        assert!(run(&records, format, ObjectRange::All, LIMITS, 4, 64, 64).is_err());
    }
    for bytes in [
        &[6, 7, 1, b'?', 0x90, 6, 1, b'?'][..],
        &[7, 8, 1, b'?', 0x90, 6, 1, b'?'],
        &[6, 8, 0],
        &[6, 8, 9, b'a'],
        &[6, 8, 1, b'?', 0x90, 6, 1, b'?', 0],
    ] {
        let mut records = source;
        records[2].bytes = bytes;
        if let RangeRepresentation::Delta {
            instructions_size, ..
        } = &mut records[2].object.representation
        {
            *instructions_size = bytes.len() as u64;
        }
        assert_eq!(
            run(&records, format, ObjectRange::All, LIMITS, 3, 64, 64),
            Err(Error::CorruptPack)
        );
    }
    for length in 0..source[2].bytes.len() {
        let mut records = source;
        records[2].bytes = &source[2].bytes[..length];
        assert_eq!(
            run(&records, format, ObjectRange::All, LIMITS, 3, 64, 64),
            Err(Error::CorruptPack)
        );
    }
    let mut records = source;
    records[2].object.info.size = 9;
    assert_eq!(
        run(&records, format, ObjectRange::All, LIMITS, 3, 64, 64),
        Err(Error::CorruptPack)
    );
    for range in [
        ObjectRange::Bytes {
            offset: 9,
            length: 0,
        },
        ObjectRange::Bytes {
            offset: u64::MAX,
            length: 1,
        },
    ] {
        assert_eq!(
            run(&source, format, range, LIMITS, 3, 64, 64),
            Err(Error::InvalidObject)
        );
    }
}

#[test]
fn empty_plain_and_delta_objects_still_close_exactly() {
    let format = ObjectFormat::Sha1;
    let base = Record {
        key: ObjectKey::Staged(0),
        object: RangeObject {
            info: info(0),
            representation: RangeRepresentation::Plain,
        },
        bytes: b"",
    };
    assert!(
        run(&[base], format, ObjectRange::All, LIMITS, 1, 1, 1)
            .unwrap()
            .is_empty()
    );
    for bytes in [&[0, 0][..], &[0, 0, 0][..]] {
        let delta = Record {
            key: ObjectKey::Staged(1),
            object: RangeObject {
                info: info(0),
                representation: RangeRepresentation::Delta {
                    base: base.key,
                    base_info: info(0),
                    base_depth: 0,
                    instructions_size: bytes.len() as u64,
                },
            },
            bytes,
        };
        let actual = run(&[base, delta], format, ObjectRange::All, LIMITS, 1, 1, 1);
        if bytes.len() == 2 {
            assert!(actual.unwrap().is_empty());
        } else {
            assert_eq!(actual, Err(Error::CorruptPack));
        }
    }
}

#[test]
fn mixed_formats_and_unsolicited_replies_are_terminal() {
    let mut frames = [const { RangeFrame::empty() }; 3];
    let records = fixture(ObjectFormat::Sha1);
    let mixed = ObjectKey::Id(ObjectFormat::Sha256.zero_id());
    assert!(matches!(
        RangeResolver::new(
            ObjectFormat::Sha1,
            mixed,
            ObjectRange::All,
            &mut frames,
            LIMITS
        ),
        Err(Error::InvalidObjectId)
    ));
    let mut source = records;
    if let RangeRepresentation::Delta { base, .. } = &mut source[2].object.representation {
        *base = mixed;
    }
    assert_eq!(
        run(
            &source,
            ObjectFormat::Sha1,
            ObjectRange::All,
            LIMITS,
            3,
            64,
            64
        ),
        Err(Error::InvalidObjectId)
    );
    let mut resolver = RangeResolver::new(
        ObjectFormat::Sha1,
        records[2].key,
        ObjectRange::All,
        &mut frames,
        LIMITS,
    )
    .unwrap();
    assert_eq!(
        resolver.update(b"wrong phase", &mut [0; 16]),
        Err(Error::InvalidRequest)
    );
    terminal(&mut resolver, Error::InvalidRequest);
    let mut resolver = RangeResolver::new(
        ObjectFormat::Sha1,
        records[0].key,
        ObjectRange::All,
        &mut frames,
        LIMITS,
    )
    .unwrap();
    resolver.describe(records[0].object).unwrap();
    assert_eq!(
        resolver.describe(records[0].object),
        Err(Error::InvalidRequest)
    );
    terminal(&mut resolver, Error::InvalidRequest);
}

#[test]
fn fixed_state_supports_u64_ranges_and_emits_without_allocations() {
    let format = ObjectFormat::Sha256;
    let mut frames = [const { RangeFrame::empty() }; 3];
    assert!(core::mem::size_of::<RangeFrame>() < 384);
    let records = fixture(format);
    allocations::without_allocations(|| {
        let mut resolver = RangeResolver::new(
            format,
            records[2].key,
            ObjectRange::All,
            &mut frames,
            LIMITS,
        )
        .unwrap();
        let mut result = [0; 8];
        let mut written = 0;
        loop {
            match resolver.request().unwrap() {
                RangeRequest::Describe(key) => resolver
                    .describe(
                        records
                            .iter()
                            .find(|record| record.key == key)
                            .unwrap()
                            .object,
                    )
                    .unwrap(),
                RangeRequest::Read {
                    object,
                    offset,
                    length,
                    ..
                } => {
                    let record = records.iter().find(|record| record.key == object).unwrap();
                    let end = (offset as usize + length).min(record.bytes.len());
                    let mut output = [0; 3];
                    let step = resolver
                        .update(&record.bytes[offset as usize..end], &mut output)
                        .unwrap();
                    result[written..written + step.written]
                        .copy_from_slice(&output[..step.written]);
                    written += step.written;
                }
                RangeRequest::Finished => break,
                _ => panic!(),
            }
        }
        resolver.finish().unwrap();
        assert_eq!(result, *b"?hello!?");
    });
    allocations::without_allocations(|| {
        let mut resolver = RangeResolver::new(
            format,
            ObjectKey::Staged(0),
            ObjectRange::Bytes {
                offset: u64::MAX - 4,
                length: 4,
            },
            &mut frames,
            LIMITS,
        )
        .unwrap();
        resolver
            .describe(RangeObject {
                info: info(u64::MAX),
                representation: RangeRepresentation::Plain,
            })
            .unwrap();
        assert_eq!(
            resolver.request().unwrap(),
            RangeRequest::Read {
                object: ObjectKey::Staged(0),
                kind: RangeInput::Plain,
                offset: u64::MAX - 4,
                length: 4
            }
        );
        let mut output = [0; 4];
        assert_eq!(
            resolver.update(b"abcd", &mut output).unwrap().request,
            RangeRequest::Finished
        );
        assert_eq!(output, *b"abcd");
        resolver.finish().unwrap();
    });
}
