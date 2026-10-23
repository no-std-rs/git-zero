//! Differential index encoding plus bounded external-range lookup. Inputs define
//! up to sixteen metadata entries and one corruption; no pack bodies or heap.
use git_zero::{
    Error, ObjectFormat, ObjectId,
    io::SliceWriter,
    pack::{self, IndexEncoder, IndexEntry, IndexHeader, LookupStatus},
};
use sha2::Digest;

pub fn indexes(input: &[u8]) {
    let input = &input[..input.len().min(super::MAX_INPUT)];
    let format = if input.first().copied().unwrap_or(0) & 1 == 0 {
        ObjectFormat::Sha1
    } else {
        ObjectFormat::Sha256
    };
    let mut entries = [IndexEntry::EMPTY; 16];
    let mut count = 0;
    for chunk in input.get(2..).unwrap_or_default().chunks_exact(20).take(16) {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&chunk[..8]);
        entries[count] = IndexEntry {
            id: ObjectId::from_bytes(format, &bytes[..format.id_length()]).unwrap(),
            offset: u64::from_le_bytes(chunk[8..16].try_into().unwrap()),
            crc32: u32::from_le_bytes(chunk[16..20].try_into().unwrap()),
        };
        count += 1;
    }
    let entries = &mut entries[..count];
    if input.first().copied().unwrap_or(0) & 2 != 0 {
        entries.sort_unstable_by_key(|entry| entry.id);
    }
    let checksum = format.zero_id();
    let mut encoded = [0xa7; 2048];
    let mut writer = SliceWriter::new(&mut encoded[1..2047]);
    let written = pack::write_index(entries, checksum, &mut writer);
    let mut cursor = match IndexEncoder::new(entries, checksum) {
        Ok(cursor) => {
            assert!(written.is_ok());
            cursor
        }
        Err(error) => {
            assert_eq!(written, Err(error));
            assert!(writer.is_empty());
            assert!(encoded.iter().all(|byte| *byte == 0xa7));
            return;
        }
    };
    let expected = writer.finish();
    let expected_length = expected.len();
    assert_eq!(cursor.size(), expected_length as u64);
    let capacity = 1 + usize::from(input.get(1).copied().unwrap_or(0)) % 32;
    let mut storage = [0xa7; 34];
    let mut offset = 0;
    for _ in 0..2048 {
        assert_eq!(
            cursor.read(&mut []),
            if offset == expected_length {
                Ok(0)
            } else {
                Err(Error::BufferTooSmall)
            }
        );
        let count = cursor.read(&mut storage[1..capacity + 1]).unwrap();
        assert!(count <= capacity);
        assert_eq!(storage[0], 0xa7);
        assert_eq!(storage[capacity + 1], 0xa7);
        assert_eq!(&storage[1..count + 1], &expected[offset..offset + count]);
        offset += count;
        if count == 0 {
            break;
        }
    }
    assert_eq!(offset, expected_length);
    assert_eq!(cursor.read(&mut storage), Ok(0));
    let width = format.id_length();
    let expected_hash = match format {
        ObjectFormat::Sha1 => {
            let mut bytes = [0; 32];
            bytes[..20].copy_from_slice(&sha1::Sha1::digest(&expected[..expected_length - width]));
            bytes
        }
        ObjectFormat::Sha256 => sha2::Sha256::digest(&expected[..expected_length - width]).into(),
    };
    assert_eq!(
        &expected_hash[..width],
        &expected[expected_length - width..]
    );
    let pack_size = entries
        .iter()
        .map(|entry| entry.offset)
        .max()
        .unwrap_or(12)
        .checked_add(width as u64 + 1);
    if let Some(pack_size) = pack_size {
        let header =
            IndexHeader::parse(format, &expected[..1032], expected_length as u64, pack_size)
                .unwrap();
        assert_eq!(header.object_count(), entries.len() as u32);
        for (row, entry) in entries.iter().enumerate() {
            assert_eq!(
                lookup(&header, expected, entry.id).unwrap(),
                LookupStatus::Found {
                    row: row as u32,
                    entry: *entry
                }
            );
        }
        let missing = ObjectId::from_bytes(format, &[0xff; 32][..width]).unwrap();
        let result = lookup(&header, expected, missing).unwrap();
        assert_eq!(
            matches!(result, LookupStatus::Found { .. }),
            entries.iter().any(|entry| entry.id == missing)
        );
    }
    // Parser contracts assume authenticated immutable data. Still exercise a
    // bounded corrupt-header/range reply without trusting the resulting absence.
    let length = expected_length;
    if let Some(&mutation) = input.last() {
        let position = usize::from(mutation) * 7 % length;
        encoded[position + 1] ^= mutation;
        let damaged = &encoded[1..length + 1];
        if let Ok(header) = IndexHeader::parse(format, &damaged[..1032], length as u64, u64::MAX) {
            let _ = lookup(&header, damaged, checksum);
        }
    }
    assert_eq!(encoded[0], 0xa7);
    assert_eq!(encoded[2047], 0xa7);
}

fn lookup(header: &IndexHeader, bytes: &[u8], id: ObjectId) -> git_zero::Result<LookupStatus> {
    let mut lookup = header.lookup(id)?;
    for _ in 0..36 {
        match lookup.status()? {
            LookupStatus::Read(range) => {
                assert!(range.length <= 32);
                let start = usize::try_from(range.offset).map_err(|_| Error::CorruptPack)?;
                let end = start.checked_add(range.length).ok_or(Error::CorruptPack)?;
                let response = bytes.get(start..end).ok_or(Error::CorruptPack)?;
                lookup.update(response)?;
            }
            status => return Ok(status),
        }
    }
    panic!("bounded index lookup failed to terminate")
}
