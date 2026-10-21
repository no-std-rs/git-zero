use super::MAX_INPUT;
use git_zero::{
    Error, Object, ObjectFormat, ObjectKind,
    pack::{
        self, DecodeLimits, DecodeProgress, DecodeStatus, Decoder, DeltaLimits, Entry, Reader,
        ResolvedObject,
    },
    zlib::{InflateLimits, WINDOW_SIZE},
};
use sha1::{Digest, Sha1};
use sha2::Sha256;

const OUTPUT: usize = 1024;
const INFLATE: InflateLimits = InflateLimits {
    max_output: OUTPUT as u64,
    max_ratio: 64,
    ratio_slack: 32,
};
const DELTA: DeltaLimits = DeltaLimits {
    max_output: OUTPUT as u64,
    max_instructions: 128,
    max_depth: 2,
};

/// Repair pack hashes to reach entry decoding after mutations. Fixed buffers and
/// budgets bound parser work; guards and cursor rollback catch failure mistakes.
pub fn entries(data: &[u8]) {
    super::delta::check(data);
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        entries_with_format(data, format);
    }
}

fn entries_with_format(data: &[u8], format: ObjectFormat) {
    if data.len() > MAX_INPUT {
        return;
    }
    let base = ResolvedObject::external_with_format(
        Object {
            kind: ObjectKind::Blob,
            data: &[b'A'; 256],
        },
        format,
    );
    let mut output = [0xa5; OUTPUT + 1];
    let _ = pack::apply_delta(base.object(), data, &mut output[..OUTPUT], DELTA);
    assert_eq!(output[OUTPUT], 0xa5);
    let mut bytes = [0; MAX_INPUT];
    let bytes = &mut bytes[..data.len()];
    bytes.copy_from_slice(data);
    if bytes.len() >= 12 + format.id_length() {
        let end = bytes.len() - format.id_length();
        match format {
            ObjectFormat::Sha1 => {
                let hash = Sha1::digest(&bytes[..end]);
                bytes[end..].copy_from_slice(&hash);
            }
            ObjectFormat::Sha256 => {
                let hash = Sha256::digest(&bytes[..end]);
                bytes[end..].copy_from_slice(&hash);
            }
        }
    }
    let mut stream_window = [0; WINDOW_SIZE];
    let mut stream = Fragmented::new(bytes, format, &mut stream_window);
    let Ok(mut reader) = Reader::new_with_format(bytes, format) else {
        assert!(stream.drain().is_err());
        return;
    };
    let mut window = [0; WINDOW_SIZE];
    let mut inflated = [0; OUTPUT + 1];
    for _ in 0..=MAX_INPUT {
        let Some(entry) = read_entry(&mut reader, &mut inflated, &mut window, &mut stream) else {
            return;
        };
        stream.compare(&entry);
        check_object(&entry, bytes, &base, &mut output[..OUTPUT]);
        assert_eq!(inflated[OUTPUT], 0xa5);
        assert_eq!(output[OUTPUT], 0xa5);
    }
    panic!("pack entries failed to make bounded cursor progress");
}

fn read_entry<'out>(
    reader: &mut Reader<'_>,
    inflated: &'out mut [u8; OUTPUT + 1],
    window: &mut [u8; WINDOW_SIZE],
    stream: &mut Fragmented<'_, '_>,
) -> Option<Entry<'out>> {
    inflated.fill(0xa5);
    let (output, guard) = inflated.split_at_mut(OUTPUT);
    let before = reader.position();
    let size = reader.next_entry_size();
    assert_eq!(reader.position(), before);
    let entry = match reader.read_entry(output, window, INFLATE) {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            assert_eq!(size.unwrap(), None);
            reader.finish().unwrap();
            assert_eq!(stream.drain().unwrap(), reader.pack_id());
            return None;
        }
        Err(error) => {
            if let Err(header_error) = size {
                assert_eq!(header_error, error);
            }
            assert_eq!(reader.position(), before);
            assert!(matches!(
                error,
                Error::CorruptPack | Error::BufferTooSmall | Error::LimitExceeded
            ));
            assert_eq!(guard[0], 0xa5);
            assert!(stream.drain().is_err());
            return None;
        }
    };
    assert_eq!(size.unwrap(), Some(entry.data().len() as u64));
    assert_eq!(entry.offset(), before);
    assert!(entry.end_offset() > before);
    assert_eq!(entry.end_offset(), reader.position());
    Some(entry)
}

struct Fragmented<'input, 'window> {
    bytes: &'input [u8],
    decoder: Decoder<'window>,
    offset: usize,
    output: [u8; 18],
}

impl<'input, 'window> Fragmented<'input, 'window> {
    fn new(
        bytes: &'input [u8],
        format: ObjectFormat,
        window: &'window mut [u8; WINDOW_SIZE],
    ) -> Self {
        Self {
            bytes,
            decoder: Decoder::new(
                format,
                window,
                DecodeLimits {
                    max_pack_bytes: MAX_INPUT as u64,
                    // The whole-pack reader has no separate count budget. The
                    // finite wire and per-entry limits bound differential work.
                    max_objects: u32::MAX,
                    inflate: INFLATE,
                },
            ),
            offset: 0,
            output: [0xa5; 18],
        }
    }

    fn step(&mut self) -> git_zero::Result<DecodeProgress> {
        let end = (self.offset + 1 + self.bytes.first().copied().unwrap_or(0) as usize % 31)
            .min(self.bytes.len());
        let result = self.decoder.update(
            &self.bytes[self.offset..end],
            &mut self.output[..17],
            end == self.bytes.len(),
        );
        assert_eq!(self.output[17], 0xa5);
        let progress = match result {
            Ok(progress) => progress,
            Err(error) => {
                assert_eq!(self.decoder.finish(), Err(error));
                assert_eq!(
                    self.decoder.update(&[], &mut self.output[..17], true),
                    Err(error)
                );
                return Err(error);
            }
        };
        assert!(progress.consumed <= end - self.offset);
        assert!(progress.written <= 17);
        self.offset += progress.consumed;
        assert_eq!(self.decoder.position(), self.offset as u64);
        match progress.status {
            DecodeStatus::Header { objects, version } => {
                assert_eq!(self.offset, 12);
                assert_eq!(
                    objects,
                    u32::from_be_bytes(self.bytes[8..12].try_into().unwrap())
                );
                assert_eq!(
                    version,
                    u32::from_be_bytes(self.bytes[4..8].try_into().unwrap())
                );
            }
            DecodeStatus::EntryEnd { end_offset, .. } => assert_eq!(end_offset, self.offset as u64),
            DecodeStatus::Finished(id) => {
                assert_eq!(self.offset, self.bytes.len());
                assert_eq!(self.decoder.finish(), Ok(id));
            }
            DecodeStatus::NeedInput | DecodeStatus::NeedOutput => {
                assert!(progress.consumed != 0 || progress.written != 0)
            }
            _ => {}
        }
        Ok(progress)
    }

    fn compare(&mut self, expected: &Entry<'_>) {
        let mut written = 0;
        let mut started = false;
        for _ in 0..MAX_INPUT + OUTPUT + 4 {
            let progress = self.step().unwrap();
            let end = written + progress.written;
            assert_eq!(
                &self.output[..progress.written],
                &expected.data()[written..end]
            );
            written = end;
            match progress.status {
                DecodeStatus::EntryStart(header) => {
                    assert!(!started);
                    started = true;
                    assert_eq!(header.offset, expected.offset());
                    assert_eq!(header.kind, expected.kind());
                    assert_eq!(header.size, expected.data().len() as u64);
                }
                DecodeStatus::EntryEnd {
                    end_offset,
                    crc32,
                    object_id,
                } => {
                    assert!(started);
                    assert_eq!(written, expected.data().len());
                    assert_eq!(end_offset, expected.end_offset());
                    assert_eq!(crc32, expected.crc32());
                    assert_eq!(object_id, expected.object().map(|object| object.id()));
                    return;
                }
                DecodeStatus::Finished(_) => panic!("stream ended before the reader's entry"),
                _ => {}
            }
        }
        panic!("entry decoder failed to make bounded progress");
    }

    fn drain(&mut self) -> git_zero::Result<git_zero::ObjectId> {
        for _ in 0..MAX_INPUT * (OUTPUT + 4) {
            if let DecodeStatus::Finished(id) = self.step()?.status {
                return Ok(id);
            }
        }
        panic!("pack decoder failed to make bounded progress");
    }
}

fn check_object(entry: &Entry<'_>, bytes: &[u8], base: &ResolvedObject<'_>, output: &mut [u8]) {
    super::entry_ranges::check(entry, bytes, base.id().format());
    assert!(entry.data().len() <= OUTPUT);
    assert_eq!(
        entry.crc32(),
        pack::crc32(&bytes[entry.offset() as usize..entry.end_offset() as usize])
    );
    if let Some(object) = entry.object() {
        assert_eq!(object.delta_depth(), 0);
        assert_eq!(
            object.id(),
            object.object().id_with_format(object.id().format())
        );
    } else {
        let _ = entry.resolve_delta(base, output, DELTA);
    }
}
