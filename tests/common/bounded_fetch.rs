use super::*;
use git_zero::{BlobFilter, FetchOptions, ObjectFormat, SelectionOptions, Snapshot};

fn select(
    graph: &Graph,
    roots: &[ObjectId],
    common: &[ObjectId],
    shallow: &[ObjectId],
    options: FetchOptions,
    tags: bool,
) -> (Vec<ObjectId>, Vec<ObjectId>, Vec<ObjectId>) {
    let validation_source = &graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(validation_source);
    let mut work = [None; CAPACITY];
    let mut indices = [0; CAPACITY];
    let indices = allocations::without_allocations(|| {
        repository.select_bounded_fetch(
            roots,
            (
                common.iter().copied().map(Ok),
                shallow.iter().copied().map(Ok),
            ),
            &mut work,
            &mut indices,
            (&mut [0; 1], &mut [0; 128]),
            (
                SelectionOptions {
                    graph_read_limit: 32768,
                    include_tags: tags,
                },
                options,
            ),
        )
    })
    .unwrap();
    let boundaries = work
        .iter()
        .flatten()
        .filter(|m| m.is_shallow())
        .map(|m| m.id)
        .collect();
    let unshallow = work
        .iter()
        .flatten()
        .filter(|m| m.is_unshallow())
        .map(|m| m.id)
        .collect();
    (
        selected_ids(repository, indices),
        sorted(boundaries),
        sorted(unshallow),
    )
}

fn expected(ids: &[Option<ObjectId>]) -> Vec<ObjectId> {
    sorted(ids.iter().copied().map(Option::unwrap).collect())
}

#[test]
fn depth_one_keeps_only_the_merge_tip_and_its_tree_closure() {
    let graph = Graph::new();
    let ids = graph.ids;
    let merge = ids.merge.unwrap();
    assert_eq!(
        select(
            &graph,
            &[merge],
            &[merge],
            &[merge],
            FetchOptions {
                depth: Some(1),
                deepen_relative: false,
                filter: BlobFilter::All,
            },
            false
        ),
        (
            expected(&[
                ids.merge,
                ids.merge_tree,
                ids.main_blob,
                ids.side_blob,
                ids.shared
            ]),
            vec![merge],
            vec![]
        )
    );
}

#[test]
fn depth_two_cuts_both_merge_parents_and_removes_the_old_boundary() {
    let graph = Graph::new();
    let ids = graph.ids;
    let merge = ids.merge.unwrap();
    assert_eq!(
        select(
            &graph,
            &[merge],
            &[merge],
            &[merge],
            FetchOptions {
                depth: Some(2),
                deepen_relative: false,
                filter: BlobFilter::All,
            },
            false
        ),
        (
            expected(&[
                ids.merge,
                ids.merge_tree,
                ids.main,
                ids.main_tree,
                ids.main_blob,
                ids.side,
                ids.side_tree,
                ids.side_blob,
                ids.shared
            ]),
            expected(&[ids.main, ids.side]),
            vec![merge]
        )
    );
}

#[test]
fn relative_depth_counts_from_client_boundaries_in_both_enumeration_orders() {
    for reverse in [false, true] {
        let mut graph = Graph::new();
        let ids = graph.ids;
        if reverse {
            graph.objects.reverse();
        }
        let options = FetchOptions {
            depth: Some(1),
            deepen_relative: true,
            filter: BlobFilter::All,
        };
        let (objects, boundaries, removed) = select(
            &graph,
            &[ids.merge.unwrap()],
            &[],
            &[ids.main.unwrap(), ids.side.unwrap()],
            options,
            false,
        );
        assert_eq!(objects, sorted(graph.merge_closure()));
        assert_eq!(boundaries, vec![ids.base.unwrap()]);
        assert_eq!(removed, expected(&[ids.main, ids.side]));
        let (_, boundaries, removed) = select(
            &graph,
            &[ids.merge.unwrap()],
            &[],
            &[ids.main.unwrap(), ids.base.unwrap()],
            options,
            false,
        );
        assert_eq!(boundaries, vec![ids.base.unwrap()]);
        assert_eq!(removed, vec![ids.main.unwrap()]);
        let (_, boundaries, removed) = select(
            &graph,
            &[ids.merge.unwrap()],
            &[],
            &[ids.merge.unwrap()],
            options,
            false,
        );
        assert_eq!(boundaries, expected(&[ids.main, ids.side]));
        assert_eq!(removed, vec![ids.merge.unwrap()]);
        let (_, boundaries, removed) = select(
            &graph,
            &[ids.main.unwrap()],
            &[],
            &[ids.side.unwrap()],
            options,
            false,
        );
        assert!(boundaries.is_empty());
        assert!(removed.is_empty());
        let (_, boundaries, removed) =
            select(&graph, &[ids.merge.unwrap()], &[], &[], options, false);
        assert!(boundaries.is_empty());
        assert!(removed.is_empty());
    }
}

#[test]
fn relative_requests_from_full_clients_preserve_common_history_pruning() {
    let graph = Graph::new();
    let options = FetchOptions {
        depth: Some(1),
        deepen_relative: true,
        filter: BlobFilter::All,
    };
    for common in [graph.ids.main.unwrap(), graph.ids.merge.unwrap()] {
        assert_eq!(
            select(
                &graph,
                &[graph.ids.merge.unwrap()],
                &[common],
                &[],
                options,
                false
            ),
            select(
                &graph,
                &[graph.ids.merge.unwrap()],
                &[common],
                &[],
                FetchOptions::default(),
                false
            )
        );
    }
    assert!(
        select(
            &graph,
            &[graph.ids.merge.unwrap()],
            &[graph.ids.merge.unwrap()],
            &[],
            options,
            false
        )
        .0
        .is_empty()
    );
}

#[test]
fn unused_relative_negotiation_preserves_ordinary_fetches_and_client_boundaries() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(&source);
    let boundary = [graph.ids.main.unwrap()];
    for shallow in [&[][..], &boundary[..]] {
        assert_eq!(
            select(
                &graph,
                &[graph.head()],
                &boundary,
                shallow,
                FetchOptions {
                    deepen_relative: true,
                    ..FetchOptions::default()
                },
                false
            ),
            select(
                &graph,
                &[graph.head()],
                &boundary,
                shallow,
                FetchOptions::default(),
                false
            )
        );
        let args: Vec<_> = shallow.iter().map(|id| format!("shallow {id}")).collect();
        for version in [0, 1, 2] {
            compare_unused_relative(repository, version, &args);
        }
    }
}

fn compare_unused_relative(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    version: u8,
    args: &[String],
) {
    let mut args: Vec<_> = args.iter().map(String::as_str).collect();
    let bytes = body(repository.head(), version, &args, true);
    let (result, expected) = Harness::default().serve(repository, request(&bytes, version), 32768);
    result.unwrap();
    args.push("deepen-relative");
    let bytes = body(repository.head(), version, &args, true);
    let mut harness = Harness::default();
    let (result, actual) = harness.serve(repository, request(&bytes, version), 32768);
    result.unwrap();
    assert_same_response(&actual, &expected, version);
    assert_same_response(
        &harness.pull(repository, request(&bytes, version)),
        &expected,
        version,
    );
}

#[test]
fn complete_pack_apis_reject_unadvertised_relative_negotiation_before_output() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 128];
        let mut commit = [0; 512];
        let snapshot = Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap();
        for version in [0, 1, 2] {
            reject_complete_relative(&snapshot, version);
        }
    }
}

fn reject_complete_relative(snapshot: &Snapshot<'_>, version: u8) {
    let bytes = body(snapshot.head(), version, &["deepen-relative"], true);
    let mut storage = [0; 4096];
    let mut output = SliceWriter::new(&mut storage);
    let result = allocations::without_allocations(|| {
        http::serve(snapshot, request(&bytes, version), &mut output)
    });
    assert!(matches!(result, Err(Error::Unsupported)));
    assert!(output.is_empty());
    let mut called = false;
    let result = allocations::without_allocations(|| {
        http::serve_with_pack(
            snapshot,
            request(&bytes, version),
            &mut [],
            &mut output,
            |_, _| {
                called = true;
                Ok(())
            },
        )
    });
    assert!(matches!(result, Err(Error::Unsupported)));
    assert!(!called && output.is_empty());
    let result = allocations::without_allocations(|| {
        http::stream::ResponseEncoder::new(snapshot, request(&bytes, version), &mut [0; 4096])
            .map(|_| ())
    });
    assert_eq!(result, Err(Error::Unsupported));
}

#[test]
fn standalone_relative_arguments_are_rejected_in_v0_and_v1() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(&source);
    for version in [0, 1] {
        for capability in ["", " deepen-relative"] {
            let mut bytes = Vec::new();
            packet(&mut bytes, &format!("want {}{capability}", graph.head()));
            packet(&mut bytes, "deepen 1");
            packet(&mut bytes, "deepen-relative");
            bytes.extend_from_slice(b"0000");
            packet(&mut bytes, "done");
            let (result, output) =
                Harness::default().serve(repository, request(&bytes, version), 32768);
            assert!(result.is_err());
            assert!(output.is_empty());
        }
    }
}

#[test]
fn relative_requests_validate_options_and_match_fragmented_streaming() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(&source);
    for version in [0, 1, 2] {
        let shallow = format!("shallow {}", graph.ids.merge.unwrap());
        let args = [shallow.as_str(), "deepen 1", "deepen-relative"];
        let bytes = body(graph.ids.merge.unwrap(), version, &args, true);
        let mut harness = Harness::default();
        let (failed, output) = harness.serve(repository, request(&bytes, version), 0);
        assert!(matches!(failed, Err(Error::LimitExceeded)));
        assert!(output.is_empty());
        let (result, expected) = harness.serve(repository, request(&bytes, version), 32768);
        result.unwrap();
        assert_same_response(
            &harness.pull(repository, request(&bytes, version)),
            &expected,
            version,
        );
    }
}

#[test]
fn root_commits_exactly_at_the_depth_limit_remain_shallow() {
    let graph = Graph::new();
    let ids = graph.ids;
    let merge = ids.merge.unwrap();
    assert_eq!(
        select(
            &graph,
            &[merge],
            &[merge],
            &[merge],
            FetchOptions {
                depth: Some(3),
                deepen_relative: false,
                filter: BlobFilter::All,
            },
            false
        ),
        (
            sorted(graph.merge_closure()),
            vec![ids.base.unwrap()],
            vec![merge]
        )
    );
}

#[test]
fn unshallow_retransmits_missing_history_even_when_the_tip_is_common() {
    let graph = Graph::new();
    let merge = graph.ids.merge.unwrap();
    assert_eq!(
        select(
            &graph,
            &[merge],
            &[merge],
            &[merge],
            FetchOptions {
                depth: Some(i32::MAX as u32),
                deepen_relative: false,
                filter: BlobFilter::All,
            },
            false
        ),
        (sorted(graph.merge_closure()), vec![], vec![merge])
    );
}

#[test]
fn shallow_haves_own_their_tree_but_cannot_exclude_missing_parents() {
    let graph = Graph::new();
    let ids = graph.ids;
    assert_eq!(
        select(
            &graph,
            &[ids.merge.unwrap()],
            &[ids.main.unwrap()],
            &[ids.main.unwrap()],
            FetchOptions::default(),
            false
        ),
        (
            expected(&[
                ids.base_blob,
                ids.side_blob,
                ids.base_tree,
                ids.side_tree,
                ids.merge_tree,
                ids.base,
                ids.side,
                ids.merge
            ]),
            vec![],
            vec![]
        )
    );
}

#[test]
fn a_client_root_boundary_persists_without_a_depth_request() {
    let graph = Graph::new();
    let ids = graph.ids;
    let base = ids.base.unwrap();
    assert_eq!(
        select(
            &graph,
            &[base],
            &[],
            &[base],
            FetchOptions::default(),
            false
        ),
        (
            expected(&[ids.base, ids.base_tree, ids.base_blob, ids.shared]),
            vec![base],
            vec![]
        )
    );
}

#[test]
fn shorter_merge_paths_reopen_previously_cut_commits_in_any_enumeration_order() {
    let mut graph = Graph::new();
    let tree = graph.ids.merge_tree.unwrap();
    let merge = graph.ids.merge.unwrap();
    let long1 = commit(&mut graph.objects, tree, &[merge], "long one");
    let long2 = commit(&mut graph.objects, tree, &[long1], "long two");
    let short = commit(&mut graph.objects, tree, &[merge], "short");
    let top = commit(&mut graph.objects, tree, &[long2, short], "unequal merge");
    graph
        .refs
        .iter_mut()
        .find(|r| r.name == "refs/heads/z-merge")
        .unwrap()
        .id = top;
    for _ in 0..2 {
        let (selected, boundaries, _) = select(
            &graph,
            &[top],
            &[],
            &[],
            FetchOptions {
                depth: Some(4),
                deepen_relative: false,
                filter: BlobFilter::None,
            },
            false,
        );
        let expected = vec![
            top,
            long2,
            long1,
            short,
            merge,
            graph.ids.main.unwrap(),
            graph.ids.side.unwrap(),
            tree,
            graph.ids.main_tree.unwrap(),
            graph.ids.side_tree.unwrap(),
        ];
        assert_eq!(
            (selected, boundaries),
            (
                sorted(expected),
                sorted(vec![graph.ids.main.unwrap(), graph.ids.side.unwrap()])
            )
        );
        graph.objects.reverse();
    }
}

#[test]
fn blob_filters_retain_explicit_wants_and_respect_the_exact_size_cutoff() {
    let graph = Graph::new();
    let ids = graph.ids;
    let roots = [ids.merge.unwrap(), ids.shared.unwrap()];
    let structural = expected(&[
        ids.base,
        ids.main,
        ids.side,
        ids.merge,
        ids.base_tree,
        ids.main_tree,
        ids.side_tree,
        ids.merge_tree,
        ids.shared,
    ]);
    assert_eq!(
        select(
            &graph,
            &roots,
            &[],
            &[],
            FetchOptions {
                depth: None,
                deepen_relative: false,
                filter: BlobFilter::None,
            },
            false
        ),
        (structural.clone(), vec![], vec![])
    );
    assert_eq!(
        select(
            &graph,
            &roots,
            &[],
            &[],
            FetchOptions {
                depth: None,
                deepen_relative: false,
                filter: BlobFilter::Limit(5),
            },
            false
        ),
        (structural, vec![], vec![])
    );
    assert_eq!(
        select(
            &graph,
            &roots,
            &[],
            &[],
            FetchOptions {
                depth: None,
                deepen_relative: false,
                filter: BlobFilter::Limit(6),
            },
            false
        ),
        (
            expected(&[
                ids.base,
                ids.main,
                ids.side,
                ids.merge,
                ids.base_tree,
                ids.main_tree,
                ids.side_tree,
                ids.merge_tree,
                ids.main_blob,
                ids.side_blob,
                ids.shared
            ]),
            vec![],
            vec![]
        )
    );
}

#[test]
fn included_tags_preserve_the_selected_commit_depth_and_blob_filter() {
    let graph = Graph::new();
    let ids = graph.ids;
    assert_eq!(
        select(
            &graph,
            &[ids.merge.unwrap()],
            &[],
            &[],
            FetchOptions {
                depth: Some(2),
                deepen_relative: false,
                filter: BlobFilter::None,
            },
            true
        ),
        (
            expected(&[
                ids.merge,
                ids.main,
                ids.side,
                ids.merge_tree,
                ids.main_tree,
                ids.side_tree,
                ids.tag,
                ids.nested
            ]),
            expected(&[ids.main, ids.side]),
            vec![]
        )
    );
}

#[test]
fn explicitly_wanted_nested_tags_peel_to_a_depth_one_commit() {
    let graph = Graph::new();
    let ids = graph.ids;
    assert_eq!(
        select(
            &graph,
            &[ids.nested.unwrap()],
            &[],
            &[],
            FetchOptions {
                depth: Some(1),
                deepen_relative: false,
                filter: BlobFilter::All,
            },
            false
        ),
        (
            expected(&[
                ids.nested,
                ids.tag,
                ids.side,
                ids.side_tree,
                ids.side_blob,
                ids.shared
            ]),
            vec![ids.side.unwrap()],
            vec![]
        )
    );
}

fn body(tip: ObjectId, version: u8, args: &[&str], done: bool) -> Vec<u8> {
    let mut result = Vec::new();
    if version == 2 {
        packet(&mut result, "command=fetch");
        packet(
            &mut result,
            core::str::from_utf8(tip.format().capability()).unwrap(),
        );
        result.extend_from_slice(b"0001");
    }
    let mut capabilities = if version == 2 {
        String::new()
    } else {
        String::from(" filter")
    };
    if version != 2 {
        for arg in args.iter().filter(|&&arg| arg == "deepen-relative") {
            capabilities.push(' ');
            capabilities.push_str(arg);
        }
    }
    packet(&mut result, &format!("want {tip}{capabilities}"));
    for arg in args
        .iter()
        .filter(|&&arg| version == 2 || arg != "deepen-relative")
    {
        packet(&mut result, arg);
    }
    if version != 2 {
        result.extend_from_slice(b"0000");
    }
    if done {
        packet(&mut result, "done");
    }
    if version == 2 {
        result.extend_from_slice(b"0000");
    }
    result
}

fn packet(output: &mut Vec<u8>, line: &str) {
    output.extend_from_slice(format!("{:04x}{line}\n", line.len() + 5).as_bytes());
}

fn request(body: &[u8], version: u8) -> Request<'_> {
    Request {
        method: "POST",
        path: "git-upload-pack",
        query: None,
        body,
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: Some(match version {
            2 => "version=2",
            1 => "version=1",
            _ => "version=0",
        }),
    }
}

#[derive(Default)]
struct Harness {
    buffers: http::SelectionBuffers<4, CAPACITY, 1, 128>,
}

impl Harness {
    fn storage(&mut self, graph_read_limit: u64) -> SelectionStorage<'_> {
        self.buffers.storage(graph_read_limit)
    }

    fn serve(
        &mut self,
        repository: &ValidatedRepository<'_, '_, impl Repository>,
        request: Request<'_>,
        limit: u64,
    ) -> (Result<http::Response>, Vec<u8>) {
        let mut bytes = [0; 4096];
        let mut output = SliceWriter::new(&mut bytes);
        let result = allocations::without_allocations(|| {
            http::serve_with_selection(repository, request, &mut self.storage(limit), &mut output)
        });
        (result, output.as_bytes().to_vec())
    }

    fn pull(
        &mut self,
        repository: &ValidatedRepository<'_, '_, impl Repository>,
        request: Request<'_>,
    ) -> Vec<u8> {
        let mut control = [0; 4096];
        let mut encoder = prepare(repository, request, self.storage(32768), &mut control).unwrap();
        let output = pull::collect(|output| encoder.read(repository, output), 1);
        encoder.cancel();
        assert_eq!(
            encoder.read(repository, &mut [0; 1]),
            Err(Error::InvalidRequest)
        );
        output
    }
}

fn prepare<'a>(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    request: Request<'_>,
    storage: SelectionStorage<'a>,
    control: &'a mut [u8],
) -> Result<http::stream::ResponseEncoder<'a>> {
    allocations::without_allocations(|| {
        http::stream::ResponseEncoder::with_selection_storage(repository, request, storage, control)
    })
}

fn assert_same_response(actual: &[u8], expected: &[u8], version: u8) {
    if version == 2 {
        assert_eq!(unpack_v2(actual), unpack_v2(expected));
    } else {
        assert_eq!(actual, expected);
    }
}

fn unpack_v2(wire: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let header = b"000dpackfile\n";
    let start = wire
        .windows(header.len())
        .position(|w| w == header)
        .unwrap()
        + header.len();
    let mut rest = &wire[start..];
    let mut packed = Vec::new();
    while let Some(packet) = pktline::read(&mut rest).unwrap() {
        match packet {
            pktline::Packet::Data(line) => {
                assert_eq!(line[0], 1);
                packed.extend_from_slice(&line[1..]);
            }
            pktline::Packet::Flush => assert!(rest.is_empty()),
        }
    }
    pack::validate(&packed).unwrap();
    (wire[..start].to_vec(), packed)
}

#[test]
fn malformed_extensions_leave_output_empty_in_both_protocols() {
    let graph = Graph::new();
    let blob_shallow = format!("shallow {}", graph.ids.shared.unwrap());
    let unknown_shallow = format!("shallow {}", ObjectId::Sha1([9; 20]));
    let cases: &[&[&str]] = &[
        &["deepen 0"],
        &["deepen "],
        &["deepen  1"],
        &["deepen 1 "],
        &["deepen 1.5"],
        &["deepen 18446744073709551616"],
        &["deepen 1\0"],
        &["deepen 1\ndone"],
        &["deepen １"],
        &["shallow "],
        &["shallow 0000000000000000000000000000000000000000"],
        &["shallow 0000000000000000000000000000000000000000000000000000000000000000"],
        &["shallow ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"],
        &["filter "],
        &["filter blob:none "],
        &["filter blob:none\0"],
        &["filter combine:blob:none+tree:1"],
        &["filter sparse:oid=HEAD"],
        &["filter object:type=blob"],
        &["deepen -1"],
        &["deepen +1"],
        &["filter blob:limit=+1"],
        &["deepen 2147483648"],
        &["deepen 1", "deepen-relative", "deepen-relative"],
        &["deepen-since 1"],
        &["deepen-not main"],
        &["filter tree:0"],
        &["filter blob:limit="],
        &["filter blob:limit=18446744073709551615k"],
        &[&blob_shallow],
        &[&unknown_shallow],
        &["shallow invalid"],
        &["deepen 1", "deepen 2"],
        &["filter blob:none", "filter blob:none"],
    ];
    let validation_source = &graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(validation_source);
    let mut harness = Harness::default();
    for version in [0, 2] {
        for args in cases {
            let bytes = body(graph.head(), version, args, true);
            let (result, output) = harness.serve(repository, request(&bytes, version), 32768);
            assert!(result.is_err(), "accepted {args:?}");
            assert!(output.is_empty());
        }
    }
}

#[test]
fn budget_failure_retries_and_guarded_one_byte_pulls_preserve_responses() {
    let graph = Graph::new();
    let validation_source = &graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(validation_source);
    let mut harness = Harness::default();
    for version in [0, 2] {
        let bytes = body(
            graph.head(),
            version,
            &["deepen 1", "filter blob:none"],
            true,
        );
        let request = request(&bytes, version);
        let (result, output) = harness.serve(repository, request, 0);
        assert!(matches!(result, Err(Error::LimitExceeded)));
        assert!(output.is_empty());
        let (result, expected) = harness.serve(repository, request, 32768);
        result.unwrap();
        let actual = harness.pull(repository, request);
        assert_same_response(&actual, &expected, version);
    }
}

#[test]
fn shallow_probe_ends_at_its_flush_without_an_ack() {
    let graph = Graph::new();
    let validation_source = &graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(validation_source);
    let bytes = body(graph.head(), 0, &["deepen 1"], false);
    let (failed, empty) = Harness::default().serve(repository, request(&bytes, 0), 0);
    assert!(matches!(failed, Err(Error::LimitExceeded)));
    assert!(empty.is_empty());
    let (result, output) = Harness::default().serve(repository, request(&bytes, 0), 32768);
    result.unwrap();
    let mut expected = Vec::new();
    packet(&mut expected, &format!("shallow {}", graph.head()));
    expected.extend_from_slice(b"0000");
    assert_eq!(output, expected);
}

#[test]
fn relative_full_client_probes_need_no_graph_storage_or_read_budget() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 128];
        let mut commit = [0; 512];
        let snapshot = Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap();
        let mut validation = ValidationBuffers::new();
        let repository = validation.validate(&snapshot);
        for version in [0, 1] {
            assert_relative_probe_without_graph(&repository, version);
        }
    }
}

fn assert_relative_probe_without_graph(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    version: u8,
) {
    let bytes = body(
        repository.head(),
        version,
        &["deepen-relative", "deepen 1"],
        false,
    );
    let request = request(&bytes, version);
    let (result, output) = Harness::default().serve(repository, request, 0);
    assert_eq!(result.unwrap().status, 200);
    assert_eq!(output, b"0000");
    let mut buffers = http::SelectionBuffers::<1, 0, 0, 0>::new();
    let mut output = [0; 4];
    let mut writer = SliceWriter::new(&mut output);
    allocations::without_allocations(|| {
        http::serve_with_selection(repository, request, &mut buffers.storage(0), &mut writer)
    })
    .unwrap();
    assert_eq!(writer.as_bytes(), b"0000");
    let mut control = [0; 4];
    let mut encoder = prepare(repository, request, buffers.storage(0), &mut control).unwrap();
    assert_eq!(
        pull::collect(|output| encoder.read(repository, output), 1),
        b"0000"
    );
}

#[test]
fn boundary_control_capacity_is_checked_before_a_stream_can_start() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(&source);
    let mut buffers = http::SelectionBuffers::<4, CAPACITY, 1, 128>::new();
    for version in [0, 2] {
        let bytes = body(
            graph.head(),
            version,
            &["deepen 1", "filter blob:none"],
            true,
        );
        let request = request(&bytes, version);
        let mut prefix = Vec::new();
        if version == 2 {
            packet(&mut prefix, "shallow-info");
        }
        packet(&mut prefix, &format!("shallow {}", graph.head()));
        prefix.extend_from_slice(if version == 2 { b"0001" } else { b"0000" });
        packet(&mut prefix, if version == 2 { "packfile" } else { "NAK" });
        for capacity in [0, 1, prefix.len() - 1] {
            let mut guard = vec![0xa5; capacity + 2];
            let result = prepare(
                repository,
                request,
                buffers.storage(32768),
                &mut guard[1..1 + capacity],
            );
            assert!(matches!(result, Err(Error::BufferTooSmall)));
            assert_eq!(guard[0], 0xa5);
            assert_eq!(guard[capacity + 1], 0xa5);
        }
        let mut guard = vec![0xa5; prefix.len() + 2];
        let mut encoder = prepare(
            repository,
            request,
            buffers.storage(32768),
            &mut guard[1..1 + prefix.len()],
        )
        .unwrap();
        let actual = pull::collect(|output| encoder.read(repository, output), 1);
        assert_eq!(&actual[..prefix.len()], prefix);
        assert_eq!(guard[0], 0xa5);
        assert_eq!(guard[prefix.len() + 1], 0xa5);
        let (result, expected) = Harness::default().serve(repository, request, 32768);
        result.unwrap();
        assert_same_response(&actual, &expected, version);
    }
}

#[test]
fn reusable_buffers_do_not_carry_depth_filter_wants_or_client_boundaries_between_requests() {
    let graph = Graph::new();
    let ids = graph.ids;
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(&source);
    let client_boundary = format!("shallow {}", ids.side.unwrap());
    let cases: &[(ObjectId, &[&str])] = &[
        (graph.head(), &["deepen 1"]),
        (ids.side.unwrap(), &["deepen 1", "filter blob:none"]),
        (ids.merge.unwrap(), &["deepen 2"]),
        (graph.head(), &[]),
        (ids.shared.unwrap(), &["filter blob:none"]),
        (ids.nested.unwrap(), &["deepen 1", "filter blob:limit=6"]),
        (ids.merge.unwrap(), &["filter blob:limit=6"]),
        (graph.head(), &[&client_boundary]),
        (ids.merge.unwrap(), &["deepen 2147483647"]),
        (graph.head(), &[]),
    ];
    let mut reused = Harness::default();
    for version in [0, 2] {
        for &(root, args) in cases {
            let bytes = body(root, version, args, true);
            let request = request(&bytes, version);
            let (expected_status, expected) = Harness::default().serve(repository, request, 32768);
            assert_eq!(expected_status.unwrap().status, 200);
            let (actual_status, actual) = reused.serve(repository, request, 32768);
            assert_eq!(actual_status.unwrap().status, 200);
            assert_eq!(actual, expected, "version={version}, options={args:?}");
        }
    }
}

#[test]
fn switching_to_a_smaller_repository_does_not_publish_old_boundary_slots() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut large_validation = ValidationBuffers::new();
    let large = &large_validation.validate(&source);
    let (mut tree, mut commit) = ([0; 128], [0; 512]);
    let snapshot = git_zero::Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut small_validation = ValidationBuffers::new();
    let small = &small_validation.validate(&snapshot);
    let mut reused = Harness::default();
    for version in [0, 2] {
        let large_body = body(graph.ids.merge.unwrap(), version, &["deepen 2"], true);
        let (result, _) = reused.serve(large, request(&large_body, version), 32768);
        assert_eq!(result.unwrap().status, 200);
        let small_body = body(snapshot.head(), version, &["deepen 1"], true);
        let small_request = request(&small_body, version);
        let (expected_status, expected) = Harness::default().serve(small, small_request, 32768);
        assert_eq!(expected_status.unwrap().status, 200);
        let (actual_status, actual) = reused.serve(small, small_request, 32768);
        assert_eq!(actual_status.unwrap().status, 200);
        assert_eq!(actual, expected);
        assert!(
            !actual
                .windows(graph.head().to_hex().len())
                .any(|part| part == graph.head().to_hex().as_ref())
        );
    }
}

#[test]
fn overlapping_bounded_cursors_keep_their_selection_and_boundary_packets() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = &validation.validate(&source);
    let first_body = body(graph.head(), 0, &["deepen 1", "filter blob:none"], true);
    let second_body = body(graph.ids.merge.unwrap(), 2, &["deepen 2"], true);
    let first_request = request(&first_body, 0);
    let second_request = request(&second_body, 2);
    let mut first_buffers = http::SelectionBuffers::<4, CAPACITY, 1, 128>::new();
    let mut second_buffers = http::SelectionBuffers::<4, CAPACITY, 1, 128>::new();
    let mut first_control = [0; 256];
    let mut second_control = [0; 256];
    let mut first = prepare(
        repository,
        first_request,
        first_buffers.storage(32768),
        &mut first_control,
    )
    .unwrap();
    let mut second = prepare(
        repository,
        second_request,
        second_buffers.storage(32768),
        &mut second_control,
    )
    .unwrap();
    let mut first_byte = [0; 1];
    let mut second_byte = [0; 1];
    assert_eq!(first.read(repository, &mut first_byte).unwrap(), 1);
    assert_eq!(second.read(repository, &mut second_byte).unwrap(), 1);
    let mut second_wire = second_byte.to_vec();
    second_wire.extend(pull::collect(|out| second.read(repository, out), 7));
    let mut first_wire = first_byte.to_vec();
    first_wire.extend(pull::collect(|out| first.read(repository, out), 1));
    let mut harness = Harness::default();
    let (first_status, first_expected) = harness.serve(repository, first_request, 32768);
    let (second_status, second_expected) = harness.serve(repository, second_request, 32768);
    assert_eq!(first_status.unwrap().status, 200);
    assert_eq!(second_status.unwrap().status, 200);
    assert_eq!(first_wire, first_expected);
    assert_eq!(unpack_v2(&second_wire), unpack_v2(&second_expected));
    first.cancel();
    assert_eq!(
        first.read(repository, &mut first_byte),
        Err(Error::InvalidRequest)
    );
    assert_eq!(second.read(repository, &mut second_byte).unwrap(), 0);
}

#[test]
fn fallible_history_iterators_preserve_errors_and_allow_a_fresh_bounded_selection() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = validation.validate(&source);
    let mut work = [None; CAPACITY];
    let mut selected = [usize::MAX; CAPACITY];
    let mut read = [0; 1];
    let mut parse = [0; 128];
    let options = (
        SelectionOptions {
            graph_read_limit: 32768,
            include_tags: false,
        },
        FetchOptions {
            depth: Some(1),
            deepen_relative: false,
            filter: BlobFilter::None,
        },
    );
    let roots = [graph.head()];
    assert_eq!(
        repository.select_bounded_fetch(
            &roots,
            ([Err(Error::Backend)], []),
            &mut work,
            &mut selected,
            (&mut read, &mut parse),
            options,
        ),
        Err(Error::Backend)
    );
    assert_eq!(
        repository.select_bounded_fetch(
            &roots,
            ([], [Ok(graph.head()), Err(Error::LimitExceeded)]),
            &mut work,
            &mut selected,
            (&mut read, &mut parse),
            options,
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        repository.select_bounded_fetch(
            &roots,
            ([Ok(ObjectId::Sha1([9; 20]))], []),
            &mut work,
            &mut selected,
            (&mut read, &mut parse),
            options,
        ),
        Err(Error::ObjectNotFound)
    );
    assert_eq!(
        repository.select_bounded_fetch(
            &roots,
            ([], [Ok(ObjectId::Sha1([9; 20]))]),
            &mut work,
            &mut selected,
            (&mut read, &mut parse),
            options,
        ),
        Err(Error::InvalidRequest)
    );
    assert_eq!(
        repository.select_bounded_fetch(
            &roots,
            ([], [Ok(graph.ids.shared.unwrap())]),
            &mut work,
            &mut selected,
            (&mut read, &mut parse),
            options,
        ),
        Err(Error::InvalidRequest)
    );
    assert_eq!(selected, [usize::MAX; CAPACITY]);
    let indices = repository
        .select_bounded_fetch(
            &roots,
            ([], []),
            &mut work,
            &mut selected,
            (&mut read, &mut parse),
            options,
        )
        .unwrap();
    assert_eq!(
        selected_ids(&repository, indices),
        expected(&[graph.ids.main, graph.ids.main_tree])
    );
    let shallow: Vec<_> = work
        .iter()
        .flatten()
        .filter(|m| m.is_shallow())
        .map(|m| m.id)
        .collect();
    assert_eq!(shallow, roots);
    assert!(!work.iter().flatten().any(|m| m.is_unshallow()));
}

#[test]
fn an_explicit_blob_want_needs_no_structural_reads_or_parse_scratch() {
    let graph = Graph::new();
    let provider = Streaming {
        source: &graph,
        fail: Cell::new(false),
        bytes: Cell::new(0),
    };
    let source = RefRepository::new(&provider, graph.state(), &graph.refs).unwrap();
    let mut validation = ValidationBuffers::new();
    let repository = validation.validate(&source);
    let root = [graph.ids.shared.unwrap()];
    let mut work = [None; CAPACITY];
    let mut selected = [0; CAPACITY];
    provider.bytes.set(0);
    let indices = allocations::without_allocations(|| {
        repository.select_bounded_fetch(
            &root,
            ([], []),
            &mut work,
            &mut selected,
            (&mut [], &mut []),
            (
                SelectionOptions {
                    graph_read_limit: 0,
                    include_tags: false,
                },
                FetchOptions {
                    depth: Some(1),
                    deepen_relative: false,
                    filter: BlobFilter::None,
                },
            ),
        )
    })
    .unwrap();
    assert_eq!(selected_ids(&repository, indices), root);
    assert!(
        !work
            .iter()
            .flatten()
            .any(|m| m.is_shallow() || m.is_unshallow())
    );
    assert_eq!(provider.bytes.get(), 0);
    // Body access belongs to the pack encoder, after graph selection succeeds.
    provider.fail.set(true);
    let mut output = [0; 128];
    let mut writer = SliceWriter::new(&mut output);
    assert_eq!(
        pack::write_selected_with_scratch(&repository, &selected[..1], &mut [0; 1], &mut writer,),
        Err(Error::Backend)
    );
}

#[test]
fn extension_only_negotiation_uses_no_graph_slots_scratch_or_read_budget() {
    let graph = Graph::new();
    let provider = Streaming {
        source: &graph,
        fail: Cell::new(false),
        bytes: Cell::new(0),
    };
    let source = RefRepository::new(&provider, graph.state(), &graph.refs).unwrap();
    let mut validation = ValidationBuffers::new();
    let repository = validation.validate(&source);
    let shallow = format!("shallow {}", graph.head());
    let cases: &[(u8, &[&str])] = &[
        (0, &["filter blob:none"]),
        (0, &[&shallow]),
        (0, &[&shallow, "filter blob:limit=5"]),
        (2, &["filter blob:none"]),
        (2, &[&shallow]),
        (2, &[&shallow, "deepen 2", "filter blob:none"]),
    ];
    let mut buffers = http::SelectionBuffers::<1, 0, 0, 0>::new();
    provider.bytes.set(0);
    for &(version, args) in cases {
        let bytes = body(graph.head(), version, args, false);
        let mut control = [0; 128];
        let mut encoder = prepare(
            &repository,
            request(&bytes, version),
            buffers.storage(0),
            &mut control,
        )
        .unwrap();
        let actual = pull::collect(|out| encoder.read(&repository, out), 1);
        let expected: &[u8] = if version == 2 {
            b"0014acknowledgments\n0008NAK\n0000"
        } else {
            b"0008NAK\n"
        };
        assert_eq!(actual, expected);
        assert_eq!(provider.bytes.get(), 0);
    }
}

#[test]
fn negotiation_rejects_absent_or_non_commit_client_boundaries_before_output() {
    let graph = Graph::new();
    let source = graph.repository();
    let mut validation = ValidationBuffers::new();
    let repository = validation.validate(&source);
    let mut buffers = http::SelectionBuffers::<1, 0, 0, 0>::new();
    for version in [0, 2] {
        for invalid in [ObjectId::Sha1([9; 20]), graph.ids.shared.unwrap()] {
            let shallow = format!("shallow {invalid}");
            let bytes = body(graph.head(), version, &[&shallow], false);
            let mut control = [0xa5; 128];
            let result = prepare(
                &repository,
                request(&bytes, version),
                buffers.storage(0),
                &mut control,
            );
            assert!(matches!(result, Err(Error::InvalidRequest)));
            assert_eq!(control, [0xa5; 128]);
        }
    }
}
