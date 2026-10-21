//! Independent range reconstruction from Git-generated compressed pack entries.
use git_zero::{
    ObjectDigest, ObjectId,
    pack::{
        ByteRange, Entry, EntryKind, EntryRangeDecoder, EntryRangeStatus, ObjectKey, ObjectRange,
        RangeFrame, RangeLimits, RangeObject, RangeRepresentation, RangeRequest, RangeResolver,
        ResolvedObject,
    },
    zlib::{InflateLimits, WINDOW_SIZE},
};

struct Record<'a> {
    entry: &'a Entry<'a>,
    id: ObjectId,
    object: RangeObject,
    expected: &'a [u8],
}

pub fn verify(bytes: &[u8], entries: &[Entry<'_>], objects: &[Option<ResolvedObject<'_>>]) {
    let records: Vec<_> = entries
        .iter()
        .zip(objects)
        .map(|(entry, object)| {
            let object = object.as_ref().unwrap();
            let base = match entry.kind() {
                EntryKind::Object(_) => None,
                EntryKind::OffsetDelta { base_offset } => objects
                    .iter()
                    .flatten()
                    .find(|base| base.offset() == Some(base_offset)),
                EntryKind::ReferenceDelta { base_id } => {
                    objects.iter().flatten().find(|base| base.id() == base_id)
                }
            };
            let representation = base.map_or(RangeRepresentation::Plain, |base| {
                RangeRepresentation::Delta {
                    base: ObjectKey::Id(base.id()),
                    base_info: base.object().info(),
                    base_depth: base.delta_depth(),
                    instructions_size: entry.data().len() as u64,
                }
            });
            Record {
                entry,
                id: object.id(),
                object: RangeObject {
                    info: object.object().info(),
                    representation,
                },
                expected: object.object().data,
            }
        })
        .collect();
    for record in &records {
        let actual = reconstruct(bytes, &records, record, ObjectRange::All);
        assert_eq!(actual, record.expected);
        let mut digest = ObjectDigest::new(record.object.info, record.id.format());
        for chunk in actual.chunks(13) {
            digest.update(chunk).unwrap();
        }
        assert_eq!(digest.finish(), Ok(record.id));
        if record.expected.len() > 2 {
            let range = ObjectRange::Bytes {
                offset: 1,
                length: record.expected.len() as u64 - 2,
            };
            assert_eq!(
                reconstruct(bytes, &records, record, range),
                &record.expected[1..record.expected.len() - 1]
            );
        }
    }
}

fn reconstruct(
    bytes: &[u8],
    records: &[Record<'_>],
    root: &Record<'_>,
    range: ObjectRange,
) -> Vec<u8> {
    let mut frames = [const { RangeFrame::empty() }; 16];
    let limits = RangeLimits {
        max_output: 65536,
        max_work_bytes: 1 << 24,
        max_instructions: 100_000,
        max_depth: 16,
        max_read_bytes: 127,
    };
    let mut resolver = RangeResolver::new(
        root.id.format(),
        ObjectKey::Id(root.id),
        range,
        &mut frames,
        limits,
    )
    .unwrap();
    let mut result = Vec::new();
    let mut output = [0; 128];
    loop {
        match resolver.request().unwrap() {
            RangeRequest::Describe(key) => resolver
                .describe(
                    records
                        .iter()
                        .find(|record| ObjectKey::Id(record.id) == key)
                        .unwrap()
                        .object,
                )
                .unwrap(),
            RangeRequest::Read {
                object,
                offset,
                length,
                ..
            } => {
                let record = records
                    .iter()
                    .find(|record| ObjectKey::Id(record.id) == object)
                    .unwrap();
                let input = read_packed_range(
                    bytes,
                    record,
                    ByteRange {
                        offset,
                        length: length as u64,
                    },
                );
                let step = resolver.update(&input, &mut output).unwrap();
                result.extend_from_slice(&output[..step.written]);
            }
            RangeRequest::Finished => {
                resolver.finish().unwrap();
                return result;
            }
            _ => panic!("range provider always supplies output capacity"),
        }
    }
}

fn read_packed_range(bytes: &[u8], record: &Record<'_>, range: ByteRange) -> Vec<u8> {
    let entry = record.entry;
    let mut window = [0; WINDOW_SIZE];
    let mut scratch = [0; 127];
    let mut output = [0; 31];
    let mut reader = EntryRangeDecoder::new(
        record.id.format(),
        entry.offset(),
        &mut window,
        entry.end_offset() - entry.offset(),
        InflateLimits {
            max_output: 100_000,
            max_ratio: 4096,
            ratio_slack: 65536,
        },
        range,
        &mut scratch,
    )
    .unwrap();
    let mut offset = entry.offset() as usize;
    let mut result = Vec::new();
    loop {
        let end = (offset + 47).min(entry.end_offset() as usize);
        let step = reader
            .update(
                &bytes[offset..end],
                &mut output,
                end == entry.end_offset() as usize,
            )
            .unwrap();
        offset += step.consumed;
        result.extend_from_slice(&output[..step.written]);
        if step.status == EntryRangeStatus::Finished {
            reader.finish().unwrap();
            assert_eq!(result.len() as u64, range.length);
            return result;
        }
    }
}
