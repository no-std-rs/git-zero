#[path = "../common/native_parser.rs"]
mod native;

#[test]
fn native_framing_seed_replay_and_deterministic_mutations() {
    let seeds: &[&[u8]] = &[
        include_bytes!("../../../../fuzz/corpus/native_http/identity"),
        include_bytes!("../../../../fuzz/corpus/native_http/chunked"),
        include_bytes!("../../../../fuzz/corpus/native_http/conflict"),
        include_bytes!("../../../../fuzz/corpus/native_http/truncated"),
    ];
    for &seed in seeds {
        native::exercise(seed);
        for length in 0..seed.len() {
            native::exercise(&seed[..length]);
        }
        let mut mutated = seed.to_vec();
        for index in 0..seed.len() {
            for value in [0, b'\r', b'\n', b':', b'0', b'F', 0xff] {
                mutated[index] = value;
                native::exercise(&mutated);
            }
            mutated[index] = seed[index];
        }
    }
    let mut data = [0u8; 512];
    let mut state = 0x6a09e667f3bcc909u64;
    for length in 0..512 {
        for byte in &mut data[..length] {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        native::exercise(&data[..length]);
    }
}
