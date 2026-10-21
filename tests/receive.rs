#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/receive_http.rs"]
mod receive_http;

use git_zero::{
    Error, ObjectFormat, ObjectId,
    http::receive::{self, Capabilities, ControlDecoder, ControlEvent, Limits, Report},
    io::{SliceWriter, Write},
    pktline::{self, Packet},
};

const LIMITS: Limits = Limits {
    max_bytes: 1_048_576,
    max_commands: 16,
    max_shallow: 16,
    max_options: 16,
};

struct Bytes(Vec<u8>);
impl Write for Bytes {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

fn capabilities() -> Capabilities {
    Capabilities::REPORT_STATUS
        | Capabilities::REPORT_STATUS_V2
        | Capabilities::DELETE_REFS
        | Capabilities::OFS_DELTA
        | Capabilities::ATOMIC
        | Capabilities::PUSH_OPTIONS
        | Capabilities::SIDE_BAND_64K
        | Capabilities::QUIET
}

fn id(format: ObjectFormat, byte: u8) -> ObjectId {
    ObjectId::from_bytes(format, &[byte; 32][..format.id_length()]).unwrap()
}

#[test]
fn ancestry_requires_commits_and_shares_a_finite_budget_between_updates() {
    use git_zero::{ObjectKind, http::receive::Ancestry};
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let old = id(format, 1);
        let mut walk = Ancestry::new(format, 3);
        assert_eq!(
            walk.visit(old, ObjectKind::Commit),
            Err(Error::InvalidRequest)
        );
        walk.start(old).unwrap();
        assert_eq!(walk.visit(old, ObjectKind::Tree), Err(Error::InvalidObject));
        let other = if format == ObjectFormat::Sha1 {
            ObjectFormat::Sha256
        } else {
            ObjectFormat::Sha1
        };
        assert_eq!(
            walk.visit(id(other, 1), ObjectKind::Commit),
            Err(Error::InvalidObjectId)
        );
        assert!(!walk.visit(id(format, 2), ObjectKind::Commit).unwrap());
        assert!(walk.visit(old, ObjectKind::Commit).unwrap());
        assert_eq!(
            walk.visit(old, ObjectKind::Commit),
            Err(Error::InvalidRequest)
        );
        walk.start(old).unwrap();
        assert!(!walk.visit(id(format, 3), ObjectKind::Commit).unwrap());
        assert_eq!(
            walk.visit(old, ObjectKind::Commit),
            Err(Error::LimitExceeded)
        );
        assert_eq!(walk.start(old), Err(Error::LimitExceeded));
        assert_eq!(walk.start(id(format, 4)), Err(Error::LimitExceeded));
    }
}

#[test]
fn strict_receive_identity_rules_match_configured_git_at_chunk_boundaries() {
    use git_zero::{ObjectKind, http::receive::FsckHeaders};
    let cases = [
        ("A <a@b> 0 +0000", true),
        (" <a@b> 0 +0000", true),
        ("<a@b> 0 +0000", false),
        ("x", false),
        ("A<a@b> 0 +0000", false),
        ("A >x <a@b> 0 +0000", false),
        ("A <a<b> 0 +0000", false),
        ("A <a@b>0 +0000", false),
        ("A <a@b> 00 +0000", false),
        ("A <a@b> 01 +0000", false),
        ("A <a@b> 0\t+0000", false),
        ("A <a@b> \t1 +9999", true),
        ("A <a@b> 9223372036854775807 +0000", true),
        ("A <a@b> 9223372036854775808 +0000", false),
        ("A <> 1 -9999", true),
        ("A <a@b> -1 +0000", false),
        ("A <a@b> 1 +00000", false),
    ];
    for (identity, accepted) in cases {
        for (kind, body) in [
            (
                ObjectKind::Commit,
                format!(
                    "tree {}\nauthor {identity}\ncommitter A <a@b> 0 +0000\n\nx\n",
                    "1".repeat(40)
                ),
            ),
            (
                ObjectKind::Commit,
                format!(
                    "tree {}\nauthor A <a@b> 0 +0000\ncommitter {identity}\n\nx\n",
                    "1".repeat(40)
                ),
            ),
            (
                ObjectKind::Tag,
                format!(
                    "object {}\ntype blob\ntag v1\ntagger {identity}\n\nx\n",
                    "1".repeat(40)
                ),
            ),
        ] {
            for chunk in [1, 2, 7, 64, body.len()] {
                allocations::without_allocations(|| {
                    let mut policy = FsckHeaders::new(kind).unwrap();
                    let result = body
                        .as_bytes()
                        .chunks(chunk)
                        .try_for_each(|bytes| policy.update(bytes));
                    assert_eq!(
                        result.and_then(|()| policy.finish()).is_ok(),
                        accepted,
                        "{identity}"
                    );
                    if result.is_err() {
                        assert_eq!(policy.update(b""), Err(Error::InvalidObject));
                        assert_eq!(policy.finish(), Err(Error::InvalidObject));
                    }
                });
            }
        }
    }
}

#[test]
fn strict_receive_headers_preserve_historical_taggers_and_stream_long_names() {
    use git_zero::{ObjectKind, http::receive::FsckHeaders};
    let prefix = format!("tree {}\nauthor ", "1".repeat(40));
    allocations::without_allocations(|| {
        let mut policy = FsckHeaders::new(ObjectKind::Commit).unwrap();
        policy.update(prefix.as_bytes()).unwrap();
        for _ in 0..1024 {
            policy.update(&[b'x'; 4096]).unwrap();
        }
        policy
            .update(b" <a@b> 1 +0000\ncommitter A <a@b> 0 +0000\n\nmessage")
            .unwrap();
        policy.finish().unwrap();
        assert_eq!(policy.update(b"\0"), Err(Error::InvalidObject));
    });
    for body in [
        b"object 1\ntype blob\ntag old\n\nmessage\0".as_slice(),
        b"object 1\ntype blob\ntag old\n",
    ] {
        let mut policy = FsckHeaders::new(ObjectKind::Tag).unwrap();
        for byte in body {
            policy.update(core::slice::from_ref(byte)).unwrap();
        }
        policy.finish().unwrap();
    }
    for body in [
        b"tree 1\nauthor x\ncommitter x\n\nx".as_slice(),
        b"tree 1\nauthor A <a@b> 0 +0000\nauthor A <a@b> 0 +0000\n\nx",
        b"object 1\ntype blob\ntag old\ntagger A <a@b> 0 +0000",
    ] {
        let kind = if body.starts_with(b"tree") {
            ObjectKind::Commit
        } else {
            ObjectKind::Tag
        };
        let mut policy = FsckHeaders::new(kind).unwrap();
        assert_eq!(
            policy.update(body).and_then(|()| policy.finish()),
            Err(Error::InvalidObject)
        );
    }
}

#[test]
fn strict_tree_policy_classifies_platform_aliases_without_allocations() {
    use git_zero::http::receive::check_tree_entry;
    for name in [
        b".git".as_slice(),
        b".git. ",
        b"GIT~1",
        b".git:stream",
        b"a\\.git\\payload",
        ".gi\u{200c}t".as_bytes(),
        b".git\xff",
    ] {
        allocations::without_allocations(|| {
            assert_eq!(
                check_tree_entry(name, 0o100644, false),
                Err(Error::InvalidFileName)
            )
        });
    }
    for name in [
        b".gitmodules".as_slice(),
        b".GITMODULES. ",
        b"gitmod~1",
        b"gitmod~4",
        b"GI7EBA~1",
        b"GI7EB~10",
        b".gitmodules:stream",
        b"a\\.gitmodules",
        ".gi\u{200c}tmodules".as_bytes(),
    ] {
        allocations::without_allocations(|| {
            assert_eq!(check_tree_entry(name, 0o100664, false).unwrap().bits(), 1);
            assert_eq!(
                check_tree_entry(name, 0o120000, false),
                Err(Error::InvalidObject)
            );
            assert_eq!(
                check_tree_entry(name, 0o040000, false),
                Err(Error::InvalidObject)
            );
        });
    }
    for name in [
        b".gitattributes".as_slice(),
        b"gitatt~1",
        b"GI7D29~1",
        ".gi\u{200c}tattributes".as_bytes(),
    ] {
        assert_eq!(check_tree_entry(name, 0o100644, false).unwrap().bits(), 2);
        assert_eq!(
            check_tree_entry(name, 0o040000, false),
            Err(Error::InvalidObject)
        );
        assert_eq!(check_tree_entry(name, 0o120000, false).unwrap().bits(), 0);
    }
    for name in [
        b"gitmod~5".as_slice(),
        b".gitmodulesx",
        b"a\\.gitmodules\\z",
        b"a\\.gitattributes",
        ".git\u{100}".as_bytes(),
    ] {
        assert_eq!(check_tree_entry(name, 0o100644, false).unwrap().bits(), 0);
    }
    for name in [b"gi~12345".as_slice(), b"~1234567"] {
        assert_eq!(check_tree_entry(name, 0o100644, false).unwrap().bits(), 3);
    }
    assert_eq!(
        check_tree_entry(b"file", 0o100664, false).unwrap().bits(),
        0
    );
    assert_eq!(
        check_tree_entry(b"file", 0o100644, true),
        Err(Error::InvalidObject)
    );
    assert_eq!(
        check_tree_entry(b"file", 0o100664, true),
        Err(Error::InvalidObject)
    );
    assert!(check_tree_entry(&[b'x'; 4096], 0o100644, false).is_ok());
    assert_eq!(
        check_tree_entry(&[b'x'; 4097], 0o100644, false),
        Err(Error::InvalidFileName)
    );
}

#[test]
fn attributes_fsck_bounds_lines_files_and_sticky_errors_without_allocations() {
    use git_zero::http::receive::FsckAttributes;
    for length in [0, 1, 2047, 2048, 2049] {
        let body = vec![b'x'; length];
        for chunk in [1, 7, 2048] {
            allocations::without_allocations(|| {
                let mut policy = FsckAttributes::new(body.len() as u64).unwrap();
                let result = body
                    .chunks(chunk)
                    .try_for_each(|bytes| policy.update(bytes));
                assert_eq!(result.and_then(|()| policy.finish()).is_ok(), length < 2048);
                if result.is_err() {
                    assert_eq!(policy.update(b""), Err(Error::InvalidObject));
                    assert_eq!(policy.finish(), Err(Error::InvalidObject));
                }
            });
        }
    }
    allocations::without_allocations(|| {
        assert!(FsckAttributes::new(100 * 1024 * 1024).is_ok());
        assert!(matches!(
            FsckAttributes::new(100 * 1024 * 1024 + 1),
            Err(Error::InvalidObject)
        ));
        assert!(matches!(
            FsckAttributes::new(u64::MAX),
            Err(Error::InvalidObject)
        ));
        let mut policy = FsckAttributes::new(0).unwrap();
        assert_eq!(policy.update(b"x"), Err(Error::InvalidObject));
        assert_eq!(policy.finish(), Err(Error::InvalidObject));
        let mut policy = FsckAttributes::new(1).unwrap();
        assert_eq!(policy.finish(), Err(Error::InvalidObject));
        policy.update(b"\n").unwrap();
        policy.finish().unwrap();
    });
    let body = [b"short\n\0".as_slice(), &[b'x'; 4096]].concat();
    for chunk in [1, 7, body.len()] {
        let mut policy = FsckAttributes::new(body.len() as u64).unwrap();
        for bytes in body.chunks(chunk) {
            policy.update(bytes).unwrap();
        }
        policy.finish().unwrap();
    }
}

#[test]
fn gitmodules_fsck_matches_configured_git_across_fragments() {
    use git_zero::http::receive::FsckModules;
    let cases: &[(&[u8], bool)] = &[
        (
            b"[submodule \"ok\"]\nurl = https://example.test/r\npath = sub\n",
            true,
        ),
        (b"[submodule \"../bad\"]\nurl = safe\n", false),
        (b"[submodule \"ok\"]\nurl = \"-bad\"\n", false),
        (b"[submodule \"ok\"]\npath = -bad\n", false),
        (b"[submodule \"ok\"]\nupdate = !echo bad\n", false),
        (b"[submodule \"ok\"]\nurl = \"-bad\n", true),
        (b"[broken\n[submodule \"ok\"]\nurl=-bad\n", true),
        (b"[submodule \"ok\"]\nurl=-bad\n[broken\n", false),
        (b"[submodule \"../bad\"]\n", true),
        (b"[submodule \"../bad\"]\nboolean\n", false),
        (b"[submodule \"ok\"]\nurl = ssh://host/%0a\n", true),
        (b"[submodule \"ok\"]\nurl = git://host/%0a\n", false),
        (b"[submodule \"ok\"]\nurl = git://host/%zz\n", true),
        (b"[submodule \"ok\"]\nurl = https://host/%zz\n", false),
        (b"[submodule \"ok\"]\nurl = https://host:65536\n", false),
        (b"[submodule \"ok\"]\nurl = https://host/a%0a/..\n", true),
        (b"[submodule \"ok\"]\nurl = https://host/a%0a/b/..\n", false),
        (b"[submodule \"ok\"]\nurl = ./a%0a:rest\n", true),
        (b"[submodule \"ok\"]\nurl = ./a%0a\n", false),
        (b"[submodule \"ok\"]\r\nurl = -\\\r\nbad\r\n", false),
        (b"[submodule \"ok\"]\nurl = -bad\\q\n", true),
        (b"[submodule \"ok\"]\nurl = -bad\\", false),
        (b"[submodule \"name.update\0hidden\"]\nurl = !bad\n", false),
        (b"[submodule \"name.UPDATE\0hidden\"]\nurl = !bad\n", true),
        (b"[submodule \"ok\"]\nurl = safe\0-bad\n", true),
        (b"[submodule \"ok\"]\nurl = -bad\0safe\n", false),
        (b"\xef\xbb\xbf[submodule \"ok\"]\nurl=-bad\n", true),
        (b"[submodule \"ok\"]\nurl=https://host\xff/%0a\n", true),
        (b"[submodule \"ok\"]\n\x0burl=-bad\n", true),
    ];
    for &(body, accepted) in cases {
        for chunk in [1, 2, 7, body.len()] {
            allocations::without_allocations(|| {
                let mut policy = FsckModules::new(body.len() as u64);
                let result = body
                    .chunks(chunk)
                    .try_for_each(|bytes| policy.update(bytes));
                assert_eq!(
                    result.and_then(|()| policy.finish()).is_ok(),
                    accepted,
                    "{body:?}"
                );
                if result.is_err() {
                    assert_eq!(policy.update(b""), Err(Error::InvalidObject));
                    assert_eq!(policy.finish(), Err(Error::InvalidObject));
                }
            });
        }
    }
}

#[test]
fn gitmodules_policy_streams_large_names_and_values_without_retaining_them() {
    use git_zero::http::receive::FsckModules;
    let prefix = b"[submodule \"";
    let middle = b"\"]\nurl = https://example.test/";
    let suffix = b"/ok\n";
    let large = 1024 * 4096;
    allocations::without_allocations(|| {
        let mut policy =
            FsckModules::new((prefix.len() + middle.len() + suffix.len() + 2 * large) as u64);
        policy.update(prefix).unwrap();
        for _ in 0..1024 {
            policy.update(&[b'x'; 4096]).unwrap();
        }
        policy.update(middle).unwrap();
        for _ in 0..1024 {
            policy.update(&[b'x'; 4096]).unwrap();
        }
        policy.update(suffix).unwrap();
        policy.finish().unwrap();
        policy.finish().unwrap();
        assert_eq!(policy.update(b"x"), Err(Error::InvalidObject));
        let mut incomplete = FsckModules::new(1);
        assert_eq!(incomplete.finish(), Err(Error::InvalidObject));
        assert_eq!(incomplete.update(b"x"), Err(Error::InvalidObject));
    });
}

#[test]
fn config_character_limit_does_not_discard_an_unsafe_pending_value() {
    use git_zero::http::receive::FsckModules;
    let prefix = b"[submodule \"ok\"]\nurl=-";
    let total = i32::MAX as u64 + 64;
    let block = [b'x'; 65536];
    allocations::without_allocations(|| {
        let mut policy = FsckModules::new(total);
        policy.update(prefix).unwrap();
        let mut left = total - prefix.len() as u64;
        while left != 0 {
            let length = left.min(block.len() as u64) as usize;
            policy.update(&block[..length]).unwrap();
            left -= length as u64;
        }
        assert_eq!(policy.finish(), Err(Error::InvalidObject));
        assert_eq!(policy.update(b""), Err(Error::InvalidObject));
    });
}

fn command(old: ObjectId, new: ObjectId, name: &[u8], caps: Option<&[u8]>) -> Vec<u8> {
    let mut line = Vec::new();
    line.extend_from_slice(old.to_hex().as_bytes());
    line.push(b' ');
    line.extend_from_slice(new.to_hex().as_bytes());
    line.push(b' ');
    line.extend_from_slice(name);
    if let Some(caps) = caps {
        line.push(0);
        line.extend_from_slice(caps);
    }
    line
}

fn packets(lines: &[&[u8]]) -> Vec<u8> {
    let mut output = Bytes(vec![]);
    for line in lines {
        pktline::write(&mut output, line).unwrap();
    }
    output.0.extend_from_slice(b"0000");
    output.0
}

#[derive(Debug, Eq, PartialEq)]
enum Event {
    Shallow(ObjectId),
    Command(ObjectId, ObjectId, Vec<u8>),
    Options,
    Option(Vec<u8>),
    End(bool),
}

fn read_control(
    bytes: &[u8],
    format: ObjectFormat,
    offered: Capabilities,
    limits: Limits,
    fragment: usize,
) -> git_zero::Result<(Vec<Event>, usize, Capabilities)> {
    let mut storage = [0; pktline::MAX_LENGTH];
    let mut decoder = ControlDecoder::new(&mut storage, format, offered, limits)?;
    let mut offset = 0;
    let mut events = vec![];
    for _ in 0..bytes.len() * 2 + 4 {
        let end = (offset + fragment).min(bytes.len());
        let step = decoder.read(&bytes[offset..end], end == bytes.len())?;
        offset += step.consumed;
        let event = match step.event {
            ControlEvent::NeedInput => {
                assert!(step.consumed > 0);
                continue;
            }
            ControlEvent::Shallow(id) => Event::Shallow(id),
            ControlEvent::Command(command) => {
                Event::Command(command.old, command.new, command.name.to_vec())
            }
            ControlEvent::Options => Event::Options,
            ControlEvent::PushOption(option) => Event::Option(option.to_vec()),
            ControlEvent::End { pack_required } => {
                events.push(Event::End(pack_required));
                return Ok((events, offset, decoder.capabilities()));
            }
        };
        events.push(event);
    }
    panic!("control decoder did not finish");
}

#[test]
fn fragmented_controls_preserve_binary_refs_options_and_the_pack_suffix() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let zero = format.zero_id();
        let old = id(format, 1);
        let new = id(format, 2);
        let mut caps =
            b" report-status-v2 side-band-64k atomic push-options quiet agent=git/2.53.0 ".to_vec();
        caps.extend_from_slice(format.capability());
        let mut shallow = b"shallow ".to_vec();
        shallow.extend_from_slice(old.to_hex().as_bytes());
        shallow.push(b'\n');
        let first = command(zero, new, b"refs/heads/\xff", Some(&caps));
        let second = command(old, zero, b"refs/tags/old", None);
        let mut bytes = packets(&[&shallow, &first, &second]);
        bytes.extend_from_slice(&packets(&[b"ci.skip", b""]));
        let control_bytes = bytes.len();
        bytes.extend_from_slice(b"PACKopaque bytes are not control data");
        let expected = vec![
            Event::Shallow(old),
            Event::Command(zero, new, b"refs/heads/\xff".to_vec()),
            Event::Command(old, zero, b"refs/tags/old".to_vec()),
            Event::Options,
            Event::Option(b"ci.skip".to_vec()),
            Event::Option(vec![]),
            Event::End(true),
        ];
        for fragment in 1..=bytes.len() {
            let (events, offset, selected) =
                read_control(&bytes, format, capabilities(), LIMITS, fragment).unwrap();
            assert_eq!(events, expected);
            assert_eq!(offset, control_bytes);
            assert!(selected.contains(Capabilities::ATOMIC));
            assert!(selected.contains(Capabilities::REPORT_STATUS_V2));
            assert!(!selected.contains(Capabilities::REPORT_STATUS));
        }
    }
}

#[test]
fn deletion_only_and_no_op_controls_do_not_request_a_pack() {
    let format = ObjectFormat::Sha1;
    let deletion = command(
        id(format, 1),
        format.zero_id(),
        b"refs/heads/main",
        Some(b"report-status"),
    );
    let (events, offset, _) =
        read_control(&packets(&[&deletion]), format, capabilities(), LIMITS, 1).unwrap();
    assert_eq!(events.last(), Some(&Event::End(false)));
    assert_eq!(offset, deletion.len() + 8);
    assert_eq!(
        read_control(b"0000", format, capabilities(), LIMITS, 1)
            .unwrap()
            .0,
        [Event::End(false)]
    );
    assert_eq!(
        read_control(
            &packets(&[&deletion]),
            format,
            Capabilities::REPORT_STATUS,
            LIMITS,
            1
        ),
        Err(Error::Unsupported)
    );
}

#[test]
fn partial_packets_truncated_flushes_and_unnegotiated_features_fail() {
    let format = ObjectFormat::Sha1;
    let update = command(
        format.zero_id(),
        id(format, 2),
        b"refs/heads/main",
        Some(b" report-status object-format=sha1"),
    );
    let bytes = packets(&[&update]);
    for length in 0..bytes.len() {
        assert!(read_control(&bytes[..length], format, capabilities(), LIMITS, 1).is_err());
    }
    for prefix in [b"0001".as_slice(), b"0002", b"0003", b"fff1", b"zzzz"] {
        assert_eq!(
            read_control(prefix, format, capabilities(), LIMITS, 1),
            Err(Error::InvalidPacket)
        );
    }
    for caps in [
        b"atomic".as_slice(),
        b"unknown",
        b"report-status report-status",
        b"object-format=sha256",
        b"agent=",
        b"agent=x\0y",
    ] {
        let line = command(
            format.zero_id(),
            id(format, 1),
            b"refs/heads/main",
            Some(caps),
        );
        assert!(
            read_control(
                &packets(&[&line]),
                format,
                Capabilities::REPORT_STATUS,
                LIMITS,
                3
            )
            .is_err()
        );
    }
    let first = command(format.zero_id(), id(format, 1), b"refs/heads/a", None);
    let late_caps = command(
        format.zero_id(),
        id(format, 2),
        b"refs/heads/b",
        Some(b"report-status"),
    );
    assert_eq!(
        read_control(
            &packets(&[&first, &late_caps]),
            format,
            capabilities(),
            LIMITS,
            7
        ),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn budgets_and_packet_storage_have_independent_terminal_errors() {
    let format = ObjectFormat::Sha1;
    let line = command(
        format.zero_id(),
        id(format, 1),
        b"refs/heads/main",
        Some(b"report-status"),
    );
    let bytes = packets(&[&line]);
    let exact = Limits {
        max_bytes: bytes.len() as u64,
        max_commands: 1,
        ..LIMITS
    };
    read_control(&bytes, format, capabilities(), exact, 1).unwrap();
    for limits in [
        Limits {
            max_bytes: bytes.len() as u64 - 1,
            ..exact
        },
        Limits {
            max_commands: 0,
            ..exact
        },
    ] {
        assert_eq!(
            read_control(&bytes, format, capabilities(), limits, 1),
            Err(Error::LimitExceeded)
        );
    }
    let mut storage = [0; 4];
    let mut decoder = ControlDecoder::new(&mut storage, format, capabilities(), LIMITS).unwrap();
    assert_eq!(decoder.read(&bytes, true), Err(Error::BufferTooSmall));
    assert_eq!(decoder.read(b"0000", true), Err(Error::BufferTooSmall));
    let mut storage = [0; 1024];
    let mut decoder = ControlDecoder::new(&mut storage, format, capabilities(), LIMITS).unwrap();
    assert_eq!(decoder.read(b"0", true), Err(Error::InvalidPacket));
    assert_eq!(decoder.read(&bytes, true), Err(Error::InvalidPacket));
}

#[test]
fn option_and_shallow_budgets_and_order_are_enforced() {
    let format = ObjectFormat::Sha1;
    let mut shallow = b"shallow ".to_vec();
    shallow.extend_from_slice(id(format, 1).to_hex().as_bytes());
    let line = command(
        format.zero_id(),
        id(format, 2),
        b"refs/heads/main",
        Some(b"push-options"),
    );
    let mut bytes = packets(&[&shallow, &line]);
    bytes.extend_from_slice(&packets(&[b"test"]));
    assert_eq!(
        read_control(
            &bytes,
            format,
            capabilities(),
            Limits {
                max_shallow: 0,
                ..LIMITS
            },
            1
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        read_control(
            &bytes,
            format,
            capabilities(),
            Limits {
                max_options: 0,
                ..LIMITS
            },
            1
        ),
        Err(Error::LimitExceeded)
    );
    for invalid in [b"has\nnewline".as_slice(), b"has\0nul", b"has\rcarriage"] {
        let mut bytes = packets(&[&line]);
        bytes.extend_from_slice(&packets(&[invalid]));
        assert_eq!(
            read_control(&bytes, format, capabilities(), LIMITS, 1),
            Err(Error::InvalidRequest)
        );
    }
    assert_eq!(
        read_control(
            &packets(&[&line, &shallow]),
            format,
            capabilities(),
            LIMITS,
            1
        ),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn smart_advertisements_preserve_raw_refs_and_validate_order_and_format() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut output = [0; 4096];
        let mut writer = SliceWriter::new(&mut output);
        let refs = [
            (id(format, 1), b"refs/heads/main".as_slice()),
            (id(format, 2), b"refs/tags/\xff".as_slice()),
        ];
        allocations::without_allocations(|| {
            receive::write_advertisement(format, capabilities(), refs, &mut writer).unwrap()
        });
        let mut bytes = writer.as_bytes();
        assert_eq!(
            pktline::read(&mut bytes).unwrap(),
            Some(Packet::Data(b"# service=git-receive-pack\n"))
        );
        assert_eq!(pktline::read(&mut bytes).unwrap(), Some(Packet::Flush));
        let Packet::Data(first) = pktline::read(&mut bytes).unwrap().unwrap() else {
            panic!()
        };
        let mut prefix = id(format, 1).to_hex().as_bytes().to_vec();
        prefix.extend_from_slice(b" refs/heads/main\0");
        assert!(first.starts_with(&prefix));
        assert!(
            first
                .windows(format.capability().len())
                .any(|bytes| bytes == format.capability())
        );
        assert!(first.ends_with(b" agent=git-zero\n"));
        let Packet::Data(second) = pktline::read(&mut bytes).unwrap().unwrap() else {
            panic!()
        };
        assert!(second.ends_with(b" refs/tags/\xff\n"));
        assert_eq!(pktline::read(&mut bytes).unwrap(), Some(Packet::Flush));
        assert!(bytes.is_empty());
        let mut writer = SliceWriter::new(&mut output);
        receive::write_advertisement(format, capabilities(), [], &mut writer).unwrap();
        assert!(
            writer
                .as_bytes()
                .windows(b" capabilities^{}\0".len())
                .any(|part| part == b" capabilities^{}\0")
        );
        assert_eq!(
            receive::write_advertisement(
                format,
                capabilities(),
                refs.into_iter().rev(),
                &mut SliceWriter::new(&mut output)
            ),
            Err(Error::InvalidReference)
        );
        assert_eq!(
            receive::write_advertisement(
                format,
                capabilities(),
                [(format.zero_id(), b"refs/heads/main".as_slice())],
                &mut SliceWriter::new(&mut output)
            ),
            Err(Error::InvalidReference)
        );
    }
}

#[test]
fn receive_creation_names_must_fit_the_future_first_advertisement() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut long = b"refs/heads/".to_vec();
        long.resize(65_375, b'a');
        git_zero::repository::validate_ref_name_bytes(&long).unwrap();
        let first = command(
            format.zero_id(),
            id(format, 1),
            b"refs/heads/z",
            Some(format.capability()),
        );
        let late = command(format.zero_id(), id(format, 2), &long, None);
        let bytes = packets(&[&first, &late]);
        assert!(matches!(
            read_control(&bytes, format, capabilities(), LIMITS, 4096),
            Err(Error::InvalidReference)
        ));
        // The actual offered set determines the bound: a smaller advertisement
        // can safely support this name, and deletions never publish a name.
        read_control(&bytes, format, Capabilities::default(), LIMITS, 4096).unwrap();
        let first = command(
            id(format, 1),
            format.zero_id(),
            b"refs/heads/z",
            Some(format.capability()),
        );
        let late = command(id(format, 2), format.zero_id(), &long, None);
        read_control(
            &packets(&[&first, &late]),
            format,
            capabilities(),
            LIMITS,
            4096,
        )
        .unwrap();
    }
}

fn demultiplex(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut wire = bytes;
    let mut result = (vec![], vec![]);
    while let Some(packet) = pktline::read(&mut wire).unwrap() {
        let Packet::Data(packet) = packet else {
            assert!(wire.is_empty());
            break;
        };
        match packet[0] {
            1 => result.0.extend_from_slice(&packet[1..]),
            2 => result.1.extend_from_slice(&packet[1..]),
            _ => panic!("unexpected channel"),
        }
    }
    result
}

#[test]
fn status_reports_frame_nested_packets_and_honor_quiet() {
    for selected in [
        Capabilities::REPORT_STATUS,
        Capabilities::REPORT_STATUS_V2 | Capabilities::SIDE_BAND_64K,
        Capabilities::REPORT_STATUS | Capabilities::SIDE_BAND_64K | Capabilities::QUIET,
    ] {
        let mut bytes = Bytes(vec![]);
        receive::write_progress(&mut bytes, selected, b"processing\n").unwrap();
        let mut report = Report::new(&mut bytes, selected, Ok(())).unwrap();
        report.reference(b"refs/heads/main", Ok(())).unwrap();
        report
            .reference(b"refs/tags/\xff", Err(b"stale ref"))
            .unwrap();
        report.finish().unwrap();
        let (status, progress) = if selected.contains(Capabilities::SIDE_BAND_64K) {
            demultiplex(&bytes.0)
        } else {
            (bytes.0, vec![])
        };
        assert_eq!(
            status,
            packets(&[
                b"unpack ok\n",
                b"ok refs/heads/main\n",
                b"ng refs/tags/\xff stale ref\n"
            ])
        );
        assert_eq!(
            !progress.is_empty(),
            selected.contains(Capabilities::SIDE_BAND_64K)
                && !selected.contains(Capabilities::QUIET)
        );
    }
}

#[test]
fn report_failures_and_long_progress_are_encoded_without_oversized_packets() {
    let mut bytes = Bytes(vec![]);
    let selected = Capabilities::REPORT_STATUS | Capabilities::SIDE_BAND_64K;
    receive::write_progress(&mut bytes, selected, &vec![b'a'; 2 * pktline::MAX_LENGTH]).unwrap();
    let mut report = Report::new(&mut bytes, selected, Err(b"bad pack")).unwrap();
    report
        .reference(b"refs/heads/main", Err(b"unpacker error"))
        .unwrap();
    report.finish().unwrap();
    let (status, progress) = demultiplex(&bytes.0);
    assert_eq!(progress.len(), 2 * pktline::MAX_LENGTH);
    assert_eq!(
        status,
        packets(&[b"unpack bad pack\n", b"ng refs/heads/main unpacker error\n"])
    );
    for invalid in [b"".as_slice(), b"ok", b"bad\nline", b"bad\0nul"] {
        assert!(
            Report::new(
                &mut Bytes(vec![]),
                Capabilities::REPORT_STATUS,
                Err(invalid)
            )
            .is_err()
        );
    }
}

#[test]
fn control_intake_and_reports_allocate_nothing() {
    let format = ObjectFormat::Sha1;
    let line = command(
        format.zero_id(),
        id(format, 1),
        b"refs/heads/main",
        Some(b"report-status"),
    );
    let bytes = packets(&[&line]);
    let mut storage = [0; 1024];
    let mut output = [0; 1024];
    allocations::without_allocations(|| {
        let mut decoder =
            ControlDecoder::new(&mut storage, format, capabilities(), LIMITS).unwrap();
        let mut offset = 0;
        loop {
            let end = (offset + 1).min(bytes.len());
            let step = decoder
                .read(&bytes[offset..end], end == bytes.len())
                .unwrap();
            offset += step.consumed;
            if let ControlEvent::End { pack_required } = step.event {
                assert!(pack_required);
                break;
            }
        }
        let mut writer = SliceWriter::new(&mut output);
        let mut report = Report::new(&mut writer, decoder.capabilities(), Ok(())).unwrap();
        report.reference(b"refs/heads/main", Ok(())).unwrap();
        report.finish().unwrap();
    });
}

#[test]
fn detached_advertisements_preserve_wire_bytes_across_fresh_sinks_and_reject_bad_state() {
    use receive::Advertisement;
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for populated in [false, true] {
            let refs = [
                (id(format, 1), b"refs/heads/a".as_slice()),
                (id(format, 2), b"refs/tags/\xff".as_slice()),
            ];
            let refs = if populated { &refs[..] } else { &refs[..0] };
            let mut expected = Bytes(vec![]);
            receive::write_advertisement(
                format,
                capabilities(),
                refs.iter().copied(),
                &mut expected,
            )
            .unwrap();
            let mut name = [0; 128];
            let mut advert = Advertisement::new(format, capabilities(), &mut name);
            let mut actual = Bytes(vec![]);
            let mut piece = [0; 512];
            let mut writer = SliceWriter::new(&mut piece);
            allocations::without_allocations(|| advert.start(&mut writer).unwrap());
            actual.0.extend_from_slice(writer.as_bytes());
            for &(id, name) in refs {
                let mut writer = SliceWriter::new(&mut piece);
                allocations::without_allocations(|| {
                    advert.reference(id, name, &mut writer).unwrap()
                });
                actual.0.extend_from_slice(writer.as_bytes());
            }
            let mut writer = SliceWriter::new(&mut piece);
            advert.finish(&mut writer).unwrap();
            actual.0.extend_from_slice(writer.as_bytes());
            assert_eq!(actual.0, expected.0);
            assert_eq!(
                advert.finish(&mut Bytes(vec![])),
                Err(Error::InvalidRequest)
            );
        }
    }
}

#[test]
fn detached_status_preserves_wire_bytes_across_fresh_sinks_and_rejects_bad_state() {
    use receive::Status;
    for selected in [
        Capabilities::default(),
        Capabilities::REPORT_STATUS,
        capabilities(),
    ] {
        let mut expected = Bytes(vec![]);
        let mut report = Report::new(&mut expected, selected, Err(b"invalid pack")).unwrap();
        report
            .reference(b"refs/heads/a", Err(b"unpacker error"))
            .unwrap();
        report.finish().unwrap();
        let mut encoder = Status::new(selected);
        assert_eq!(
            encoder.finish(&mut Bytes(vec![])),
            Err(Error::InvalidRequest)
        );
        let mut one = [0; 512];
        let mut two = [0; 512];
        let mut three = [0; 512];
        let mut a = SliceWriter::new(&mut one);
        let mut b = SliceWriter::new(&mut two);
        let mut c = SliceWriter::new(&mut three);
        allocations::without_allocations(|| {
            encoder.unpack(Err(b"invalid pack"), &mut a).unwrap();
            encoder
                .reference(b"refs/heads/a", Err(b"unpacker error"), &mut b)
                .unwrap();
            encoder.finish(&mut c).unwrap();
        });
        assert_eq!(
            [a.as_bytes(), b.as_bytes(), c.as_bytes()].concat(),
            expected.0
        );
        assert_eq!(
            encoder.reference(b"refs/heads/a", Ok(()), &mut Bytes(vec![])),
            Err(Error::InvalidRequest)
        );
    }
}

#[cfg(feature = "read")]
#[test]
fn external_catalog_requests_validate_then_revisit_typed_objects_without_allocations() {
    use git_zero::http::stored::{Command, ObjectRequest, Request};
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let wanted = id(format, 1);
        let have = id(format, 2);
        let mut want = b"want ".to_vec();
        want.extend_from_slice(wanted.to_hex().as_bytes());
        want.extend_from_slice(b" multi_ack_detailed side-band-64k ");
        want.extend_from_slice(format.capability());
        let mut body = packets(&[&want]);
        let mut output = Bytes(body);
        pktline::write_parts(&mut output, &[b"have ", have.to_hex().as_bytes(), b"\n"]).unwrap();
        pktline::write(&mut output, b"done\n").unwrap();
        body = output.0;
        allocations::without_allocations(|| {
            let request = Request::parse(&body, format, 0).unwrap();
            assert_eq!(request.command(), Command::Fetch);
            assert!(request.done());
            assert_eq!(request.band(), 2);
            let mut objects = request.objects();
            assert_eq!(objects.next(), Some(ObjectRequest::Want(wanted)));
            assert_eq!(objects.next(), Some(ObjectRequest::Have(have)));
            assert_eq!(objects.next(), None);
            request.require_complete().unwrap();
        });
        body.extend_from_slice(b"0000");
        assert!(Request::parse(&body, format, 0).is_err());
    }
}

#[cfg(feature = "read")]
#[test]
fn external_ls_refs_matches_raw_prefixes_and_preserves_symbolic_attributes() {
    use git_zero::http::stored::Request;
    let mut output = Bytes(vec![]);
    pktline::write(&mut output, b"command=ls-refs\n").unwrap();
    output.0.extend_from_slice(b"0001");
    output.0.extend_from_slice(&packets(&[
        b"ref-prefix refs/tags/\xff",
        b"symrefs",
        b"peel",
    ]));
    let request = Request::parse(&output.0, ObjectFormat::Sha1, 2).unwrap();
    let mut storage = [0; 512];
    let mut writer = SliceWriter::new(&mut storage);
    allocations::without_allocations(|| {
        request
            .reference(
                Some(id(ObjectFormat::Sha1, 1)),
                b"refs/heads/main",
                None,
                None,
                &mut writer,
            )
            .unwrap();
        assert!(writer.as_bytes().is_empty());
        request
            .reference(
                Some(id(ObjectFormat::Sha1, 2)),
                b"refs/tags/\xff",
                Some(b"refs/heads/main"),
                Some(id(ObjectFormat::Sha1, 3)),
                &mut writer,
            )
            .unwrap();
    });
    assert!(
        writer
            .as_bytes()
            .windows(b" refs/tags/\xff symref-target:refs/heads/main peeled:".len())
            .any(|bytes| bytes == b" refs/tags/\xff symref-target:refs/heads/main peeled:")
    );
}

#[cfg(feature = "read")]
#[test]
fn external_ref_prefix_index_preserves_unions_and_bounds_disjoint_prefixes() {
    use git_zero::http::stored::Request;
    let names: &[&[u8]] = &[
        b"HEAD",
        b"refs/heads/main",
        b"refs/heads/master",
        b"refs/tags/v1",
        b"refs/tags/\xff",
        b"refs/other",
        b"refs/heads",
    ];
    for prefixes in [
        vec![],
        vec![
            b"refs/heads/main".as_slice(),
            b"refs/heads/",
            b"refs/tags/\xff",
            b"refs/heads/",
        ],
        vec![b"refs/tags/".as_slice(), b"refs/heads/master"],
        vec![b"refs/tags/".as_slice(), b"", b"refs/heads/main"],
        vec![b"refs/heads/".as_slice(); 4097],
    ] {
        check_prefix_matches(&prefixes, names);
    }
    let prefixes: Vec<_> = (0..257)
        .map(|i| format!("refs/heads/{i:04}/").into_bytes())
        .collect();
    let prefixes: Vec<_> = prefixes.iter().map(Vec::as_slice).collect();
    check_prefix_matches(
        &prefixes[..256],
        &[
            b"refs/heads/0000/topic".as_slice(),
            b"refs/heads/0127/topic",
            b"refs/heads/0255/topic",
            b"refs/heads/0256/topic",
            b"refs/heads/0000",
        ],
    );
    assert!(matches!(
        Request::parse(&prefix_body(&prefixes), ObjectFormat::Sha1, 2),
        Err(Error::LimitExceeded)
    ));
}

#[cfg(feature = "read")]
fn prefix_body(prefixes: &[&[u8]]) -> Vec<u8> {
    let mut output = Bytes(vec![]);
    pktline::write(&mut output, b"command=ls-refs\n").unwrap();
    output.0.extend_from_slice(b"0001");
    for prefix in prefixes {
        pktline::write_parts(&mut output, &[b"ref-prefix ", prefix]).unwrap();
    }
    output.0.extend_from_slice(b"0000");
    output.0
}

#[cfg(feature = "read")]
fn check_prefix_matches(prefixes: &[&[u8]], names: &[&[u8]]) {
    use git_zero::http::stored::Request;
    let body = prefix_body(prefixes);
    let id = id(ObjectFormat::Sha1, 1);
    let mut storage = [0; 512];
    allocations::without_allocations(|| {
        let request = Request::parse(&body, ObjectFormat::Sha1, 2).unwrap();
        for name in names {
            let mut output = SliceWriter::new(&mut storage);
            request
                .reference(Some(id), name, None, None, &mut output)
                .unwrap();
            let expected =
                prefixes.is_empty() || prefixes.iter().any(|prefix| name.starts_with(prefix));
            assert_eq!(!output.as_bytes().is_empty(), expected, "{name:?}");
        }
    });
}

#[cfg(feature = "read")]
fn shallow_body(format: ObjectFormat, version: u8, depth: bool, done: bool) -> Vec<u8> {
    let mut output = Bytes(vec![]);
    if version == 2 {
        pktline::write(&mut output, b"command=fetch\n").unwrap();
        pktline::write(&mut output, format.capability()).unwrap();
        output.0.extend_from_slice(b"0001");
    }
    let suffix = if version == 2 {
        String::new()
    } else {
        format!(
            " shallow {}",
            core::str::from_utf8(format.capability()).unwrap()
        )
    };
    pktline::write(
        &mut output,
        format!("want {}{suffix}\n", id(format, 1)).as_bytes(),
    )
    .unwrap();
    if depth {
        pktline::write(&mut output, b"deepen 2\n").unwrap();
    }
    if version != 2 {
        output.0.extend_from_slice(b"0000");
    }
    if done {
        pktline::write(&mut output, b"done\n").unwrap();
    }
    if version == 2 {
        output.0.extend_from_slice(b"0000");
    }
    output.0
}

#[cfg(feature = "read")]
#[test]
fn paged_shallow_updates_match_protocol_sections_without_allocations() {
    use git_zero::http::stored::Request;
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for version in 0..=2 {
            for done in [false, true] {
                for count in 0..=2 {
                    check_shallow_updates(format, version, done, count);
                }
                let body = shallow_body(format, version, false, done);
                let request = Request::parse(&body, format, version).unwrap();
                let mut output = SliceWriter::new(&mut []);
                request
                    .shallow_updates()
                    .unwrap()
                    .finish(&mut output)
                    .unwrap();
            }
        }
    }
}

#[cfg(feature = "read")]
fn shallow_response(format: ObjectFormat, version: u8, enabled: bool, count: u8) -> Vec<u8> {
    let mut expected = Bytes(vec![]);
    if !enabled {
        return expected.0;
    }
    if version == 2 && count > 0 {
        pktline::write(&mut expected, b"shallow-info\n").unwrap();
    }
    for index in 0..count {
        let marker = if index == 0 { "shallow" } else { "unshallow" };
        pktline::write(
            &mut expected,
            format!("{marker} {}\n", id(format, index + 1)).as_bytes(),
        )
        .unwrap();
    }
    if version != 2 || count > 0 {
        expected
            .0
            .extend_from_slice(if version == 2 { b"0001" } else { b"0000" });
    }
    expected.0
}

#[cfg(feature = "read")]
fn check_shallow_updates(format: ObjectFormat, version: u8, done: bool, count: u8) {
    use git_zero::http::stored::Request;
    let body = shallow_body(format, version, true, done);
    let enabled = version != 2 || done;
    let expected = shallow_response(format, version, enabled, count);
    let mut storage = [0; 512];
    allocations::without_allocations(|| {
        let request = Request::parse(&body, format, version).unwrap();
        let mut updates = request.shallow_updates().unwrap();
        let mut output = SliceWriter::new(&mut storage);
        if enabled {
            for index in 0..count {
                updates
                    .change(id(format, index + 1), index != 0, &mut output)
                    .unwrap();
            }
        }
        updates.finish(&mut output).unwrap();
        assert_eq!(output.as_bytes(), expected);
        assert_eq!(updates.finish(&mut output), Err(Error::InvalidRequest));
    });
}

#[cfg(feature = "read")]
#[test]
fn shallow_update_errors_are_sticky_and_never_finish_partial_sections() {
    use git_zero::http::stored::Request;
    for version in 0..=2 {
        let format = ObjectFormat::Sha1;
        let body = shallow_body(format, version, true, true);
        let request = Request::parse(&body, format, version).unwrap();
        for invalid in [format.zero_id(), id(ObjectFormat::Sha256, 1)] {
            let mut updates = request.shallow_updates().unwrap();
            let mut output = SliceWriter::new(&mut []);
            assert_eq!(
                updates.change(invalid, false, &mut output),
                Err(Error::InvalidObjectId)
            );
            assert_eq!(updates.finish(&mut output), Err(Error::InvalidObjectId));
        }
        let mut updates = request.shallow_updates().unwrap();
        let mut storage = [0; 3];
        let mut output = SliceWriter::new(&mut storage);
        assert_eq!(
            updates.change(id(format, 1), false, &mut output),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(
            updates.change(id(format, 1), false, &mut output),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(updates.finish(&mut output), Err(Error::BufferTooSmall));
    }
}
