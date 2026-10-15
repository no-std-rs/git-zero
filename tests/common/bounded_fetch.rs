use super::*;
use git_zero::{BlobFilter, FetchOptions, SelectionOptions};

fn select(
    graph: &Graph,
    roots: &[ObjectId],
    common: &[ObjectId],
    shallow: &[ObjectId],
    options: FetchOptions,
    tags: bool,
) -> (Vec<ObjectId>, Vec<ObjectId>, Vec<ObjectId>) {
    validated(&graph.repository(), |repository| {
        let mut work = [None; CAPACITY];
        let mut indices = [0; CAPACITY];
        let indices = allocations::without_allocations(|| {
            repository
                .select_bounded_fetch(
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
                .unwrap()
        });
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
    })
}

#[test]
fn absolute_depths_preserve_merge_boundaries_and_reset_shallow_haves() {
    let graph = Graph::new();
    let ids = graph.ids;
    let merge = ids.merge.unwrap();
    for depth in [1, 2, 3, i32::MAX as u32] {
        let (selected, shallow, unshallow) = select(
            &graph,
            &[merge],
            &[merge],
            &[merge],
            FetchOptions {
                depth: Some(depth),
                filter: BlobFilter::All,
            },
            false,
        );
        let omitted = if depth == 1 {
            vec![
                ids.base,
                ids.base_tree,
                ids.base_blob,
                ids.main,
                ids.main_tree,
                ids.side,
                ids.side_tree,
            ]
        } else if depth == 2 {
            vec![ids.base, ids.base_tree, ids.base_blob]
        } else {
            vec![]
        };
        let expected: Vec<_> = graph
            .merge_closure()
            .into_iter()
            .filter(|id| !omitted.contains(&Some(*id)))
            .collect();
        assert_eq!(selected, sorted(expected));
        assert_eq!(
            shallow,
            sorted(match depth {
                1 => vec![merge],
                2 => vec![ids.main.unwrap(), ids.side.unwrap()],
                3 => vec![ids.base.unwrap()],
                _ => vec![],
            })
        );
        assert_eq!(unshallow, if depth == 1 { vec![] } else { vec![merge] });
    }
    let (selected, _, _) = select(
        &graph,
        &[merge],
        &[ids.main.unwrap()],
        &[ids.main.unwrap()],
        FetchOptions::default(),
        false,
    );
    // A shallow have owns its tree, but cannot exclude its missing parents.
    assert!(selected.contains(&ids.base.unwrap()));
    assert!(selected.contains(&ids.base_blob.unwrap()));
    assert!(!selected.contains(&ids.main.unwrap()));
    let (_, boundary, unshallow) = select(
        &graph,
        &[ids.base.unwrap()],
        &[],
        &[ids.base.unwrap()],
        FetchOptions::default(),
        false,
    );
    assert_eq!(boundary, vec![ids.base.unwrap()]);
    assert!(unshallow.is_empty());
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
                filter: BlobFilter::None,
            },
            false,
        );
        assert!(selected.contains(&graph.ids.main.unwrap()));
        assert!(selected.contains(&graph.ids.side.unwrap()));
        assert!(!selected.contains(&graph.ids.base.unwrap()));
        assert_eq!(
            boundaries,
            sorted(vec![graph.ids.main.unwrap(), graph.ids.side.unwrap()])
        );
        graph.objects.reverse();
    }
}

#[test]
fn filters_keep_explicit_blobs_and_tags_do_not_restart_commit_depths() {
    let graph = Graph::new();
    for filter in [BlobFilter::None, BlobFilter::Limit(5), BlobFilter::Limit(6)] {
        let (selected, _, _) = select(
            &graph,
            &[graph.ids.merge.unwrap(), graph.ids.shared.unwrap()],
            &[],
            &[],
            FetchOptions {
                depth: None,
                filter,
            },
            false,
        );
        assert!(selected.contains(&graph.ids.shared.unwrap()));
        assert_eq!(
            selected.contains(&graph.ids.main_blob.unwrap()),
            filter == BlobFilter::Limit(6)
        );
    }
    let (selected, shallow, _) = select(
        &graph,
        &[graph.ids.merge.unwrap()],
        &[],
        &[],
        FetchOptions {
            depth: Some(2),
            filter: BlobFilter::None,
        },
        true,
    );
    assert!(selected.contains(&graph.ids.nested.unwrap()));
    assert!(!selected.contains(&graph.ids.base.unwrap()));
    assert_eq!(
        shallow,
        sorted(vec![graph.ids.main.unwrap(), graph.ids.side.unwrap()])
    );
    let (_, shallow, _) = select(
        &graph,
        &[graph.ids.nested.unwrap()],
        &[],
        &[],
        FetchOptions {
            depth: Some(1),
            filter: BlobFilter::All,
        },
        false,
    );
    assert_eq!(shallow, vec![graph.ids.side.unwrap()]);
}

fn body(tip: ObjectId, version: u8, args: &[String], done: bool) -> Vec<u8> {
    let mut result = Vec::new();
    let mut packet =
        |line: &str| result.extend_from_slice(format!("{:04x}{line}\n", line.len() + 5).as_bytes());
    if version == 2 {
        packet("command=fetch");
        packet("object-format=sha1");
        result.extend_from_slice(b"0001");
    }
    let line = format!("want {tip}{}", if version == 2 { "" } else { " filter" });
    result.extend_from_slice(format!("{:04x}{line}\n", line.len() + 5).as_bytes());
    for arg in args {
        result.extend_from_slice(format!("{:04x}{arg}\n", arg.len() + 5).as_bytes());
    }
    if version != 2 {
        result.extend_from_slice(b"0000");
    }
    if done {
        result.extend_from_slice(b"0009done\n");
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
        body,
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: Some(if version == 2 {
            "version=2"
        } else {
            "version=0"
        }),
    }
}

#[test]
fn extensions_preflight_all_budgets_and_failures_leave_output_empty_then_retry() {
    let graph = Graph::new();
    validated(&graph.repository(), |repository| {
        let mut wants = [ZERO; 4];
        let mut objects = [None; CAPACITY];
        let mut indices = [0; CAPACITY];
        let mut storage = SelectionStorage {
            wants: &mut wants,
            objects: &mut objects,
            indices: &mut indices,
            read: &mut [0; 1],
            parse: &mut [0; 128],
            graph_read_limit: 32768,
        };
        for version in [0, 2] {
            let mut invalid = vec![
                "deepen 0".to_string(),
                "deepen -1".into(),
                "deepen 2147483648".into(),
                "deepen-relative".into(),
                "deepen-since 1".into(),
                "deepen-not main".into(),
                "filter tree:0".into(),
                "filter blob:limit=".into(),
                "filter blob:limit=18446744073709551615k".into(),
                format!("shallow {}", graph.ids.shared.unwrap()),
                format!("shallow {}", ObjectId::Sha1([9; 20])),
            ];
            invalid.push("shallow invalid".into());
            for arg in invalid {
                let bytes = body(graph.head(), version, &[arg], true);
                let mut output_bytes = [0; 4096];
                let mut output = SliceWriter::new(&mut output_bytes);
                assert!(
                    allocations::without_allocations(|| http::serve_with_selection(
                        repository,
                        request(&bytes, version),
                        &mut storage,
                        &mut output
                    ))
                    .is_err()
                );
                assert!(output.is_empty());
            }
            for args in [
                vec!["deepen 1".into(), "deepen 2".into()],
                vec!["filter blob:none".into(), "filter blob:none".into()],
            ] {
                let bytes = body(graph.head(), version, &args, true);
                let mut output_bytes = [0; 4096];
                let mut output = SliceWriter::new(&mut output_bytes);
                assert!(
                    http::serve_with_selection(
                        repository,
                        request(&bytes, version),
                        &mut storage,
                        &mut output
                    )
                    .is_err()
                );
                assert!(output.is_empty());
            }
            let bytes = body(
                graph.head(),
                version,
                &["deepen 1".into(), "filter blob:none".into()],
                true,
            );
            let mut output_bytes = [0; 4096];
            let mut output = SliceWriter::new(&mut output_bytes);
            storage.graph_read_limit = 0;
            assert!(matches!(
                http::serve_with_selection(
                    repository,
                    request(&bytes, version),
                    &mut storage,
                    &mut output
                ),
                Err(Error::LimitExceeded)
            ));
            assert!(output.is_empty());
            storage.graph_read_limit = 32768;
            allocations::without_allocations(|| {
                http::serve_with_selection(
                    repository,
                    request(&bytes, version),
                    &mut storage,
                    &mut output,
                )
            })
            .unwrap();
            let expected = output.as_bytes().to_vec();
            let mut control = [0; 4096];
            let mut encoder = allocations::without_allocations(|| {
                http::stream::ResponseEncoder::with_selection(
                    repository,
                    request(&bytes, version),
                    &mut storage,
                    &mut control,
                )
            })
            .unwrap();
            let mut collected = Vec::new();
            loop {
                let mut guard = [0xa5; 3];
                let count =
                    allocations::without_allocations(|| encoder.read(repository, &mut guard[1..2]))
                        .unwrap();
                assert_eq!(guard[0], 0xa5);
                assert_eq!(guard[2], 0xa5);
                if count == 0 {
                    break;
                }
                collected.push(guard[1]);
            }
            if version == 2 {
                let unpack = |wire: &[u8]| {
                    let header = b"000dpackfile\n";
                    let start = wire
                        .windows(header.len())
                        .position(|w| w == header)
                        .unwrap()
                        + header.len();
                    let prefix = wire[..start].to_vec();
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
                    (prefix, packed)
                };
                assert_eq!(unpack(&collected), unpack(&expected));
            } else {
                assert_eq!(collected, expected);
            }
            encoder.cancel();
            assert_eq!(
                encoder.read(repository, &mut [0; 1]),
                Err(Error::InvalidRequest)
            );
        }
        let probe = body(graph.head(), 0, &["deepen 1".into()], false);
        let mut output_bytes = [0; 4096];
        let mut output = SliceWriter::new(&mut output_bytes);
        http::serve_with_selection(
            repository,
            Request {
                method: "POST",
                path: "git-upload-pack",
                query: None,
                body: &probe,
                content_type: Some("application/x-git-upload-pack-request"),
                git_protocol: None,
            },
            &mut storage,
            &mut output,
        )
        .unwrap();
        let mut wire = output.as_bytes();
        assert!(
            matches!(pktline::read(&mut wire).unwrap(), Some(pktline::Packet::Data(line)) if line.starts_with(b"shallow "))
        );
        assert_eq!(
            pktline::read(&mut wire).unwrap(),
            Some(pktline::Packet::Flush)
        );
        assert!(wire.is_empty());
    });
}
