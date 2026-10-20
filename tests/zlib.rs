use git_zero::{io::SliceWriter, zlib};

#[test]
fn streamed_stored_blocks_have_exact_size_and_are_fragment_independent() {
    use zlib::StoredEncoder;
    for length in [0, 1, 65534, 65535, 65536, 131071] {
        let body: Vec<_> = (0..length).map(|i| (i * 17) as u8).collect();
        let mut reference_storage = vec![0; length + 256];
        let mut reference = SliceWriter::new(&mut reference_storage);
        zlib::write_parts(&mut reference, &[&body]).unwrap();
        assert_eq!(
            StoredEncoder::encoded_size(length as u64).unwrap(),
            reference.as_bytes().len() as u64
        );
        for fragment in [1, 7, 65535, 65536] {
            let mut storage = vec![0; reference.as_bytes().len()];
            let mut output = SliceWriter::new(&mut storage);
            let mut encoder = StoredEncoder::new(length as u64, &mut output).unwrap();
            for bytes in body.chunks(fragment) {
                encoder.write(bytes, &mut output).unwrap();
            }
            encoder.finish(&mut output).unwrap();
            assert_eq!(output.as_bytes(), reference.as_bytes());
            assert_eq!(
                encoder.finish(&mut output),
                Err(git_zero::Error::InvalidRequest)
            );
        }
    }
    assert_eq!(
        StoredEncoder::encoded_size(u64::MAX),
        Err(git_zero::Error::LimitExceeded)
    );
    let mut storage = [0; 64];
    let mut output = SliceWriter::new(&mut storage);
    let mut encoder = StoredEncoder::new(1, &mut output).unwrap();
    assert_eq!(
        encoder.write(b"ab", &mut output),
        Err(git_zero::Error::InvalidObject)
    );
    assert_eq!(output.as_bytes(), &[0x78, 0x01]);
    assert_eq!(
        encoder.write(b"a", &mut output),
        Err(git_zero::Error::InvalidObject)
    );
    assert_eq!(
        encoder.finish(&mut output),
        Err(git_zero::Error::InvalidObject)
    );
}

fn checksum(parts: &[&[u8]]) -> u32 {
    let mut storage = vec![0; parts.iter().map(|part| part.len()).sum::<usize>() + 256];
    let mut output = SliceWriter::new(&mut storage);
    zlib::write_parts(&mut output, parts).unwrap();
    let bytes = output.as_bytes();
    u32::from_be_bytes(bytes[bytes.len() - 4..].try_into().unwrap())
}

fn reference(bytes: &[u8]) -> u32 {
    let (mut first, mut second) = (1_u32, 0_u32);
    for &byte in bytes {
        first = (first + u32::from(byte)) % 65521;
        second = (second + first) % 65521;
    }
    (second << 16) | first
}

#[test]
fn worst_case_bytes_match_zlib_at_checksum_and_deflate_boundaries() {
    // Independent golden values from Python's zlib.adler32.
    for (length, expected) in [
        (0, 0x0000_0001),
        (1, 0x0100_0100),
        (5551, 0x5603_9a8d),
        (5552, 0xf18f_9b8c),
        (5553, 0x8e29_9c8b),
        (65535, 0x68a5_0df3),
        (65536, 0x7797_0ef2),
        (131071, 0xb179_1ce4),
    ] {
        let bytes = vec![0xff; length];
        assert_eq!(checksum(&[&bytes]), expected);
        for split in [0, 1, 5551, 5552, 5553, length / 2, length] {
            if split <= length {
                assert_eq!(checksum(&[&bytes[..split], &[], &bytes[split..]]), expected);
            }
        }
    }
}

#[test]
fn repeated_reductions_and_arbitrary_partitions_match_scalar_reference() {
    let mut bytes = vec![0; 1_048_577];
    let mut state = 0x71ab_0259_u32;
    for byte in &mut bytes {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    let expected = reference(&bytes);
    assert_eq!(checksum(&[&bytes]), expected);
    // Each call starts from the previous call's reduced sums. Boundaries here
    // differ from both the Adler batch and DEFLATE block boundaries.
    for chunk_size in [7, 257, 5551, 5552, 5553, 65536] {
        let parts: Vec<_> = bytes.chunks(chunk_size).collect();
        let overhead = parts.len() * 5 + bytes.len() / 65535 * 5 + 11;
        let mut storage = vec![0; bytes.len() + overhead];
        let mut output = SliceWriter::new(&mut storage);
        zlib::write_parts(&mut output, &parts).unwrap();
        let encoded = output.as_bytes();
        assert_eq!(
            u32::from_be_bytes(encoded[encoded.len() - 4..].try_into().unwrap()),
            expected
        );
    }
}
