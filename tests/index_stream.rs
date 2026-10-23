#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, ObjectFormat, ObjectId,
    io::SliceWriter,
    pack::{self, IndexEncoder, IndexEntry},
};
use sha2::Digest;

fn id(format: ObjectFormat, first: u8, last: u8) -> ObjectId {
    let mut bytes = [0; 32];
    bytes[0] = first;
    bytes[format.id_length() - 1] = last;
    ObjectId::from_bytes(format, &bytes[..format.id_length()]).unwrap()
}

fn collect(entries: &[IndexEntry], checksum: ObjectId, capacity: usize) -> Vec<u8> {
    let mut encoder =
        allocations::without_allocations(|| IndexEncoder::new(entries, checksum)).unwrap();
    let mut result = Vec::new();
    let mut guarded = vec![0xa7; capacity + 2];
    while result.len() < encoder.size() as usize {
        assert_eq!(encoder.read(&mut []), Err(Error::BufferTooSmall));
        let count =
            allocations::without_allocations(|| encoder.read(&mut guarded[1..capacity + 1]))
                .unwrap();
        assert!(count > 0 && count <= capacity);
        assert_eq!(guarded[0], 0xa7);
        assert_eq!(guarded[capacity + 1], 0xa7);
        result.extend_from_slice(&guarded[1..count + 1]);
    }
    assert_eq!(result.len(), encoder.size() as usize);
    assert_eq!(encoder.read(&mut []), Ok(0));
    assert_eq!(encoder.read(&mut guarded), Ok(0));
    result
}

#[test]
fn pull_indexes_match_write_indexes_across_every_field_and_checksum_partition() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let checksum = id(format, 73, 19);
        let entries = [
            IndexEntry {
                id: id(format, 0, 0),
                offset: 12,
                crc32: 0,
            },
            IndexEntry {
                id: id(format, 0, 1),
                offset: 0x7fff_ffff,
                crc32: u32::MAX,
            },
            IndexEntry {
                id: id(format, 1, 0),
                offset: 0x8000_0000,
                crc32: 0x1234_5678,
            },
            IndexEntry {
                id: id(format, 254, 0),
                offset: 0xffff_ffff,
                crc32: 0x8765_4321,
            },
            IndexEntry {
                id: id(format, 255, 0),
                offset: 0x1_0000_0000,
                crc32: 15,
            },
            IndexEntry {
                id: id(format, 255, 1),
                offset: u64::MAX,
                crc32: 16,
            },
        ];
        for subset in [&entries[..0], &entries[..1], &entries[..3], &entries[..]] {
            let mut expected = [0; 4096];
            let mut output = SliceWriter::new(&mut expected);
            let trailer = pack::write_index(subset, checksum, &mut output).unwrap();
            let expected = output.as_bytes();
            let width = format.id_length();
            let digest = match format {
                ObjectFormat::Sha1 => {
                    sha1::Sha1::digest(&expected[..expected.len() - width]).to_vec()
                }
                ObjectFormat::Sha256 => {
                    sha2::Sha256::digest(&expected[..expected.len() - width]).to_vec()
                }
            };
            assert_eq!(&digest, trailer.as_bytes());
            for capacity in [1, 2, 3, 4, 7, 19, 20, 21, 31, 32, 33, 4096] {
                assert_eq!(collect(subset, checksum, capacity), expected);
            }
        }
    }
}

#[test]
fn fanout_counts_dense_and_sparse_ids_and_keeps_cursors_independent() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let entries: Vec<_> = (0..=255)
            .flat_map(|first| {
                (0..3).map(move |last| IndexEntry {
                    id: id(format, first, last),
                    offset: 12 + u64::from(first) * 7 + u64::from(last),
                    crc32: 123,
                })
            })
            .collect();
        let checksum = id(format, 83, 7);
        let full = collect(&entries, checksum, 7);
        for first in 0..=255_usize {
            let offset = 8 + first * 4;
            assert_eq!(
                u32::from_be_bytes(full[offset..offset + 4].try_into().unwrap()),
                ((first + 1) * 3) as u32
            );
        }
        let mut left = IndexEncoder::new(&entries, checksum).unwrap();
        let mut right = IndexEncoder::new(&entries, checksum).unwrap();
        let mut l = Vec::new();
        let mut r = Vec::new();
        let mut short = [0; 3];
        let mut long = [0; 64];
        loop {
            let a = left.read(&mut short).unwrap();
            let b = right.read(&mut long).unwrap();
            l.extend_from_slice(&short[..a]);
            r.extend_from_slice(&long[..b]);
            if a == 0 && b == 0 {
                break;
            }
        }
        assert_eq!(l, full);
        assert_eq!(r, full);
    }
}

#[test]
fn invalid_metadata_is_rejected_before_a_cursor_exists() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let checksum = id(format, 20, 0);
        let valid = IndexEntry {
            id: id(format, 1, 0),
            offset: 12,
            crc32: 0,
        };
        let other = if format == ObjectFormat::Sha1 {
            ObjectFormat::Sha256
        } else {
            ObjectFormat::Sha1
        };
        for entries in [
            vec![IndexEntry {
                offset: 11,
                ..valid
            }],
            vec![IndexEntry {
                id: id(other, 1, 0),
                ..valid
            }],
            vec![valid, valid],
            vec![
                IndexEntry {
                    id: id(format, 2, 0),
                    ..valid
                },
                valid,
            ],
        ] {
            assert!(matches!(
                IndexEncoder::new(&entries, checksum),
                Err(Error::InvalidObject)
            ));
            let mut guard = [0xa7; 4096];
            let mut sink = SliceWriter::new(&mut guard);
            assert_eq!(
                pack::write_index(&entries, checksum, &mut sink),
                Err(Error::InvalidObject)
            );
            assert!(sink.is_empty());
            assert!(guard.iter().all(|byte| *byte == 0xa7));
        }
    }
}
