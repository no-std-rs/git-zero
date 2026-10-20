#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, Object, ObjectDigest, ObjectFormat, ObjectInfo, ObjectKind,
    pack::{self, DeltaDecoder, DeltaLimits, DeltaStatus},
};

const LIMITS: DeltaLimits = DeltaLimits {
    max_output: u64::MAX,
    max_instructions: 1000,
    max_depth: 64,
};

fn variable(mut value: u64, bytes: &mut Vec<u8>) {
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        bytes.push(byte | if value == 0 { 0 } else { 128 });
        if value == 0 {
            return;
        }
    }
}

fn delta(base: u64, size: u64, instructions: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    variable(base, &mut bytes);
    variable(size, &mut bytes);
    bytes.extend_from_slice(instructions);
    bytes
}

fn execute(
    base: &[u8],
    bytes: &[u8],
    chunk: usize,
    limits: DeltaLimits,
) -> git_zero::Result<Vec<u8>> {
    let mut decoder = DeltaDecoder::new(base.len() as u64, 0, limits)?;
    let mut offset = 0;
    let mut output = Vec::new();
    for _ in 0..bytes.len() * 3 + 10 {
        let end = (offset + chunk).min(bytes.len());
        let result = decoder.update(&bytes[offset..end], end == bytes.len());
        let progress = match result {
            Ok(progress) => progress,
            Err(error) => {
                assert_eq!(decoder.finish(), Err(error));
                assert_eq!(decoder.update(&[], true), Err(error));
                return Err(error);
            }
        };
        assert!(progress.consumed <= end - offset);
        offset += progress.consumed;
        match progress.status {
            DeltaStatus::Header {
                base_size,
                result_size,
            } => {
                assert_eq!(base_size, base.len() as u64);
                assert!(result_size <= limits.max_output);
            }
            DeltaStatus::Copy { offset, length } => {
                output.extend_from_slice(&base[offset as usize..(offset + length) as usize])
            }
            DeltaStatus::Insert(bytes) => output.extend_from_slice(bytes),
            DeltaStatus::NeedInput => assert_eq!(offset, end),
            DeltaStatus::Finished => {
                assert_eq!(offset, bytes.len());
                assert_eq!(decoder.finish(), Ok(()));
                assert_eq!(decoder.update(&[], true)?.status, DeltaStatus::Finished);
                return Ok(output);
            }
        }
        assert_eq!(decoder.position(), output.len() as u64);
    }
    panic!("delta decoder made no bounded progress")
}

fn compare(base: &[u8], bytes: &[u8], expected: &[u8]) {
    let mut storage = vec![0; expected.len()];
    let object = pack::apply_delta(
        Object {
            kind: ObjectKind::Blob,
            data: base,
        },
        bytes,
        &mut storage,
        LIMITS,
    )
    .unwrap();
    assert_eq!(object.data, expected);
    for chunk in 1..=bytes.len().max(1) {
        assert_eq!(execute(base, bytes, chunk, LIMITS).unwrap(), expected);
    }
}

#[test]
fn fragmented_copies_literals_and_empty_objects() {
    compare(
        b"abcdefgh",
        &delta(8, 9, &[0x91, 2, 3, 3, b'X', b'Y', b'Z', 0x91, 0, 3]),
        b"cdeXYZabc",
    );
    compare(b"", &[0, 0], b"");
    compare(b"base", &[4, 0], b"");
    let base = vec![b'A'; 65536];
    compare(&base, &delta(65536, 65536, &[0x80]), &base);
    // Sparse operands keep their original significance when intervening bytes
    // are omitted. Offset 0x010002, length 0x010000.
    let base = vec![b'B'; 0x020002];
    compare(
        &base,
        &delta(base.len() as u64, 65536, &[0xc5, 2, 1, 1]),
        &base[..65536],
    );
    let literals = vec![b'L'; 127];
    let mut instructions = vec![127];
    instructions.extend_from_slice(&literals);
    compare(b"", &delta(0, 127, &instructions), &literals);
}

#[test]
fn exact_closure_and_all_truncations() {
    let bytes = delta(8, 8, &[0x91, 1, 3, 5, b'1', b'2', b'3', b'4', b'5']);
    for end in 0..bytes.len() {
        for chunk in [1, 3, 100] {
            assert_eq!(
                execute(b"abcdefgh", &bytes[..end], chunk, LIMITS),
                Err(Error::CorruptPack),
                "prefix {end}"
            );
        }
    }
    for trailing in [0, 1, 128] {
        let mut extra = bytes.clone();
        extra.push(trailing);
        assert_eq!(
            execute(b"abcdefgh", &extra, 100, LIMITS),
            Err(Error::CorruptPack)
        );
    }
    let mut decoder = DeltaDecoder::new(0, 0, LIMITS).unwrap();
    assert!(matches!(
        decoder.update(&[0, 0], false).unwrap().status,
        DeltaStatus::Header { .. }
    ));
    assert_eq!(decoder.finish(), Err(Error::CorruptPack));
    assert_eq!(
        decoder.update(&[], false).unwrap().status,
        DeltaStatus::NeedInput
    );
    assert_eq!(
        decoder.update(&[], true).unwrap().status,
        DeltaStatus::Finished
    );
    assert_eq!(decoder.update(&[0], true), Err(Error::CorruptPack));
    assert_eq!(decoder.finish(), Err(Error::CorruptPack));
}

#[test]
fn malformed_ranges_sizes_and_instructions() {
    let cases: &[&[u8]] = &[
        &[1, 0],                                              // Wrong base size.
        &[0, 1, 0],                                           // Reserved opcode.
        &[0, 1, 1],                                           // Truncated literal.
        &[0, 1, 2, b'a', b'b'],                               // Literal exceeds result.
        &[0, 1, 0x80],                                        // Copy exceeds empty base.
        &[0, 1, 0xff],                                        // Truncated copy operands.
        &[0, 2, 1, b'a'],                                     // Short result.
        &[0, 128, 128, 128, 128, 128, 128, 128, 128, 128, 2], // u64 overflow.
        &[0, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 0],
    ];
    for bytes in cases {
        for chunk in [1, 2, 100] {
            assert_eq!(
                execute(b"", bytes, chunk, LIMITS),
                Err(Error::CorruptPack),
                "{bytes:?}"
            );
        }
    }
    for instructions in [&[0x91, 7, 2][..], &[0x90, 9][..]] {
        assert_eq!(
            execute(b"abcdefgh", &delta(8, 8, instructions), 1, LIMITS),
            Err(Error::CorruptPack)
        );
    }
}

#[test]
fn limits_depth_and_u64_ranges() {
    let bytes = delta(0, 2, &[1, b'a', 1, b'b']);
    assert_eq!(
        execute(
            b"",
            &bytes,
            1,
            DeltaLimits {
                max_output: 1,
                ..LIMITS
            }
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        execute(
            b"",
            &bytes,
            1,
            DeltaLimits {
                max_instructions: 1,
                ..LIMITS
            }
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        execute(
            b"",
            &bytes,
            1,
            DeltaLimits {
                max_depth: 0,
                ..LIMITS
            }
        ),
        Err(Error::LimitExceeded)
    );
    assert!(matches!(
        DeltaDecoder::new(
            0,
            u32::MAX,
            DeltaLimits {
                max_depth: u32::MAX,
                ..LIMITS
            }
        ),
        Err(Error::LimitExceeded)
    ));
    let mut decoder = DeltaDecoder::new(
        u64::MAX,
        u32::MAX - 1,
        DeltaLimits {
            max_depth: u32::MAX,
            ..LIMITS
        },
    )
    .unwrap();
    assert_eq!(decoder.depth(), u32::MAX);
    let bytes = delta(u64::MAX, 65536, &[0x8f, 255, 255, 255, 255]);
    let header = decoder.update(&bytes, true).unwrap();
    assert_eq!(
        header.status,
        DeltaStatus::Header {
            base_size: u64::MAX,
            result_size: 65536
        }
    );
    let copy = decoder.update(&bytes[header.consumed..], true).unwrap();
    assert_eq!(
        copy.status,
        DeltaStatus::Copy {
            offset: u32::MAX as u64,
            length: 65536
        }
    );
    assert_eq!(
        decoder.update(&[], true).unwrap().status,
        DeltaStatus::Finished
    );
    assert_eq!(decoder.position(), 65536);
}

#[test]
fn digest_checks_lengths_and_hashes_chunked_bodies_without_allocating() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for kind in [
            ObjectKind::Commit,
            ObjectKind::Tree,
            ObjectKind::Blob,
            ObjectKind::Tag,
        ] {
            let body = b"a fragmented object body";
            // Independent canonical header + hash rather than Object::id,
            // which also uses ObjectDigest.
            let mut header = [0; 32];
            let info = ObjectInfo {
                kind,
                size: body.len() as u64,
            };
            let mut canonical = info.header(&mut header).unwrap().to_vec();
            canonical.extend_from_slice(body);
            use sha1::Digest;
            let expected = match format {
                ObjectFormat::Sha1 => {
                    git_zero::ObjectId::Sha1(sha1::Sha1::digest(&canonical).into())
                }
                ObjectFormat::Sha256 => {
                    git_zero::ObjectId::Sha256(sha2::Sha256::digest(&canonical).into())
                }
            };
            allocations::without_allocations(|| {
                let mut digest = ObjectDigest::new(info, format);
                assert_eq!(digest.finish(), Err(Error::InvalidObject));
                assert_eq!(digest.update(&[0; 100]), Err(Error::InvalidObject));
                assert_eq!(digest.remaining(), info.size);
                for chunk in body.chunks(3) {
                    digest.update(chunk).unwrap();
                }
                assert_eq!(digest.finish(), Ok(expected));
                assert_eq!(digest.finish(), Ok(expected));
                assert_eq!(digest.update(b"x"), Err(Error::InvalidObject));
                assert_eq!(digest.finish(), Ok(expected));
            });
        }
    }
}

#[test]
fn delta_state_has_no_allocator_or_base_buffer() {
    assert!(core::mem::size_of::<DeltaDecoder>() < 256);
    allocations::without_allocations(|| {
        let mut decoder = DeltaDecoder::new(0, 0, LIMITS).unwrap();
        let header = decoder.update(&[0, 1, 1, b'a'], true).unwrap();
        assert_eq!(header.consumed, 2);
        assert_eq!(
            decoder.update(&[1, b'a'], true).unwrap().status,
            DeltaStatus::Insert(b"a")
        );
        assert_eq!(
            decoder.update(&[], true).unwrap().status,
            DeltaStatus::Finished
        );
        decoder.finish().unwrap();
    });
}
