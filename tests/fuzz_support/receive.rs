use super::MAX_INPUT;
use git_zero::{
    ObjectFormat,
    http::receive::{self, Capabilities, ControlDecoder, ControlEvent, Limits, Report},
    io::SliceWriter,
    repository::validate_ref_name_bytes,
};

pub fn receive(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        control(data, format);
    }
    let mut output = [0xa5; 513];
    let mut sink = SliceWriter::new(&mut output[..512]);
    let selected = Capabilities::REPORT_STATUS | Capabilities::SIDE_BAND_64K;
    let _ = receive::write_progress(&mut sink, selected, data);
    if let Ok(mut report) = Report::new(&mut sink, selected, Ok(())) {
        let _ = report.reference(b"refs/heads/main", Err(data));
        let _ = report.finish();
    }
    assert_eq!(output[512], 0xa5);
}

fn control(data: &[u8], format: ObjectFormat) {
    let mut storage = [0xa5; 1025];
    let (storage, guard) = storage.split_at_mut(1024);
    let offered = Capabilities::REPORT_STATUS
        | Capabilities::REPORT_STATUS_V2
        | Capabilities::DELETE_REFS
        | Capabilities::OFS_DELTA
        | Capabilities::ATOMIC
        | Capabilities::PUSH_OPTIONS
        | Capabilities::SIDE_BAND_64K
        | Capabilities::QUIET;
    let limits = Limits {
        max_bytes: MAX_INPUT as u64,
        max_commands: 8,
        max_shallow: 8,
        max_options: 8,
    };
    let mut decoder = ControlDecoder::new(storage, format, offered, limits).unwrap();
    let mut offset = 0;
    let mut commands = 0;
    let mut shallow = 0;
    let mut options = 0;
    for _ in 0..MAX_INPUT * 2 + 4 {
        let end = (offset + 1 + data.first().copied().unwrap_or(0) as usize % 7).min(data.len());
        let step = match decoder.read(&data[offset..end], end == data.len()) {
            Ok(step) => step,
            Err(error) => {
                assert_eq!(decoder.read(data, true), Err(error));
                assert_eq!(guard[0], 0xa5);
                return;
            }
        };
        assert!(step.consumed <= end - offset);
        offset += step.consumed;
        match step.event {
            ControlEvent::Command(command) => {
                validate_ref_name_bytes(command.name).unwrap();
                assert_eq!(command.old.format(), format);
                assert_eq!(command.new.format(), format);
                commands += 1;
            }
            ControlEvent::Shallow(id) => {
                assert_eq!(id.format(), format);
                assert!(!id.is_zero());
                shallow += 1;
            }
            ControlEvent::PushOption(option) => {
                assert!(!option.iter().any(|byte| matches!(byte, 0 | b'\n' | b'\r')));
                options += 1;
            }
            ControlEvent::End { pack_required } => {
                let repeated = decoder.read(&data[offset..], true).unwrap();
                assert_eq!(repeated.consumed, 0);
                assert_eq!(repeated.event, ControlEvent::End { pack_required });
                assert_eq!(guard[0], 0xa5);
                return;
            }
            _ => assert!(step.consumed > 0),
        }
        assert!(
            commands <= limits.max_commands
                && shallow <= limits.max_shallow
                && options <= limits.max_options
        );
        assert_eq!(guard[0], 0xa5);
    }
    panic!("receive control parser failed to make bounded progress");
}
