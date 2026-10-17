#[path="http_progress.rs"] mod http;
fn main() -> std::io::Result<()> {
    http::serve(0, |request| {
        eprintln!("HTTP handler {} {} bytes", request.target, request.body.len());
        http::Response::error(200, &format!("{}\n", request.body.len()))
    })
}
