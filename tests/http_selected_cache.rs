#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, File, ObjectFormat, ObjectId, ObjectInfo, ObjectKind, Repository, Result, Snapshot,
    ValidatedRepository,
    cache::selected::{self, Buffers, FixedCache, Key, Outcome, PackCache, Selection},
    http::{self, Request, Response, SelectionBuffers, WantOptions},
    io::{SliceWriter, Write},
    pack,
};
use std::cell::Cell;

const CAPACITY: usize = 32_768;

struct Owned {
    kind: ObjectKind,
    bytes: Vec<u8>,
    id: ObjectId,
}

struct Fixture {
    format: ObjectFormat,
    objects: Vec<Owned>,
    tips: [ObjectId; 2],
    blob_reads: Cell<usize>,
    fail_blobs: Cell<bool>,
}

impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let mut fixture = Self {
            format,
            objects: Vec::new(),
            tips: [format.zero_id(); 2],
            blob_reads: Cell::new(0),
            fail_blobs: Cell::new(false),
        };
        for (index, contents) in [b"first\0\xff".as_slice(), b"second"]
            .into_iter()
            .enumerate()
        {
            let files = [
                File {
                    name: "file",
                    contents,
                },
                File {
                    name: "shared",
                    contents: b"shared\n",
                },
            ];
            let (mut tree, mut commit) = ([0; 256], [0; 512]);
            let snapshot =
                Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
            fixture.tips[index] = snapshot.head();
            for slot in 0..snapshot.object_count() {
                let object = snapshot.object(slot).unwrap();
                let id = snapshot.object_id(slot).unwrap();
                if !fixture.objects.iter().any(|object| object.id == id) {
                    fixture.objects.push(Owned {
                        kind: object.kind,
                        bytes: object.data.into(),
                        id,
                    });
                }
            }
        }
        fixture
    }
    fn validate<'a>(
        &'a self,
        metadata: &'a mut [Option<git_zero::ValidationMetadata>],
        ids: &'a mut [usize],
    ) -> ValidatedRepository<'a, 'a, Self> {
        ValidatedRepository::new(self, metadata, ids, &mut [0; 17], &mut [0; 128]).unwrap()
    }
}

impl Repository for Fixture {
    fn head(&self) -> ObjectId {
        self.tips[0]
    }
    fn object_format(&self) -> ObjectFormat {
        self.format
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.objects.get(index).map(|object| object.id)
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        let object = self.objects.get(index).ok_or(Error::ObjectNotFound)?;
        Ok(ObjectInfo {
            kind: object.kind,
            size: object.bytes.len() as u64,
        })
    }
    fn read_object(&self, index: usize, offset: u64, output: &mut [u8]) -> Result<usize> {
        let object = self.objects.get(index).ok_or(Error::ObjectNotFound)?;
        if object.kind == ObjectKind::Blob {
            self.blob_reads.set(self.blob_reads.get() + 1);
            if self.fail_blobs.get() {
                return Err(Error::Backend);
            }
        }
        let offset = usize::try_from(offset)
            .map_err(|_| Error::LimitExceeded)?
            .min(object.bytes.len());
        let count = output.len().min(7).min(object.bytes.len() - offset);
        output[..count].copy_from_slice(&object.bytes[offset..offset + count]);
        Ok(count)
    }
    fn reference_count(&self) -> usize {
        2
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        ["refs/heads/main", "refs/heads/side"].get(index).copied()
    }
    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        self.tips.get(index).copied()
    }
}

fn packet(bytes: &mut Vec<u8>, line: &[u8]) {
    bytes.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    bytes.extend_from_slice(line);
}

fn body(
    format: ObjectFormat,
    version: u8,
    wants: &[ObjectId],
    haves: &[ObjectId],
    extra: &[&str],
    done: bool,
    band: &str,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    if version == 2 {
        packet(&mut bytes, b"command=fetch\n");
        packet(
            &mut bytes,
            format!("{}\n", std::str::from_utf8(format.capability()).unwrap()).as_bytes(),
        );
        bytes.extend_from_slice(b"0001");
    }
    for (index, id) in wants.iter().enumerate() {
        let caps = if version != 2 && index == 0 {
            format!(
                " {}{band}",
                std::str::from_utf8(format.capability()).unwrap()
            )
        } else {
            String::new()
        };
        packet(&mut bytes, format!("want {id}{caps}\n").as_bytes());
    }
    for line in extra {
        packet(&mut bytes, format!("{line}\n").as_bytes());
    }
    if version != 2 {
        bytes.extend_from_slice(b"0000");
    }
    for id in haves {
        packet(&mut bytes, format!("have {id}\n").as_bytes());
    }
    if done {
        packet(&mut bytes, b"done\n");
    }
    if version == 2 || !done && !haves.is_empty() {
        bytes.extend_from_slice(b"0000");
    }
    bytes
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

struct Run {
    result: Result<Response>,
    bytes: Vec<u8>,
    outcome: Option<Outcome>,
    calls: usize,
}

fn cached(
    repository: &ValidatedRepository<'_, '_, Fixture>,
    request: Request<'_>,
    cache: &mut impl PackCache,
    graph: u64,
    pack_capacity: usize,
    output: &mut impl Write,
    options: WantOptions,
) -> (Result<Response>, Option<Outcome>, usize) {
    let mut selection = SelectionBuffers::<16, 16, 17, 128>::new();
    let mut ids = [repository.object_format().zero_id(); 16];
    let mut pack = [0; CAPACITY];
    let (mut outcome, mut calls) = (None, 0);
    let result = allocations::without_allocations(|| {
        http::serve_with_selected_pack_policy(
            repository,
            request,
            &mut selection.storage(graph),
            options,
            output,
            |indices, read, framed| {
                calls += 1;
                outcome = Some(selected::write(
                    repository,
                    Selection::Indices(indices),
                    cache,
                    Buffers {
                        ids: &mut ids,
                        pack: &mut pack[..pack_capacity],
                        read,
                        byte_limit: CAPACITY,
                    },
                    framed,
                )?);
                Ok(())
            },
        )
    });
    (result, outcome, calls)
}

fn run(
    repository: &ValidatedRepository<'_, '_, Fixture>,
    request: Request<'_>,
    cache: &mut impl PackCache,
    graph: u64,
    pack_capacity: usize,
) -> Run {
    run_policy(
        repository,
        request,
        cache,
        graph,
        pack_capacity,
        WantOptions::STRICT,
    )
}

fn run_policy(
    repository: &ValidatedRepository<'_, '_, Fixture>,
    request: Request<'_>,
    cache: &mut impl PackCache,
    graph: u64,
    pack_capacity: usize,
    options: WantOptions,
) -> Run {
    let mut output = [0; CAPACITY + 4096];
    let mut output = SliceWriter::new(&mut output);
    let (result, outcome, calls) = cached(
        repository,
        request,
        cache,
        graph,
        pack_capacity,
        &mut output,
        options,
    );
    Run {
        result,
        bytes: output.as_bytes().into(),
        outcome,
        calls,
    }
}

fn uncached(repository: &ValidatedRepository<'_, '_, Fixture>, request: Request<'_>) -> Vec<u8> {
    let mut output = [0; CAPACITY + 4096];
    let mut output = SliceWriter::new(&mut output);
    http::serve_with_selection(
        repository,
        request,
        &mut SelectionBuffers::<16, 16, 17, 128>::new().storage(65536),
        &mut output,
    )
    .unwrap();
    output.as_bytes().into()
}

#[test]
fn actual_cache_hits_reframe_current_requests_without_blob_reads() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
        let repository = fixture.validate(&mut metadata, &mut by_id);
        let mut resident = [0; CAPACITY];
        let mut cache = FixedCache::new(&mut resident);
        for version in 0..=2 {
            for band in ["", " side-band", " side-band-64k"] {
                let body = body(format, version, &[fixture.tips[1]], &[], &[], true, band);
                let expected = uncached(&repository, request(&body, version));
                let result = run(
                    &repository,
                    request(&body, version),
                    &mut cache,
                    65536,
                    CAPACITY,
                );
                result.result.unwrap();
                assert_eq!(
                    result.outcome,
                    Some(if version == 0 && band.is_empty() {
                        Outcome::Miss
                    } else {
                        Outcome::Hit
                    })
                );
                assert_eq!(result.bytes, expected);
                fixture.fail_blobs.set(true);
                let hit = run(&repository, request(&body, version), &mut cache, 65536, 0);
                assert_eq!(hit.outcome, Some(Outcome::Hit));
                assert_eq!(hit.bytes, expected);
                hit.result.unwrap();
                fixture.fail_blobs.set(false);
            }
        }
        assert_eq!(cache.statistics().insertions, 1);
        assert_eq!(cache.statistics().hits, 17);
    }
}

#[test]
fn haves_empty_selections_and_eviction_change_only_the_raw_pack_identity() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
    let repository = fixture.validate(&mut metadata, &mut by_id);
    let mut resident = [0; CAPACITY];
    let mut cache = FixedCache::new(&mut resident);
    let cases = [
        (&fixture.tips[..], &[][..]),
        (&fixture.tips[..], &fixture.tips[..1]),
        (&fixture.tips[1..], &fixture.tips[..1]),
        (&fixture.tips[1..], &fixture.tips[1..]),
    ];
    for (index, (wants, haves)) in cases.into_iter().enumerate() {
        let body = body(fixture.format, 0, wants, haves, &[], true, "");
        let expected = uncached(&repository, request(&body, 0));
        let result = run(&repository, request(&body, 0), &mut cache, 65536, CAPACITY);
        result.result.unwrap();
        assert_eq!(result.calls, 1);
        assert_eq!(
            result.outcome,
            Some(if index == 2 {
                Outcome::Hit
            } else {
                Outcome::Miss
            })
        );
        assert_eq!(result.bytes, expected);
        if index == 3 {
            let raw = &result.bytes[result
                .bytes
                .windows(4)
                .position(|bytes| bytes == b"PACK")
                .unwrap()..];
            pack::validate(raw).unwrap();
            assert_eq!(&raw[8..12], &[0; 4]);
        }
    }
    cache.evict();
    let body = body(fixture.format, 0, &fixture.tips[..1], &[], &[], true, "");
    assert_eq!(
        run(&repository, request(&body, 0), &mut cache, 65536, CAPACITY).outcome,
        Some(Outcome::Miss)
    );
    cache.set_enabled(false);
    assert_eq!(
        run(&repository, request(&body, 0), &mut cache, 65536, CAPACITY).outcome,
        Some(Outcome::Miss)
    );
    cache.set_enabled(true);
    assert_eq!(
        run(&repository, request(&body, 0), &mut cache, 65536, 0).outcome,
        Some(Outcome::Hit)
    );
}

#[test]
fn routing_denials_ack_only_and_probes_never_reach_cache() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
    let repository = fixture.validate(&mut metadata, &mut by_id);
    let mut resident = [0; CAPACITY];
    let mut cache = FixedCache::new(&mut resident);
    let done = body(fixture.format, 0, &fixture.tips[..1], &[], &[], true, "");
    assert_eq!(
        run(&repository, request(&done, 0), &mut cache, 65536, CAPACITY).outcome,
        Some(Outcome::Miss)
    );
    let before = cache.statistics();
    for version in 0..=2 {
        let ack = body(
            fixture.format,
            version,
            &fixture.tips[..1],
            &[],
            &[],
            false,
            "",
        );
        let result = run(&repository, request(&ack, version), &mut cache, 0, 0);
        result.result.unwrap();
        assert_eq!(result.calls, 0);
    }
    let probe = body(
        fixture.format,
        0,
        &fixture.tips[..1],
        &[],
        &["deepen 1"],
        false,
        "",
    );
    assert_eq!(
        run(&repository, request(&probe, 0), &mut cache, 65536, 0).calls,
        0
    );
    let invalid = body(
        fixture.format,
        0,
        &[fixture
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Blob)
            .unwrap()
            .id],
        &[],
        &[],
        true,
        "",
    );
    let denied = run(&repository, request(&invalid, 0), &mut cache, 65536, 0);
    assert_eq!(denied.result.unwrap_err(), Error::InvalidRequest);
    assert!(denied.bytes.is_empty());
    assert_eq!(denied.calls, 0);
    let limited = run(&repository, request(&done, 0), &mut cache, 0, CAPACITY);
    assert_eq!(limited.result.unwrap_err(), Error::LimitExceeded);
    assert!(limited.bytes.is_empty());
    assert_eq!(limited.calls, 0);
    for path in ["HEAD", "info/refs", "objects/info/packs"] {
        let mut get = request(&[], 0);
        get.method = "GET";
        get.path = path;
        assert_eq!(run(&repository, get, &mut cache, 0, 0).calls, 0);
    }
    let mut command = Vec::new();
    packet(&mut command, b"command=ls-refs\n");
    command.extend_from_slice(b"00010000");
    assert_eq!(
        run(&repository, request(&command, 2), &mut cache, 0, 0).calls,
        0
    );
    assert_eq!(cache.statistics(), before);
}

struct FaultCache {
    key: Option<Key>,
    bytes: [u8; CAPACITY],
    length: usize,
    get_error: bool,
    put_error: bool,
}
impl PackCache for FaultCache {
    fn get(&mut self, key: Key) -> Result<Option<&[u8]>> {
        if self.get_error {
            return Err(Error::Backend);
        }
        Ok((self.key == Some(key)).then_some(&self.bytes[..self.length]))
    }
    fn put(&mut self, key: Key, bytes: &[u8]) -> Result<()> {
        if self.put_error {
            return Err(Error::Backend);
        }
        self.bytes[..bytes.len()].copy_from_slice(bytes);
        self.length = bytes.len();
        self.key = Some(key);
        Ok(())
    }
}

#[test]
fn cache_failures_invalidate_provisional_controls_and_output_failure_keeps_a_valid_entry() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
    let repository = fixture.validate(&mut metadata, &mut by_id);
    let body = body(
        fixture.format,
        0,
        &fixture.tips[..1],
        &[],
        &[],
        true,
        " side-band-64k",
    );
    let mut cache = FaultCache {
        key: None,
        bytes: [0; CAPACITY],
        length: 0,
        get_error: true,
        put_error: false,
    };
    for (get, put, capacity, expected) in [
        (true, false, CAPACITY, Error::Backend),
        (false, true, CAPACITY, Error::Backend),
        (false, false, 0, Error::BufferTooSmall),
    ] {
        cache.get_error = get;
        cache.put_error = put;
        let failed = run(&repository, request(&body, 0), &mut cache, 65536, capacity);
        assert_eq!(failed.result.unwrap_err(), expected);
        assert_eq!(failed.bytes, b"0008NAK\n");
        assert!(cache.key.is_none());
    }
    let mut short_output = [0; 8];
    let result = cached(
        &repository,
        request(&body, 0),
        &mut cache,
        65536,
        CAPACITY,
        &mut SliceWriter::new(&mut short_output),
        WantOptions::STRICT,
    );
    assert_eq!(result.0.unwrap_err(), Error::BufferTooSmall);
    assert!(cache.key.is_some());
    assert_eq!(
        run(&repository, request(&body, 0), &mut cache, 65536, 0).outcome,
        Some(Outcome::Hit)
    );
    cache.bytes[20] ^= 1;
    let corrupt = run(&repository, request(&body, 0), &mut cache, 65536, 0);
    assert!(corrupt.result.is_err());
    assert_eq!(corrupt.bytes, b"0008NAK\n");
}

#[test]
fn failed_replacement_preserves_old_http_entry_and_a_wrong_set_hit_fails_closed() {
    let fixture = Fixture::new(ObjectFormat::Sha256);
    let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
    let repository = fixture.validate(&mut metadata, &mut by_id);
    let mut cache = FaultCache {
        key: None,
        bytes: [0; CAPACITY],
        length: 0,
        get_error: false,
        put_error: false,
    };
    let first = body(fixture.format, 2, &fixture.tips[..1], &[], &[], true, "");
    let second = body(fixture.format, 2, &fixture.tips[1..], &[], &[], true, "");
    assert_eq!(
        run(&repository, request(&first, 2), &mut cache, 65536, CAPACITY).outcome,
        Some(Outcome::Miss)
    );
    let old_key = cache.key;
    let old_bytes = cache.bytes[..cache.length].to_vec();
    cache.put_error = true;
    assert_eq!(
        run(
            &repository,
            request(&second, 2),
            &mut cache,
            65536,
            CAPACITY
        )
        .result
        .unwrap_err(),
        Error::Backend
    );
    assert_eq!(cache.key, old_key);
    assert_eq!(&cache.bytes[..cache.length], old_bytes);
    assert_eq!(
        run(&repository, request(&first, 2), &mut cache, 65536, 0).outcome,
        Some(Outcome::Hit)
    );
    let mut selected = SelectionBuffers::<16, 16, 17, 128>::new();
    let mut new_key = None;
    http::serve_with_selected_pack(
        &repository,
        request(&second, 2),
        &mut selected.storage(65536),
        &mut SliceWriter::new(&mut [0; CAPACITY]),
        |indices, read, framed| {
            new_key = Some(Key::derive(
                &repository,
                Selection::Indices(indices),
                &mut [fixture.format.zero_id(); 16],
            )?);
            pack::write_selected_with_scratch(&repository, indices, read, framed).map(|_| ())
        },
    )
    .unwrap();
    assert_ne!(new_key, old_key);
    cache.key = new_key; // An intact old pack falsely published under a new set.
    assert!(
        run(&repository, request(&second, 2), &mut cache, 65536, 0)
            .result
            .is_err()
    );
}

#[test]
fn explicit_blob_grant_is_rechecked_before_each_warmed_cache_access() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
        let repository = fixture.validate(&mut metadata, &mut by_id);
        let blob = fixture
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Blob)
            .unwrap()
            .id;
        for version in 0..=2 {
            let mut resident = [0; CAPACITY];
            let mut cache = FixedCache::new(&mut resident);
            let wanted = body(
                format,
                version,
                &[blob],
                &[],
                &["filter blob:none"],
                true,
                " filter",
            );
            let grant = WantOptions::all_visible_reachable(1);
            let miss = run_policy(
                &repository,
                request(&wanted, version),
                &mut cache,
                0,
                CAPACITY,
                grant,
            );
            miss.result.unwrap();
            assert_eq!(miss.outcome, Some(Outcome::Miss));
            fixture.fail_blobs.set(true);
            let hit = run_policy(
                &repository,
                request(&wanted, version),
                &mut cache,
                0,
                0,
                grant,
            );
            hit.result.unwrap();
            assert_eq!(hit.outcome, Some(Outcome::Hit));
            assert_eq!(hit.bytes, miss.bytes);
            let before = cache.statistics();
            for (options, wire, expected) in [
                (WantOptions::STRICT, wanted.clone(), Error::InvalidRequest),
                (
                    WantOptions::all_visible_reachable(0),
                    wanted.clone(),
                    Error::LimitExceeded,
                ),
                (
                    grant,
                    body(format, version, &[blob, blob], &[], &[], true, ""),
                    Error::LimitExceeded,
                ),
                (
                    grant,
                    body(
                        format,
                        version,
                        &[ObjectId::from_bytes(format, &vec![9; format.id_length()]).unwrap()],
                        &[],
                        &[],
                        true,
                        "",
                    ),
                    Error::InvalidRequest,
                ),
            ] {
                let denied = run_policy(
                    &repository,
                    request(&wire, version),
                    &mut cache,
                    0,
                    0,
                    options,
                );
                assert_eq!(denied.result.unwrap_err(), expected);
                assert_eq!(denied.calls, 0);
                assert!(denied.bytes.is_empty());
                assert_eq!(cache.statistics(), before);
            }
            // Revocation and failures never replace the later host's policy.
            assert_eq!(
                run_policy(
                    &repository,
                    request(&wanted, version),
                    &mut cache,
                    0,
                    0,
                    grant
                )
                .outcome,
                Some(Outcome::Hit)
            );
            fixture.fail_blobs.set(false);
        }
    }
}
