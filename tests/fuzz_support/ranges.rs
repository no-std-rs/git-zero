use git_zero::{
    Error, ObjectFormat, ObjectInfo, ObjectKind,
    pack::{
        DeltaDecoder, DeltaLimits, DeltaStatus, ObjectKey, ObjectRange, RangeFrame, RangeLimits,
        RangeObject, RangeRepresentation, RangeRequest, RangeResolver,
    },
};

pub fn check(base: &[u8], bytes: &[u8], expected: Result<&[u8], &Error>) {
    let limits = DeltaLimits {
        max_output: 1024,
        max_instructions: 128,
        max_depth: 2,
    };
    let mut header = DeltaDecoder::new(base.len() as u64, 0, limits).unwrap();
    let size = match header.update(bytes, true) {
        Ok(step) => {
            let DeltaStatus::Header { result_size, .. } = step.status else {
                panic!("delta must emit its header before instructions")
            };
            result_size
        }
        Err(_) => {
            assert!(expected.is_err());
            return;
        }
    };
    let mut output = [0xa5; 1025];
    match reconstruct(base, bytes, size, &mut output[..1024]) {
        Ok(length) => {
            let expected = expected.unwrap();
            assert_eq!(length, expected.len());
            assert_eq!(&output[..length], expected);
        }
        Err(_) => assert!(expected.is_err()),
    }
    assert_eq!(output[1024], 0xa5);
}

fn reconstruct(base: &[u8], bytes: &[u8], size: u64, output: &mut [u8]) -> git_zero::Result<usize> {
    let base_info = ObjectInfo {
        kind: ObjectKind::Blob,
        size: base.len() as u64,
    };
    let mut frames = [const { RangeFrame::empty() }; 2];
    let limits = RangeLimits {
        max_output: 1024,
        max_work_bytes: super::MAX_INPUT as u64 + 1024,
        max_instructions: 128,
        max_depth: 2,
        max_read_bytes: 31,
    };
    let mut resolver = RangeResolver::new(
        ObjectFormat::Sha1,
        ObjectKey::Staged(1),
        ObjectRange::All,
        &mut frames,
        limits,
    )?;
    let mut scratch = [0xa5; 18];
    let mut written = 0;
    for _ in 0..super::MAX_INPUT * 4 + 10 {
        let result = match resolver.request() {
            Ok(RangeRequest::Describe(key)) => {
                let object = match key {
                    ObjectKey::Staged(0) => RangeObject {
                        info: base_info,
                        representation: RangeRepresentation::Plain,
                    },
                    ObjectKey::Staged(1) => RangeObject {
                        info: ObjectInfo {
                            kind: ObjectKind::Blob,
                            size,
                        },
                        representation: RangeRepresentation::Delta {
                            base: ObjectKey::Staged(0),
                            base_info,
                            base_depth: 0,
                            instructions_size: bytes.len() as u64,
                        },
                    },
                    _ => panic!("unexpected object request"),
                };
                resolver.describe(object)
            }
            Ok(RangeRequest::Read {
                object,
                offset,
                length,
                ..
            }) => {
                let source = if object == ObjectKey::Staged(0) {
                    base
                } else {
                    bytes
                };
                let end = (offset as usize + length).min(source.len());
                resolver
                    .update(&source[offset as usize..end], &mut scratch[..17])
                    .map(|step| {
                        output[written..written + step.written]
                            .copy_from_slice(&scratch[..step.written]);
                        written += step.written;
                    })
            }
            Ok(RangeRequest::Finished) => {
                resolver.finish().unwrap();
                assert_eq!(resolver.position(), written as u64);
                return Ok(written);
            }
            Ok(RangeRequest::NeedOutput) => panic!("provider has scratch"),
            Err(error) => Err(error),
        };
        assert_eq!(scratch[17], 0xa5);
        if let Err(error) = result {
            assert_eq!(resolver.request(), Err(error));
            assert_eq!(resolver.finish(), Err(error));
            assert_eq!(resolver.update(&[], &mut scratch), Err(error));
            return Err(error);
        }
    }
    panic!("range reconstruction failed to make bounded progress")
}
