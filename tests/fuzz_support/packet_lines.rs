use super::MAX_INPUT;
use git_zero::pktline::{self, Packet};

pub fn packets(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let mut remaining = data;
    loop {
        let before = remaining;
        match pktline::read(&mut remaining) {
            Ok(Some(packet)) => {
                assert!(before[..4].iter().all(u8::is_ascii_hexdigit));
                let consumed = before.len() - remaining.len();
                match packet {
                    Packet::Flush => {
                        assert_eq!(&before[..4], b"0000");
                        assert_eq!(consumed, 4);
                    }
                    Packet::Data(body) => {
                        assert_eq!(consumed, body.len() + 4);
                        assert_eq!(body, &before[4..consumed]);
                        assert!(consumed <= pktline::MAX_LENGTH);
                    }
                }
                assert_eq!(remaining, &before[consumed..]);
                assert!(remaining.len() < before.len(), "parser must make progress");
            }
            Ok(None) => {
                assert!(before.is_empty());
                break;
            }
            Err(_) => {
                assert_eq!(remaining, before, "failed parse must not consume input");
                break;
            }
        }
    }
}
