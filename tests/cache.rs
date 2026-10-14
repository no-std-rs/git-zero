#![cfg(all(feature = "read", feature = "write"))]

#[path = "common/cache.rs"]
mod support;

use git_zero::{
    Error, File, Object, ObjectId, Repository, Snapshot,
    cache::{self, Key, NoCache, PackCache},
    io::{SliceWriter, Write},
    pack,
};
use std::cell::Cell;
use support::Cache;

struct Observed<'a, R> {
    source: &'a R,
    reads: Cell<usize>,
}

impl<R: Repository> Repository for Observed<'_, R> {
    fn head(&self) -> ObjectId {
        self.source.head()
    }

    fn object_count(&self) -> usize {
        self.reads.set(self.reads.get() + 1);
        self.source.object_count()
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.reads.set(self.reads.get() + 1);
        self.source.object(index)
    }
}

fn key(repository: &impl Repository) -> Key {
    Key {
        head: repository.head(),
        encoder_version: 1,
    }
}

fn generated(repository: &impl Repository) -> Vec<u8> {
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    pack::write(repository, &mut output).unwrap();
    output.as_bytes().to_vec()
}

#[test]
fn changed_heads_and_eviction_regenerate_the_exact_pack() {
    let mut first_tree = [0; 256];
    let mut first_commit = [0; 512];
    let first = Snapshot::new(&[], &mut first_tree, &mut first_commit).unwrap();
    let files = [File {
        name: "changed",
        contents: b"new snapshot\0\xff",
    }];
    let mut second_tree = [0; 256];
    let mut second_commit = [0; 512];
    let second = Snapshot::new(&files, &mut second_tree, &mut second_commit).unwrap();
    assert_ne!(first.head(), second.head());
    let mut cache = Cache::new();
    let mut scratch = [0; 4096];
    let mut bytes = [0; 4096];
    for (index, source) in [&first, &second, &first].into_iter().enumerate() {
        let repository = Observed {
            source,
            reads: Cell::new(0),
        };
        let expected = generated(source);
        let mut output = SliceWriter::new(&mut bytes);
        cache::write(&repository, &mut cache, &mut scratch, &mut output).unwrap();
        assert_eq!(output.as_bytes(), expected);
        pack::validate(output.as_bytes()).unwrap();
        assert!(repository.reads.get() > 0);
        assert_eq!(cache.key, Some(key(source)));
        assert_eq!(cache.puts, index + 1);

        repository.reads.set(0);
        let mut output = SliceWriter::new(&mut bytes);
        cache::write(&repository, &mut cache, &mut [], &mut output).unwrap();
        assert_eq!(output.as_bytes(), expected);
        assert_eq!(repository.reads.get(), 0);
        assert_eq!(cache.puts, index + 1);
    }
    cache.key = None; // Host eviction between requests must become a fresh miss.
    cache::write(
        &first,
        &mut cache,
        &mut scratch,
        &mut SliceWriter::new(&mut bytes),
    )
    .unwrap();
    assert_eq!(cache.puts, 4);
    assert_eq!(cache.gets.get(), 7);
}

#[test]
fn encoder_version_is_part_of_the_cache_identity() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut cache = Cache::new();
    let mut scratch = [0; 4096];
    let mut bytes = [0; 4096];
    for encoder_version in [0, 2] {
        cache
            .put(
                Key {
                    encoder_version,
                    ..key(&repository)
                },
                b"foreign encoding",
            )
            .unwrap();
        let mut output = SliceWriter::new(&mut bytes);
        cache::write(&repository, &mut cache, &mut scratch, &mut output).unwrap();
        assert_eq!(output.as_bytes(), generated(&repository));
        assert_eq!(cache.key, Some(key(&repository)));
    }
    assert_eq!(cache.puts, 4);
}

#[test]
fn backend_and_scratch_failures_preserve_the_previous_entry_and_output() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let old_key = Key {
        encoder_version: 0,
        ..key(&repository)
    };
    let mut cache = Cache::new();
    cache.put(old_key, b"previous pack").unwrap();
    let mut scratch = [0; 4096];
    let mut bytes = [0; 4096];
    cache.fail_get = true;
    let mut output = SliceWriter::new(&mut bytes);
    output.write_all(b"prefix").unwrap();
    assert_eq!(
        cache::write(&repository, &mut cache, &mut scratch, &mut output),
        Err(Error::Backend)
    );
    assert_eq!(output.as_bytes(), b"prefix");
    cache.fail_get = false;
    cache.fail_put = true;
    assert_eq!(
        cache::write(&repository, &mut cache, &mut scratch, &mut output),
        Err(Error::Backend)
    );
    assert_eq!(output.as_bytes(), b"prefix");
    cache.fail_put = false;
    assert_eq!(
        cache::write(&repository, &mut cache, &mut [], &mut output),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(output.as_bytes(), b"prefix");
    assert_eq!(
        cache.get(old_key).unwrap(),
        Some(b"previous pack".as_slice())
    );
    assert_eq!(cache.puts, 1);
    cache::write(&repository, &mut cache, &mut scratch, &mut output).unwrap();
    assert_eq!(&output.as_bytes()[6..], generated(&repository));
    assert_eq!(cache.puts, 2);
}

#[test]
fn insufficient_cache_capacity_keeps_the_previous_entry() {
    let large = [0xff; 4096];
    let files = [File {
        name: "large",
        contents: &large,
    }];
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let old_key = Key {
        encoder_version: 0,
        ..key(&repository)
    };
    let mut cache = Cache::new();
    cache.put(old_key, b"old pack").unwrap();
    let mut scratch = [0; 8192];
    let mut bytes = [0; 8192];
    let mut output = SliceWriter::new(&mut bytes);
    assert_eq!(
        cache::write(&repository, &mut cache, &mut scratch, &mut output),
        Err(Error::BufferTooSmall)
    );
    assert!(output.is_empty());
    assert_eq!(cache.get(old_key).unwrap(), Some(b"old pack".as_slice()));
    assert_eq!(cache.puts, 1);
}

#[test]
fn failed_output_leaves_a_complete_cache_entry_for_a_scratchless_retry() {
    struct FailedOutput;
    impl Write for FailedOutput {
        fn write_all(&mut self, _: &[u8]) -> git_zero::Result<()> {
            Err(Error::Backend)
        }
    }
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut cache = Cache::new();
    let mut scratch = [0; 4096];
    assert_eq!(
        cache::write(&repository, &mut cache, &mut scratch, &mut FailedOutput),
        Err(Error::Backend)
    );
    assert_eq!(cache.key, Some(key(&repository)));
    assert_eq!(cache.puts, 1);
    assert_eq!(
        cache::write(&repository, &mut cache, &mut [], &mut FailedOutput),
        Err(Error::Backend)
    );
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    cache::write(&repository, &mut cache, &mut [], &mut output).unwrap();
    assert_eq!(output.as_bytes(), generated(&repository));
    assert_eq!(cache.puts, 1);
}

#[test]
fn no_cache_regenerates_and_reports_scratch_exhaustion() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut bytes = [0; 4096];
    let mut scratch = [0; 4096];
    for _ in 0..2 {
        let mut output = SliceWriter::new(&mut bytes);
        cache::write(&repository, &mut NoCache, &mut scratch, &mut output).unwrap();
        assert_eq!(output.as_bytes(), generated(&repository));
    }
    assert_eq!(
        cache::write(
            &repository,
            &mut NoCache,
            &mut [],
            &mut SliceWriter::new(&mut bytes)
        ),
        Err(Error::BufferTooSmall)
    );
}
