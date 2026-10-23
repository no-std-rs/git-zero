//! Revision request parsing, wire responses and cursor lifecycle contracts.
use super::*;

fn packets(lines: &[String]) -> Vec<u8> {
    let mut result = Vec::new();
    for line in lines {
        result.extend_from_slice(format!("{:04x}{}\n", line.len() + 5, line).as_bytes());
    }
    result
}

fn body(fixture: &Fixture, version: u8, arguments: &[&str], done: bool) -> Vec<u8> {
    let filter = version != 2 && arguments.iter().any(|line| line.starts_with("filter "));
    let want = format!(
        "want {}{}",
        fixture.ids[D],
        if filter { " filter" } else { "" }
    );
    let mut lines = if version == 2 {
        vec![
            "command=fetch".into(),
            format!("object-format={}", fixture.format.name()),
        ]
    } else {
        vec![]
    };
    let mut result = packets(&lines);
    if version == 2 {
        result.extend_from_slice(b"0001");
    }
    lines.clear();
    lines.push(want);
    lines.extend(arguments.iter().map(|argument| (*argument).into()));
    result.extend(packets(&lines));
    if version != 2 {
        result.extend_from_slice(b"0000");
    }
    if done {
        result.extend(packets(&["done".into()]));
    }
    if version == 2 {
        result.extend_from_slice(b"0000");
    }
    result
}

fn request(body: &[u8], version: u8) -> Request<'_> {
    Request {
        method: "POST",
        path: "git-upload-pack",
        query: None,
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: match version {
            1 => Some("version=1"),
            2 => Some("version=2"),
            _ => None,
        },
        body,
    }
}

fn canonical_response(bytes: &[u8], version: u8) -> Vec<u8> {
    if version != 2 {
        return bytes.to_vec();
    }
    let mut input = bytes;
    let mut result = Vec::new();
    let mut packed = false;
    while !input.is_empty() {
        if input.starts_with(b"0001") {
            result.extend_from_slice(b"DELIMITER");
            input = &input[4..];
            continue;
        }
        if input.starts_with(b"0002") {
            result.extend_from_slice(b"END");
            input = &input[4..];
            continue;
        }
        let packet = git_zero::pktline::read(&mut input).unwrap().unwrap();
        match packet {
            git_zero::pktline::Packet::Data(line) if packed => {
                assert_eq!(line.first(), Some(&1));
                result.extend_from_slice(&line[1..]);
            }
            git_zero::pktline::Packet::Data(line) => {
                packed = line == b"packfile\n";
                result.extend_from_slice(line);
            }
            git_zero::pktline::Packet::Flush => result.extend_from_slice(b"FLUSH"),
        }
    }
    result
}

fn check_streamed_revision(
    fixture: &Fixture,
    repository: &ValidatedRepository<'_, '_, Fixture>,
    version: u8,
    arguments: &[&str],
) {
    let body = body(fixture, version, arguments, true);
    let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
    let mut bytes = [0; 8192];
    let mut output = SliceWriter::new(&mut bytes);
    allocations::without_allocations(|| {
        http::serve_with_revisions(
            repository,
            request(&body, version),
            &mut buffers.storage(100_000, limits()),
            &mut output,
        )
    })
    .unwrap();
    let mut control = [0; 2048];
    let mut encoder = allocations::without_allocations(|| {
        http::stream::ResponseEncoder::with_revision_storage(
            repository,
            request(&body, version),
            buffers.storage(100_000, limits()),
            &mut control,
        )
    })
    .unwrap();
    let actual = pull::collect(|output| encoder.read(repository, output), 1);
    assert_eq!(
        canonical_response(&actual, version),
        canonical_response(output.as_bytes(), version)
    );
    let mut encoder = http::stream::ResponseEncoder::with_revision_storage(
        repository,
        request(&body, version),
        buffers.storage(100_000, limits()),
        &mut control,
    )
    .unwrap();
    encoder.cancel();
    fixture.bytes.set(0);
    assert_eq!(
        encoder.read(repository, &mut [0; 1]),
        Err(Error::InvalidRequest)
    );
    assert_eq!(fixture.bytes.get(), 0);
}

fn check_revision_discovery(repository: &ValidatedRepository<'_, '_, Fixture>, version: u8) {
    let discovery = Request {
        method: "GET",
        path: "info/refs",
        query: Some("service=git-upload-pack"),
        content_type: None,
        git_protocol: request(&[], version).git_protocol,
        body: &[],
    };
    let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    http::serve_with_revisions(
        repository,
        discovery,
        &mut buffers.storage(100_000, limits()),
        &mut output,
    )
    .unwrap();
    if version != 2 {
        assert!(
            output
                .as_bytes()
                .windows(12)
                .any(|bytes| bytes == b"deepen-since")
        );
    }
}

fn check_revision_protocol(
    fixture: &Fixture,
    repository: &ValidatedRepository<'_, '_, Fixture>,
    version: u8,
) {
    for args in [
        vec!["deepen-since 1700000200"],
        vec!["deepen-not symcut"],
        vec!["deepen-not tagC", "deepen-since 0"],
        vec!["deepen-not blob"],
    ] {
        check_streamed_revision(fixture, repository, version, &args);
    }
    check_revision_discovery(repository, version);
}

#[test]
fn opt_in_caps_buffered_and_one_byte_cursors_match_and_cancel_is_terminal() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let mut metadata = [None; N];
        let mut ids = [0; N];
        let repository = validate(&fixture, &mut metadata, &mut ids);
        for version in [0, 1, 2] {
            check_revision_protocol(&fixture, &repository, version);
        }
    }
}

fn check_revision_error(
    fixture: &Fixture,
    repository: &ValidatedRepository<'_, '_, Fixture>,
    version: u8,
    arguments: &[&str],
    revision_limits: RevisionLimits,
    expected: Error,
) {
    let body = body(fixture, version, arguments, true);
    let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    fixture.bytes.set(0);
    assert_eq!(
        http::serve_with_revisions(
            repository,
            request(&body, version),
            &mut buffers.storage(100_000, revision_limits),
            &mut output
        )
        .map(|_| ()),
        Err(expected)
    );
    assert!(output.is_empty());
    assert_eq!(fixture.bytes.get(), 0);
}

#[test]
fn invalid_refs_conflicts_limits_and_disabled_paths_produce_no_output_or_body_reads() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut metadata = [None; N];
    let mut ids = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut ids);
    for version in [0, 1, 2] {
        for (args, expected) in [
            (vec!["deepen-not ambiguous"], Error::InvalidRequest),
            (vec!["deepen-not refs/heads/private"], Error::InvalidRequest),
            (vec!["deepen-not main^"], Error::InvalidRequest),
            (vec!["deepen 1", "deepen-since 0"], Error::InvalidRequest),
            (vec!["deepen-since 08"], Error::InvalidRequest),
            (vec!["deepen-since 0", "filter tree:1"], Error::Unsupported),
            (
                vec!["deepen-since 0", "filter object:type=commit"],
                Error::Unsupported,
            ),
            (
                vec!["deepen-since 18446744073709551616"],
                Error::InvalidRequest,
            ),
        ] {
            check_revision_error(&fixture, &repository, version, &args, limits(), expected);
        }
        check_revision_error(
            &fixture,
            &repository,
            version,
            &["deepen-not cutoff"],
            RevisionLimits { max_exclusions: 0 },
            Error::LimitExceeded,
        );
        let body = body(&fixture, version, &["deepen-since invalid"], true);
        let mut buffers = http::SelectionBuffers::<8, N, 1, 256>::new();
        let mut bytes = [0; 4096];
        let mut output = SliceWriter::new(&mut bytes);
        assert_eq!(
            http::serve_with_selection(
                &repository,
                request(&body, version),
                &mut buffers.storage(100_000),
                &mut output
            )
            .map(|_| ()),
            Err(Error::Unsupported)
        );
        assert!(output.is_empty());
    }
}

#[test]
fn probes_end_at_boundary_flush_and_ordinary_acks_need_no_revision_storage() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut metadata = [None; N];
    let mut ids = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut ids);
    for version in [0, 1] {
        let body = body(&fixture, version, &["deepen-since 1700000200"], false);
        let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
        let mut bytes = [0; 256];
        let mut output = SliceWriter::new(&mut bytes);
        http::serve_with_revisions(
            &repository,
            request(&body, version),
            &mut buffers.storage(100_000, limits()),
            &mut output,
        )
        .unwrap();
        let expected = packets(&[format!("shallow {}", fixture.ids[B])]);
        assert_eq!(output.as_bytes(), [expected.as_slice(), b"0000"].concat());
    }
    for version in [0, 1, 2] {
        let body = body(&fixture, version, &[], false);
        let mut buffers = RevisionBuffers::<8, 0, 0, 0>::new();
        let mut bytes = [0; 128];
        let mut output = SliceWriter::new(&mut bytes);
        fixture.bytes.set(0);
        http::serve_with_revisions(
            &repository,
            request(&body, version),
            &mut buffers.storage(0, RevisionLimits { max_exclusions: 0 }),
            &mut output,
        )
        .unwrap();
        assert_eq!(fixture.bytes.get(), 0);
    }
}

#[test]
fn insufficient_control_storage_cannot_prepare_a_success_response() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut by_id);
    for version in [0, 1, 2] {
        let body = body(&fixture, version, &["deepen-since 1700000200"], true);
        let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
        let result = http::stream::ResponseEncoder::with_revision_storage(
            &repository,
            request(&body, version),
            buffers.storage(100_000, limits()),
            &mut [],
        );
        assert!(matches!(result, Err(Error::BufferTooSmall)));
    }
}

fn check_capability_boundary(format: ObjectFormat, unborn: bool, overflow: usize) {
    use git_zero::{
        Head, RefRepository, Reference, Snapshot,
        repository::{head_ref_limit, ref_name_limit},
    };
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let snapshot = Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap();
    let extra_caps = b" deepen-since deepen-not".len();
    let limit = if unborn {
        ref_name_limit(format)
            - 1
            - format.capability().len()
            - git_zero::repository::UPLOAD_CAPABILITIES.len()
    } else {
        head_ref_limit(format)
    };
    let name = format!(
        "refs/heads/{}",
        "x".repeat(limit - 11 - extra_caps + overflow)
    );
    let refs = [Reference::direct(&name, snapshot.head())];
    let head = if unborn {
        Head::Unborn("refs/heads/new")
    } else {
        Head::Symbolic {
            target: &name,
            id: snapshot.head(),
        }
    };
    let source = RefRepository::new(&snapshot, head, &refs).unwrap();
    let mut metadata = [None; 3];
    let mut indices = [0; 3];
    let repository = ValidatedRepository::new(
        &source,
        &mut metadata,
        &mut indices,
        &mut [0; 1],
        &mut [0; 256],
    )
    .unwrap();
    let mut buffers = RevisionBuffers::<1, 3, 1, 256>::new();
    let mut bytes = vec![0; 131072];
    let mut output = SliceWriter::new(&mut bytes);
    let discovery = Request {
        method: "GET",
        path: "info/refs",
        query: Some("service=git-upload-pack"),
        content_type: None,
        git_protocol: None,
        body: &[],
    };
    let result = allocations::without_allocations(|| {
        http::serve_with_revisions(
            &repository,
            discovery,
            &mut buffers.storage(0, limits()),
            &mut output,
        )
    });
    assert_eq!(
        result.map(|_| ()),
        if overflow == 0 {
            Ok(())
        } else {
            Err(Error::LimitExceeded)
        }
    );
    assert_eq!(output.is_empty(), overflow != 0);
}

#[test]
fn revision_capability_advertisement_preflights_exact_packet_limits() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for unborn in [false, true] {
            for overflow in [0, 1] {
                check_capability_boundary(format, unborn, overflow);
            }
        }
    }
}

#[test]
fn v2_ack_only_revision_requests_validate_exclusions_before_output() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut metadata = [None; N];
    let mut index = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut index);
    for name in ["ambiguous", "missing", "main^"] {
        let argument = format!("deepen-not {name}");
        let body = body(&fixture, 2, &[&argument], false);
        let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
        let mut bytes = [0; 2048];
        let mut output = SliceWriter::new(&mut bytes);
        fixture.bytes.set(0);
        let result = http::serve_with_revisions(
            &repository,
            request(&body, 2),
            &mut buffers.storage(0, limits()),
            &mut output,
        );
        assert_eq!(result.map(|_| ()), Err(Error::InvalidRequest));
        assert!(output.is_empty());
        assert_eq!(fixture.bytes.get(), 0);
    }
}
