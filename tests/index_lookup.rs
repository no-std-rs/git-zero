#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, ObjectFormat, ObjectId,
    io::SliceWriter,
    pack::{self, IndexEntry, IndexHeader, IndexLookup, LookupStatus},
};

fn id(format: ObjectFormat, bucket: u8, value: u32) -> ObjectId {
    let mut bytes = [0; 32];
    bytes[0] = bucket;
    bytes[1..5].copy_from_slice(&value.to_be_bytes());
    ObjectId::from_bytes(format, &bytes[..format.id_length()]).unwrap()
}

fn encoded(format: ObjectFormat, entries: &[IndexEntry]) -> Vec<u8> {
    let mut storage = vec![0; 2048 + entries.len() * 48];
    let mut writer = SliceWriter::new(&mut storage);
    pack::write_index(entries, id(format, 1, 1), &mut writer).unwrap();
    writer.finish().to_vec()
}

fn lookup(header: &IndexHeader, bytes: &[u8], target: ObjectId) -> git_zero::Result<LookupStatus> {
    let mut lookup = header.lookup(target)?;
    for _ in 0..36 {
        match lookup.status()? {
            LookupStatus::Read(range) => {
                assert!(range.length <= 32);
                assert_eq!(lookup.status(), Ok(LookupStatus::Read(range)));
                lookup
                    .update(&bytes[range.offset as usize..range.offset as usize + range.length])?;
            }
            result => {
                assert_eq!(lookup.update(&[]), Ok(result));
                return Ok(result);
            }
        }
    }
    panic!("lookup exceeded 32 binary-search comparisons and three metadata reads")
}

#[test]
fn every_id_and_absence_match_in_both_formats_without_allocating() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let entries: Vec<_> = (0..1024)
            .map(|row| IndexEntry {
                id: id(format, 127, row * 2 + 2),
                crc32: row * 177,
                offset: if row % 3 == 0 {
                    (1_u64 << 40) + u64::from(row)
                } else {
                    12 + u64::from(row)
                },
            })
            .collect();
        let bytes = encoded(format, &entries);
        let header =
            IndexHeader::parse(format, &bytes[..1032], bytes.len() as u64, 1_u64 << 41).unwrap();
        assert_eq!(header.object_count(), entries.len() as u32);
        assert!(core::mem::size_of::<IndexLookup>() < 256);
        assert!(core::mem::size_of::<IndexHeader>() < 1152);
        allocations::without_allocations(|| {
            for (row, entry) in entries.iter().enumerate() {
                assert_eq!(
                    lookup(&header, &bytes, entry.id).unwrap(),
                    LookupStatus::Found {
                        row: row as u32,
                        entry: *entry
                    }
                );
                assert_eq!(
                    lookup(&header, &bytes, id(format, 127, row as u32 * 2 + 1)).unwrap(),
                    LookupStatus::Missing
                );
            }
            for bucket in [0, 126, 128, 255] {
                assert_eq!(
                    lookup(&header, &bytes, id(format, bucket, 0)).unwrap(),
                    LookupStatus::Missing
                );
            }
            assert_eq!(
                lookup(&header, &bytes, id(format, 127, 4096)).unwrap(),
                LookupStatus::Missing
            );
        });
    }
}

#[test]
fn empty_indexes_and_mixed_formats() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let bytes = encoded(format, &[]);
        let header = IndexHeader::parse(
            format,
            &bytes[..1032],
            bytes.len() as u64,
            12 + format.id_length() as u64,
        )
        .unwrap();
        assert_eq!(header.object_count(), 0);
        assert_eq!(
            lookup(&header, &bytes, id(format, 0, 0)).unwrap(),
            LookupStatus::Missing
        );
        let other = if format == ObjectFormat::Sha1 {
            ObjectFormat::Sha256
        } else {
            ObjectFormat::Sha1
        };
        assert!(matches!(
            header.lookup(id(other, 0, 0)),
            Err(Error::InvalidObjectId)
        ));
    }
}

#[test]
fn malformed_headers_and_table_lengths_are_rejected() {
    let entries = [IndexEntry {
        id: id(ObjectFormat::Sha1, 127, 1),
        offset: 12,
        crc32: 1,
    }];
    let bytes = encoded(ObjectFormat::Sha1, &entries);
    for end in 0..1032 {
        assert!(matches!(
            IndexHeader::parse(ObjectFormat::Sha1, &bytes[..end], bytes.len() as u64, 100),
            Err(Error::CorruptPack)
        ));
    }
    for position in [0, 7, 8 + 126 * 4] {
        let mut malformed = bytes[..1032].to_vec();
        malformed[position] ^= 1;
        assert!(matches!(
            IndexHeader::parse(ObjectFormat::Sha1, &malformed, bytes.len() as u64, 100),
            Err(Error::CorruptPack)
        ));
    }
    for length in [0, bytes.len() - 1, bytes.len() + 1, bytes.len() + 16] {
        assert!(matches!(
            IndexHeader::parse(ObjectFormat::Sha1, &bytes[..1032], length as u64, 100),
            Err(Error::CorruptPack)
        ));
    }
    for pack_bytes in 0..32 {
        assert!(matches!(
            IndexHeader::parse(
                ObjectFormat::Sha1,
                &bytes[..1032],
                bytes.len() as u64,
                pack_bytes
            ),
            Err(Error::CorruptPack)
        ));
    }
}

#[test]
fn malformed_range_replies_and_offsets_fail_terminally() {
    for offset in [12, 1_u64 << 40] {
        let entries = [IndexEntry {
            id: id(ObjectFormat::Sha1, 127, 1),
            offset,
            crc32: 42,
        }];
        let original = encoded(ObjectFormat::Sha1, &entries);
        let header = IndexHeader::parse(
            ObjectFormat::Sha1,
            &original[..1032],
            original.len() as u64,
            1_u64 << 41,
        )
        .unwrap();
        let mut lookup = header.lookup(entries[0].id).unwrap();
        assert_eq!(lookup.update(&[]), Err(Error::CorruptPack));
        assert_eq!(lookup.status(), Err(Error::CorruptPack));
        assert_eq!(lookup.update(&[0; 32]), Err(Error::CorruptPack));
        // Candidate is in a different fanout bucket, or offset points into the
        // pack header/trailer, or a large-offset pointer is outside its table.
        for (position, value) in [(1032, 0), (1056, 0), (1056, 11), (1056, u32::MAX)] {
            let mut bytes = original.clone();
            if position == 1032 {
                bytes[position] = value as u8;
            } else {
                bytes[position..position + 4].copy_from_slice(&value.to_be_bytes());
            }
            assert_eq!(
                self::lookup(&header, &bytes, entries[0].id),
                Err(Error::CorruptPack)
            );
        }
        if offset > 0x7fff_ffff {
            let mut bytes = original.clone();
            bytes[1060..1068].copy_from_slice(&(1_u64 << 41).to_be_bytes());
            assert_eq!(
                self::lookup(&header, &bytes, entries[0].id),
                Err(Error::CorruptPack)
            );
        }
    }
}

#[test]
fn logical_indexes_larger_than_32_bits_need_only_requested_ranges() {
    let count = u32::MAX;
    let mut bytes = [0; 1032];
    bytes[..8].copy_from_slice(b"\xfftOc\0\0\0\x02");
    for slot in bytes[8..].chunks_exact_mut(4) {
        slot.copy_from_slice(&count.to_be_bytes());
    }
    let format = ObjectFormat::Sha256;
    let length = 1032 + u64::from(count) * 40 + 64;
    let header = IndexHeader::parse(format, &bytes, length, 1_u64 << 50).unwrap();
    let target = id(format, 0, 1);
    let mut lookup = header.lookup(target).unwrap();
    let LookupStatus::Read(range) = lookup.status().unwrap() else {
        panic!()
    };
    assert!(range.offset > u64::from(u32::MAX));
    assert_eq!(range.length, 32);
    lookup.update(target.as_bytes()).unwrap();
    lookup.update(&42_u32.to_be_bytes()).unwrap();
    let result = lookup.update(&1234_u32.to_be_bytes()).unwrap();
    assert_eq!(
        result,
        LookupStatus::Found {
            row: count / 2,
            entry: IndexEntry {
                id: target,
                crc32: 42,
                offset: 1234
            }
        }
    );
}
