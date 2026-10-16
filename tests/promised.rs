#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    EmptyRepository, Error, File, Head, Object, ObjectFormat, ObjectId, RefRepository, Reference,
    Repository, Snapshot, ValidatedRepository,
    http::{self, Request, SelectionBuffers, WantOptions},
    io::SliceWriter,
    repository::{head_ref_limit, ref_name_limit, unborn_ref_limit},
};

fn packet(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend_from_slice(format!("{:04x}{text}", text.len() + 4).as_bytes());
}
fn body(ids: &[ObjectId], version: u8, done: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if version == 2 {
        packet(&mut bytes, "command=fetch\n");
        packet(
            &mut bytes,
            &format!("object-format={}\n", ids[0].format().name()),
        );
        bytes.extend_from_slice(b"0001");
    }
    for id in ids {
        packet(&mut bytes, &format!("want {id}\n"));
    }
    if version == 2 {
        if done {
            packet(&mut bytes, "done\n");
        }
        bytes.extend_from_slice(b"0000");
    } else {
        bytes.extend_from_slice(b"0000");
        if done {
            packet(&mut bytes, "done\n");
        }
    }
    bytes
}
fn request(bytes: &[u8], version: u8) -> Request<'_> {
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
        body: bytes,
    }
}
fn discovery(version: u8) -> Request<'static> {
    Request {
        method: "GET",
        path: "info/refs",
        query: Some("service=git-upload-pack"),
        content_type: None,
        git_protocol: match version {
            1 => Some("version=1"),
            2 => Some("version=2"),
            _ => None,
        },
        body: &[],
    }
}

// A provider lookup must not turn a physical/private ID into validated membership.
struct Pool<'a>(&'a Snapshot<'a>, ObjectId, std::cell::Cell<usize>);
impl Repository for Pool<'_> {
    fn head(&self) -> ObjectId {
        self.0.head()
    }
    fn object_format(&self) -> ObjectFormat {
        self.0.object_format()
    }
    fn object_count(&self) -> usize {
        self.0.object_count()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.0.object(index)
    }
    fn read_object(
        &self,
        index: usize,
        offset: u64,
        storage: &mut [u8],
    ) -> git_zero::Result<usize> {
        self.2.set(self.2.get() + 1);
        self.0.read_object(index, offset, storage)
    }
    fn find_index(&self, id: ObjectId) -> Option<usize> {
        if id == self.1 {
            Some(0)
        } else {
            self.0.find_index(id)
        }
    }
}

#[test]
fn reachable_grant_is_request_local_and_denies_mixed_private_wants_before_output() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let files = [File {
            name: "public",
            contents: b"public\n",
        }];
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let snapshot = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        let private = ObjectId::from_bytes(format, &vec![7; format.id_length()]).unwrap();
        let pool = Pool(&snapshot, private, std::cell::Cell::new(0));
        let mut metadata = [None; 3];
        let mut by_id = [0; 3];
        let repository = ValidatedRepository::new(
            &pool,
            &mut metadata,
            &mut by_id,
            &mut [0; 16],
            &mut [0; 256],
        )
        .unwrap();
        assert_eq!(repository.find_index(private), None);
        let blob = snapshot.object_id(2).unwrap();
        pool.2.set(0);
        for version in [0, 1, 2] {
            let mut storage = SelectionBuffers::<3, 3, 16, 256>::new();
            let mut bytes = [0; 4096];
            pool.2.set(0);
            for ids in [[blob, private], [private, blob]] {
                for done in [false, true] {
                    let body = body(&ids, version, done);
                    let mut output = SliceWriter::new(&mut bytes);
                    assert_eq!(
                        allocations::without_allocations(|| http::serve_with_selection_policy(
                            &repository,
                            request(&body, version),
                            &mut storage.storage(4096),
                            WantOptions::all_visible_reachable(2),
                            &mut output
                        ))
                        .map(|_| ()),
                        Err(Error::InvalidRequest)
                    );
                    assert!(output.as_bytes().is_empty());
                    assert_eq!(
                        pool.2.get(),
                        0,
                        "denied wants performed provider body reads"
                    );
                }
            }
            for id in [blob, snapshot.object_id(1).unwrap()] {
                let body = body(&[id], version, true);
                let mut output = SliceWriter::new(&mut bytes);
                assert_eq!(
                    http::serve_with_selection(
                        &repository,
                        request(&body, version),
                        &mut storage.storage(4096),
                        &mut output
                    )
                    .map(|_| ()),
                    Err(Error::InvalidRequest)
                );
                assert!(output.as_bytes().is_empty());
                let options = WantOptions::all_visible_reachable(1);
                allocations::without_allocations(|| {
                    http::serve_with_selection_policy(
                        &repository,
                        request(&body, version),
                        &mut storage.storage(4096),
                        options,
                        &mut output,
                    )
                })
                .unwrap();
                let expected = output.as_bytes().to_vec();
                let mut control = [0; 512];
                let mut encoder = allocations::without_allocations(|| {
                    http::stream::ResponseEncoder::with_selection_storage_policy(
                        &repository,
                        request(&body, version),
                        storage.storage(4096),
                        options,
                        &mut control,
                    )
                })
                .unwrap();
                let mut actual = Vec::new();
                let mut one = [0; 1];
                loop {
                    let count =
                        allocations::without_allocations(|| encoder.read(&repository, &mut one))
                            .unwrap();
                    if count == 0 {
                        break;
                    }
                    actual.extend_from_slice(&one[..count]);
                }
                if version == 2 {
                    assert_eq!(v2_pack(&actual), v2_pack(&expected));
                } else {
                    assert_eq!(actual, expected);
                }
                let mut output = SliceWriter::new(&mut bytes);
                assert_eq!(
                    http::serve_with_selection(
                        &repository,
                        request(&body, version),
                        &mut storage.storage(4096),
                        &mut output
                    )
                    .map(|_| ()),
                    Err(Error::InvalidRequest)
                );
            }
            let duplicate = body(&[blob, blob], version, true);
            let mut output = SliceWriter::new(&mut bytes);
            assert_eq!(
                http::serve_with_selection_policy(
                    &repository,
                    request(&duplicate, version),
                    &mut storage.storage(0),
                    WantOptions::all_visible_reachable(1),
                    &mut output
                )
                .map(|_| ()),
                Err(Error::LimitExceeded)
            );
            assert!(output.as_bytes().is_empty());
            // Explicit leaf blobs consume no structural graph budget, and a retry resets the occurrence counter.
            http::serve_with_selection_policy(
                &repository,
                request(&duplicate, version),
                &mut storage.storage(0),
                WantOptions::all_visible_reachable(2),
                &mut output,
            )
            .unwrap();
        }
    }
}

fn v2_pack(mut wire: &[u8]) -> Vec<u8> {
    let mut pack = Vec::new();
    while let Some(packet) = git_zero::pktline::read(&mut wire).unwrap() {
        if let git_zero::pktline::Packet::Data(data) = packet {
            if data.first() == Some(&1) {
                pack.extend_from_slice(&data[1..]);
            }
        }
    }
    pack
}

fn check_advertisement(
    repository: &impl Repository,
    options: WantOptions,
    expected: Result<(), Error>,
) {
    let mut metadata = [None; 3];
    let mut by_id = [0; 3];
    let validated = ValidatedRepository::new(
        repository,
        &mut metadata,
        &mut by_id,
        &mut [0; 16],
        &mut [0; 512],
    )
    .unwrap();
    let mut buffers = SelectionBuffers::<1, 3, 16, 512>::new();
    let mut bytes = vec![0; 200_000];
    for version in [0, 1] {
        let mut output = SliceWriter::new(&mut bytes);
        let result = allocations::without_allocations(|| {
            http::serve_with_selection_policy(
                &validated,
                discovery(version),
                &mut buffers.storage(4096),
                options,
                &mut output,
            )
        });
        assert_eq!(result.map(|_| ()), expected);
        if expected.is_err() {
            assert!(output.as_bytes().is_empty());
        } else {
            assert_eq!(
                output
                    .as_bytes()
                    .windows(29)
                    .any(|s| s == b" allow-reachable-sha1-in-want"),
                options != WantOptions::STRICT
            );
        }
    }
}

#[test]
fn capability_packet_limits_cover_symbolic_unborn_and_first_ref_in_both_formats() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let snapshot = Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap();
        for extra in [0, 1, 29] {
            let name = format!(
                "refs/heads/{}",
                "x".repeat(head_ref_limit(format) - 11 - 29 + extra)
            );
            let refs = [Reference::direct(&name, snapshot.head())];
            let repo = RefRepository::new(
                &snapshot,
                Head::Symbolic {
                    target: &name,
                    id: snapshot.head(),
                },
                &refs,
            )
            .unwrap();
            check_advertisement(&repo, WantOptions::STRICT, Ok(()));
            check_advertisement(
                &repo,
                WantOptions::all_visible_reachable(1),
                if extra == 0 {
                    Ok(())
                } else {
                    Err(Error::LimitExceeded)
                },
            );
            let name = format!(
                "refs/heads/{}",
                "x".repeat(unborn_ref_limit(format) - 11 - 29 + extra)
            );
            let empty = EmptyRepository::new_with_format(&name, format).unwrap();
            check_advertisement(&empty, WantOptions::STRICT, Ok(()));
            check_advertisement(
                &empty,
                WantOptions::all_visible_reachable(1),
                if extra == 0 {
                    Ok(())
                } else {
                    Err(Error::LimitExceeded)
                },
            );
        }
        // Unborn HEAD with refs moves all capabilities to the first ref, whose
        // existing plain-name limit does not reserve any capability suffix.
        let suffix = b"\0".len()
            + format.capability().len()
            + git_zero::repository::UPLOAD_CAPABILITIES.len()
            + 29;
        for extra in [0, 1] {
            let name = format!(
                "refs/heads/{}",
                "x".repeat(ref_name_limit(format) - 11 - suffix + extra)
            );
            let refs = [Reference::direct(&name, snapshot.head())];
            let repo =
                RefRepository::new(&snapshot, Head::Unborn("refs/heads/new"), &refs).unwrap();
            check_advertisement(
                &repo,
                WantOptions::all_visible_reachable(1),
                if extra == 0 {
                    Ok(())
                } else {
                    Err(Error::LimitExceeded)
                },
            );
        }
    }
}
