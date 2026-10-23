#![no_main]
#[path = "../../crates/git-zero-rustix/tests/common/native_parser.rs"]
mod native;
libfuzzer_sys::fuzz_target!(|data: &[u8]| native::exercise(data));
