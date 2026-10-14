#![cfg(all(feature = "read", feature = "write"))]

#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, Object, ObjectId, ObjectKind,
    io::Write,
    pack::{self, DeltaLimits, Entry, EntryKind, Reader, ResolvedObject},
    zlib::{self, InflateLimits, WINDOW_SIZE},
};
use sha1::{Digest, Sha1};
use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

const INFLATE: InflateLimits = InflateLimits {
    max_output: 100_000,
    max_ratio: 2048,
    ratio_slack: 32,
};
const DELTA: DeltaLimits = DeltaLimits {
    max_output: 100_000,
    max_instructions: 100_000,
    max_depth: 16,
};

struct Bytes(Vec<u8>);
impl Write for Bytes {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}
fn wire(header: &[u8], body: &[u8]) -> Vec<u8> {
    let mut output = Bytes(header.to_vec());
    zlib::write_parts(&mut output, &[body]).unwrap();
    output.0
}
fn packed(entries: &[&[u8]], count: u32) -> Vec<u8> {
    let mut bytes = b"PACK\0\0\0\x02".to_vec();
    bytes.extend_from_slice(&count.to_be_bytes());
    for entry in entries {
        bytes.extend_from_slice(entry);
    }
    bytes.extend_from_slice(&Sha1::digest(&bytes));
    bytes
}

#[test]
fn crc_matches_bitwise_reference_at_every_slice_and_tail_boundary() {
    let bytes: Vec<_> = (0..1025)
        .map(|index| (index * 71 + index / 7) as u8)
        .collect();
    for start in 0..8 {
        let mut reference = u32::MAX;
        assert_eq!(pack::crc32(&bytes[start..start]), !reference);
        for (index, &byte) in bytes[start..].iter().enumerate() {
            reference = crc_bitwise_byte(reference, byte);
            assert_eq!(pack::crc32(&bytes[start..start + index + 1]), !reference);
        }
    }
}

// Independent one-byte IEEE CRC update, with no production lookup tables.
fn crc_bitwise_byte(mut crc: u32, byte: u8) -> u32 {
    crc ^= u32::from(byte);
    for _ in 0..8 {
        crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
    }
    crc
}

#[test]
fn entry_crcs_ids_and_two_delta_levels_use_only_caller_storage() {
    assert_eq!(pack::crc32(b""), 0);
    assert_eq!(pack::crc32(b"123456789"), 0xcbf4_3926);
    let plain = wire(&[0x35], b"hello");
    let first_delta = wire(&[0x66, plain.len() as u8], &[5, 6, 0x90, 5, 1, b'!']);
    let second_delta = wire(&[0x66, first_delta.len() as u8], &[6, 7, 0x90, 6, 1, b'?']);
    let bytes = packed(&[&plain, &first_delta, &second_delta], 3);
    let mut window = [0; WINDOW_SIZE];
    let (mut plain_storage, mut first_instructions, mut second_instructions) =
        ([0; 32], [0; 32], [0; 32]);
    let (mut first_body, mut second_body) = ([0; 32], [0; 32]);
    allocations::without_allocations(|| {
        let mut reader = Reader::new(&bytes).unwrap();
        assert_eq!(reader.object_count(), 3);
        assert_eq!(reader.finish(), Err(Error::CorruptPack));
        let entry = reader
            .read_entry(&mut plain_storage, &mut window, INFLATE)
            .unwrap()
            .unwrap();
        assert_eq!(entry.offset(), 12);
        assert_eq!(entry.kind(), EntryKind::Object(ObjectKind::Blob));
        assert_eq!(entry.crc32(), pack::crc32(&plain));
        entry.verify_crc32(entry.crc32()).unwrap();
        assert_eq!(
            entry.verify_crc32(entry.crc32() ^ 1),
            Err(Error::CorruptPack)
        );
        let base = entry.object().unwrap();
        base.verify_id(
            Object {
                kind: ObjectKind::Blob,
                data: b"hello",
            }
            .id(),
        )
        .unwrap();
        let first = reader
            .read_entry(&mut first_instructions, &mut window, INFLATE)
            .unwrap()
            .unwrap();
        assert!(first.object().is_none());
        let resolved = first.resolve_delta(&base, &mut first_body, DELTA).unwrap();
        assert_eq!(resolved.object().data, b"hello!");
        assert_eq!(resolved.delta_depth(), 1);
        let second = reader
            .read_entry(&mut second_instructions, &mut window, INFLATE)
            .unwrap()
            .unwrap();
        assert!(matches!(
            second.resolve_delta(
                &resolved,
                &mut second_body,
                DeltaLimits {
                    max_depth: 1,
                    ..DELTA
                }
            ),
            Err(Error::LimitExceeded)
        ));
        let tip = second
            .resolve_delta(&resolved, &mut second_body, DELTA)
            .unwrap();
        assert_eq!(tip.object().data, b"hello!?");
        assert_eq!(tip.delta_depth(), 2);
        assert_eq!(tip.id(), tip.object().id());
        reader.finish().unwrap();
        assert!(
            reader
                .read_entry(&mut [], &mut window, INFLATE)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn offset_encoding_reference_bases_and_pack_identity_are_checked() {
    let base_body = [b'a'; 256];
    let plain = wire(&[0xb0, 0x10], &base_body);
    assert_eq!(plain.len(), 274);
    let delta = wire(&[0x65, 0x81, 0x12], &[0x80, 2, 1, 1, b'X']);
    let bytes = packed(&[&plain, &delta], 2);
    let mut window = [0; WINDOW_SIZE];
    let (mut body, mut instructions, mut result) = ([0; 256], [0; 32], [0; 32]);
    let mut reader = Reader::new(&bytes).unwrap();
    let base = reader
        .read_entry(&mut body, &mut window, INFLATE)
        .unwrap()
        .unwrap()
        .object()
        .unwrap();
    let entry = reader
        .read_entry(&mut instructions, &mut window, INFLATE)
        .unwrap()
        .unwrap();
    assert_eq!(entry.kind(), EntryKind::OffsetDelta { base_offset: 12 });
    assert_eq!(
        entry
            .resolve_delta(&base, &mut result, DELTA)
            .unwrap()
            .object()
            .data,
        b"X"
    );
    let mut other_body = [0; 256];
    let other_wire = wire(&[0xb0, 0x10], &[b'b'; 256]);
    let other_pack = packed(&[&other_wire], 1);
    let other = Reader::new(&other_pack)
        .unwrap()
        .read_entry(&mut other_body, &mut window, INFLATE)
        .unwrap()
        .unwrap()
        .object()
        .unwrap();
    assert!(matches!(
        entry.resolve_delta(&other, &mut result, DELTA),
        Err(Error::ObjectNotFound)
    ));
    let external = ResolvedObject::external(base.object());
    assert!(matches!(
        entry.resolve_delta(&external, &mut result, DELTA),
        Err(Error::ObjectNotFound)
    ));
    let mut header = vec![0x75];
    header.extend_from_slice(&base.id().0);
    let reference = wire(&header, &[0x80, 2, 1, 1, b'X']);
    let ref_pack = packed(&[&reference], 1);
    let mut ref_reader = Reader::new(&ref_pack).unwrap();
    let reference = ref_reader
        .read_entry(&mut instructions, &mut window, INFLATE)
        .unwrap()
        .unwrap();
    assert_eq!(
        reference
            .resolve_delta(&external, &mut result, DELTA)
            .unwrap()
            .object()
            .data,
        b"X"
    );
    assert!(matches!(
        reference.resolve_delta(&other, &mut result, DELTA),
        Err(Error::ObjectNotFound)
    ));
}

#[test]
fn malformed_pack_entries_counts_and_zlib_lengths_fail_without_advancing() {
    let plain = wire(&[0x35], b"hello");
    let mut window = [0; WINDOW_SIZE];
    let mut storage = [0; 128];
    let bytes = packed(&[&plain], 1);
    let mut reader = Reader::new(&bytes).unwrap();
    assert_eq!(reader.next_entry_size().unwrap(), Some(5));
    assert_eq!(reader.next_entry_size().unwrap(), Some(5));
    assert_eq!(reader.position(), 12);
    assert!(matches!(
        reader.read_entry(&mut storage[..4], &mut window, INFLATE),
        Err(Error::BufferTooSmall)
    ));
    assert_eq!(reader.position(), 12);
    assert_eq!(reader.next_entry_size().unwrap(), Some(5));
    assert!(matches!(
        reader.read_entry(
            &mut storage,
            &mut window,
            InflateLimits {
                max_output: 4,
                ..INFLATE
            }
        ),
        Err(Error::LimitExceeded)
    ));
    assert_eq!(reader.position(), 12);
    assert_eq!(
        reader
            .read_entry(&mut storage, &mut window, INFLATE)
            .unwrap()
            .unwrap()
            .data(),
        b"hello"
    );
    reader.finish().unwrap();
    let mut invalid = vec![
        wire(&[0x00], b""),
        wire(&[0x50], b""),
        wire(&[0x34], b"hello"),
        wire(&[0x36], b"hello"),
        wire(&[0x60, 0], b""),
        wire(&[0x60, 127], b""),
    ];
    let mut corrupt = plain.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    invalid.push(corrupt);
    invalid.push(vec![0x80; 12]);
    invalid.push(vec![0x70; 4]); // truncated reference ID
    let mut overflow = vec![0xb0];
    overflow.extend_from_slice(&[0xff; 8]);
    overflow.push(0x10);
    invalid.push(overflow);
    for entry in invalid {
        let bytes = packed(&[&entry], 1);
        let mut reader = Reader::new(&bytes).unwrap();
        assert!(matches!(
            reader.read_entry(&mut storage, &mut window, INFLATE),
            Err(Error::CorruptPack)
        ));
        assert_eq!((reader.position(), reader.remaining()), (12, 1));
    }
    assert!(matches!(
        Reader::new(&packed(&[&plain], 0)),
        Err(Error::CorruptPack)
    ));
    let extra = packed(&[&plain, &plain], 1);
    assert!(matches!(
        Reader::new(&extra)
            .unwrap()
            .read_entry(&mut storage, &mut window, INFLATE),
        Err(Error::CorruptPack)
    ));
    let missing = packed(&[&plain], 2);
    let mut reader = Reader::new(&missing).unwrap();
    reader
        .read_entry(&mut storage, &mut window, INFLATE)
        .unwrap();
    assert!(matches!(
        reader.read_entry(&mut storage, &mut window, INFLATE),
        Err(Error::CorruptPack)
    ));
    assert_eq!(reader.finish(), Err(Error::CorruptPack));
}

#[test]
fn delta_copy_defaults_sparse_fields_and_envelope_limits() {
    let body: Vec<u8> = (0..65_539).map(|index| (index % 251) as u8).collect();
    let base = Object {
        kind: ObjectKind::Tag,
        data: &body[..65536],
    };
    let mut output = [0xcc; 65_537];
    let all = [0x80, 0x80, 4, 0x80, 0x80, 4, 0x80];
    let object = pack::apply_delta(base, &all, &mut output, DELTA).unwrap();
    assert_eq!(object.kind, ObjectKind::Tag);
    assert_eq!(object.data, base.data);
    assert_eq!(output[65536], 0xcc);
    let base = Object {
        kind: ObjectKind::Blob,
        data: &body,
    };
    // Base size 65539, target size 3; only offset byte 3 and count byte 1.
    let sparse = [0x83, 0x80, 4, 3, 0x94, 1, 3];
    assert_eq!(
        pack::apply_delta(base, &sparse, &mut output, DELTA)
            .unwrap()
            .data,
        &body[65536..]
    );
    for limits in [
        DeltaLimits {
            max_output: 2,
            ..DELTA
        },
        DeltaLimits {
            max_instructions: 0,
            ..DELTA
        },
        DeltaLimits {
            max_depth: 0,
            ..DELTA
        },
    ] {
        assert!(matches!(
            pack::apply_delta(base, &sparse, &mut output, limits),
            Err(Error::LimitExceeded)
        ));
    }
    assert!(matches!(
        pack::apply_delta(base, &sparse, &mut output[..2], DELTA),
        Err(Error::BufferTooSmall)
    ));
}

#[test]
fn malformed_delta_instructions_lengths_and_varints_are_rejected() {
    let base = Object {
        kind: ObjectKind::Blob,
        data: b"hello",
    };
    let mut output = [0; 128];
    for delta in [
        b"".as_slice(),
        &[5],
        &[6, 0],
        &[5, 1, 0],
        &[5, 3, 3, b'a'],
        &[5, 1, 0x91],
        &[5, 1, 0x91, 5, 1],
        &[5, 1, 0x90, 2],
        &[5, 3, 1, b'a'],
        &[5, 0, 1, b'a'],
        &[5, 1, 0x80],
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2],
    ] {
        assert!(matches!(
            pack::apply_delta(base, delta, &mut output, DELTA),
            Err(Error::CorruptPack)
        ));
    }
    assert!(
        pack::apply_delta(base, &[5, 0], &mut [], DELTA)
            .unwrap()
            .data
            .is_empty()
    );
    assert!(matches!(
        pack::apply_delta(
            base,
            &[5, 2, 1, b'a', 1, b'b'],
            &mut output,
            DeltaLimits {
                max_instructions: 1,
                ..DELTA
            }
        ),
        Err(Error::LimitExceeded)
    ));
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child =
        Command::new(std::env::var_os("GIT_ZERO_TEST_GIT").unwrap_or_else(|| "git".into()))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Git Zero")
            .env("GIT_AUTHOR_EMAIL", "git-zero@example.com")
            .env("GIT_COMMITTER_NAME", "Git Zero")
            .env("GIT_COMMITTER_EMAIL", "git-zero@example.com")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .arg("-C")
            .arg(directory)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let result = std::thread::scope(|scope| {
        scope.spawn(move || stdin.write_all(input).unwrap());
        child.wait_with_output().unwrap()
    });
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

#[test]
fn git_generated_offset_and_reference_packs_match_index_crcs_ids_and_every_body() {
    let directory =
        std::env::temp_dir().join(format!("git-zero-pack-reader-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    git(
        &directory,
        &[
            "init",
            "-q",
            "--initial-branch=main",
            "--object-format=sha1",
        ],
        &[],
    );
    for revision in 0..14 {
        let mut body: Vec<u8> = (0..40_000).map(|index| (index % 251) as u8).collect();
        for index in 0..100 {
            body[revision * 997 + index] = revision as u8;
        }
        std::fs::write(directory.join("data.bin"), body).unwrap();
        git(&directory, &["add", "data.bin"], &[]);
        git(
            &directory,
            &["commit", "-q", "-m", &format!("revision {revision}")],
            &[],
        );
    }
    git(&directory, &["tag", "-a", "v1", "-m", "annotated tag"], &[]);
    let names = git(&directory, &["rev-list", "--objects", "--all"], &[]);
    let mut ids = Vec::new();
    for line in names
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        ids.extend_from_slice(&line[..40]);
        ids.push(b'\n');
    }
    for offsets in [true, false] {
        verify_git_pack(&directory, &ids, offsets);
    }
}

fn verify_git_pack(directory: &Path, ids: &[u8], offsets: bool) {
    const CAPACITY: usize = 65536;
    let mut args = vec![
        "pack-objects",
        "--stdout",
        "--window=50",
        "--depth=10",
        "--compression=9",
    ];
    if offsets {
        args.push("--delta-base-offset");
    }
    let bytes = git(directory, &args, ids);
    let path = directory.join(if offsets {
        "offset.pack"
    } else {
        "reference.pack"
    });
    let index_path = path.with_extension("idx");
    std::fs::write(&path, &bytes).unwrap();
    git(
        directory,
        &[
            "index-pack",
            "--strict",
            "-o",
            index_path.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
        &[],
    );
    let index = std::fs::read(&index_path).unwrap();
    let rows = index_rows(&index);
    let count = rows.len();
    let mut window = [0; WINDOW_SIZE];
    let mut buffers: Vec<_> = (0..count).map(|_| vec![0; CAPACITY]).collect();
    let mut reader = Reader::new(&bytes).unwrap();
    assert_eq!(reader.object_count() as usize, count);
    let mut entries = Vec::new();
    for buffer in &mut buffers {
        entries.push(
            reader
                .read_entry(buffer, &mut window, INFLATE)
                .unwrap()
                .unwrap(),
        );
    }
    reader.finish().unwrap();
    let mut arena = vec![0; count * CAPACITY];
    let mut staging: Vec<_> = arena.chunks_mut(CAPACITY).map(Some).collect();
    let mut resolved: Vec<Option<ResolvedObject<'_>>> = (0..count).map(|_| None).collect();
    while !resolved.iter().all(Option::is_some) {
        let mut progress = false;
        for (index, entry) in entries.iter().enumerate() {
            if resolved[index].is_none() {
                resolved[index] = resolve_entry(entry, &resolved, &mut staging[index]);
                progress |= resolved[index].is_some();
            }
        }
        assert!(progress, "fixture has a missing or cyclic delta base");
    }
    let (mut deltas, mut kinds) = (0, [false; 4]);
    let (mut requests, mut decoded) = (Vec::new(), Vec::new());
    for (entry, object) in entries.iter().zip(&resolved) {
        let object = object.as_ref().unwrap();
        let &(expected, crc, _) = rows.iter().find(|row| row.2 == entry.offset()).unwrap();
        verify_git_object(entry, object, expected, crc, offsets);
        writeln!(requests, "{expected}").unwrap();
        let body = object.object();
        writeln!(
            decoded,
            "{expected} {} {}",
            body.kind.name(),
            body.data.len()
        )
        .unwrap();
        decoded.extend_from_slice(body.data);
        decoded.push(b'\n');
        kinds[object.object().kind as usize - 1] = true;
        deltas += usize::from(object.delta_depth() != 0);
    }
    assert!(deltas > 0, "Git must actually produce deltas");
    assert_eq!(kinds, [true; 4], "commit/tree/blob/tag coverage");
    // One independent Git process validates every body and its batch envelope.
    assert_eq!(decoded, git(directory, &["cat-file", "--batch"], &requests));
}

fn index_rows(index: &[u8]) -> Vec<(ObjectId, u32, u64)> {
    assert_eq!(&index[..8], b"\xfftOc\0\0\0\x02");
    let count = u32::from_be_bytes(index[1028..1032].try_into().unwrap()) as usize;
    // Git v2 indexes store separate ID, CRC and offset tables in ID order.
    let crc_start = 1032 + count * 20;
    let offset_start = crc_start + count * 4;
    let ids = index[1032..crc_start].chunks_exact(20);
    let crcs = index[crc_start..offset_start].chunks_exact(4);
    let offsets = index[offset_start..offset_start + count * 4].chunks_exact(4);
    ids.zip(crcs)
        .zip(offsets)
        .map(|((id, crc), offset)| {
            let id = ObjectId(id.try_into().unwrap());
            let crc = u32::from_be_bytes(crc.try_into().unwrap());
            let offset = u32::from_be_bytes(offset.try_into().unwrap());
            assert_eq!(offset & 0x8000_0000, 0, "small fixture uses direct offsets");
            (id, crc, u64::from(offset))
        })
        .collect()
}

fn resolve_entry<'a>(
    entry: &Entry<'a>,
    resolved: &[Option<ResolvedObject<'a>>],
    staging: &mut Option<&'a mut [u8]>,
) -> Option<ResolvedObject<'a>> {
    if let Some(object) = entry.object() {
        return Some(object);
    }
    let base = resolved
        .iter()
        .flatten()
        .find(|object| match entry.kind() {
            EntryKind::OffsetDelta { base_offset } => object.offset() == Some(base_offset),
            EntryKind::ReferenceDelta { base_id } => object.id() == base_id,
            EntryKind::Object(_) => unreachable!(),
        })?;
    Some(
        entry
            .resolve_delta(base, staging.take().unwrap(), DELTA)
            .unwrap(),
    )
}

fn verify_git_object(
    entry: &Entry<'_>,
    object: &ResolvedObject<'_>,
    expected: ObjectId,
    crc: u32,
    offsets: bool,
) {
    entry.verify_crc32(crc).unwrap();
    object.verify_id(expected).unwrap();
    if object.delta_depth() != 0 {
        assert!(matches!(
            (offsets, entry.kind()),
            (true, EntryKind::OffsetDelta { .. }) | (false, EntryKind::ReferenceDelta { .. })
        ));
    }
}
