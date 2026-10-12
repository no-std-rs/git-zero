mod fuzz_support;

use git_zero::{Error, pktline};

type Target = (&'static str, fn(&[u8]));

#[test]
fn packet_prefixes_require_four_hex_digits() {
    for data in [b"+000".as_slice(), b"+004", b"+00a123456", b"-000"] {
        let mut remaining = data;
        assert_eq!(
            pktline::read(&mut remaining),
            Err(Error::InvalidPacket),
            "{data:?}"
        );
        assert_eq!(remaining, data);
    }
    for index in 0..4 {
        for byte in 0..=255_u8 {
            if byte.is_ascii_hexdigit() {
                continue;
            }
            let mut data = *b"0004";
            data[index] = byte;
            let mut remaining = data.as_slice();
            assert_eq!(pktline::read(&mut remaining), Err(Error::InvalidPacket));
            assert_eq!(remaining, data);
        }
    }
    let mut remaining = b"000Aabcdef".as_slice();
    assert_eq!(
        pktline::read(&mut remaining),
        Ok(Some(pktline::Packet::Data(b"abcdef")))
    );
    assert!(remaining.is_empty());
}

#[test]
fn packet_length_boundary_is_enforced() {
    for length in [
        pktline::MAX_LENGTH - 1,
        pktline::MAX_LENGTH,
        pktline::MAX_LENGTH + 1,
    ] {
        let mut data = format!("{length:04x}").into_bytes();
        data.resize(length, 0xff);
        let mut remaining = data.as_slice();
        if length <= pktline::MAX_LENGTH {
            assert_eq!(
                pktline::read(&mut remaining),
                Ok(Some(pktline::Packet::Data(&data[4..])))
            );
            assert!(remaining.is_empty());
        } else {
            assert_eq!(pktline::read(&mut remaining), Err(Error::InvalidPacket));
            assert_eq!(remaining, data);
        }
    }
}

#[test]
fn seeded_fuzz_targets_replay_and_mutate_the_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus");
    let targets: [Target; 4] = [
        ("packets", fuzz_support::packets),
        ("responses", fuzz_support::responses),
        ("routing", fuzz_support::routing),
        ("graph", fuzz_support::graph),
    ];
    for (name, target) in targets {
        let mut paths: Vec<_> = std::fs::read_dir(root.join(name))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "missing corpus for {name}");
        for path in paths {
            let seed = std::fs::read(&path).unwrap();
            assert!(seed.len() <= fuzz_support::MAX_INPUT);
            replay(&seed, target);
        }
        random_inputs(target);
    }
}

fn replay(seed: &[u8], target: fn(&[u8])) {
    target(seed);
    for length in 0..seed.len() {
        target(&seed[..length]);
    }
    let mut mutated = seed.to_vec();
    for index in 0..seed.len() {
        for byte in [0, b'+', b'\n', b'0', b'f', 0xff] {
            mutated[index] = byte;
            target(&mutated);
        }
        mutated[index] = seed[index];
    }
}

fn random_inputs(target: fn(&[u8])) {
    // Preserve the fixed xorshift sequence while reusing one bounded buffer.
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    let mut storage = [0; 512];
    for length in 0..storage.len() {
        let data = &mut storage[..length];
        for byte in data.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        target(data);
    }
}
