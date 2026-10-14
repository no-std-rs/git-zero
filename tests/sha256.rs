#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/cache.rs"]
mod cache_backend;
#[path = "common/mod.rs"]
mod common;
#[path = "common/short_reads.rs"]
mod short_reads;

use git_zero::{
    EmptyRepository, Error, Head, IndexedRepository, Object, ObjectFormat, ObjectHasher, ObjectId,
    ObjectKind, RefRepository, Reference, Repository, Snapshot, SnapshotBuilder,
    ValidatedRepository, cache,
    http::{self, Request},
    io::{SliceWriter, Write},
    pack::{self, DeltaLimits, Reader, ResolvedObject},
    pktline::{self, Packet},
    repository::{head_ref_limit, ref_name_limit, unborn_ref_limit},
    zlib::{self, InflateLimits, WINDOW_SIZE},
};
use sha2::{Digest, Sha256};
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

const FORMAT: ObjectFormat = ObjectFormat::Sha256;
const INFLATE: InflateLimits = InflateLimits {
    max_output: 4096,
    max_ratio: 1024,
    ratio_slack: 32,
};

fn request(body: &[u8]) -> Request<'_> {
    Request {
        method: "POST",
        path: "git-upload-pack",
        query: None,
        body,
    }
}

fn upload(id: ObjectId, capabilities: &str, extra: &str) -> Vec<u8> {
    let line = format!("want {id}{capabilities}\n");
    let mut bytes = format!("{:04x}{line}", line.len() + 4).into_bytes();
    bytes.extend_from_slice(b"0000");
    if !extra.is_empty() {
        bytes.extend_from_slice(format!("{:04x}{extra}", extra.len() + 4).as_bytes());
    }
    bytes.extend_from_slice(b"0009done\n");
    bytes
}

fn git(directory: &std::path::Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut command =
        Command::new(std::env::var_os("GIT_ZERO_TEST_GIT").unwrap_or_else(|| "git".into()));
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("-C")
        .arg(directory)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn format_tagged_ids_preserve_every_byte_and_match_git_vectors() {
    // Independently computed SHA-256 of Git's canonical "blob 6\0hello\n".
    let blob = Object {
        kind: ObjectKind::Blob,
        data: b"hello\n",
    };
    let expected = Sha256::digest(b"blob 6\0hello\n");
    allocations::without_allocations(|| {
        let id = blob.id_with_format(FORMAT);
        assert_eq!(id.as_bytes(), expected.as_slice());
        assert_eq!(id.format(), FORMAT);
        assert_eq!(ObjectId::from_hex(&id.to_hex()), Ok(id));
        assert_eq!(id.to_hex().len(), 64);
        assert_eq!(ObjectId::from_bytes(FORMAT, &expected), Ok(id));
        assert_eq!(
            ObjectId::from_bytes(ObjectFormat::Sha1, &expected),
            Err(Error::InvalidObjectId)
        );
        assert_eq!(
            ObjectId::from_bytes(FORMAT, blob.id().as_bytes()),
            Err(Error::InvalidObjectId)
        );
        assert_ne!(FORMAT.zero_id(), ObjectFormat::Sha1.zero_id());
        let mut changed = expected;
        changed[31] ^= 1;
        assert_ne!(id, ObjectId::Sha256(changed.into()));
        for length in [0, 39, 41, 63, 65] {
            assert_eq!(
                ObjectId::from_hex(&[b'0'; 65][..length]),
                Err(Error::InvalidObjectId)
            );
        }
        assert_eq!(ObjectId::from_hex(&[b'g'; 64]), Err(Error::InvalidObjectId));
        assert_eq!(
            Object::from_loose(b"blob 6\0hello\n", id).unwrap().data,
            blob.data
        );
        assert!(Object::from_loose(b"blob 6\0Hello\n", id).is_err());
    });
}

#[test]
fn nested_sha256_builder_indexes_streams_decodes_and_caches_without_allocating() {
    let (mut trees, mut commit, mut entries) = ([0; 1024], [0; 512], [git_zero::Entry::EMPTY; 8]);
    let snapshot = allocations::without_allocations(|| {
        let mut builder = SnapshotBuilder::new(&mut entries, &mut trees, &mut commit);
        for entry in common::ENTRIES.iter().rev() {
            builder.push(*entry).unwrap();
        }
        builder.finish_with_format(FORMAT).unwrap()
    })
    .with_refs(common::HEAD_REF, common::REFS)
    .unwrap();
    let (mut metadata, mut indices) = ([None; 8], [0; 8]);
    let indexed = allocations::without_allocations(|| {
        IndexedRepository::new(&snapshot, &mut metadata, &mut indices).unwrap()
    });
    let streamed = short_reads::Streamed {
        source: &indexed,
        limit: 1,
    };
    let (mut validation, mut by_id) = ([None; 8], [0; 8]);
    let validated = allocations::without_allocations(|| {
        ValidatedRepository::new(
            &streamed,
            &mut validation,
            &mut by_id,
            &mut [0; 3],
            &mut [0; 128],
        )
        .unwrap()
    });
    let body = upload(validated.head(), " object-format=sha256", "");
    let (mut bytes, mut staging, mut window, mut cache_bytes) =
        ([0; 4096], [0; 512], [0; WINDOW_SIZE], [0; 4096]);
    let mut backend = cache_backend::Cache::new();
    allocations::without_allocations(|| {
        assert_eq!(validated.object_format(), FORMAT);
        assert_eq!(validated.object_count(), 6);
        for index in 0..validated.object_count() {
            assert_eq!(
                ObjectHasher::new(&validated, index)
                    .unwrap()
                    .finish(&mut [0; 3])
                    .unwrap(),
                validated.object_id(index).unwrap()
            );
        }
        let mut output = SliceWriter::new(&mut bytes);
        http::serve_with_scratch(&validated, request(&body), &mut [0; 3], &mut output).unwrap();
        let packed = &output.as_bytes()[8..];
        assert!(pack::validate(packed).is_err());
        pack::validate_with_format(packed, FORMAT).unwrap();
        let mut reader = Reader::new_with_format(packed, FORMAT).unwrap();
        for index in 0..validated.object_count() {
            let entry = reader
                .read_entry(&mut staging, &mut window, INFLATE)
                .unwrap()
                .unwrap();
            assert_eq!(
                entry.object().unwrap().id(),
                validated.object_id(index).unwrap()
            );
            assert_eq!(
                entry.kind(),
                pack::EntryKind::Object(validated.object_info(index).unwrap().kind)
            );
        }
        reader.finish().unwrap();
        let mut cached = SliceWriter::new(&mut cache_bytes);
        cache::write(&snapshot, &mut backend, &mut [0; 4096], &mut cached).unwrap();
        assert_eq!(cached.as_bytes(), packed);
        cached = SliceWriter::new(&mut cache_bytes);
        cache::write(&snapshot, &mut backend, &mut [], &mut cached).unwrap();
        assert_eq!(cached.as_bytes(), packed);
        assert_eq!(backend.puts, 1);
        assert_eq!(backend.key.unwrap().head.format(), FORMAT);
    });
}

#[test]
fn git_strictly_imports_both_formats_and_independently_verifies_snapshot_ids() {
    let directory = std::env::temp_dir().join(format!("git-zero-formats-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    for format in [ObjectFormat::Sha1, FORMAT] {
        let root = directory.join(format.name());
        std::fs::create_dir(&root).unwrap();
        git(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                &format!("--object-format={}", format.name()),
            ],
            None,
        );
        let (mut trees, mut commit) = ([0; 1024], [0; 512]);
        let snapshot =
            Snapshot::from_entries_with_format(common::ENTRIES, &mut trees, &mut commit, format)
                .unwrap();
        for index in 0..snapshot.object_count() {
            let object = snapshot.object(index).unwrap();
            let actual = git(
                &root,
                &["hash-object", "--stdin", "-t", object.kind.name()],
                Some(object.data),
            );
            assert_eq!(
                actual.strip_suffix(b"\n").unwrap(),
                snapshot.object_id(index).unwrap().to_hex().as_bytes()
            );
        }
        let mut storage = [0; 4096];
        let mut output = SliceWriter::new(&mut storage);
        pack::write(&snapshot, &mut output).unwrap();
        git(
            &root,
            &["index-pack", "--strict", "--stdin"],
            Some(output.as_bytes()),
        );
        git(
            &root,
            &[
                "update-ref",
                "refs/heads/main",
                &snapshot.head().to_string(),
            ],
            None,
        );
        git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"], None);
        git(&root, &["fsck", "--strict"], None);
        let modes = git(&root, &["ls-tree", "-r", "HEAD"], None);
        assert!(modes.windows(6).any(|value| value == b"100755"));
        assert!(modes.windows(6).any(|value| value == b"120000"));
    }
}

struct Graph<'a> {
    objects: &'a [Object<'a>],
    ids: Option<&'a [ObjectId]>,
}
impl Repository for Graph<'_> {
    fn object_format(&self) -> ObjectFormat {
        FORMAT
    }
    fn head(&self) -> ObjectId {
        self.object_id(0).unwrap()
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).copied()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.ids.map_or_else(
            || {
                self.object(index)
                    .map(|object| object.id_with_format(FORMAT))
            },
            |ids| ids.get(index).copied(),
        )
    }
}

fn validate(repository: &impl Repository) -> git_zero::Result<()> {
    ValidatedRepository::new(
        repository,
        &mut [None; 4],
        &mut [0; 4],
        &mut [0; 1],
        &mut [0; 128],
    )
    .map(|_| ())
}

#[test]
fn mixed_formats_in_refs_hashes_commit_tree_and_tag_links_are_rejected() {
    let blob = Object {
        kind: ObjectKind::Blob,
        data: b"hello\n",
    };
    for binary_format in [ObjectFormat::Sha1, FORMAT] {
        let mut tree_body = b"100644 file\0".to_vec();
        tree_body.extend_from_slice(blob.id_with_format(binary_format).as_bytes());
        let tree = Object {
            kind: ObjectKind::Tree,
            data: &tree_body,
        };
        for link_format in [ObjectFormat::Sha1, FORMAT] {
            let body = format!(
                "tree {}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nmessage\n",
                tree.id_with_format(link_format)
            );
            let objects = [
                Object {
                    kind: ObjectKind::Commit,
                    data: body.as_bytes(),
                },
                tree,
                blob,
            ];
            let source = Graph {
                objects: &objects,
                ids: None,
            };
            if binary_format == FORMAT && link_format == FORMAT {
                validate(&source).unwrap();
            } else {
                assert!(validate(&source).is_err());
            }
            let ids = [
                objects[0].id_with_format(FORMAT),
                tree.id(),
                blob.id_with_format(FORMAT),
            ];
            assert_eq!(
                validate(&Graph {
                    objects: &objects,
                    ids: Some(&ids)
                }),
                Err(Error::InvalidObject)
            );
        }
    }
    for link_format in [ObjectFormat::Sha1, FORMAT] {
        let body = format!(
            "object {}\ntype blob\ntag format\ntagger A <a@b> 0 +0000\n\ntag\n",
            blob.id_with_format(link_format)
        );
        let objects = [
            Object {
                kind: ObjectKind::Tag,
                data: body.as_bytes(),
            },
            blob,
        ];
        let source = Graph {
            objects: &objects,
            ids: None,
        };
        let refs = [Reference {
            name: "refs/tags/format",
            id: objects[0].id_with_format(FORMAT),
            symbolic_target: None,
            peeled: Some(blob.id_with_format(FORMAT)),
        }];
        let repository =
            RefRepository::new(&source, Head::Unborn("refs/heads/main"), &refs).unwrap();
        if link_format == FORMAT {
            validate(&repository).unwrap();
        } else {
            assert!(validate(&repository).is_err());
        }
        let mut mixed = refs;
        mixed[0].id = objects[0].id();
        assert!(RefRepository::new(&source, Head::Unborn("refs/heads/main"), &mixed).is_err());
        mixed = refs;
        mixed[0].peeled = Some(blob.id());
        assert!(RefRepository::new(&source, Head::Unborn("refs/heads/main"), &mixed).is_err());
    }
}

#[test]
fn format_negotiation_rejects_mismatch_and_malformed_requests_before_output() {
    let (mut trees, mut commit) = ([0; 4], [0; 512]);
    let snapshot =
        Snapshot::from_entries_with_format(&[], &mut trees, &mut commit, FORMAT).unwrap();
    for capability in ["", " object-format=sha256"] {
        let body = upload(snapshot.head(), capability, "");
        let mut storage = [0; 1024];
        let mut output = SliceWriter::new(&mut storage);
        http::serve(&snapshot, request(&body), &mut output).unwrap();
        pack::validate_with_format(&output.as_bytes()[8..], FORMAT).unwrap();
    }
    let mut invalid = vec![];
    for capability in [
        " object-format=sha1",
        " object-format=unknown",
        " object-format=sha256 object-format=sha256",
        " ",
        " object-format=sha256 ",
        " side-band",
    ] {
        invalid.push(upload(snapshot.head(), capability, ""));
    }
    invalid.push(upload(ObjectId::Sha1([1; 20]), " object-format=sha256", ""));
    invalid.push(upload(
        snapshot.head(),
        "",
        &format!("have {}\n", ObjectId::Sha1([1; 20])),
    ));
    invalid.push(upload(
        snapshot.head(),
        "",
        &format!("want {} object-format=sha256\n", snapshot.head()),
    ));
    for body in invalid {
        let mut output_storage = [0; 1024];
        let mut output = SliceWriter::new(&mut output_storage);
        let mut calls = 0;
        assert!(
            http::serve_with_pack(&snapshot, request(&body), &mut [], &mut output, |_, _| {
                calls += 1;
                Ok(())
            })
            .is_err()
        );
        assert!(output.as_bytes().is_empty());
        assert_eq!(calls, 0);
    }
}

#[test]
fn format_specific_name_limits_exactly_fill_smart_packets() {
    for format in [ObjectFormat::Sha1, FORMAT] {
        let target = format!("refs/heads/{}", "x".repeat(unborn_ref_limit(format) - 11));
        let empty = EmptyRepository::new_with_format(&target, format).unwrap();
        assert!(EmptyRepository::new_with_format(&format!("{target}x"), format).is_err());
        let mut bytes = vec![0; 3 * pktline::MAX_LENGTH + 128];
        let mut output = SliceWriter::new(&mut bytes);
        let discovery = Request {
            method: "GET",
            path: "info/refs",
            query: Some("service=git-upload-pack"),
            body: &[],
        };
        http::serve(&empty, discovery, &mut output).unwrap();
        let mut wire = output.as_bytes();
        pktline::read(&mut wire).unwrap();
        pktline::read(&mut wire).unwrap();
        let Some(Packet::Data(line)) = pktline::read(&mut wire).unwrap() else {
            panic!("missing empty capabilities")
        };
        assert_eq!(line.len() + 4, pktline::MAX_LENGTH);
        assert!(
            line.windows(format.capability().len())
                .any(|value| value == format.capability())
        );
        let head = format!("refs/heads/{}", "x".repeat(head_ref_limit(format) - 11));
        let alias = format!("refs/tags/{}", "x".repeat(ref_name_limit(format) - 10));
        let refs = [head.as_str(), alias.as_str()];
        let (mut trees, mut commit) = ([0; 4], [0; 512]);
        let snapshot = Snapshot::from_entries_with_format(&[], &mut trees, &mut commit, format)
            .unwrap()
            .with_refs(&head, &refs)
            .unwrap();
        output = SliceWriter::new(&mut bytes);
        http::serve(&snapshot, discovery, &mut output).unwrap();
        wire = output.as_bytes();
        pktline::read(&mut wire).unwrap();
        pktline::read(&mut wire).unwrap();
        let Some(Packet::Data(line)) = pktline::read(&mut wire).unwrap() else {
            panic!("missing HEAD")
        };
        assert_eq!(line.len() + 4, pktline::MAX_LENGTH);
        pktline::read(&mut wire).unwrap();
        let Some(Packet::Data(line)) = pktline::read(&mut wire).unwrap() else {
            panic!("missing alias")
        };
        assert_eq!(line.len() + 4, pktline::MAX_LENGTH);
        let oversized = format!("{alias}x");
        assert!(
            RefRepository::new(
                &snapshot,
                snapshot.head_state(),
                &[Reference::direct(&oversized, snapshot.head())]
            )
            .is_err()
        );
    }
}

#[test]
fn sha256_reference_and_offset_deltas_check_full_base_ids_and_pack_checksum() {
    let mut base_storage = [0; 64];
    let mut base_wire = SliceWriter::new(&mut base_storage);
    base_wire.write_all(&[0x35]).unwrap();
    zlib::write_parts(&mut base_wire, &[b"hello"]).unwrap();
    let base = Object {
        kind: ObjectKind::Blob,
        data: b"hello",
    };
    let mut reference_storage = [0; 96];
    let mut reference_wire = SliceWriter::new(&mut reference_storage);
    reference_wire.write_all(&[0x76]).unwrap();
    reference_wire
        .write_all(base.id_with_format(FORMAT).as_bytes())
        .unwrap();
    zlib::write_parts(&mut reference_wire, &[&[5, 6, 0x90, 5, 1, b'!']]).unwrap();
    let mut offset_storage = [0; 64];
    let mut offset_wire = SliceWriter::new(&mut offset_storage);
    offset_wire
        .write_all(&[0x66, reference_wire.as_bytes().len() as u8])
        .unwrap();
    zlib::write_parts(&mut offset_wire, &[&[6, 7, 0x90, 6, 1, b'?']]).unwrap();
    let mut packed = b"PACK\0\0\0\x02\0\0\0\x03".to_vec();
    for entry in [
        base_wire.as_bytes(),
        reference_wire.as_bytes(),
        offset_wire.as_bytes(),
    ] {
        packed.extend_from_slice(entry);
    }
    packed.extend_from_slice(&Sha256::digest(&packed));
    let (mut window, mut base_bytes, mut instructions, mut first_bytes, mut tip_bytes) =
        ([0; WINDOW_SIZE], [0; 16], [0; 16], [0; 16], [0; 16]);
    allocations::without_allocations(|| {
        let mut reader = Reader::new_with_format(&packed, FORMAT).unwrap();
        let plain = reader
            .read_entry(&mut base_bytes, &mut window, INFLATE)
            .unwrap()
            .unwrap()
            .object()
            .unwrap();
        let reference = reader
            .read_entry(&mut instructions, &mut window, INFLATE)
            .unwrap()
            .unwrap();
        let limits = DeltaLimits {
            max_output: 16,
            max_depth: 2,
            max_instructions: 16,
        };
        assert!(
            reference
                .resolve_delta(&ResolvedObject::external(base), &mut first_bytes, limits)
                .is_err()
        );
        assert_eq!(plain.id(), base.id_with_format(FORMAT));
        let first = reference
            .resolve_delta(
                &ResolvedObject::external_with_format(base, FORMAT),
                &mut first_bytes,
                limits,
            )
            .unwrap();
        let offset = reader
            .read_entry(&mut instructions, &mut window, INFLATE)
            .unwrap()
            .unwrap();
        let tip = offset
            .resolve_delta(&first, &mut tip_bytes, limits)
            .unwrap();
        assert_eq!(tip.object().data, b"hello!?");
        assert_eq!(tip.id(), tip.object().id_with_format(FORMAT));
        reader.finish().unwrap();
        assert!(Reader::new(&packed).is_err());
        assert!(Reader::new_with_format(&packed[..packed.len() - 1], FORMAT).is_err());
    });
    let end = packed.len() - 1;
    packed[end] ^= 1;
    assert!(Reader::new_with_format(&packed, FORMAT).is_err());
}
