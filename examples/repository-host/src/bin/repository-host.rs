use git_zero_host::http;
use git_zero_host::native::{Metrics, NativeHost, http_backend};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let mut arguments = std::env::args().skip(1).peekable();
    if arguments.peek().is_some_and(|arg| arg == "--cloudflare") {
        arguments.next();
        let origin = arguments
            .next()
            .ok_or_else(|| io::Error::other("missing emulator origin"))?;
        let mut repo = "demo".to_owned();
        let mut token = "git-zero-local-services".to_owned();
        let mut cold = true;
        let mut unregister = false;
        let mut gateway_first_read = true;
        let mut gateway_first_receive = true;
        let mut socket_rpc = true;
        let mut protected_chunk = git_zero_host::native::ProtectedChunkMode::Joint;
        loop {
            match arguments.peek().map(String::as_str) {
                Some("--repo") => {
                    arguments.next();
                    repo = arguments
                        .next()
                        .ok_or_else(|| io::Error::other("missing repository ID"))?;
                }
                Some("--token") => {
                    arguments.next();
                    token = arguments
                        .next()
                        .ok_or_else(|| io::Error::other("missing local token"))?;
                }
                Some("--view-cache") => {
                    arguments.next();
                    cold = match arguments.next().as_deref() {
                        Some("cold") => true,
                        Some("warm") => false,
                        _ => return Err(io::Error::other("view cache must be cold or warm")),
                    };
                }
                Some("--gateway-first-read") => {
                    arguments.next();
                    gateway_first_read = match arguments.next().as_deref() {
                        Some("on") => true,
                        Some("off") => false,
                        _ => return Err(io::Error::other("gateway first read must be on or off")),
                    };
                }
                Some("--gateway-first-receive") => {
                    arguments.next();
                    gateway_first_receive = match arguments.next().as_deref() {
                        Some("on") => true,
                        Some("off") => false,
                        _ => {
                            return Err(io::Error::other(
                                "gateway first receive must be on or off",
                            ));
                        }
                    };
                }
                Some("--unregister") => {
                    arguments.next();
                    unregister = true;
                }
                Some("--socket-rpc") => {
                    arguments.next();
                    socket_rpc = match arguments.next().as_deref() {
                        Some("on") => true,
                        Some("off") => false,
                        _ => return Err(io::Error::other("socket RPC must be on or off")),
                    };
                }
                Some("--protected-chunk") => {
                    arguments.next();
                    protected_chunk = match arguments.next().as_deref() {
                        Some("on" | "one") => git_zero_host::native::ProtectedChunkMode::One,
                        Some("multi") => git_zero_host::native::ProtectedChunkMode::Multi,
                        Some("joint") => git_zero_host::native::ProtectedChunkMode::Joint,
                        Some("off") => git_zero_host::native::ProtectedChunkMode::Off,
                        _ => {
                            return Err(io::Error::other(
                                "protected chunk must be off, on, one, multi or joint",
                            ));
                        }
                    };
                }
                _ => break,
            }
        }
        let services = git_zero_host::cloudflare::HttpServices::new(&origin, &repo, &token)?;
        if unregister {
            return services.unregister();
        }
        let root = arguments
            .next()
            .ok_or_else(|| io::Error::other("missing local codec root"))?;
        let port = arguments
            .next()
            .map(|s| s.parse::<u16>())
            .transpose()
            .map_err(io::Error::other)?
            .unwrap_or(0);
        if arguments.next().is_some() {
            return Err(io::Error::other("unexpected argument"));
        }
        let mut host =
            NativeHost::open_cloudflare(PathBuf::from(root), &origin, &repo, &token, cold)?;
        host.set_protected_chunk_mode(protected_chunk);
        let services = host
            .cloudflare_services()
            .ok_or_else(|| io::Error::other("missing Cloudflare registration services"))?;
        services.enable_first_read(cold && gateway_first_read);
        services.enable_first_receive(cold && gateway_first_read && gateway_first_receive);
        services.enable_socket_rpc(socket_rpc);
        return http::serve_with_ready(
            port,
            |request| host.handle(request),
            |native_origin| {
                services.register(native_origin)?;
                eprintln!("Git gateway: {origin}/{repo}/repo.git");
                eprintln!("Object statistics and cost estimates: {native_origin}/_object-stats");
                Ok(())
            },
        );
    }
    let first = arguments
        .next()
        .ok_or_else(|| io::Error::other("usage: repository-host [--reference] ROOT [PORT]"))?;
    let reference = first == "--reference";
    let root = if reference {
        arguments
            .next()
            .ok_or_else(|| io::Error::other("missing bare repository"))?
    } else {
        first
    };
    let port = arguments
        .next()
        .map(|s| s.parse::<u16>())
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or(0);
    if reference {
        let root = PathBuf::from(root).canonicalize()?;
        if root.file_name().and_then(|s| s.to_str()) != Some("repo.git") {
            return Err(io::Error::other(
                "reference repository must be named repo.git",
            ));
        }
        let mut metrics = Metrics::default();
        http::serve(port, |request| {
            if request.method == "GET" && request.target == "/metrics" {
                return http::Response {
                    status: 200,
                    headers: BTreeMap::from([("Content-Type".into(), "application/json".into())]),
                    body: serde_json::to_vec(&metrics).unwrap_or_default().into(),
                };
            }
            metrics.requests += 1;
            metrics.input_bytes += request.body.len() as u64;
            let response = http_backend(&root, &request)
                .unwrap_or_else(|error| http::Response::error(503, &error.to_string()));
            metrics.output_bytes += response.body.len() as u64;
            response
        })
    } else {
        let mut host = NativeHost::open(PathBuf::from(root))?;
        http::serve(port, |request| host.handle(request))
    }
}
