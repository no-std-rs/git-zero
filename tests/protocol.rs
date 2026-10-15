#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, ObjectFormat, ObjectId, Repository, Snapshot, ValidatedRepository,
    http::{self, Request, SelectionStorage},
    io::SliceWriter,
    pack,
    pktline::{self, Packet},
};

fn packet(body: &mut Vec<u8>, line: &str) {
    body.extend_from_slice(format!("{:04x}{line}", line.len() + 4).as_bytes());
}

fn command(name: &str, format: ObjectFormat, args: &[String]) -> Vec<u8> {
    let mut body = Vec::new();
    packet(&mut body, &format!("command={name}\n"));
    packet(
        &mut body,
        core::str::from_utf8(format.capability()).unwrap(),
    );
    body.extend_from_slice(b"0001");
    for arg in args {
        packet(&mut body, arg);
    }
    body.extend_from_slice(b"0000");
    body
}

fn request(body: &[u8]) -> Request<'_> {
    Request {
        method: "POST",
        path: "git-upload-pack",
        query: None,
        body,
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: Some("version=2"),
    }
}

fn each_format(mut check: impl FnMut(&Snapshot<'_>)) {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 128];
        let mut commit = [0; 512];
        check(&Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap());
    }
}

fn response(repository: &impl Repository, request: Request<'_>) -> (u16, Vec<u8>) {
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    let response =
        allocations::without_allocations(|| http::serve(repository, request, &mut output)).unwrap();
    (response.status, output.as_bytes().to_vec())
}

#[test]
fn discovery_v1_v2_and_ls_refs_are_format_aware_and_allocation_free() {
    each_format(|repository| {
        for version in ["version=1", "version=2"] {
            let (_, wire) = response(
                repository,
                Request {
                    method: "GET",
                    path: "/info/refs",
                    query: Some("service=git-upload-pack"),
                    body: &[],
                    content_type: None,
                    git_protocol: Some(version),
                },
            );
            assert!(
                wire.windows(version.len())
                    .any(|w| w == version.replace('=', " ").as_bytes())
            );
            assert!(
                wire.windows(repository.object_format().capability().len())
                    .any(|w| w == repository.object_format().capability())
            );
            if version == "version=2" {
                assert!(wire.windows(9).any(|w| w == b"ls-refs=u"));
                assert!(!wire.windows(7).any(|w| w == b"shallow" || w == b"filter="));
            }
        }
        let body = command(
            "ls-refs",
            repository.object_format(),
            &[
                "symrefs\n".into(),
                "peel\n".into(),
                "ref-prefix HEAD\n".into(),
            ],
        );
        let (_, wire) = response(repository, request(&body));
        let mut wire = wire.as_slice();
        assert_eq!(
            pktline::read(&mut wire).unwrap(),
            Some(Packet::Data(
                format!("{} HEAD symref-target:refs/heads/main\n", repository.head()).as_bytes()
            ))
        );
        assert_eq!(pktline::read(&mut wire).unwrap(), Some(Packet::Flush));
        assert!(wire.is_empty());
    });
}

#[test]
fn v2_fetch_done_omits_ack_section_and_always_frames_pack() {
    each_format(|repository| {
        let body = command(
            "fetch",
            repository.object_format(),
            &[
                "thin-pack\n".into(),
                "ofs-delta\n".into(),
                "no-progress\n".into(),
                "include-tag\n".into(),
                format!("want {}\n", repository.head()),
                "done\n".into(),
            ],
        );
        let (_, wire) = response(repository, request(&body));
        let mut wire = wire.as_slice();
        assert_eq!(
            pktline::read(&mut wire).unwrap(),
            Some(Packet::Data(b"packfile\n"))
        );
        let mut packed = Vec::new();
        while let Some(Packet::Data(data)) = pktline::read(&mut wire).unwrap() {
            assert_eq!(data[0], 1);
            packed.extend_from_slice(&data[1..]);
        }
        assert!(wire.is_empty());
        pack::validate_with_format(&packed, repository.object_format()).unwrap();
    });
}

#[test]
fn v2_rounds_ack_known_commits_and_do_not_mix_nak_or_pack() {
    each_format(|repository| {
        let unknown = ObjectId::from_bytes(
            repository.object_format(),
            &[9; 32][..repository.object_format().id_length()],
        )
        .unwrap();
        for known in [false, true] {
            let mut args = vec![
                format!("want {}\n", repository.head()),
                format!("have {unknown}\n"),
            ];
            if known {
                args.push(format!("have {}\n", repository.head()));
            }
            let body = command("fetch", repository.object_format(), &args);
            let (_, wire) = response(repository, request(&body));
            let mut wire = wire.as_slice();
            assert_eq!(
                pktline::read(&mut wire).unwrap(),
                Some(Packet::Data(b"acknowledgments\n"))
            );
            let ack = if known {
                format!("ACK {}\n", repository.head())
            } else {
                "NAK\n".into()
            };
            assert_eq!(
                pktline::read(&mut wire).unwrap(),
                Some(Packet::Data(ack.as_bytes()))
            );
            assert_eq!(pktline::read(&mut wire).unwrap(), Some(Packet::Flush));
            assert!(wire.is_empty());
        }
    });
}

#[test]
fn unborn_ls_refs_requires_symrefs_to_describe_the_missing_head_target() {
    let repository = git_zero::EmptyRepository::new("refs/heads/new/default").unwrap();
    for args in [vec![], vec!["unborn\n".into()], vec!["symrefs\n".into()]] {
        let body = command("ls-refs", ObjectFormat::Sha1, &args);
        assert_eq!(
            response(&repository, request(&body)),
            (200, b"0000".to_vec())
        );
    }
    let body = command(
        "ls-refs",
        ObjectFormat::Sha1,
        &["unborn\n".into(), "symrefs\n".into()],
    );
    let (_, wire) = response(&repository, request(&body));
    let mut wire = wire.as_slice();
    assert_eq!(
        pktline::read(&mut wire).unwrap(),
        Some(Packet::Data(
            b"unborn HEAD symref-target:refs/heads/new/default\n"
        ))
    );
    assert_eq!(pktline::read(&mut wire).unwrap(), Some(Packet::Flush));
    assert!(wire.is_empty());
}

fn rejected(repository: &impl Repository, body: &[u8]) {
    let mut bytes = [0xcc; 4098];
    let mut output = SliceWriter::new(&mut bytes[1..4097]);
    let mut called = false;
    let result = allocations::without_allocations(|| {
        http::serve_with_pack(repository, request(body), &mut [], &mut output, |_, _| {
            called = true;
            Ok(())
        })
    });
    assert!(result.is_err(), "invalid request passed: {body:?}");
    assert!(!called && output.is_empty());
    assert_eq!((bytes[0], bytes[4097]), (0xcc, 0xcc));
}

#[test]
fn malformed_v2_commands_are_rejected_before_output_or_pack_hooks() {
    each_format(|repository| {
        let format = repository.object_format();
        let valid = command(
            "fetch",
            format,
            &[format!("want {}\n", repository.head()), "done\n".into()],
        );
        for length in 0..valid.len() {
            rejected(repository, &valid[..length]);
        }
        for arg in [
            "shallow bad\n",
            "filter blob:none\n",
            "deepen 1\n",
            "sideband-all\n",
            "packfile-uris https\n",
            "want-ref refs/heads/main\n",
            "wait-for-done\n",
            "done\ndone\n",
        ] {
            rejected(
                repository,
                &command(
                    "fetch",
                    format,
                    &[format!("want {}\n", repository.head()), arg.into()],
                ),
            );
        }
        for invalid in [
            b"0001".as_slice(),
            b"0002",
            b"00000000",
            b"000fcommand=xx\n00010000",
        ] {
            rejected(repository, invalid);
        }
        let mut trailing = valid.clone();
        trailing.extend_from_slice(b"0000");
        rejected(repository, &trailing);
        let mut duplicate = Vec::new();
        packet(&mut duplicate, "command=fetch\n");
        packet(&mut duplicate, "object-format=sha1\n");
        packet(&mut duplicate, "object-format=sha1\n");
        duplicate.extend_from_slice(b"00010000");
        rejected(repository, &duplicate);
        assert_eq!(response(repository, request(b"0000")), (200, vec![]));
    });
}

#[test]
fn strict_http_metadata_and_canonical_routes_precede_all_output() {
    each_format(|repository| {
        let baseline = request(b"0000");
        for (request, status) in [
            (
                Request {
                    content_type: None,
                    ..baseline
                },
                415,
            ),
            (
                Request {
                    content_type: Some("text/plain"),
                    ..baseline
                },
                415,
            ),
            (
                Request {
                    method: "GET",
                    ..baseline
                },
                405,
            ),
            (
                Request {
                    method: "POST",
                    path: "info/refs",
                    ..baseline
                },
                405,
            ),
            (
                Request {
                    method: "GET",
                    path: "info/refs",
                    query: Some("service=git-receive-pack"),
                    ..baseline
                },
                403,
            ),
        ] {
            let (actual, bytes) = response(repository, request);
            assert_eq!(actual, status);
            assert!(bytes.is_empty());
        }
        for path in [
            "//HEAD",
            "./HEAD",
            "a/../HEAD",
            "refs/%2e%2e/HEAD",
            "refs/%2Fmain",
            "refs/%00main",
            "refs/%zz",
            "refs/main/",
            "refs\\main",
            "HEAD#x",
        ] {
            let mut bytes = [0; 128];
            let mut output = SliceWriter::new(&mut bytes);
            assert!(
                http::serve(
                    repository,
                    Request {
                        path,
                        method: "GET",
                        ..baseline
                    },
                    &mut output
                )
                .is_err(),
                "{path}"
            );
            assert!(output.is_empty());
        }
        for protocol in ["version=3", "version=2:version=1", "version=2\r\n"] {
            let mut bytes = [0; 128];
            assert!(
                http::serve(
                    repository,
                    Request {
                        git_protocol: Some(protocol),
                        ..baseline
                    },
                    &mut SliceWriter::new(&mut bytes)
                )
                .is_err()
            );
        }
        let mut bytes = [0; 128];
        assert!(
            http::serve(
                repository,
                Request {
                    query: Some("service=git-upload-pack&service=git-upload-pack"),
                    ..baseline
                },
                &mut SliceWriter::new(&mut bytes)
            )
            .is_err()
        );
    });
}

#[test]
fn selected_v2_fetch_enforces_budgets_and_resets_state_after_failure() {
    each_format(|snapshot| {
        let mut metadata = [None; 2];
        let mut by_id = [0; 2];
        let repository = ValidatedRepository::new(
            snapshot,
            &mut metadata,
            &mut by_id,
            &mut [0; 1],
            &mut [0; 128],
        )
        .unwrap();
        let mut wants = [snapshot.head(); 1];
        let mut work = [None; 2];
        let mut indices = [0; 2];
        let mut read = [0; 1];
        let mut parse = [0; 128];
        let mut selection = SelectionStorage {
            wants: &mut wants,
            objects: &mut work,
            indices: &mut indices,
            read: &mut read,
            parse: &mut parse,
            graph_read_limit: 0,
        };
        let body = command(
            "fetch",
            snapshot.object_format(),
            &[format!("want {}\n", snapshot.head()), "done\n".into()],
        );
        let mut bytes = [0; 4096];
        let mut output = SliceWriter::new(&mut bytes);
        assert!(matches!(
            http::serve_with_selection(&repository, request(&body), &mut selection, &mut output),
            Err(Error::LimitExceeded)
        ));
        assert!(output.is_empty());
        selection.graph_read_limit = 4096;
        allocations::without_allocations(|| {
            http::serve_with_selection(&repository, request(&body), &mut selection, &mut output)
        })
        .unwrap();
        let expected = output.as_bytes().to_vec();
        for capacity in [0, 4, 20] {
            assert!(
                http::serve_with_selection(
                    &repository,
                    request(&body),
                    &mut selection,
                    &mut SliceWriter::new(&mut [0; 20][..capacity])
                )
                .is_err()
            );
        }
        let mut output = SliceWriter::new(&mut bytes);
        http::serve_with_selection(&repository, request(&body), &mut selection, &mut output)
            .unwrap();
        assert_eq!(output.as_bytes(), expected);
    });
}
