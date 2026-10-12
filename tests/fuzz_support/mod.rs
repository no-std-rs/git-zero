//! Bounded targets shared by ordinary regression tests and libFuzzer.
mod client_responses;
mod http_routing;
mod packet_lines;
mod repository_graph;

pub use client_responses::responses;
pub use http_routing::routing;
pub use packet_lines::packets;
pub use repository_graph::graph;

pub const MAX_INPUT: usize = 4096;
