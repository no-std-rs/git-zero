mod fuzz_support;

type Target = (&'static str, fn(&[u8]), &'static [&'static [u8]]);

const TARGETS: [Target; 9] = [
    (
        "indexes",
        fuzz_support::indexes,
        &[
            include_bytes!("../fuzz/corpus/indexes/sha1-large"),
            include_bytes!("../fuzz/corpus/indexes/sha256-large"),
            include_bytes!("../fuzz/corpus/indexes/duplicates"),
            include_bytes!("../fuzz/corpus/indexes/bad-offset"),
            b"",
            b"\x01",
        ],
    ),
    (
        "receive",
        fuzz_support::receive,
        &[
            include_bytes!("../fuzz/corpus/receive/sha1"),
            include_bytes!("../fuzz/corpus/receive/sha256"),
            include_bytes!("../fuzz/corpus/receive/options"),
            include_bytes!("../fuzz/corpus/receive/shallow"),
            b"0000",
            b"0003",
        ],
    ),
    (
        "loose",
        fuzz_support::loose,
        &[
            include_bytes!("../fuzz/corpus/loose/blob"),
            include_bytes!("../fuzz/corpus/loose/empty"),
            include_bytes!("../fuzz/corpus/loose/tag"),
            include_bytes!("../fuzz/corpus/loose/bad-length"),
            include_bytes!("../fuzz/corpus/loose/leading-zero"),
        ],
    ),
    (
        "entries",
        fuzz_support::entries,
        &[
            include_bytes!("../fuzz/corpus/entries/plain"),
            include_bytes!("../fuzz/corpus/entries/offset"),
            include_bytes!("../fuzz/corpus/entries/reference"),
            include_bytes!("../fuzz/corpus/entries/plain-sha256"),
            include_bytes!("../fuzz/corpus/entries/offset-sha256"),
            include_bytes!("../fuzz/corpus/entries/reference-sha256"),
            include_bytes!("../fuzz/corpus/entries/delta"),
            include_bytes!("../fuzz/corpus/entries/reserved"),
        ],
    ),
    (
        "inflation",
        fuzz_support::inflation,
        &[
            include_bytes!("../fuzz/corpus/inflation/stored"),
            include_bytes!("../fuzz/corpus/inflation/fixed"),
            include_bytes!("../fuzz/corpus/inflation/dynamic"),
            include_bytes!("../fuzz/corpus/inflation/empty"),
            include_bytes!("../fuzz/corpus/inflation/gzip"),
            include_bytes!("../fuzz/corpus/inflation/expansion"),
            include_bytes!("../fuzz/corpus/inflation/bad-checksum"),
        ],
    ),
    (
        "packets",
        fuzz_support::packets,
        &[
            include_bytes!("../fuzz/corpus/packets/bad-hex"),
            include_bytes!("../fuzz/corpus/packets/data"),
            include_bytes!("../fuzz/corpus/packets/empty"),
            include_bytes!("../fuzz/corpus/packets/flush"),
            include_bytes!("../fuzz/corpus/packets/signed"),
            include_bytes!("../fuzz/corpus/packets/truncated"),
            include_bytes!("../fuzz/corpus/packets/v2-controls"),
        ],
    ),
    (
        "responses",
        fuzz_support::responses,
        &[
            include_bytes!("../fuzz/corpus/responses/bad-pack"),
            include_bytes!("../fuzz/corpus/responses/dumb"),
            include_bytes!("../fuzz/corpus/responses/empty"),
            include_bytes!("../fuzz/corpus/responses/pack"),
            include_bytes!("../fuzz/corpus/responses/smart"),
            include_bytes!("../fuzz/corpus/responses/sha256-smart"),
            include_bytes!("../fuzz/corpus/responses/mixed-smart"),
            include_bytes!("../fuzz/corpus/responses/mixed-dumb"),
            include_bytes!("../fuzz/corpus/responses/symbolic-head"),
            include_bytes!("../fuzz/corpus/responses/upload-pack"),
        ],
    ),
    (
        "routing",
        fuzz_support::routing,
        &[
            include_bytes!("../fuzz/corpus/routing/bad-escape"),
            include_bytes!("../fuzz/corpus/routing/bad-packet"),
            include_bytes!("../fuzz/corpus/routing/dumb"),
            include_bytes!("../fuzz/corpus/routing/encoded-ref"),
            include_bytes!("../fuzz/corpus/routing/head"),
            include_bytes!("../fuzz/corpus/routing/loose"),
            include_bytes!("../fuzz/corpus/routing/pack"),
            include_bytes!("../fuzz/corpus/routing/negotiation"),
            include_bytes!("../fuzz/corpus/routing/sideband"),
            include_bytes!("../fuzz/corpus/routing/v2-refs"),
            include_bytes!("../fuzz/corpus/routing/v2-fetch"),
            include_bytes!("../fuzz/corpus/routing/v2-filter"),
            include_bytes!("../fuzz/corpus/routing/v0-shallow"),
            include_bytes!("../fuzz/corpus/routing/v2-shallow"),
            include_bytes!("../fuzz/corpus/routing/conflicting-capabilities"),
            include_bytes!("../fuzz/corpus/routing/push"),
            include_bytes!("../fuzz/corpus/routing/smart"),
            include_bytes!("../fuzz/corpus/routing/tiny-pack"),
            include_bytes!("../fuzz/corpus/routing/wrong-method"),
        ],
    ),
    (
        "graph",
        fuzz_support::graph,
        &[
            include_bytes!("../fuzz/corpus/graph/bad-commit"),
            include_bytes!("../fuzz/corpus/graph/bad-mode"),
            include_bytes!("../fuzz/corpus/graph/commit"),
            include_bytes!("../fuzz/corpus/graph/duplicate"),
            include_bytes!("../fuzz/corpus/graph/empty-tree"),
            include_bytes!("../fuzz/corpus/graph/tree"),
            include_bytes!("../fuzz/corpus/graph/tag"),
            include_bytes!("../fuzz/corpus/graph/tree-sha256"),
            include_bytes!("../fuzz/corpus/graph/commit-sha256"),
            include_bytes!("../fuzz/corpus/graph/tag-sha256"),
            include_bytes!("../fuzz/corpus/graph/truncated-id"),
        ],
    ),
];

#[test]
fn seeded_fuzz_targets_replay_and_mutate_the_corpus() {
    for (name, target, seeds) in TARGETS {
        assert!(!seeds.is_empty(), "missing corpus for {name}");
        for seed in seeds {
            assert!(seed.len() <= fuzz_support::MAX_INPUT);
            replay(seed, target);
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
            *byte = random_byte(&mut state);
        }
        target(data);
    }
}

fn random_byte(state: &mut u64) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state as u8
}
