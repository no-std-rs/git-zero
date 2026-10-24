//! Logical admission policies and accepted-socket settings through public APIs.
#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "admission/chunked.rs"]
mod chunked;
#[path = "admission/deadlines.rs"]
mod deadlines;
#[path = "admission/headers.rs"]
mod headers;
#[path = "common/http.rs"]
mod http;
#[path = "admission/intake.rs"]
mod intake;
#[path = "admission/parser_fuzz.rs"]
mod parser_fuzz;
#[path = "admission/response_deadlines.rs"]
mod response_deadlines;
#[path = "admission/timeouts.rs"]
mod timeouts;
