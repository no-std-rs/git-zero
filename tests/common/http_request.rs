use git_zero::http::Request;

pub fn upload(body: &[u8], version: u8) -> Request<'_> {
    Request {
        method: "POST",
        path: "git-upload-pack",
        query: None,
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: match version {
            1 => Some("version=1"),
            2 => Some("version=2"),
            _ => None,
        },
        body,
    }
}
