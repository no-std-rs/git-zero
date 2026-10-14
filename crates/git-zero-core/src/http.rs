//! Shared HTTP request and response values; framing belongs to the host.

#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub method: &'a str,
    /// Repository-relative URL path. Escape URL-reserved bytes with `%HH`.
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub body: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
}
