//! Bounded targets shared by ordinary regression tests and libFuzzer.
mod client_responses;
mod delta;
mod entry_ranges;
mod http_routing;
mod indexes;
mod loose_objects;
mod object_links;
mod pack_entries;
mod packet_lines;
mod ranges;
mod receive;
mod repository_graph;
mod zlib_inflation;

pub use {
    client_responses::responses, http_routing::routing, indexes::indexes, loose_objects::loose,
    pack_entries::entries, packet_lines::packets, receive::receive, repository_graph::graph,
    zlib_inflation::inflation,
};

pub const MAX_INPUT: usize = 4096;
