//! Bounded targets shared by ordinary regression tests and libFuzzer.
mod client_responses;
mod http_routing;
mod packet_lines;
mod repository_graph;
mod zlib_inflation;

pub use {
    client_responses::responses, http_routing::routing, packet_lines::packets,
    repository_graph::graph, zlib_inflation::inflation,
};

pub const MAX_INPUT: usize = 4096;
