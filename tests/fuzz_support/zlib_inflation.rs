use super::MAX_INPUT;
use git_zero::{
    Error,
    zlib::{InflateLimits, InflateStatus, Inflater, WINDOW_SIZE},
};

const MAX_OUTPUT: usize = 4096;

#[derive(Debug, Eq, PartialEq)]
struct Outcome {
    error: Option<Error>,
    consumed: u64,
    written: usize,
}

/// Compare whole-input and fragmented decoding, including terminal errors,
/// exact stream boundaries, output guards, old history and expansion budgets.
pub fn inflation(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let mut window = [0; WINDOW_SIZE];
    let mut whole = [0; MAX_OUTPUT];
    let mut fragmented = [0; MAX_OUTPUT];
    for limits in [
        InflateLimits {
            max_output: MAX_OUTPUT as u64,
            max_ratio: 64,
            ratio_slack: 64,
        },
        InflateLimits {
            max_output: 257,
            max_ratio: 1,
            ratio_slack: 0,
        },
    ] {
        let first = run(data, MAX_INPUT, 257, limits, &mut window, &mut whole);
        // History from a different stream must never become accessible.
        window.fill(0xff);
        let input_size = usize::from(data.first().copied().unwrap_or(0) % 17) + 1;
        let output_size = usize::from(data.last().copied().unwrap_or(0) % 31) + 1;
        let second = run(
            data,
            input_size,
            output_size,
            limits,
            &mut window,
            &mut fragmented,
        );
        assert_eq!(first, second);
        assert_eq!(&whole[..first.written], &fragmented[..second.written]);
    }
}

fn run(
    data: &[u8],
    input_size: usize,
    output_size: usize,
    limits: InflateLimits,
    window: &mut [u8; WINDOW_SIZE],
    storage: &mut [u8; MAX_OUTPUT],
) -> Outcome {
    let mut decoder = Inflater::new(window, limits);
    let mut scratch = [0; 258];
    let (mut consumed, mut written) = (0, 0);
    for _ in 0..=MAX_INPUT + MAX_OUTPUT {
        scratch.fill(0xa5);
        let end = data.len().min(consumed + input_size);
        let before = decoder.total_output();
        let result = decoder.update(
            &data[consumed..end],
            &mut scratch[..output_size],
            end == data.len(),
        );
        // Even on error, only the bytes counted by the decoder were emitted.
        let emitted = (decoder.total_output() - before) as usize;
        assert!(emitted <= output_size);
        assert!(scratch[emitted..].iter().all(|&byte| byte == 0xa5));
        assert!(decoder.total_output() <= limits.max_output);
        assert!(
            decoder.total_output()
                <= decoder
                    .total_input()
                    .saturating_mul(u64::from(limits.max_ratio))
                    .saturating_add(limits.ratio_slack)
        );
        storage[written..written + emitted].copy_from_slice(&scratch[..emitted]);
        written += emitted;
        match result {
            Err(error) => {
                assert!(matches!(
                    error,
                    Error::InvalidZlib | Error::Unsupported | Error::LimitExceeded
                ));
                assert_eq!(decoder.update(b"new input", &mut [], false), Err(error));
                return Outcome {
                    error: Some(error),
                    consumed: decoder.total_input(),
                    written,
                };
            }
            Ok(progress) => {
                assert!(progress.consumed <= end - consumed);
                assert_eq!(progress.written, emitted);
                consumed += progress.consumed;
                assert_eq!(decoder.total_input(), consumed as u64);
                if progress.status == InflateStatus::Finished {
                    let again = decoder.update(data, &mut scratch, true).unwrap();
                    assert_eq!(
                        (again.consumed, again.written, again.status),
                        (0, 0, InflateStatus::Finished)
                    );
                    return Outcome {
                        error: None,
                        consumed: decoder.total_input(),
                        written,
                    };
                }
                assert!(progress.consumed != 0 || progress.written != 0);
                if progress.status == InflateStatus::NeedInput {
                    assert_eq!(consumed, end);
                }
            }
        }
    }
    panic!("inflation exceeded its input/output progress budget");
}
