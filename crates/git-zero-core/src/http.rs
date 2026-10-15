//! Shared HTTP request and response values; framing belongs to the host.

#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub method: &'a str,
    /// Repository-relative URL path. Escape URL-reserved bytes with `%HH`.
    pub path: &'a str,
    pub query: Option<&'a str>,
    /// Request Content-Type; upload-pack requires its Git request media type.
    pub content_type: Option<&'a str>,
    /// Git-Protocol header, for example `version=2`. Absent selects v0.
    pub git_protocol: Option<&'a str>,
    pub body: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
}
