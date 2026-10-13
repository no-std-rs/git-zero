#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    EmptyRepository, Error, File, Head, IndexedRepository, Object, ObjectId, Repository, Snapshot,
    ValidatedRepository,
    http::{self, Request},
    io::SliceWriter,
    pktline::{self, Packet},
    repository::{MAX_UNBORN_REF_LENGTH, validate_refs},
};

struct WithHead<'a, R> {
    source: &'a R,
    state: Head<'a>,
    refs: &'a [&'a str],
}

impl<R: Repository> Repository for WithHead<'_, R> {
    fn head(&self) -> ObjectId {
        self.state.id().unwrap_or(ObjectId([0; 20]))
    }
    fn head_state(&self) -> Head<'_> {
        self.state
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.source.object(index)
    }
    fn reference_count(&self) -> usize {
        self.refs.len()
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        self.refs.get(index).copied()
    }
}

fn response(repository: &impl Repository, path: &str, smart: bool) -> Vec<u8> {
    let mut storage = vec![0; 70_000];
    let mut output = SliceWriter::new(&mut storage);
    assert_eq!(
        http::serve(
            repository,
            Request {
                method: "GET",
                path,
                query: smart.then_some("service=git-upload-pack"),
                body: &[],
            },
            &mut output,
        )
        .unwrap()
        .status,
        200,
    );
    output.as_bytes().to_vec()
}

#[test]
fn empty_repository_advertises_unborn_head_and_rejects_all_wants() {
    let repository = EmptyRepository::new("refs/heads/new").unwrap();
    assert_eq!(repository.head_state(), Head::Unborn("refs/heads/new"));
    assert_eq!(repository.head_state().id(), None);
    assert_eq!(repository.reference_name(0), None);
    assert_eq!(
        response(&repository, "HEAD", false),
        b"ref: refs/heads/new\n"
    );
    assert!(response(&repository, "info/refs", false).is_empty());
    assert!(response(&repository, "objects/info/packs", false).is_empty());
    let wire = response(&repository, "info/refs", true);
    let mut packets = wire.as_slice();
    assert_eq!(
        pktline::read(&mut packets),
        Ok(Some(Packet::Data(b"# service=git-upload-pack\n")))
    );
    assert_eq!(pktline::read(&mut packets), Ok(Some(Packet::Flush)));
    assert_eq!(pktline::read(&mut packets), Ok(Some(Packet::Data(
        b"0000000000000000000000000000000000000000 capabilities^{}\0symref=HEAD:refs/heads/new\n"
    ))));
    assert_eq!(pktline::read(&mut packets), Ok(Some(Packet::Flush)));
    assert!(packets.is_empty());
    for id in [ObjectId([0; 20]), ObjectId([1; 20])] {
        let request = format!("0032want {id}\n00000009done\n").into_bytes();
        let mut storage = [0; 128];
        let mut output = SliceWriter::new(&mut storage);
        let mut called = false;
        assert!(matches!(
            http::serve_with_pack(
                &repository,
                Request {
                    method: "POST",
                    path: "git-upload-pack",
                    query: None,
                    body: &request
                },
                &mut [],
                &mut output,
                |_, _| {
                    called = true;
                    Ok(())
                },
            ),
            Err(Error::InvalidRequest)
        ));
        assert!(!called);
        assert!(output.as_bytes().is_empty());
    }
}

#[test]
fn head_state_survives_indexes_and_allocation_free_validation() {
    let empty = EmptyRepository::new("refs/heads/main").unwrap();
    allocations::without_allocations(|| {
        let indexed = IndexedRepository::new(&empty, &mut [], &mut []).unwrap();
        let validated =
            ValidatedRepository::new(&indexed, &mut [], &mut [], &mut [], &mut []).unwrap();
        assert_eq!(validated.head_state(), empty.head_state());
        let mut storage = [0; 256];
        http::serve(
            &validated,
            Request {
                method: "GET",
                path: "info/refs",
                query: Some("service=git-upload-pack"),
                body: &[],
            },
            &mut SliceWriter::new(&mut storage),
        )
        .unwrap();
    });
    let files = [File {
        name: "file",
        contents: b"contents\n",
    }];
    let mut tree = [0; 128];
    let mut commit = [0; 256];
    let snapshot = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    for refs in [&[][..], &["refs/heads/main"][..]] {
        let detached = WithHead {
            source: &snapshot,
            state: Head::Detached(snapshot.head()),
            refs,
        };
        let mut metadata = [None; 3];
        let mut by_id = [0; 3];
        let indexed = IndexedRepository::new(&detached, &mut metadata, &mut by_id).unwrap();
        let mut metadata = [None; 3];
        let mut by_id = [0; 3];
        let validated = allocations::without_allocations(|| {
            ValidatedRepository::new(
                &indexed,
                &mut metadata,
                &mut by_id,
                &mut [0; 1],
                &mut [0; 128],
            )
            .unwrap()
        });
        assert_eq!(validated.head_state(), detached.head_state());
        assert_eq!(
            response(&validated, "HEAD", false),
            format!("{}\n", snapshot.head()).as_bytes()
        );
        let wire = response(&validated, "info/refs", true);
        assert!(!wire.windows(7).any(|bytes| bytes == b"symref="));
        let mut storage = [0; 2048];
        let mut output = SliceWriter::new(&mut storage);
        let request = format!("0032want {}\n00000009done\n", snapshot.head()).into_bytes();
        http::serve(
            &validated,
            Request {
                method: "POST",
                path: "git-upload-pack",
                query: None,
                body: &request,
            },
            &mut output,
        )
        .unwrap();
        git_zero::pack::validate(&output.as_bytes()[8..]).unwrap();
    }
}

#[test]
fn inconsistent_heads_and_unborn_packet_limits_fail_explicitly() {
    for target in ["main", "refs/tags/main", "refs/heads/../main"] {
        assert!(matches!(
            EmptyRepository::new(target),
            Err(Error::InvalidReference)
        ));
    }
    let target = format!("refs/heads/{}", "x".repeat(MAX_UNBORN_REF_LENGTH - 11));
    let empty = EmptyRepository::new(&target).unwrap();
    let wire = response(&empty, "info/refs", true);
    let mut packets = &wire[34..];
    assert!(matches!(
        pktline::read(&mut packets),
        Ok(Some(Packet::Data(_)))
    ));
    assert!(matches!(
        EmptyRepository::new(&format!("{target}x")),
        Err(Error::InvalidReference)
    ));
    for (state, refs, expected) in [
        (
            Head::Detached(ObjectId([0; 20])),
            &[][..],
            Error::InvalidObject,
        ),
        (
            Head::Unborn("refs/heads/main"),
            &["refs/heads/main"][..],
            Error::InvalidObject,
        ),
        (
            Head::Symbolic {
                target: "refs/heads/main",
                id: ObjectId([1; 20]),
            },
            &[][..],
            Error::InvalidReference,
        ),
    ] {
        assert_eq!(
            validate_refs(&WithHead {
                source: &empty,
                state,
                refs
            }),
            Err(expected)
        );
    }
    let files = [File {
        name: "file",
        contents: b"body",
    }];
    let mut tree = [0; 128];
    let mut commit = [0; 256];
    let snapshot = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let invalid = WithHead {
        source: &snapshot,
        state: Head::Unborn("refs/heads/main"),
        refs: &[],
    };
    assert_eq!(validate_refs(&invalid), Err(Error::InvalidObject));
}
