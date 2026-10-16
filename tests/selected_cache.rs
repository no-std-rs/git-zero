#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{
    Error, Object, ObjectFormat, ObjectId, ObjectKind, Repository, Result,
    cache::{
        NoCache,
        selected::{self, Buffers, FixedCache, Key, Outcome, PackCache, Selection},
    },
    io::SliceWriter,
    pack,
};
use sha2::Digest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

struct Objects<'a> {
    objects: &'a [Object<'a>],
    format: ObjectFormat,
    claimed: Option<&'a [ObjectId]>,
}
impl Repository for Objects<'_> {
    fn head(&self) -> ObjectId {
        self.format.zero_id()
    }
    fn object_format(&self) -> ObjectFormat {
        self.format
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).copied()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        match self.claimed {
            Some(ids) => ids.get(index).copied(),
            None => self
                .object(index)
                .map(|object| object.id_with_format(self.format)),
        }
    }
}
fn blob(data: &[u8]) -> Object<'_> {
    Object {
        kind: ObjectKind::Blob,
        data,
    }
}
fn key(repository: &impl Repository, selected: Option<&[usize]>) -> Key {
    Key::derive(
        repository,
        selection(selected),
        &mut vec![repository.object_format().zero_id(); repository.object_count()],
    )
    .unwrap()
}
fn selection(indices: Option<&[usize]>) -> Selection<'_> {
    match indices {
        Some(indices) => Selection::Indices(indices),
        None => Selection::All,
    }
}
fn encode(repository: &impl Repository) -> Vec<u8> {
    let size: usize = (0..repository.object_count())
        .map(|index| repository.object_info(index).unwrap().size as usize + 32)
        .sum();
    let mut bytes = vec![0; size + 64];
    let mut output = SliceWriter::new(&mut bytes);
    pack::write(repository, &mut output).unwrap();
    output.finish().to_vec()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn serialization_matches_independent_python_hashlib_vectors() {
    let objects = [blob(b""), blob(b"alpha"), blob(b"binary\0\xff")];
    // Python hashlib: SHA256(domain + prefix + sorted(canonical object IDs)).
    for (format, empty, three) in [
        (
            ObjectFormat::Sha1,
            "475a50430101000000000001000000007e4228a929a38ef80b587135ca4747219ed7741ab3c92b1ba0f290523823638c",
            "475a5043010100000000000100000003515d8f8a8813c2f66ea46347d1610b5e6377168a5c06c72dc342ba2f5db376ad",
        ),
        (
            ObjectFormat::Sha256,
            "475a50430102000000000001000000002dfee9e6c4518503de4312c4325afafe9b729d590a554964226b0f87d7dbaf96",
            "475a5043010200000000000100000003b99e5beb75517f0b8ade62cfb69fcd255c5aff69f563e976fc2a29f02e9e60c7",
        ),
    ] {
        let repository = Objects {
            objects: &objects,
            format,
            claimed: None,
        };
        for (indices, expected) in [(Some(&[][..]), empty), (None, three)] {
            let identity = key(&repository, indices);
            assert_eq!(hex(identity.as_bytes()), expected);
            assert_eq!(Key::from_bytes(identity.as_bytes()), Ok(identity));
            assert_eq!(identity.object_format(), format);
        }
        let identity = key(&repository, None);
        for position in 0..12 {
            let mut invalid = *identity.as_bytes();
            invalid[position] ^= 128;
            assert!(Key::from_bytes(&invalid).is_err(), "position {position}");
        }
        for length in [0, 47, 49] {
            assert!(Key::from_bytes(&vec![0; length]).is_err());
        }
    }
}

#[test]
fn identity_follows_the_set_across_provider_orders_and_independent_roots() {
    let objects = [blob(b"first"), blob(b"second"), blob(b"third")];
    let reversed = [objects[2], objects[1], objects[0]];
    let repository = Objects {
        objects: &objects,
        format: ObjectFormat::Sha1,
        claimed: None,
    };
    let reordered = Objects {
        objects: &reversed,
        ..repository
    };
    assert_eq!(key(&repository, None), key(&reordered, None));
    assert_eq!(
        key(&repository, Some(&[0, 2])),
        key(&reordered, Some(&[0, 2]))
    );
    assert_ne!(
        key(&repository, Some(&[0])),
        key(&repository, Some(&[0, 1]))
    );
    let changed = [objects[0], blob(b"changed independent tip"), objects[2]];
    let changed = Objects {
        objects: &changed,
        ..repository
    };
    assert_eq!(repository.head(), changed.head());
    assert_ne!(key(&repository, None), key(&changed, None));
    assert_eq!(
        key(&repository, Some(&[0, 2])),
        key(&changed, Some(&[0, 2]))
    );
}

#[test]
fn miss_hit_eviction_and_disabled_cache_use_bounded_allocation_free_storage() {
    let body = vec![0xff; 131071];
    let objects = [blob(b""), blob(&body), blob(b"tail")];
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let repository = Objects {
            objects: &objects,
            format,
            claimed: None,
        };
        let expected = encode(&repository);
        let mut storage = vec![0; expected.len()];
        let mut cache = FixedCache::new(&mut storage);
        assert_cached_request(&repository, &mut cache, Outcome::Miss, &expected);
        assert_cached_request(&repository, &mut cache, Outcome::Hit, &expected);
        cache.evict();
        assert_cached_request(&repository, &mut cache, Outcome::Miss, &expected);
        cache.set_enabled(false);
        assert_cached_request(&repository, &mut cache, Outcome::Miss, &expected);
        cache.set_enabled(true);
        assert_cached_request(&repository, &mut cache, Outcome::Hit, &expected);
        assert_eq!(
            cache.statistics(),
            selected::Statistics {
                hits: 2,
                misses: 3,
                insertions: 2,
                evictions: 1
            }
        );
        import_with_git(&expected, format, &objects);
    }
}

fn assert_cached_request(
    repository: &impl Repository,
    cache: &mut impl PackCache,
    outcome: Outcome,
    expected: &[u8],
) {
    let mut ids = vec![repository.object_format().zero_id(); repository.object_count()];
    let staging_length = match outcome {
        Outcome::Hit => 0,
        Outcome::Miss => expected.len(),
    };
    let mut staging = vec![0; staging_length];
    let mut output_bytes = vec![0; expected.len()];
    let mut output = SliceWriter::new(&mut output_bytes);
    let actual = allocations::without_allocations(|| {
        selected::write(
            repository,
            Selection::All,
            cache,
            Buffers {
                ids: &mut ids,
                pack: &mut staging,
                read: &mut [0; 7],
                byte_limit: expected.len(),
            },
            &mut output,
        )
    })
    .unwrap();
    assert_eq!(actual, outcome);
    assert_eq!(output.as_bytes(), expected);
}

fn import_with_git(bytes: &[u8], format: ObjectFormat, objects: &[Object<'_>]) {
    let path = std::env::temp_dir().join(format!(
        "git-zero-selected-cache-{}-{}",
        std::process::id(),
        format.name()
    ));
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    std::fs::create_dir(&path).unwrap();
    let _directory = Directory(path.clone());
    let git = std::env::var_os("GIT_ZERO_TEST_GIT").unwrap_or_else(|| "git".into());
    let command = || {
        let mut command = Command::new(&git);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(&path);
        command
    };
    assert!(
        command()
            .args([
                "init",
                "--bare",
                "-q",
                &format!("--object-format={}", format.name())
            ])
            .status()
            .unwrap()
            .success()
    );
    let mut child = command()
        .args(["index-pack", "--strict", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = command()
        .args([
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    let actual = String::from_utf8(result.stdout).unwrap();
    let mut actual: Vec<_> = actual.lines().map(str::to_owned).collect();
    let mut expected: Vec<_> = objects
        .iter()
        .map(|object| object.id_with_format(format).to_string())
        .collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
    for object in objects {
        let result = command()
            .args([
                "cat-file",
                object.kind.name(),
                &object.id_with_format(format).to_string(),
            ])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, object.data);
    }
}

struct Backend {
    bytes: Option<Vec<u8>>,
    reads: usize,
    writes: usize,
    fail_get: bool,
    fail_put: bool,
}
impl Backend {
    fn new() -> Self {
        Self {
            bytes: None,
            reads: 0,
            writes: 0,
            fail_get: false,
            fail_put: false,
        }
    }
}
impl PackCache for Backend {
    fn get(&mut self, _: Key) -> Result<Option<&[u8]>> {
        self.reads += 1;
        if self.fail_get {
            return Err(Error::Backend);
        }
        Ok(self.bytes.as_deref())
    }
    fn put(&mut self, _: Key, bytes: &[u8]) -> Result<()> {
        if self.fail_put {
            return Err(Error::Backend);
        }
        self.writes += 1;
        self.bytes = Some(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn invalid_selection_and_id_scratch_fail_before_backend_or_output() {
    let objects = [blob(b"first"), blob(b"second")];
    let repository = Objects {
        objects: &objects,
        format: ObjectFormat::Sha1,
        claimed: None,
    };
    let mut backend = Backend::new();
    for (indices, slots, error) in [
        (Some(&[1, 0][..]), 2, Error::InvalidObject),
        (Some(&[0, 0][..]), 2, Error::InvalidObject),
        (Some(&[2][..]), 2, Error::ObjectNotFound),
        (None, 1, Error::BufferTooSmall),
    ] {
        let mut output_bytes = [0; 1024];
        let mut output = SliceWriter::new(&mut output_bytes);
        let mut ids = [ObjectFormat::Sha1.zero_id(); 2];
        assert_eq!(
            selected::write(
                &repository,
                selection(indices),
                &mut backend,
                Buffers {
                    ids: &mut ids[..slots],
                    pack: &mut [0; 1024],
                    read: &mut [],
                    byte_limit: 1024
                },
                &mut output
            ),
            Err(error)
        );
        assert!(output.is_empty());
    }
    assert_eq!((backend.reads, backend.writes), (0, 0));
    let duplicates = [objects[0], objects[0]];
    assert_eq!(
        Key::derive(
            &Objects {
                objects: &duplicates,
                ..repository
            },
            Selection::All,
            &mut [repository.format.zero_id(); 2]
        ),
        Err(Error::InvalidObject)
    );
    let wrong_format = [ObjectFormat::Sha256.zero_id(); 2];
    assert_eq!(
        Key::derive(
            &Objects {
                claimed: Some(&wrong_format),
                ..repository
            },
            Selection::All,
            &mut [repository.format.zero_id(); 2]
        ),
        Err(Error::InvalidObjectId)
    );
}

fn trailer(bytes: &mut [u8], format: ObjectFormat) {
    let end = bytes.len() - format.id_length();
    let hash: Vec<u8> = match format {
        ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..end]).to_vec(),
        ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..end]).to_vec(),
    };
    bytes[end..].copy_from_slice(&hash);
}

#[test]
fn corrupted_pack_framing_and_checksums_never_reach_the_sink_or_insertion() {
    let objects = [blob(b"abc")];
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let repository = Objects {
            objects: &objects,
            format,
            claimed: None,
        };
        let identity = key(&repository, None);
        let good = encode(&repository);
        let mut cases: Vec<Vec<u8>> = (0..good.len()).map(|end| good[..end].to_vec()).collect();
        for (position, mask) in [
            (0, 128),
            (7, 128),
            (11, 128),
            (12, 128),
            (13, 128),
            (14, 128),
            (15, 2),
            (16, 128),
            (18, 128),
            (20, 128),
        ] {
            let mut bad = good.clone();
            bad[position] ^= mask;
            cases.push(bad.clone());
            trailer(&mut bad, format);
            cases.push(bad);
        }
        let mut bad = good.clone();
        *bad.last_mut().unwrap() ^= 128;
        cases.push(bad);
        for bad in cases {
            assert_invalid_cached_pack(&repository, identity, bad, good.len());
        }
    }
}

fn assert_invalid_cached_pack(
    repository: &impl Repository,
    key: Key,
    bad: Vec<u8>,
    byte_limit: usize,
) {
    let mut ids = vec![repository.object_format().zero_id(); repository.object_count()];
    assert!(key.verify_pack(&bad, &mut ids, byte_limit).is_err());
    let mut backend = Backend::new();
    backend.bytes = Some(bad);
    let mut bytes = [0; 256];
    let mut output = SliceWriter::new(&mut bytes);
    assert!(
        selected::write(
            repository,
            Selection::All,
            &mut backend,
            Buffers {
                ids: &mut ids,
                pack: &mut [],
                read: &mut [],
                byte_limit
            },
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(backend.writes, 0);
}

#[test]
fn valid_checksums_do_not_hide_wrong_bodies_sets_or_duplicate_objects() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let objects = [blob(b"abc"), blob(b"xyz")];
        let repository = Objects {
            objects: &objects,
            format,
            claimed: None,
        };
        let identity = key(&repository, None);
        let wrong_objects = [blob(b"abc"), blob(b"other")];
        let duplicate_objects = [blob(b"abc"), blob(b"abc")];
        for wrong in [&wrong_objects, &duplicate_objects] {
            // Encoder recomputes both Adler32 and the pack trailer, so this
            // reaches canonical body identity / exact-set validation.
            let bytes = encode(&Objects {
                objects: wrong,
                ..repository
            });
            assert!(
                identity
                    .verify_pack(&bytes, &mut [format.zero_id(); 2], bytes.len())
                    .is_err()
            );
        }
        let claims = objects.map(|object| object.id_with_format(format));
        let liar = Objects {
            objects: &wrong_objects,
            claimed: Some(&claims),
            ..repository
        };
        let mut backend = Backend::new();
        let mut bytes = [0; 256];
        let mut output = SliceWriter::new(&mut bytes);
        assert_eq!(
            selected::write(
                &liar,
                Selection::All,
                &mut backend,
                Buffers {
                    ids: &mut [format.zero_id(); 2],
                    pack: &mut [0; 256],
                    read: &mut [],
                    byte_limit: 256
                },
                &mut output
            ),
            Err(Error::CorruptPack)
        );
        assert!(output.is_empty());
        assert_eq!(backend.writes, 0);
    }
}

#[test]
fn limits_backend_and_output_failures_preserve_safe_retry_behavior() {
    let objects = [blob(b"abc")];
    let repository = Objects {
        objects: &objects,
        format: ObjectFormat::Sha1,
        claimed: None,
    };
    let identity = key(&repository, None);
    let good = encode(&repository);
    assert_eq!(
        identity.verify_pack(&good, &mut [], good.len()),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(
        identity.verify_pack(&good, &mut [repository.format.zero_id()], good.len() - 1),
        Err(Error::LimitExceeded)
    );
    let mut cache_bytes = vec![0; good.len()];
    let mut cache = FixedCache::new(&mut cache_bytes);
    cache.put(identity, &good).unwrap();
    assert_eq!(
        cache.put(identity, &vec![0; good.len() + 1]),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(cache.get(identity).unwrap(), Some(good.as_slice()));
    let mut backend = Backend::new();
    for fail_get in [true, false] {
        backend.fail_get = fail_get;
        backend.fail_put = !fail_get;
        let mut bytes = [0; 256];
        let mut output = SliceWriter::new(&mut bytes);
        assert_eq!(
            selected::write(
                &repository,
                Selection::All,
                &mut backend,
                Buffers {
                    ids: &mut [repository.format.zero_id()],
                    pack: &mut [0; 256],
                    read: &mut [],
                    byte_limit: 256
                },
                &mut output
            ),
            Err(Error::Backend)
        );
        assert!(output.is_empty());
        assert_eq!(backend.writes, 0);
    }
    let mut small_output = SliceWriter::new(&mut []);
    cache.evict();
    assert_eq!(
        selected::write(
            &repository,
            Selection::All,
            &mut cache,
            Buffers {
                ids: &mut [repository.format.zero_id()],
                pack: &mut [0; 256],
                read: &mut [],
                byte_limit: 256
            },
            &mut small_output
        ),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(cache.get(identity).unwrap(), Some(good.as_slice()));
    for capacity in [0, good.len() - 1] {
        let mut bytes = [0; 256];
        let mut output = SliceWriter::new(&mut bytes);
        assert_eq!(
            selected::write(
                &repository,
                Selection::All,
                &mut NoCache,
                Buffers {
                    ids: &mut [repository.format.zero_id()],
                    pack: &mut [0; 256][..capacity],
                    read: &mut [],
                    byte_limit: 256
                },
                &mut output
            ),
            Err(Error::BufferTooSmall)
        );
        assert!(output.is_empty());
    }
}

#[test]
fn empty_selected_pack_uses_no_id_or_body_scratch() {
    let objects = [blob(b"excluded")];
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let repository = Objects {
            objects: &objects,
            format,
            claimed: None,
        };
        let mut bytes = [0; 64];
        let mut output = SliceWriter::new(&mut bytes);
        allocations::without_allocations(|| {
            selected::write(
                &repository,
                Selection::Indices(&[]),
                &mut NoCache,
                Buffers {
                    ids: &mut [],
                    pack: &mut [0; 64],
                    read: &mut [],
                    byte_limit: 64,
                },
                &mut output,
            )
        })
        .unwrap();
        let identity = key(&repository, Some(&[]));
        assert_eq!(identity.object_count(), 0);
        identity
            .verify_pack(output.as_bytes(), &mut [], 64)
            .unwrap();
        assert_eq!(output.len(), 12 + format.id_length());
    }
}

#[test]
fn streaming_miss_accepts_one_byte_reads_and_hit_skips_bodies() {
    struct Streaming<'a> {
        source: &'a streamed::GeneratedRepository,
        reads: std::cell::Cell<usize>,
        fail_reads: std::cell::Cell<bool>,
    }
    impl Repository for Streaming<'_> {
        fn head(&self) -> ObjectId {
            self.source.head()
        }
        fn object_count(&self) -> usize {
            self.source.object_count()
        }
        fn object_id(&self, index: usize) -> Option<ObjectId> {
            self.source.object_id(index)
        }
        fn object_info(&self, index: usize) -> Result<git_zero::ObjectInfo> {
            self.source.object_info(index)
        }
        fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
            self.reads.set(self.reads.get() + 1);
            if self.fail_reads.get() {
                return Err(Error::Backend);
            }
            self.source.read_object(index, offset, storage)
        }
    }
    let source = streamed::GeneratedRepository::new(65536, 1);
    let repository = Streaming {
        source: &source,
        reads: std::cell::Cell::new(0),
        fail_reads: std::cell::Cell::new(false),
    };
    let expected = encode(&source);
    let mut storage = vec![0; expected.len()];
    let mut staging = vec![0; expected.len()];
    let mut output_bytes = vec![0; expected.len()];
    let mut cache = FixedCache::new(&mut storage);
    for outcome in [Outcome::Miss, Outcome::Hit] {
        let before = repository.reads.get();
        let mut output = SliceWriter::new(&mut output_bytes);
        let result = allocations::without_allocations(|| {
            selected::write(
                &repository,
                Selection::All,
                &mut cache,
                Buffers {
                    ids: &mut [ObjectFormat::Sha1.zero_id(); 3],
                    pack: &mut staging,
                    read: &mut [0],
                    byte_limit: expected.len(),
                },
                &mut output,
            )
        })
        .unwrap();
        assert_eq!(result, outcome);
        assert_eq!(output.as_bytes(), expected);
        if outcome == Outcome::Hit {
            assert_eq!(repository.reads.get(), before);
        } else {
            assert!(repository.reads.get() > 65536);
        }
        repository.fail_reads.set(true);
    }
    let mut output = SliceWriter::new(&mut output_bytes);
    assert_eq!(
        selected::write(
            &repository,
            Selection::All,
            &mut NoCache,
            Buffers {
                ids: &mut [ObjectFormat::Sha1.zero_id(); 3],
                pack: &mut staging,
                read: &mut [],
                byte_limit: expected.len()
            },
            &mut output
        ),
        Err(Error::BufferTooSmall)
    );
    assert!(output.is_empty());
}

#[test]
fn selected_hit_handles_an_equivalent_reordered_provider_and_preserves_old_entry_on_failed_miss() {
    let objects = [blob(b"first"), blob(b"second"), blob(b"third")];
    let reordered = [objects[2], objects[1], objects[0]];
    let repository = Objects {
        objects: &objects,
        format: ObjectFormat::Sha256,
        claimed: None,
    };
    let alternate = Objects {
        objects: &reordered,
        ..repository
    };
    let mut storage = [0; 512];
    let mut cache = FixedCache::new(&mut storage);
    let mut staging = [0; 512];
    let mut bytes = [0; 512];
    let mut output = SliceWriter::new(&mut bytes);
    selected::write(
        &repository,
        Selection::Indices(&[0, 2]),
        &mut cache,
        Buffers {
            ids: &mut [repository.format.zero_id(); 2],
            pack: &mut staging,
            read: &mut [],
            byte_limit: 512,
        },
        &mut output,
    )
    .unwrap();
    let expected = output.as_bytes().to_vec();
    let old_key = key(&repository, Some(&[0, 2]));
    let mut output = SliceWriter::new(&mut bytes);
    assert_eq!(
        selected::write(
            &alternate,
            Selection::Indices(&[0, 2]),
            &mut cache,
            Buffers {
                ids: &mut [repository.format.zero_id(); 2],
                pack: &mut [],
                read: &mut [],
                byte_limit: 512
            },
            &mut output
        ),
        Ok(Outcome::Hit)
    );
    assert_eq!(output.as_bytes(), expected);
    let mut output = SliceWriter::new(&mut bytes);
    assert_eq!(
        selected::write(
            &repository,
            Selection::All,
            &mut cache,
            Buffers {
                ids: &mut [repository.format.zero_id(); 3],
                pack: &mut [],
                read: &mut [],
                byte_limit: 512
            },
            &mut output
        ),
        Err(Error::BufferTooSmall)
    );
    assert!(output.is_empty());
    assert_eq!(cache.get(old_key).unwrap(), Some(expected.as_slice()));
}
