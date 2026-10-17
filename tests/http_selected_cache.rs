#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/selected_http.rs"]
mod protocol;
use protocol::{body, packet, request};

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

// Both snapshots contain commit/tree/file/shared. The second shared blob is
// the same canonical object, so the physical union has seven stable slots.
const OBJECTS: [(usize, usize); 7] = [(0, 0), (0, 1), (0, 2), (0, 3), (1, 0), (1, 1), (1, 2)];

struct Fixture<'a> {
    format: ObjectFormat,
    snapshots: [Snapshot<'a>; 2],
    ids: [ObjectId; 7],
    tips: [ObjectId; 2],
    blob_reads: Cell<usize>,
    fail_blobs: Cell<bool>,
}

impl<'a> Fixture<'a> {
    fn new(snapshots: [Snapshot<'a>; 2]) -> Self {
        assert_eq!(snapshots[0].object_id(3), snapshots[1].object_id(3));
        let ids = OBJECTS.map(|(source, slot)| snapshots[source].object_id(slot).unwrap());
        Self {
            format: snapshots[0].object_format(),
            tips: snapshots.each_ref().map(Repository::head),
            snapshots,
            ids,
            blob_reads: Cell::new(0),
            fail_blobs: Cell::new(false),
        }
    }

    fn blob(&self) -> ObjectId {
        self.ids[2]
    }

    fn source(&self, index: usize) -> Result<(&Snapshot<'_>, usize)> {
        let &(source, slot) = OBJECTS.get(index).ok_or(Error::ObjectNotFound)?;
        Ok((&self.snapshots[source], slot))
    }
}

impl Repository for Fixture<'_> {
    fn head(&self) -> ObjectId {
        self.tips[0]
    }
    fn object_format(&self) -> ObjectFormat {
        self.format
    }
    fn object_count(&self) -> usize {
        self.ids.len()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.ids.get(index).copied()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        let (source, slot) = self.source(index)?;
        source.object_info(slot)
    }
    fn read_object(&self, index: usize, offset: u64, output: &mut [u8]) -> Result<usize> {
        let (source, slot) = self.source(index)?;
        if source.object_info(slot)?.kind == ObjectKind::Blob {
            self.blob_reads.set(self.blob_reads.get() + 1);
            if self.fail_blobs.get() {
                return Err(Error::Backend);
            }
        }
        let offset = usize::try_from(offset).map_err(|_| Error::LimitExceeded)? as u64;
        let count = output.len().min(7);
        source.read_object(slot, offset, &mut output[..count])
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

type TestRepository<'a> = ValidatedRepository<'a, 'a, Fixture<'a>>;

struct Run {
    result: Result<Response>,
    bytes: Vec<u8>,
    outcome: Option<Outcome>,
    calls: usize,
}

impl Run {
    fn pack(self, expected: Outcome) -> Vec<u8> {
        self.result.unwrap();
        assert_eq!(self.outcome, Some(expected));
        self.bytes
    }

    fn denied(self, expected: Error) {
        assert_eq!(self.result.unwrap_err(), expected);
        assert_eq!(self.calls, 0);
        assert!(self.bytes.is_empty());
    }
}

struct Client<'a, C> {
    repository: &'a TestRepository<'a>,
    cache: C,
    graph: u64,
    pack_capacity: usize,
    options: WantOptions,
}

impl<'a, C: PackCache> Client<'a, C> {
    fn new(repository: &'a TestRepository<'a>, cache: C) -> Self {
        Self {
            repository,
            cache,
            graph: 65536,
            pack_capacity: CAPACITY,
            options: WantOptions::STRICT,
        }
    }

    fn write(
        &mut self,
        request: Request<'_>,
        output: &mut impl Write,
    ) -> (Result<Response>, Option<Outcome>, usize) {
        let mut selection = SelectionBuffers::<16, 16, 17, 128>::new();
        let mut ids = [self.repository.object_format().zero_id(); 16];
        let mut pack = [0; CAPACITY];
        let (mut outcome, mut calls) = (None, 0);
        let (repository, graph, options) = (self.repository, self.graph, self.options);
        let result = allocations::without_allocations(|| {
            http::serve_with_selected_pack_policy(
                repository,
                request,
                &mut selection.storage(graph),
                options,
                output,
                |indices, read, framed| {
                    calls += 1;
                    outcome = Some(self.write_pack(indices, &mut ids, &mut pack, read, framed)?);
                    Ok(())
                },
            )
        });
        (result, outcome, calls)
    }

    fn write_pack(
        &mut self,
        indices: &[usize],
        ids: &mut [ObjectId],
        pack: &mut [u8],
        read: &mut [u8],
        output: &mut impl Write,
    ) -> Result<Outcome> {
        selected::write(
            self.repository,
            Selection::Indices(indices),
            &mut self.cache,
            Buffers {
                ids,
                pack: &mut pack[..self.pack_capacity],
                read,
                byte_limit: CAPACITY,
            },
            output,
        )
    }

    fn run(&mut self, request: Request<'_>) -> Run {
        let mut output = [0; CAPACITY + 4096];
        let mut output = SliceWriter::new(&mut output);
        let (result, outcome, calls) = self.write(request, &mut output);
        Run {
            result,
            bytes: output.as_bytes().into(),
            outcome,
            calls,
        }
    }
}

fn with_repository(format: ObjectFormat, test: impl FnOnce(&Fixture<'_>, &TestRepository<'_>)) {
    let files = [b"first\0\xff".as_slice(), b"second"].map(|contents| {
        [
            File {
                name: "file",
                contents,
            },
            File {
                name: "shared",
                contents: b"shared\n",
            },
        ]
    });
    let (mut trees, mut commits) = ([[0; 256]; 2], [[0; 512]; 2]);
    let [first_tree, second_tree] = &mut trees;
    let [first_commit, second_commit] = &mut commits;
    let fixture = Fixture::new([
        Snapshot::new_with_format(&files[0], first_tree, first_commit, format).unwrap(),
        Snapshot::new_with_format(&files[1], second_tree, second_commit, format).unwrap(),
    ]);
    let (mut metadata, mut by_id) = ([None; 16], [0; 16]);
    let repository = ValidatedRepository::new(
        &fixture,
        &mut metadata,
        &mut by_id,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    test(&fixture, &repository);
}

fn formats(scenario: impl Fn(&Fixture<'_>, &TestRepository<'_>)) {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        with_repository(format, &scenario);
    }
}

fn uncached(repository: &TestRepository<'_>, request: Request<'_>) -> Vec<u8> {
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
    formats(check_reframing);
}

fn check_reframing(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let format = fixture.format;
    let mut resident = [0; CAPACITY];
    let mut client = Client::new(repository, FixedCache::new(&mut resident));
    for (version, band, outcome) in [
        (0, "", Outcome::Miss),
        (0, " side-band", Outcome::Hit),
        (0, " side-band-64k", Outcome::Hit),
        (1, "", Outcome::Hit),
        (1, " side-band", Outcome::Hit),
        (1, " side-band-64k", Outcome::Hit),
        (2, "", Outcome::Hit),
        (2, " side-band", Outcome::Hit),
        (2, " side-band-64k", Outcome::Hit),
    ] {
        let body = body(format, version, &[fixture.tips[1]], &[], &[], true, band);
        let expected = uncached(repository, request(&body, version));
        client.pack_capacity = CAPACITY;
        assert_eq!(client.run(request(&body, version)).pack(outcome), expected);
        fixture.fail_blobs.set(true);
        client.pack_capacity = 0;
        assert_eq!(
            client.run(request(&body, version)).pack(Outcome::Hit),
            expected
        );
        fixture.fail_blobs.set(false);
    }
    assert_eq!(client.cache.statistics().insertions, 1);
    assert_eq!(client.cache.statistics().hits, 17);
}

#[test]
fn haves_empty_selections_and_eviction_change_only_the_raw_pack_identity() {
    with_repository(ObjectFormat::Sha1, check_selected_sets);
}

fn check_selected_sets(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let mut resident = [0; CAPACITY];
    let mut client = Client::new(repository, FixedCache::new(&mut resident));
    let cases = [
        (&fixture.tips[..], &[][..], Outcome::Miss, false),
        (&fixture.tips[..], &fixture.tips[..1], Outcome::Miss, false),
        (&fixture.tips[1..], &fixture.tips[..1], Outcome::Hit, false),
        (&fixture.tips[1..], &fixture.tips[1..], Outcome::Miss, true),
    ];
    for (wants, haves, outcome, empty_pack) in cases {
        let body = body(fixture.format, 0, wants, haves, &[], true, "");
        let expected = uncached(repository, request(&body, 0));
        let result = client.run(request(&body, 0));
        assert_eq!(result.calls, 1);
        let bytes = result.pack(outcome);
        assert_eq!(bytes, expected);
        if empty_pack {
            let raw = &bytes[bytes.windows(4).position(|bytes| bytes == b"PACK").unwrap()..];
            pack::validate(raw).unwrap();
            assert_eq!(&raw[8..12], &[0; 4]);
        }
    }
    client.cache.evict();
    let body = body(fixture.format, 0, &fixture.tips[..1], &[], &[], true, "");
    client.run(request(&body, 0)).pack(Outcome::Miss);
    client.cache.set_enabled(false);
    client.run(request(&body, 0)).pack(Outcome::Miss);
    client.cache.set_enabled(true);
    client.pack_capacity = 0;
    client.run(request(&body, 0)).pack(Outcome::Hit);
}

#[test]
fn routing_denials_ack_only_and_probes_never_reach_cache() {
    with_repository(ObjectFormat::Sha1, check_routes);
}

fn check_routes(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let mut resident = [0; CAPACITY];
    let mut client = Client::new(repository, FixedCache::new(&mut resident));
    let done = body(fixture.format, 0, &fixture.tips[..1], &[], &[], true, "");
    client.run(request(&done, 0)).pack(Outcome::Miss);
    let before = client.cache.statistics();
    client.graph = 0;
    client.pack_capacity = 0;
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
        let result = client.run(request(&ack, version));
        result.result.unwrap();
        assert_eq!(result.calls, 0);
    }
    client.graph = 65536;
    let probe = body(
        fixture.format,
        0,
        &fixture.tips[..1],
        &[],
        &["deepen 1"],
        false,
        "",
    );
    assert_eq!(client.run(request(&probe, 0)).calls, 0);
    let invalid = body(fixture.format, 0, &[fixture.blob()], &[], &[], true, "");
    client
        .run(request(&invalid, 0))
        .denied(Error::InvalidRequest);
    client.graph = 0;
    client.pack_capacity = CAPACITY;
    client.run(request(&done, 0)).denied(Error::LimitExceeded);
    client.pack_capacity = 0;
    for path in ["HEAD", "info/refs", "objects/info/packs"] {
        let mut get = request(&[], 0);
        get.method = "GET";
        get.path = path;
        assert_eq!(client.run(get).calls, 0);
    }
    let mut command = Vec::new();
    packet(&mut command, b"command=ls-refs\n");
    command.extend_from_slice(b"00010000");
    assert_eq!(client.run(request(&command, 2)).calls, 0);
    assert_eq!(client.cache.statistics(), before);
}

struct FaultCache {
    key: Option<Key>,
    bytes: [u8; CAPACITY],
    length: usize,
    get_error: bool,
    put_error: bool,
}
impl FaultCache {
    fn new() -> Self {
        Self {
            key: None,
            bytes: [0; CAPACITY],
            length: 0,
            get_error: false,
            put_error: false,
        }
    }
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
    with_repository(ObjectFormat::Sha1, check_cache_errors);
}

fn check_cache_errors(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let body = body(
        fixture.format,
        0,
        &fixture.tips[..1],
        &[],
        &[],
        true,
        " side-band-64k",
    );
    let mut client = Client::new(repository, FaultCache::new());
    for (get, put, capacity, expected) in [
        (true, false, CAPACITY, Error::Backend),
        (false, true, CAPACITY, Error::Backend),
        (false, false, 0, Error::BufferTooSmall),
    ] {
        client.cache.get_error = get;
        client.cache.put_error = put;
        client.pack_capacity = capacity;
        let failed = client.run(request(&body, 0));
        assert_eq!(failed.result.unwrap_err(), expected);
        assert_eq!(failed.bytes, b"0008NAK\n");
        assert!(client.cache.key.is_none());
    }
    client.pack_capacity = CAPACITY;
    let mut short_output = [0; 8];
    let result = client.write(request(&body, 0), &mut SliceWriter::new(&mut short_output));
    assert_eq!(result.0.unwrap_err(), Error::BufferTooSmall);
    assert!(client.cache.key.is_some());
    client.pack_capacity = 0;
    client.run(request(&body, 0)).pack(Outcome::Hit);
    client.cache.bytes[20] ^= 1;
    let corrupt = client.run(request(&body, 0));
    assert!(corrupt.result.is_err());
    assert_eq!(corrupt.bytes, b"0008NAK\n");
}

#[test]
fn failed_replacement_preserves_old_http_entry_and_a_wrong_set_hit_fails_closed() {
    with_repository(ObjectFormat::Sha256, check_replacement);
}

fn check_replacement(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let mut client = Client::new(repository, FaultCache::new());
    let first = body(fixture.format, 2, &fixture.tips[..1], &[], &[], true, "");
    let second = body(fixture.format, 2, &fixture.tips[1..], &[], &[], true, "");
    client.run(request(&first, 2)).pack(Outcome::Miss);
    let old_key = client.cache.key;
    let old_bytes = client.cache.bytes[..client.cache.length].to_vec();
    client.cache.put_error = true;
    assert_eq!(
        client.run(request(&second, 2)).result.unwrap_err(),
        Error::Backend
    );
    assert_eq!(client.cache.key, old_key);
    assert_eq!(&client.cache.bytes[..client.cache.length], old_bytes);
    client.pack_capacity = 0;
    client.run(request(&first, 2)).pack(Outcome::Hit);
    let mut selected = SelectionBuffers::<16, 16, 17, 128>::new();
    let mut new_key = None;
    http::serve_with_selected_pack(
        repository,
        request(&second, 2),
        &mut selected.storage(65536),
        &mut SliceWriter::new(&mut [0; CAPACITY]),
        |indices, read, framed| {
            new_key = Some(Key::derive(
                repository,
                Selection::Indices(indices),
                &mut [fixture.format.zero_id(); 16],
            )?);
            pack::write_selected_with_scratch(repository, indices, read, framed).map(|_| ())
        },
    )
    .unwrap();
    assert_ne!(new_key, old_key);
    client.cache.key = new_key; // An intact old pack falsely published under a new set.
    assert!(client.run(request(&second, 2)).result.is_err());
}

#[test]
fn explicit_blob_grant_is_rechecked_before_each_warmed_cache_access() {
    formats(check_blob_grant);
}

fn check_blob_grant(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    for version in 0..=2 {
        check_blob_grant_version(fixture, repository, version);
    }
}

fn check_blob_grant_version(fixture: &Fixture<'_>, repository: &TestRepository<'_>, version: u8) {
    let format = fixture.format;
    let blob = fixture.blob();
    let mut resident = [0; CAPACITY];
    let mut client = Client::new(repository, FixedCache::new(&mut resident));
    client.graph = 0;
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
    client.options = grant;
    let miss = client.run(request(&wanted, version)).pack(Outcome::Miss);
    fixture.fail_blobs.set(true);
    client.pack_capacity = 0;
    assert_eq!(
        client.run(request(&wanted, version)).pack(Outcome::Hit),
        miss
    );
    let before = client.cache.statistics();
    for (options, wire, expected) in blob_grant_denials(format, version, blob, &wanted) {
        client.options = options;
        client.run(request(&wire, version)).denied(expected);
        assert_eq!(client.cache.statistics(), before);
    }
    // Revocation and failures never replace the later host's policy.
    client.options = grant;
    client.run(request(&wanted, version)).pack(Outcome::Hit);
    fixture.fail_blobs.set(false);
}

fn blob_grant_denials(
    format: ObjectFormat,
    version: u8,
    blob: ObjectId,
    wanted: &[u8],
) -> [(WantOptions, Vec<u8>, Error); 4] {
    let grant = WantOptions::all_visible_reachable(1);
    [
        (WantOptions::STRICT, wanted.to_vec(), Error::InvalidRequest),
        (
            WantOptions::all_visible_reachable(0),
            wanted.to_vec(),
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
    ]
}
