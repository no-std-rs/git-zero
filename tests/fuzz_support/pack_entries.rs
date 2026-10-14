use super::MAX_INPUT;
use git_zero::{
    Error, Object, ObjectFormat, ObjectKind,
    pack::{self, DeltaLimits, Entry, Reader, ResolvedObject},
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
    let Ok(mut reader) = Reader::new_with_format(bytes, format) else {
        return;
    };
    let mut window = [0; WINDOW_SIZE];
    let mut inflated = [0; OUTPUT + 1];
    for _ in 0..=MAX_INPUT {
        inflated.fill(0xa5);
        let before = reader.position();
        let size = reader.next_entry_size();
        assert_eq!(reader.position(), before);
        let entry = match reader.read_entry(&mut inflated[..OUTPUT], &mut window, INFLATE) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                assert_eq!(size.unwrap(), None);
                reader.finish().unwrap();
                return;
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
                assert_eq!(inflated[OUTPUT], 0xa5);
                return;
            }
        };
        assert_eq!(size.unwrap(), Some(entry.data().len() as u64));
        assert_eq!(entry.offset(), before);
        assert!(entry.end_offset() > before);
        assert_eq!(entry.end_offset(), reader.position());
        check_object(&entry, bytes, &base, &mut output[..OUTPUT]);
        assert_eq!(inflated[OUTPUT], 0xa5);
        assert_eq!(output[OUTPUT], 0xa5);
    }
    panic!("pack entries failed to make bounded cursor progress");
}

fn check_object(entry: &Entry<'_>, bytes: &[u8], base: &ResolvedObject<'_>, output: &mut [u8]) {
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
