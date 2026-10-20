use super::MAX_INPUT;
use git_zero::{
    Error, Object, ObjectKind,
    pack::{self, DeltaDecoder, DeltaLimits, DeltaStatus},
};

const LIMITS: DeltaLimits = DeltaLimits {
    max_output: 1024,
    max_instructions: 128,
    max_depth: 2,
};

pub fn check(bytes: &[u8]) {
    if bytes.len() > MAX_INPUT {
        return;
    }
    // Vary the base's bytes to catch incorrect sparse copy offsets.
    let mut base = [0; 256];
    for (index, byte) in base.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let mut expected = [0xa5; 1025];
    let result = pack::apply_delta(
        Object {
            kind: ObjectKind::Blob,
            data: &base,
        },
        bytes,
        &mut expected[..1024],
        LIMITS,
    );
    let mut actual = [0xa5; 1025];
    let streamed = execute(&base, bytes, &mut actual[..1024]);
    match result {
        Ok(object) => {
            assert_eq!(streamed, Ok(object.data.len()));
            assert_eq!(&actual[..object.data.len()], object.data);
        }
        Err(_) => assert!(streamed.is_err()),
    }
    assert_eq!(actual[1024], 0xa5);
    assert_eq!(expected[1024], 0xa5);
}

fn execute(base: &[u8], bytes: &[u8], output: &mut [u8]) -> git_zero::Result<usize> {
    let mut decoder = DeltaDecoder::new(base.len() as u64, 0, LIMITS)?;
    let chunk = usize::from(bytes.last().copied().unwrap_or(0) % 17) + 1;
    let (mut offset, mut written) = (0, 0);
    for _ in 0..MAX_INPUT * 3 + 10 {
        let end = (offset + chunk).min(bytes.len());
        let progress = match decoder.update(&bytes[offset..end], end == bytes.len()) {
            Ok(progress) => progress,
            Err(error) => {
                assert_eq!(decoder.finish(), Err(error));
                assert_eq!(decoder.update(bytes, true), Err(error));
                return Err(error);
            }
        };
        assert!(progress.consumed <= end - offset);
        offset += progress.consumed;
        match progress.status {
            DeltaStatus::NeedInput => assert_eq!(offset, end),
            DeltaStatus::Header {
                base_size,
                result_size,
            } => {
                assert_eq!(base_size, base.len() as u64);
                assert!(result_size <= LIMITS.max_output);
            }
            DeltaStatus::Copy { offset, length } => {
                let bytes = &base[offset as usize..(offset + length) as usize];
                output[written..written + bytes.len()].copy_from_slice(bytes);
                written += bytes.len();
            }
            DeltaStatus::Insert(bytes) => {
                output[written..written + bytes.len()].copy_from_slice(bytes);
                written += bytes.len();
            }
            DeltaStatus::Finished => {
                assert_eq!(offset, bytes.len());
                assert_eq!(decoder.finish(), Ok(()));
                assert_eq!(
                    decoder.update(&[], true).unwrap().status,
                    DeltaStatus::Finished
                );
                assert_eq!(decoder.update(&[0], true), Err(Error::CorruptPack));
                return Ok(written);
            }
        }
        assert_eq!(decoder.position(), written as u64);
    }
    panic!("delta decoder failed to make bounded progress");
}
