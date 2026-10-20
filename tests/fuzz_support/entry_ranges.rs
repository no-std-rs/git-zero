use git_zero::{
    ObjectFormat,
    pack::{ByteRange, Entry, EntryRangeDecoder, EntryRangeStatus},
    zlib::{InflateLimits, WINDOW_SIZE},
};

pub fn check(entry: &Entry<'_>, bytes: &[u8], format: ObjectFormat) {
    let offset = entry.data().len() / 2;
    let expected = &entry.data()[offset..];
    let mut window = [0; WINDOW_SIZE];
    let mut scratch = [0xa5; 14];
    let mut output = [0xa5; 18];
    let mut reader = EntryRangeDecoder::new(
        format,
        entry.offset(),
        &mut window,
        entry.end_offset() - entry.offset(),
        InflateLimits {
            max_output: 1024,
            max_ratio: 64,
            ratio_slack: 32,
        },
        ByteRange {
            offset: offset as u64,
            length: expected.len() as u64,
        },
        &mut scratch[..13],
    )
    .unwrap();
    let mut wire = entry.offset() as usize;
    let mut written = 0;
    for _ in 0..super::MAX_INPUT * 2 + 1040 {
        let end = (wire + 1 + bytes[0] as usize % 31).min(entry.end_offset() as usize);
        let step = reader
            .update(
                &bytes[wire..end],
                &mut output[..17],
                end == entry.end_offset() as usize,
            )
            .unwrap();
        assert!(step.consumed <= end - wire);
        assert!(step.written <= 17);
        wire += step.consumed;
        assert_eq!(
            &output[..step.written],
            &expected[written..written + step.written]
        );
        written += step.written;
        assert_eq!(reader.position(), written as u64);
        assert_eq!(reader.wire_position(), wire as u64);
        assert_eq!(output[17], 0xa5);
        if step.status == EntryRangeStatus::Finished {
            assert_eq!(written, expected.len());
            reader.finish().unwrap();
            let repeat = reader
                .update(&bytes[wire..], &mut output[..17], true)
                .unwrap();
            assert_eq!(
                (repeat.consumed, repeat.written, repeat.status),
                (0, 0, EntryRangeStatus::Finished)
            );
            assert_eq!(scratch[13], 0xa5);
            return;
        }
    }
    panic!("entry range failed to make bounded internal progress");
}
