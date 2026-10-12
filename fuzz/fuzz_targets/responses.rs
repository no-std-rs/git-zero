#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| git_zero_fuzz::targets::responses(data));
