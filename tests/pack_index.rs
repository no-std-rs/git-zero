#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{
    EmptyRepository, Error, File, ObjectFormat, ObjectId, Repository, Snapshot,
    io::{SliceWriter, Write},
    pack::{self, IndexEntry},
};
use sha2::Digest;
use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

fn git(directory: &Path) -> Command {
    let mut command =
        Command::new(std::env::var_os("GIT_ZERO_TEST_GIT").unwrap_or_else(|| "git".into()));
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("-C")
        .arg(directory);
    command
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new(name: &str, format: ObjectFormat) -> Self {
        let path = std::env::temp_dir().join(format!(
            "git-zero-index-{}-{name}-{}",
            std::process::id(),
            format.name()
        ));
        std::fs::create_dir(&path).unwrap();
        let result = git(&path)
            .args([
                "init",
                "--bare",
                "-q",
                &format!("--object-format={}", format.name()),
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn streamed_indexes_match_gits_independent_indexer_in_both_formats() {
    let body: Vec<u8> = (0..131071).map(|index| (index % 251) as u8).collect();
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = Directory::new("comparison", format);
        let files = [
            File {
                name: "a-empty",
                contents: b"",
            },
            File {
                name: "b-binary",
                contents: &body,
            },
        ];
        let mut tree = [0; 512];
        let mut commit = [0; 512];
        let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        for capacity in [1, 13, 65536] {
            let (pack, index, entries) = encode(&repository, None, capacity);
            let pack_path = directory.0.join("generated.pack");
            let index_path = directory.0.join("generated.idx");
            std::fs::write(&pack_path, &pack).unwrap();
            std::fs::write(&index_path, &index).unwrap();
            let result = git(&directory.0)
                .args(["verify-pack", "-v"])
                .arg(&index_path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let reference = directory.0.join("reference.idx");
            let result = git(&directory.0)
                .args(["index-pack", "--strict", "--index-version=2", "-o"])
                .arg(&reference)
                .arg(&pack_path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(std::fs::read(reference).unwrap(), index);
            check_entries(&pack, &entries);
            let mut expected = vec![0; pack.len()];
            let mut output = SliceWriter::new(&mut expected);
            pack::write_with_scratch(&repository, &mut vec![0; capacity], &mut output).unwrap();
            assert_eq!(output.as_bytes(), pack);
        }
        let empty = EmptyRepository::new_with_format("refs/heads/main", format).unwrap();
        let (pack, index, entries) = encode(&empty, None, 0);
        assert!(entries.is_empty());
        let pack_path = directory.0.join("empty.pack");
        let index_path = directory.0.join("empty.idx");
        std::fs::write(&pack_path, pack).unwrap();
        let result = git(&directory.0)
            .args(["index-pack", "--index-version=2", "-o"])
            .arg(&index_path)
            .arg(pack_path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(std::fs::read(index_path).unwrap(), index);
    }
}

fn encode(
    repository: &impl Repository,
    order: Option<&[usize]>,
    capacity: usize,
) -> (Vec<u8>, Vec<u8>, Vec<IndexEntry>) {
    let count = order.map_or(repository.object_count(), <[usize]>::len);
    let body_bytes: u64 = (0..repository.object_count())
        .map(|index| repository.object_info(index).unwrap().size)
        .sum();
    let mut pack_bytes = vec![0; body_bytes as usize + count * 64 + 64];
    let mut index_bytes = vec![0; 1100 + count * 80];
    let mut pack_output = SliceWriter::new(&mut pack_bytes);
    let mut index_output = SliceWriter::new(&mut index_bytes);
    let mut scratch = vec![0; capacity];
    let mut entries = vec![IndexEntry::EMPTY; count + 1];
    let checksum = allocations::without_allocations(|| {
        pack::write_indexed_with_scratch(
            repository,
            order,
            &mut entries,
            &mut scratch,
            &mut pack_output,
            &mut index_output,
        )
    })
    .unwrap();
    assert_eq!(entries[count], IndexEntry::EMPTY);
    assert_eq!(
        &pack_output.as_bytes()[pack_output.len() - checksum.as_bytes().len()..],
        checksum.as_bytes()
    );
    assert!(
        entries[..count]
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    );
    let pack_len = pack_output.len();
    let index_len = index_output.len();
    pack_bytes.truncate(pack_len);
    index_bytes.truncate(index_len);
    entries.truncate(count);
    (pack_bytes, index_bytes, entries)
}

fn check_entries(bytes: &[u8], entries: &[IndexEntry]) {
    for entry in entries {
        let end = entries
            .iter()
            .filter(|other| other.offset > entry.offset)
            .map(|other| other.offset)
            .min()
            .unwrap_or((bytes.len() - entry.id.format().id_length()) as u64);
        assert_eq!(
            entry.crc32,
            pack::crc32(&bytes[entry.offset as usize..end as usize])
        );
    }
}

#[test]
fn generated_short_read_bodies_and_selected_orders_use_bounded_storage() {
    let repository = streamed::GeneratedRepository::new(131071, 7);
    for order in [None, Some(&[2, 0][..]), Some(&[][..])] {
        let (bytes, _, entries) = encode(&repository, order, 17);
        let mut storage = vec![0; bytes.len()];
        let mut output = SliceWriter::new(&mut storage);
        match order {
            Some(order) => {
                pack::write_selected_with_scratch(&repository, order, &mut [0; 29], &mut output)
            }
            None => pack::write_with_scratch(&repository, &mut [0; 29], &mut output),
        }
        .unwrap();
        assert_eq!(output.as_bytes(), bytes);
        check_entries(&bytes, &entries);
    }
}

#[test]
fn index_large_offsets_roundtrip_through_git_and_use_the_repository_hash() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = Directory::new("large", format);
        let mut ids = [[0; 32]; 4];
        for (index, id) in ids.iter_mut().enumerate() {
            id[0] = index as u8;
        }
        let offsets = [12, 0x7fff_ffff, 0x8000_0000, 0x1_0000_0000];
        let entries: Vec<_> = ids
            .iter()
            .zip(offsets)
            .map(|(id, offset)| IndexEntry {
                id: ObjectId::from_bytes(format, &id[..format.id_length()]).unwrap(),
                offset,
                crc32: 0x12345678,
            })
            .collect();
        let mut bytes = [0; 2048];
        let mut output = SliceWriter::new(&mut bytes);
        let checksum = pack::write_index(&entries, format.zero_id(), &mut output).unwrap();
        let bytes = output.as_bytes();
        let hash_end = bytes.len() - format.id_length();
        let expected = match format {
            ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..hash_end]).to_vec(),
            ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..hash_end]).to_vec(),
        };
        assert_eq!(checksum.as_bytes(), expected);
        let mut child = git(&directory.0)
            .args(["show-index", &format!("--object-format={}", format.name())])
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
        let decoded: Vec<u64> = String::from_utf8(result.stdout)
            .unwrap()
            .lines()
            .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
            .collect();
        assert_eq!(decoded, offsets);
    }
}

#[test]
fn invalid_metadata_and_exhausted_storage_fail_without_successful_publication() {
    let files = [File {
        name: "file",
        contents: b"data",
    }];
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let mut pack_bytes = [0xa5; 4096];
    let mut index_bytes = [0xa5; 4096];
    for order in [Some(&[0, 0][..]), Some(&[99][..])] {
        let mut output = SliceWriter::new(&mut pack_bytes);
        let mut index = SliceWriter::new(&mut index_bytes);
        assert!(
            pack::write_indexed_with_scratch(
                &repository,
                order,
                &mut [IndexEntry::EMPTY; 3],
                &mut [0; 5],
                &mut output,
                &mut index
            )
            .is_err()
        );
        assert!(output.is_empty() && index.is_empty());
    }
    for (slots, scratch) in [(2, 1), (3, 0)] {
        let mut output = SliceWriter::new(&mut pack_bytes);
        let mut index = SliceWriter::new(&mut index_bytes);
        assert_eq!(
            pack::write_indexed_with_scratch(
                &repository,
                None,
                &mut [IndexEntry::EMPTY; 3][..slots],
                &mut [0; 1][..scratch],
                &mut output,
                &mut index
            ),
            Err(Error::BufferTooSmall)
        );
        assert!(output.is_empty() && index.is_empty());
    }
    let entry = IndexEntry {
        id: repository.head(),
        offset: 12,
        crc32: 0,
    };
    for entries in [
        &[entry, entry][..],
        &[IndexEntry {
            offset: 11,
            ..entry
        }][..],
        &[IndexEntry {
            id: ObjectId::Sha256([0; 32]),
            ..entry
        }][..],
    ] {
        let mut index = SliceWriter::new(&mut index_bytes);
        assert_eq!(
            pack::write_index(entries, repository.head(), &mut index),
            Err(Error::InvalidObject)
        );
        assert!(index.is_empty());
    }
    struct Failed;
    impl Write for Failed {
        fn write_all(&mut self, _: &[u8]) -> git_zero::Result<()> {
            Err(Error::Backend)
        }
    }
    let mut output = SliceWriter::new(&mut pack_bytes);
    assert_eq!(
        pack::write_indexed_with_scratch(
            &repository,
            None,
            &mut [IndexEntry::EMPTY; 3],
            &mut [0; 7],
            &mut output,
            &mut Failed
        ),
        Err(Error::Backend)
    );
    assert!(!output.is_empty());
    let mut index = SliceWriter::new(&mut index_bytes);
    assert_eq!(
        pack::write_indexed_with_scratch(
            &repository,
            None,
            &mut [IndexEntry::EMPTY; 3],
            &mut [0; 7],
            &mut Failed,
            &mut index
        ),
        Err(Error::Backend)
    );
    assert!(index.is_empty());
}
