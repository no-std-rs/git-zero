#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error,
    io::SliceWriter,
    zlib::{self, InflateLimits, InflateStatus, Inflater, WINDOW_SIZE},
};
use std::{
    io::Write,
    process::{Command, Stdio},
};

const LIMITS: InflateLimits = InflateLimits {
    max_output: 2_000_000,
    max_ratio: 2048,
    ratio_slack: 0,
};

fn python(script: &str, input: &[u8], args: &[&str]) -> Vec<u8> {
    let mut child = Command::new("python3")
        .args(["-c", script])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain stdout while writing large fixtures, avoiding pipe-capacity deadlocks.
    let mut stdin = child.stdin.take().unwrap();
    let result = std::thread::scope(|scope| {
        scope.spawn(move || stdin.write_all(input).unwrap());
        child.wait_with_output().unwrap()
    });
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn compress(input: &[u8], level: &str, strategy: &str) -> Vec<u8> {
    python(
        "import sys,zlib\nc=zlib.compressobj(int(sys.argv[1]),zlib.DEFLATED,15,8,int(sys.argv[2]))\nsys.stdout.buffer.write(c.compress(sys.stdin.buffer.read())+c.flush())",
        input,
        &[level, strategy],
    )
}

fn decode(
    data: &[u8],
    input_size: usize,
    output_size: usize,
    limits: InflateLimits,
) -> git_zero::Result<Vec<u8>> {
    let mut window = [0xa5; WINDOW_SIZE];
    let mut inflater = Inflater::new(&mut window, limits);
    let mut output = vec![0; output_size];
    let mut result = Vec::new();
    let mut consumed = 0;
    let mut end = data.len().min(input_size);
    loop {
        let progress = inflater.update(&data[consumed..end], &mut output, end == data.len())?;
        assert!(progress.consumed <= end - consumed);
        assert!(progress.written <= output_size);
        result.extend_from_slice(&output[..progress.written]);
        consumed += progress.consumed;
        assert_eq!(inflater.total_input(), consumed as u64);
        assert_eq!(inflater.total_output(), result.len() as u64);
        match progress.status {
            InflateStatus::Finished => {
                assert_eq!(consumed, data.len());
                return Ok(result);
            }
            InflateStatus::NeedInput => {
                assert_eq!(consumed, end);
                assert!(end < data.len(), "EOF should have failed");
                end = data.len().min(end + input_size);
            }
            InflateStatus::NeedOutput => assert!(progress.consumed != 0 || progress.written != 0),
        }
    }
}

#[test]
fn python_stored_fixed_and_dynamic_streams_survive_chunk_boundaries() {
    let binary: Vec<u8> = (0..131_073).map(|index| (index % 251) as u8).collect();
    for body in [b"".as_slice(), b"hello\n", &binary] {
        for (level, strategy, block) in [("0", "0", 0), ("6", "4", 1), ("9", "0", 2)] {
            let encoded = compress(body, level, strategy);
            if body.len() > 1000 {
                assert_eq!((encoded[2] >> 1) & 3, block, "fixture block type");
            }
            for (input, output) in [
                (1, 1),
                (2, 17),
                (7, 257),
                (encoded.len(), 32_768),
                (13, 65_537),
            ] {
                assert_eq!(decode(&encoded, input, output, LIMITS).unwrap(), body);
            }
        }
    }
}

#[test]
fn mixed_blocks_and_full_window_back_references_match_python() {
    let mut seed = 0x1234_5678_u32;
    let prefix: Vec<u8> = (0..32_768)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let mut body = prefix.clone();
    body.extend_from_slice(&prefix);
    body.extend_from_slice(&vec![b'a'; 70_000]);
    let encoded = python(
        "import sys,zlib\nb=sys.stdin.buffer.read(); c=zlib.compressobj(9)\nout=c.compress(b[:32768])+c.flush(zlib.Z_SYNC_FLUSH)\nout+=c.compress(b[32768:])+c.flush()\nsys.stdout.buffer.write(out)",
        &body,
        &[],
    );
    for (input, output) in [(1, 3), (31, 1), (257, 65536)] {
        assert_eq!(decode(&encoded, input, output, LIMITS).unwrap(), body);
    }
}

#[test]
fn stored_encoder_round_trip_and_incremental_operations_do_not_allocate() {
    let body = [0xff; 65_537];
    let mut encoded = [0; 66_000];
    let mut writer = SliceWriter::new(&mut encoded);
    zlib::write_parts(&mut writer, &[&body[..5551], &body[5551..]]).unwrap();
    let encoded = writer.as_bytes();
    assert_eq!(
        decode(encoded, encoded.len(), body.len(), LIMITS).unwrap(),
        body
    );
    assert_eq!(
        python(
            "import sys,zlib\nsys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))",
            encoded,
            &[]
        ),
        body
    );
    let mut window = [0; WINDOW_SIZE];
    allocations::without_allocations(|| {
        let mut inflater = Inflater::new(&mut window, LIMITS);
        for _ in 0..2 {
            let (mut consumed, mut written) = (0, 0);
            let mut scratch = [0; 17];
            loop {
                let end = encoded.len().min(consumed + 3);
                let progress = inflater
                    .update(&encoded[consumed..end], &mut scratch, end == encoded.len())
                    .unwrap();
                assert_eq!(
                    &scratch[..progress.written],
                    &body[written..written + progress.written]
                );
                consumed += progress.consumed;
                written += progress.written;
                if progress.status == InflateStatus::Finished {
                    break;
                }
            }
            assert_eq!(written, body.len());
            inflater.reset(LIMITS);
        }
    });
    assert!(std::mem::size_of::<Inflater<'_>>() < 2048);
}

#[test]
fn exact_output_budget_empty_chunks_and_trailing_bytes() {
    for body in [b"".as_slice(), b"hello"] {
        let encoded = compress(body, "9", "0");
        let limits = InflateLimits {
            max_output: body.len() as u64,
            ..LIMITS
        };
        let mut window = [0; WINDOW_SIZE];
        let mut inflater = Inflater::new(&mut window, limits);
        let idle = inflater.update(&[], &mut [], false).unwrap();
        assert_eq!(idle.status, InflateStatus::NeedInput);
        assert_eq!((idle.consumed, idle.written), (0, 0));
        let mut input = encoded.clone();
        input.extend_from_slice(b"next pack entry");
        let mut output = [0xcc; 16];
        let first = inflater.update(&input, &mut [], true).unwrap();
        let progress = inflater
            .update(&input[first.consumed..], &mut output[..body.len()], true)
            .unwrap();
        assert_eq!(progress.status, InflateStatus::Finished);
        assert_eq!(first.consumed + progress.consumed, encoded.len());
        assert_eq!(progress.written, body.len());
        assert_eq!(&output[..body.len()], body);
        assert!(output[body.len()..].iter().all(|&byte| byte == 0xcc));
        let again = inflater
            .update(b"another stream", &mut output, true)
            .unwrap();
        assert_eq!(
            (again.consumed, again.written, again.status),
            (0, 0, InflateStatus::Finished)
        );
    }
}

#[test]
fn limits_stop_before_the_first_excess_byte_and_are_chunk_independent() {
    for level in ["0", "9"] {
        check_chunk_independent_limits(&compress(&[b'a'; 100_000], level, "0"));
    }
}

fn check_chunk_independent_limits(encoded: &[u8]) {
    for limits in [
        InflateLimits {
            max_output: 100,
            ..LIMITS
        },
        InflateLimits {
            max_output: 0,
            ..LIMITS
        },
        InflateLimits {
            max_output: 99_999,
            max_ratio: 1,
            ratio_slack: 100,
        },
        InflateLimits {
            max_ratio: 0,
            ratio_slack: 99,
            ..LIMITS
        },
        InflateLimits {
            max_output: 32_769,
            ..LIMITS
        },
        InflateLimits {
            max_ratio: 0,
            ratio_slack: 65_536,
            ..LIMITS
        },
    ] {
        let mut totals = Vec::new();
        for (input_size, output_size) in [(1, 1), (3, 17), (encoded.len(), 200_000)] {
            let mut window = [0; WINDOW_SIZE];
            let mut inflater = Inflater::new(&mut window, limits);
            let mut output = vec![0xcc; output_size];
            let mut consumed = 0;
            loop {
                let end = encoded.len().min(consumed + input_size);
                match inflater.update(&encoded[consumed..end], &mut output, end == encoded.len()) {
                    Ok(progress) => {
                        assert_ne!(
                            progress.status,
                            InflateStatus::Finished,
                            "expected a budget failure"
                        );
                        consumed += progress.consumed;
                    }
                    Err(error) => {
                        assert_eq!(error, Error::LimitExceeded);
                        assert!(inflater.total_output() <= limits.max_output);
                        assert!(
                            inflater.total_output()
                                <= inflater
                                    .total_input()
                                    .saturating_mul(u64::from(limits.max_ratio))
                                    .saturating_add(limits.ratio_slack)
                        );
                        assert_eq!(inflater.update(&[], &mut [], false), Err(error));
                        totals.push((inflater.total_input(), inflater.total_output()));
                        break;
                    }
                }
            }
        }
        assert!(totals.windows(2).all(|pair| pair[0] == pair[1]));
    }
    let limits = InflateLimits {
        max_output: u64::MAX,
        max_ratio: u32::MAX,
        ratio_slack: u64::MAX,
    };
    assert_eq!(decode(encoded, 1, 17, limits).unwrap().len(), 100_000);
    assert_eq!(
        decode(encoded, encoded.len(), 100_000, limits)
            .unwrap()
            .len(),
        100_000
    );
}

#[test]
fn every_truncated_prefix_and_bad_wrapper_is_rejected() {
    for encoded in [
        compress(b"hello\n", "0", "0"),
        compress(b"hello\n", "6", "4"),
        compress(&[b'a'; 1000], "9", "0"),
    ] {
        for length in 0..encoded.len() {
            assert_eq!(
                decode(&encoded[..length], 1, 3, LIMITS),
                Err(Error::InvalidZlib)
            );
        }
        for index in [0, 1, encoded.len() - 1] {
            let mut corrupt = encoded.clone();
            corrupt[index] ^= 1;
            assert_eq!(decode(&corrupt, 1, 3, LIMITS), Err(Error::InvalidZlib));
        }
    }
    for header in [[0x77, 0x85], [0x88, 0x98], [0x78, 0x98]] {
        assert_eq!(decode(&header, 1, 1, LIMITS), Err(Error::InvalidZlib));
    }
    assert_eq!(decode(&[0x78, 0x20], 1, 1, LIMITS), Err(Error::Unsupported));
    assert_eq!(
        decode(&[0x78, 0x01, 0x07], 1, 1, LIMITS),
        Err(Error::InvalidZlib)
    );
    assert_eq!(
        decode(&[0x78, 0x01, 1, 1, 0, 0xff, 0xff], 1, 1, LIMITS),
        Err(Error::InvalidZlib)
    );
}

#[test]
fn reset_cannot_reuse_history_from_a_previous_stream() {
    let good = compress(b"previous history", "9", "0");
    let mut window = [0; WINDOW_SIZE];
    let mut inflater = Inflater::new(&mut window, LIMITS);
    let mut output = [0; 100];
    assert_eq!(
        inflater.update(&good, &mut output, true).unwrap().status,
        InflateStatus::Finished
    );
    inflater.reset(LIMITS);
    let mut bad = Deflate::fixed();
    bad.fixed_symbol(257); // length 3
    bad.bits(0, 5); // distance 1, before any output
    assert_eq!(
        inflater.update(&bad.bytes, &mut output, true),
        Err(Error::InvalidZlib)
    );
    inflater.reset(LIMITS);
    assert_eq!(
        inflater.update(&good, &mut output, true).unwrap().status,
        InflateStatus::Finished
    );
}

/// Test-only bit writer. Huffman codes are emitted most-significant bit first;
/// numeric fields use DEFLATE's least-significant-bit-first convention.
struct Deflate {
    bytes: Vec<u8>,
    used: u8,
}

impl Deflate {
    fn new() -> Self {
        Self {
            bytes: vec![0x78, 0x01],
            used: 0,
        }
    }
    fn bits(&mut self, value: u32, count: u8) {
        for bit in 0..count {
            if self.used == 0 {
                self.bytes.push(0);
            }
            *self.bytes.last_mut().unwrap() |= (((value >> bit) & 1) as u8) << self.used;
            self.used = (self.used + 1) % 8;
        }
    }
    fn fixed() -> Self {
        let mut result = Self::new();
        result.bits(3, 3); // final, fixed Huffman
        result
    }
    fn fixed_symbol(&mut self, symbol: u16) {
        let (code, count) = match symbol {
            0..=143 => (0x30 + u32::from(symbol), 8),
            144..=255 => (0x190 + u32::from(symbol - 144), 9),
            256..=279 => (u32::from(symbol - 256), 7),
            _ => (0xc0 + u32::from(symbol - 280), 8),
        };
        self.bits(code.reverse_bits() >> (32 - count), count);
    }
    fn dynamic(literal: u16, distance: u16, code_lengths: &[u8; 19]) -> Self {
        let mut result = Self::new();
        result.bits(5, 3); // final, dynamic Huffman
        result.bits(u32::from(literal - 257), 5);
        result.bits(u32::from(distance - 1), 5);
        result.bits(15, 4); // all 19 code-length codes
        for index in [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ] {
            result.bits(u32::from(code_lengths[index]), 3);
        }
        result
    }
    fn huffman(&mut self, symbol: usize, lengths: &[u8]) {
        let length = lengths[symbol];
        assert_ne!(length, 0);
        let shorter: u32 = lengths
            .iter()
            .filter(|&&bits| bits != 0 && bits < length)
            .map(|&bits| 1 << (length - bits))
            .sum();
        let earlier = lengths[..symbol]
            .iter()
            .filter(|&&bits| bits == length)
            .count() as u32;
        self.bits((shorter + earlier).reverse_bits() >> (32 - length), length);
    }
    fn trailer(&mut self, body: &[u8]) {
        self.used = 0;
        let (mut a, mut b) = (1_u32, 0_u32);
        for &byte in body {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        self.bytes.extend_from_slice(&((b << 16) | a).to_be_bytes());
    }
}

#[test]
fn maximum_distance_overlap_and_reserved_fixed_codes() {
    let mut fixture = Deflate::fixed();
    let body: Vec<u8> = (0..WINDOW_SIZE).map(|index| (index % 251) as u8).collect();
    for &byte in &body {
        fixture.fixed_symbol(u16::from(byte));
    }
    fixture.fixed_symbol(285); // length 258
    fixture.bits(29_u32.reverse_bits() >> 27, 5);
    fixture.bits(8191, 13); // distance 32768
    fixture.fixed_symbol(257); // length 3
    fixture.bits(0, 5); // distance 1: overlapping copy
    fixture.fixed_symbol(256);
    let mut expected = body.clone();
    expected.extend_from_slice(&body[..258]);
    expected.extend_from_slice(&[body[257]; 3]);
    fixture.trailer(&expected);
    assert_eq!(
        python(
            "import sys,zlib\nsys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))",
            &fixture.bytes,
            &[]
        ),
        expected
    );
    assert_eq!(decode(&fixture.bytes, 1, 1, LIMITS).unwrap(), expected);
    for code in [286, 287] {
        let mut bad = Deflate::fixed();
        bad.fixed_symbol(code);
        assert_eq!(decode(&bad.bytes, 1, 1, LIMITS), Err(Error::InvalidZlib));
    }
    for code in [30_u32, 31] {
        let mut bad = Deflate::fixed();
        bad.fixed_symbol(257);
        bad.bits(code.reverse_bits() >> 27, 5);
        assert_eq!(decode(&bad.bytes, 1, 1, LIMITS), Err(Error::InvalidZlib));
    }
}

fn plain_dynamic(lengths: &[u8], distance: &[u8], body: &[u8]) -> Vec<u8> {
    let mut code = [0; 19];
    code[..16].fill(4);
    let mut fixture = Deflate::dynamic(lengths.len() as u16, distance.len() as u16, &code);
    for &length in lengths.iter().chain(distance) {
        fixture.huffman(usize::from(length), &code);
    }
    for &byte in body {
        fixture.huffman(usize::from(byte), lengths);
    }
    if lengths[256] != 0 {
        fixture.huffman(256, lengths);
    }
    fixture.trailer(body);
    fixture.bytes
}

#[test]
fn dynamic_single_codes_empty_distances_and_fifteen_bit_symbols() {
    let mut single = [0; 257];
    single[256] = 1;
    let mut pair = single;
    pair[usize::from(b'a')] = 1;
    let mut deep = [0; 257];
    for (index, length) in deep[..15].iter_mut().enumerate() {
        *length = index as u8 + 1;
    }
    deep[256] = 15;
    for (lengths, distance, body) in [
        (single.as_slice(), [0].as_slice(), b"".as_slice()),
        (&single, &[1], b""),
        (&pair, &[0], b"aaaa"),
        (&deep, &[0], &[0, 1, 7, 13, 14]),
    ] {
        let encoded = plain_dynamic(lengths, distance, body);
        assert_eq!(
            python(
                "import sys,zlib\nsys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))",
                &encoded,
                &[]
            ),
            body
        );
        assert_eq!(decode(&encoded, 1, 1, LIMITS).unwrap(), body);
    }
}

#[test]
fn code_length_repeats_cross_the_literal_distance_boundary() {
    let mut code = [0; 19];
    code[0] = 2;
    code[1] = 2;
    for index in [2, 16, 17, 18] {
        code[index] = 3;
    }
    let mut fixture = Deflate::dynamic(258, 2, &code);
    // 256 zero lengths, using both repeat forms, then four one-bit lengths.
    fixture.huffman(17, &code);
    fixture.bits(7, 3); // 10 zeros
    fixture.huffman(18, &code);
    fixture.bits(127, 7); // 138 zeros
    fixture.huffman(18, &code);
    fixture.bits(97, 7); // 108 zeros
    fixture.huffman(1, &code);
    fixture.huffman(16, &code);
    fixture.bits(0, 2); // one literal + two distances
    fixture.bits(0, 1); // EOB, canonical code 0
    fixture.trailer(b"");
    assert!(
        python(
            "import sys,zlib\nsys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))",
            &fixture.bytes,
            &[]
        )
        .is_empty()
    );
    assert_eq!(decode(&fixture.bytes, 1, 1, LIMITS).unwrap(), b"");
}

#[test]
fn malformed_dynamic_alphabets_repeats_and_missing_end_codes_fail() {
    let mut invalid = Vec::new();
    invalid.push(Deflate::dynamic(257, 1, &[0; 19]).bytes);
    let mut code = [0; 19];
    code[0] = 2; // incomplete code-length alphabet
    invalid.push(Deflate::dynamic(257, 1, &code).bytes);
    code[0] = 1;
    code[1] = 1;
    code[2] = 1; // oversubscribed alphabet
    invalid.push(Deflate::dynamic(257, 1, &code).bytes);
    code.fill(0);
    code[0] = 1;
    code[16] = 1;
    let mut repeat_first = Deflate::dynamic(257, 1, &code);
    repeat_first.huffman(16, &code);
    invalid.push(repeat_first.bytes);
    code.fill(0);
    code[0] = 1;
    code[18] = 1;
    let mut overrun = Deflate::dynamic(257, 1, &code);
    for _ in 0..2 {
        overrun.huffman(18, &code);
        overrun.bits(127, 7);
    }
    invalid.push(overrun.bytes);
    let mut lengths = [0; 257];
    invalid.push(plain_dynamic(&lengths, &[0], b"")); // no EOB
    lengths[256] = 2; // incomplete literal tree
    invalid.push(plain_dynamic(&lengths, &[0], b""));
    lengths[0] = 1;
    lengths[1] = 1;
    lengths[256] = 1; // oversubscribed
    invalid.push(plain_dynamic(&lengths, &[0], b""));
    lengths.fill(0);
    lengths[256] = 1;
    invalid.push(plain_dynamic(&lengths, &[1, 1, 1], b"")); // oversubscribed distances
    invalid.push(plain_dynamic(&lengths, &[2], b"")); // incomplete distances
    let mut excessive_literal = Deflate::new();
    excessive_literal.bits(5, 3);
    excessive_literal.bits(31, 5);
    excessive_literal.bits(0, 9);
    invalid.push(excessive_literal.bytes);
    for encoded in invalid {
        for (input, output) in [(1, 1), (encoded.len(), 17)] {
            assert_eq!(
                decode(&encoded, input, output, LIMITS),
                Err(Error::InvalidZlib)
            );
        }
        python(
            "import sys,zlib\ntry: zlib.decompress(sys.stdin.buffer.read())\nexcept zlib.error: pass\nelse: raise AssertionError('accepted malformed stream')",
            &encoded,
            &[],
        );
    }
}

#[test]
fn real_git_loose_object_inflates_with_small_caller_buffers() {
    let directory = std::env::temp_dir().join(format!("git-zero-inflate-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let git = || {
        let mut command =
            Command::new(std::env::var_os("GIT_ZERO_TEST_GIT").unwrap_or_else(|| "git".into()));
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(&directory);
        command
    };
    assert!(
        git()
            .args(["init", "-q", "--bare", "--object-format=sha1"])
            .status()
            .unwrap()
            .success()
    );
    let body: Vec<u8> = (0..131_073).map(|index| (index % 251) as u8).collect();
    let mut child = git()
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&body).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    let id = String::from_utf8(result.stdout).unwrap();
    let id = id.trim();
    let encoded = std::fs::read(directory.join("objects").join(&id[..2]).join(&id[2..])).unwrap();
    let mut expected = format!("blob {}\0", body.len()).into_bytes();
    expected.extend_from_slice(&body);
    assert_eq!(decode(&encoded, 1, 17, LIMITS).unwrap(), expected);
    let mut staging = vec![0; expected.len()];
    let mut window = [0; WINDOW_SIZE];
    let object = git_zero::client::decode_loose(
        &encoded,
        git_zero::ObjectId::from_hex(id.as_bytes()).unwrap(),
        &mut staging,
        &mut window,
        LIMITS,
    )
    .unwrap();
    assert_eq!(object.data, body);
}
