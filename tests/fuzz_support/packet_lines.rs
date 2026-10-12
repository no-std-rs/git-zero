use super::MAX_INPUT;
use git_zero::pktline::{self, Packet};

/// Check a sequence of packet lines with at most 4096 input bytes.
///
/// Accepted frames must match their independently formatted wire length and
/// preserve the unconsumed suffix. A flush encodes zero but consumes four bytes.
/// Errors leave the input unchanged; only empty input returns `None`. Full
/// 65520-byte boundary cases run separately in the ordinary core tests.
pub fn packets(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let mut remaining = data;
    let mut before = data;
    while let Some(packet) = next_packet(&mut remaining) {
        check_packet(packet, before, remaining);
        before = remaining;
    }
}

fn next_packet<'a>(remaining: &mut &'a [u8]) -> Option<Packet<'a>> {
    let before = *remaining;
    let result = pktline::read(remaining);
    if let Ok(Some(packet)) = result {
        return Some(packet);
    }
    assert_eq!(*remaining, before, "failed parse must not consume input");
    assert_eq!(
        result.is_ok(),
        before.is_empty(),
        "only empty input yields None"
    );
    None
}

fn check_packet(packet: Packet<'_>, before: &[u8], remaining: &[u8]) {
    // A flush consumes four bytes but encodes zero; data encodes its full length.
    let (length, prefix) = match packet {
        Packet::Flush => (4, 0),
        Packet::Data(body) => {
            let length = body.len() + 4;
            assert_eq!(body, &before[4..length]);
            (length, length)
        }
    };
    assert!(length <= pktline::MAX_LENGTH);
    assert!(before[..4].eq_ignore_ascii_case(format!("{prefix:04x}").as_bytes()));
    assert_eq!(remaining, &before[length..]);
}
