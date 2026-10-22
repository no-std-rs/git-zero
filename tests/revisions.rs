#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/revision_fixture.rs"]
mod fixture;
use fixture::*;
use git_zero::{
    BlobFilter, Error, FetchOptions, ObjectFormat, ObjectId, Repository, RevisionLimits,
    RevisionOptions, RevisionState, SelectionOptions, ShallowChange, ValidatedRepository,
    http::{self, Request, RevisionBuffers},
    io::SliceWriter,
};

fn validate<'a>(
    fixture: &'a Fixture,
    metadata: &'a mut [Option<git_zero::ValidationMetadata>],
    index: &'a mut [usize],
) -> ValidatedRepository<'a, 'a, Fixture> {
    ValidatedRepository::new(fixture, metadata, index, &mut [0; 1], &mut [0; 256]).unwrap()
}

fn limits() -> RevisionLimits {
    RevisionLimits {
        max_exclusions: 8,
        work_limit: 1_000_000,
    }
}
fn sorted(mut values: Vec<ObjectId>) -> Vec<ObjectId> {
    values.sort_unstable();
    values.dedup();
    values
}

struct Selection {
    wants: Vec<usize>,
    common: Vec<usize>,
    shallow: Vec<usize>,
    excluded: Vec<usize>,
    since: Option<u64>,
    filter: BlobFilter,
    tags: bool,
}
impl Selection {
    fn since(want: usize, date: u64) -> Self {
        Self {
            wants: vec![want],
            common: vec![],
            shallow: vec![],
            excluded: vec![],
            since: Some(date),
            filter: BlobFilter::All,
            tags: false,
        }
    }
}

fn run_selection<'a>(
    fixture: &Fixture,
    repository: &ValidatedRepository<'_, '_, Fixture>,
    selection: &Selection,
    arrays: (&mut [Option<git_zero::ValidationMetadata>], &'a mut [usize]),
    scratch: (&mut [u8], &mut [u8], &mut RevisionState<'_>),
    bytes: u64,
) -> git_zero::Result<&'a [usize]> {
    let mut roots = [ObjectId::Sha1([0; 20]); N];
    for (slot, &index) in roots.iter_mut().zip(&selection.wants) {
        *slot = fixture.ids[index];
    }
    repository.select_revision_fetch(
        &roots[..selection.wants.len()],
        (
            selection.common.iter().map(|&i| Ok(fixture.ids[i])),
            selection.shallow.iter().map(|&i| Ok(fixture.ids[i])),
            selection.excluded.iter().map(|&i| Ok(fixture.ids[i])),
        ),
        arrays.0,
        arrays.1,
        scratch,
        (
            SelectionOptions {
                graph_read_limit: bytes,
                include_tags: selection.tags,
            },
            FetchOptions {
                filter: selection.filter,
                ..Default::default()
            },
            RevisionOptions {
                since: selection.since,
            },
        ),
    )
}

fn select(
    fixture: &Fixture,
    selection: &Selection,
) -> (Vec<ObjectId>, Vec<ShallowChange>, u64, u64) {
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(fixture, &mut metadata, &mut by_id);
    let mut work = [None; N];
    let mut selected = [0; N];
    let mut state = [0; N];
    let mut revision = RevisionState::new(&mut state, limits());
    fixture.bytes.set(0);
    let indices = allocations::without_allocations(|| {
        run_selection(
            fixture,
            &repository,
            selection,
            (&mut work, &mut selected),
            (&mut [0; 1], &mut [0; 256], &mut revision),
            1_000_000,
        )
    })
    .unwrap();
    assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
    let ids = sorted(
        indices
            .iter()
            .map(|&index| repository.object_id(index).unwrap())
            .collect(),
    );
    let changes = revision.changes(&work).collect();
    (
        ids,
        changes,
        fixture.bytes.get(),
        limits().work_limit - revision.remaining_work(),
    )
}

fn check(
    fixture: &Fixture,
    selection: &Selection,
    objects: &[usize],
    boundaries: &[usize],
    unshallow: &[usize],
) {
    let (actual, changes, _, _) = select(fixture, selection);
    assert_eq!(
        actual,
        sorted(objects.iter().map(|&index| fixture.ids[index]).collect())
    );
    let actual_shallow = sorted(
        changes
            .iter()
            .filter_map(|change| {
                if let ShallowChange::Shallow(id) = change {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect(),
    );
    let actual_unshallow = sorted(
        changes
            .iter()
            .filter_map(|change| {
                if let ShallowChange::Unshallow(id) = change {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect(),
    );
    assert_eq!(
        actual_shallow,
        sorted(boundaries.iter().map(|&index| fixture.ids[index]).collect())
    );
    assert_eq!(
        actual_unshallow,
        sorted(unshallow.iter().map(|&index| fixture.ids[index]).collect())
    );
}

#[test]
fn dates_skew_roots_merges_and_exclusions_match_independent_git_oracles() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for reverse in [false, true] {
            let mut fixture = Fixture::new(format);
            if reverse {
                fixture.objects.reverse();
            }
            check(
                &fixture,
                &Selection::since(D, 1700000100),
                &[A, B, C, D, TREE, BLOB],
                &[],
                &[],
            );
            check(
                &fixture,
                &Selection::since(D, 1700000200),
                &[B, C, D, TREE, BLOB],
                &[B],
                &[],
            );
            check(
                &fixture,
                &Selection::since(D, 1700000250),
                &[D, TREE, BLOB],
                &[D],
                &[],
            );
            check(
                &fixture,
                &Selection::since(M, 1700000250),
                &[M, D, E, B, TREE, BLOB],
                &[D, B],
                &[],
            );
            check(
                &fixture,
                &Selection::since(M, 1700000400),
                &[M, TREE, BLOB],
                &[M, D],
                &[],
            );
            let mut explicit = Selection::since(D, 1700000400);
            explicit.wants.push(B);
            check(&fixture, &explicit, &[D, B, A, TREE, BLOB], &[D], &[]);
            explicit = Selection::since(D, 0);
            explicit.excluded.push(B);
            explicit.wants.push(B);
            check(&fixture, &explicit, &[D, C, B, A, TREE, BLOB], &[C], &[]);
            for excluded in [
                vec![B],
                vec![B, B],
                vec![TAG],
                vec![B, TAG],
                vec![BLOB],
                vec![BLOB_TAG],
            ] {
                let mut selection = Selection::since(D, 0);
                selection.excluded = excluded.clone();
                let (objects, boundaries) = if excluded.contains(&TAG) {
                    (vec![D, TREE, BLOB], vec![D])
                } else if excluded.contains(&B) {
                    (vec![D, C, TREE, BLOB], vec![C])
                } else {
                    (vec![D, C, B, A, TREE, BLOB], vec![])
                };
                check(&fixture, &selection, &objects, &boundaries, &[]);
            }
            let mut selection = Selection::since(M, 0);
            selection.excluded = vec![D];
            check(&fixture, &selection, &[M, TREE, BLOB], &[M, E], &[]);
        }
    }
}

#[test]
fn unshallow_seeds_missing_parents_and_safe_haves_keep_known_closures() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let mut selection = Selection::since(D, 1700000200);
        selection.shallow = vec![D];
        check(&fixture, &selection, &[B, C, TREE, BLOB], &[B], &[D]);
        selection.common = vec![D];
        check(&fixture, &selection, &[B, C], &[B], &[D]);
        selection.common = vec![C];
        check(&fixture, &selection, &[], &[B], &[D]);
        selection.common = vec![B];
        check(&fixture, &selection, &[C], &[B], &[D]);
        selection.common = vec![A];
        check(&fixture, &selection, &[B, C, TREE, BLOB], &[B], &[D]);
        selection.common = vec![M, D];
        selection.shallow = vec![M, D];
        check(&fixture, &selection, &[B, C], &[B], &[D]);
        selection = Selection::since(D, 1700000100);
        selection.shallow = vec![C];
        check(&fixture, &selection, &[A, B, D, TREE, BLOB], &[], &[C]);
        selection = Selection::since(D, 1700000350);
        selection.shallow = vec![C];
        check(&fixture, &selection, &[D, TREE, BLOB], &[D], &[]);
        selection = Selection::since(A, 0);
        selection.shallow = vec![A];
        check(&fixture, &selection, &[], &[], &[A]);
    }
}

#[test]
fn explicit_noncommit_wants_and_blob_filters_keep_their_existing_contract() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut selection = Selection::since(TAG, 1700000200);
    check(&fixture, &selection, &[TAG, C, B, TREE, BLOB], &[B], &[]);
    selection = Selection::since(D, 1700000200);
    selection.filter = BlobFilter::None;
    check(&fixture, &selection, &[D, C, B, TREE], &[B], &[]);
    selection.wants.push(BLOB);
    check(&fixture, &selection, &[D, C, B, TREE, BLOB], &[B], &[]);
}

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
            format!(
                "object-format={}",
                if fixture.format == ObjectFormat::Sha1 {
                    "sha1"
                } else {
                    "sha256"
                }
            ),
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

#[test]
fn opt_in_caps_buffered_and_one_byte_cursors_match_and_cancel_is_terminal() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let mut metadata = [None; N];
        let mut ids = [0; N];
        let repository = validate(&fixture, &mut metadata, &mut ids);
        for version in [0, 1, 2] {
            for args in [
                vec!["deepen-since 1700000200"],
                vec!["deepen-not symcut"],
                vec!["deepen-not tagC", "deepen-since 0"],
                vec!["deepen-not blob"],
            ] {
                let body = body(&fixture, version, &args, true);
                let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
                let mut bytes = [0; 8192];
                let mut output = SliceWriter::new(&mut bytes);
                allocations::without_allocations(|| {
                    http::serve_with_revisions(
                        &repository,
                        request(&body, version),
                        &mut buffers.storage(100_000, limits()),
                        &mut output,
                    )
                })
                .unwrap();
                let mut control = [0; 2048];
                let mut encoder = allocations::without_allocations(|| {
                    http::stream::ResponseEncoder::with_revision_storage(
                        &repository,
                        request(&body, version),
                        buffers.storage(100_000, limits()),
                        &mut control,
                    )
                })
                .unwrap();
                let mut actual = Vec::new();
                loop {
                    let mut byte = [0];
                    let n =
                        allocations::without_allocations(|| encoder.read(&repository, &mut byte))
                            .unwrap();
                    if n == 0 {
                        break;
                    }
                    actual.push(byte[0]);
                }
                assert_eq!(
                    canonical_response(&actual, version),
                    canonical_response(output.as_bytes(), version)
                );
                let mut encoder = http::stream::ResponseEncoder::with_revision_storage(
                    &repository,
                    request(&body, version),
                    buffers.storage(100_000, limits()),
                    &mut control,
                )
                .unwrap();
                encoder.cancel();
                fixture.bytes.set(0);
                assert_eq!(
                    encoder.read(&repository, &mut [0; 1]),
                    Err(Error::InvalidRequest)
                );
                assert_eq!(fixture.bytes.get(), 0);
            }
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
                &repository,
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
    }
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
            let body = body(&fixture, version, &args, true);
            let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
            let mut bytes = [0; 4096];
            let mut output = SliceWriter::new(&mut bytes);
            fixture.bytes.set(0);
            assert_eq!(
                http::serve_with_revisions(
                    &repository,
                    request(&body, version),
                    &mut buffers.storage(100_000, limits()),
                    &mut output
                )
                .map(|_| ()),
                Err(expected)
            );
            assert!(output.is_empty());
            assert_eq!(fixture.bytes.get(), 0);
        }
        for limit in [
            RevisionLimits {
                max_exclusions: 0,
                ..limits()
            },
            RevisionLimits {
                work_limit: 0,
                ..limits()
            },
        ] {
            let body = body(&fixture, version, &["deepen-not cutoff"], true);
            let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
            let mut bytes = [0; 4096];
            let mut output = SliceWriter::new(&mut bytes);
            fixture.bytes.set(0);
            assert_eq!(
                http::serve_with_revisions(
                    &repository,
                    request(&body, version),
                    &mut buffers.storage(100_000, limit),
                    &mut output
                )
                .map(|_| ()),
                Err(Error::LimitExceeded)
            );
            assert!(output.is_empty());
            assert_eq!(fixture.bytes.get(), 0);
        }
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
fn exact_shared_budgets_capacity_failures_and_fresh_retries_are_bounded() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let selection = Selection::since(D, 1700000200);
        let (_, _, required_bytes, required_work) = select(&fixture, &selection);
        let mut metadata = [None; N];
        let mut by_id = [0; N];
        let repository = validate(&fixture, &mut metadata, &mut by_id);
        let count = fixture.object_count();
        let mut objects = [None; N];
        let mut indices = [usize::MAX; N];
        let mut states = [0xa5; N];
        let mut read = [0; 1];
        let mut parse = [0; 256];
        for (bytes, work, state_len, object_len, index_len, read_len, parse_len, expected) in [
            (
                required_bytes,
                required_work,
                count,
                count,
                count,
                1,
                256,
                Ok(()),
            ),
            (
                required_bytes - 1,
                required_work,
                count,
                count,
                count,
                1,
                256,
                Err(Error::LimitExceeded),
            ),
            (
                required_bytes,
                required_work - 1,
                count,
                count,
                count,
                1,
                256,
                Err(Error::LimitExceeded),
            ),
            (
                required_bytes,
                required_work,
                count - 1,
                count,
                count,
                1,
                256,
                Err(Error::BufferTooSmall),
            ),
            (
                required_bytes,
                required_work,
                count,
                count - 1,
                count,
                1,
                256,
                Err(Error::BufferTooSmall),
            ),
            (
                required_bytes,
                required_work,
                count,
                count,
                count - 1,
                1,
                256,
                Err(Error::BufferTooSmall),
            ),
            (
                required_bytes,
                required_work,
                count,
                count,
                count,
                0,
                256,
                Err(Error::BufferTooSmall),
            ),
            (
                required_bytes,
                required_work,
                count,
                count,
                count,
                1,
                0,
                Err(Error::BufferTooSmall),
            ),
            (
                required_bytes,
                required_work,
                count,
                count,
                count,
                1,
                256,
                Ok(()),
            ),
        ] {
            let mut revision = RevisionState::new(
                &mut states[..state_len],
                RevisionLimits {
                    work_limit: work,
                    ..limits()
                },
            );
            let result = allocations::without_allocations(|| {
                run_selection(
                    &fixture,
                    &repository,
                    &selection,
                    (&mut objects[..object_len], &mut indices[..index_len]),
                    (
                        &mut read[..read_len],
                        &mut parse[..parse_len],
                        &mut revision,
                    ),
                    bytes,
                )
            });
            assert_eq!(result.map(|_| ()), expected);
            if expected.is_err() {
                assert_eq!(revision.changes(&objects).count(), 0);
            }
            assert!(states[count..].iter().all(|&byte| byte == 0xa5));
            assert!(objects[count..].iter().all(Option::is_none));
            assert!(indices[count..].iter().all(|&index| index == usize::MAX));
        }
        fixture.failed.set(true);
        let mut revision = RevisionState::new(&mut states, limits());
        assert_eq!(
            run_selection(
                &fixture,
                &repository,
                &selection,
                (&mut objects, &mut indices),
                (&mut read, &mut parse, &mut revision),
                100_000
            )
            .map(|_| ()),
            Err(Error::Backend)
        );
        assert_eq!(revision.changes(&objects).count(), 0);
        fixture.failed.set(false);
        check(&fixture, &selection, &[B, C, D, TREE, BLOB], &[B], &[]);
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
            &mut buffers.storage(
                0,
                RevisionLimits {
                    work_limit: 0,
                    max_exclusions: 0,
                },
            ),
            &mut output,
        )
        .unwrap();
        assert_eq!(fixture.bytes.get(), 0);
    }
}

#[test]
fn matching_tag_work_reserves_both_searches_and_exact_limits() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let mut selection = Selection::since(D, 1700000200);
        selection.filter = BlobFilter::None;
        let (_, _, _, ordinary_work) = select(&fixture, &selection);
        selection.tags = true;
        let (selected, _, bytes, tag_work) = select(&fixture, &selection);
        assert!(selected.contains(&fixture.ids[TAG]));
        assert!(!selected.contains(&fixture.ids[BLOB_TAG]));
        let count = fixture.object_count() as u64;
        let lookup = u64::from(usize::BITS - fixture.object_count().leading_zeros()) + 1;
        let header = fixture.objects[TAG]
            .2
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .unwrap()
            + 2;
        // Independently account the two validated index searches per ref,
        // one tag-header parse, two object scans and the repeated filter scan.
        assert_eq!(
            tag_work - ordinary_work,
            (fixture.reference_count() as u64) * (2 * lookup + 1)
                + (header as u64) * (lookup + 8)
                + 5 * count
        );
        let mut metadata = [None; N];
        let mut by_id = [0; N];
        let repository = validate(&fixture, &mut metadata, &mut by_id);
        for (work, expected) in [
            (tag_work, Ok(())),
            (tag_work - 1, Err(Error::LimitExceeded)),
        ] {
            let mut objects = [None; N];
            let mut indices = [0; N];
            let mut states = [0; N];
            let mut revision = RevisionState::new(
                &mut states,
                RevisionLimits {
                    work_limit: work,
                    ..limits()
                },
            );
            let result = run_selection(
                &fixture,
                &repository,
                &selection,
                (&mut objects, &mut indices),
                (&mut [0; 1], &mut [0; 256], &mut revision),
                bytes,
            );
            assert_eq!(result.map(|_| ()), expected);
        }
    }
}

#[test]
fn boundary_emission_scan_is_reserved_before_any_control_output() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let selection = Selection::since(D, 1700000200);
    let (_, _, _, core_work) = select(&fixture, &selection);
    let body = body(&fixture, 0, &["deepen-since 1700000200"], true);
    let lookup = u64::from(usize::BITS - fixture.object_count().leading_zeros()) + 1;
    let server_work = (body.len() as u64) * 6 * (lookup + 4) + fixture.reference_count() as u64;
    let exact = server_work + core_work + fixture.object_count() as u64;
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut by_id);
    for (work, expected) in [(exact - 1, Err(Error::LimitExceeded)), (exact, Ok(()))] {
        let mut buffers = RevisionBuffers::<8, N, 1, 256>::new();
        let mut storage = [0; 4096];
        let mut output = SliceWriter::new(&mut storage);
        let result = http::serve_with_revisions(
            &repository,
            request(&body, 0),
            &mut buffers.storage(
                100_000,
                RevisionLimits {
                    work_limit: work,
                    ..limits()
                },
            ),
            &mut output,
        );
        assert_eq!(result.map(|_| ()), expected);
        assert_eq!(output.is_empty(), expected.is_err());
    }
}

#[test]
fn exclusions_dates_common_history_unshallow_and_tags_share_one_byte_budget() {
    let fixture = Fixture::new(ObjectFormat::Sha256);
    let mut selection = Selection::since(D, 1700000200);
    selection.excluded = vec![B];
    selection.shallow = vec![D];
    selection.common = vec![D];
    selection.tags = true;
    selection.filter = BlobFilter::None;
    let (_, _, bytes, work) = select(&fixture, &selection);
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut by_id);
    for (limit, expected) in [(bytes, Ok(())), (bytes - 1, Err(Error::LimitExceeded))] {
        let mut objects = [None; N];
        let mut indices = [0; N];
        let mut states = [0; N];
        let mut revision = RevisionState::new(
            &mut states,
            RevisionLimits {
                work_limit: work,
                ..limits()
            },
        );
        fixture.bytes.set(0);
        let result = allocations::without_allocations(|| {
            run_selection(
                &fixture,
                &repository,
                &selection,
                (&mut objects, &mut indices),
                (&mut [0; 1], &mut [0; 256], &mut revision),
                limit,
            )
        });
        assert_eq!(result.map(|_| ()), expected);
        assert!(fixture.bytes.get() <= limit);
    }
    assert_eq!(
        RevisionBuffers::<64, 64, 512, 512>::new().storage_bytes(),
        http::SelectionBuffers::<64, 64, 512, 512>::new().storage_bytes() + 64
    );
}

#[test]
fn revision_capability_advertisement_preflights_exact_packet_limits() {
    use git_zero::{
        Head, RefRepository, Reference, Snapshot,
        repository::{head_ref_limit, ref_name_limit},
    };
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let snapshot = Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap();
        let extra_caps = b" deepen-since deepen-not".len();
        for unborn in [false, true] {
            let limit = if unborn {
                ref_name_limit(format)
                    - 1
                    - format.capability().len()
                    - git_zero::repository::UPLOAD_CAPABILITIES.len()
            } else {
                head_ref_limit(format)
            };
            for overflow in [0, 1] {
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
