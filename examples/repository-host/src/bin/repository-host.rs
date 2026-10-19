use git_zero_host::http;
use git_zero_host::native::{Metrics, NativeHost, http_backend};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let first = required_argument(
        &mut arguments,
        "usage: repository-host [--reference] ROOT [PORT]",
    )?;
    match first.as_str() {
        "--cloudflare" => serve_cloudflare(arguments),
        "--reference" => {
            let root = required_argument(&mut arguments, "missing bare repository")?;
            serve_reference(PathBuf::from(root), optional_port(&mut arguments)?)
        }
        _ => {
            let port = optional_port(&mut arguments)?;
            let mut host = NativeHost::open(PathBuf::from(first))?;
            http::serve(port, |request| host.handle(request))
        }
    }
}

fn required_argument(
    arguments: &mut impl Iterator<Item = String>,
    message: &'static str,
) -> io::Result<String> {
    arguments.next().ok_or_else(|| io::Error::other(message))
}

fn optional_port(arguments: &mut impl Iterator<Item = String>) -> io::Result<u16> {
    arguments
        .next()
        .map(|s| s.parse::<u16>())
        .transpose()
        .map_err(io::Error::other)
        .map(|port| port.unwrap_or(0))
}

struct CloudflareOptions {
    origin: String,
    repo: String,
    token: String,
    cold: bool,
    unregister: bool,
    root: Option<String>,
}

impl CloudflareOptions {
    fn parse(arguments: &mut impl Iterator<Item = String>) -> io::Result<Self> {
        let mut options = Self {
            origin: required_argument(arguments, "missing emulator origin")?,
            repo: "demo".to_owned(),
            token: "git-zero-local-services".to_owned(),
            cold: true,
            unregister: false,
            root: None,
        };
        options.root = loop {
            let argument = arguments.next();
            match argument.as_deref() {
                Some("--repo") => {
                    options.repo = required_argument(arguments, "missing repository ID")?;
                }
                Some("--token") => {
                    options.token = required_argument(arguments, "missing local token")?;
                }
                Some("--view-cache") => {
                    options.cold = match arguments.next().as_deref() {
                        Some("cold") => true,
                        Some("warm") => false,
                        _ => return Err(io::Error::other("view cache must be cold or warm")),
                    };
                }
                Some("--unregister") => options.unregister = true,
                _ => break argument,
            }
        };
        Ok(options)
    }
}

fn serve_cloudflare(mut arguments: impl Iterator<Item = String>) -> io::Result<()> {
    let CloudflareOptions {
        origin,
        repo,
        token,
        cold,
        unregister,
        root,
    } = CloudflareOptions::parse(&mut arguments)?;
    let services = git_zero_host::cloudflare::HttpServices::new(&origin, &repo, &token)?;
    if unregister {
        return services.unregister();
    }
    let root = root.ok_or_else(|| io::Error::other("missing local codec root"))?;
    let port = optional_port(&mut arguments)?;
    if arguments.next().is_some() {
        return Err(io::Error::other("unexpected argument"));
    }
    http::serve_with_factory(port, |native_origin| {
        services.register(native_origin)?;
        let mut host =
            match NativeHost::open_cloudflare(PathBuf::from(root), &origin, &repo, &token, cold) {
                Ok(host) => host,
                Err(error) => {
                    if let Err(release_error) = services.unregister() {
                        eprintln!("failed to release codec registration: {release_error}");
                    }
                    return Err(error);
                }
            };
        eprintln!("Git gateway: {origin}/{repo}/repo.git");
        eprintln!("Object statistics and cost estimates: {native_origin}/_object-stats");
        Ok(move |request| host.handle(request))
    })
}

fn serve_reference(root: PathBuf, port: u16) -> io::Result<()> {
    let root = root.canonicalize()?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> impl Iterator<Item = String> {
        values.iter().map(|value| (*value).to_owned())
    }

    #[test]
    fn cloudflare_unregister_needs_no_root_and_last_option_wins() {
        let options = CloudflareOptions::parse(&mut arguments(&[
            "http://127.0.0.1:8787",
            "--repo",
            "first",
            "--repo",
            "second",
            "--token",
            "first-token",
            "--token",
            "second-token",
            "--view-cache",
            "cold",
            "--view-cache",
            "warm",
            "--unregister",
        ]))
        .unwrap();
        assert!(options.unregister);
        assert!(options.root.is_none());
        assert_eq!(options.repo, "second");
        assert_eq!(options.token, "second-token");
        assert!(!options.cold);
    }

    #[test]
    fn port_is_optional_and_uses_unsigned_16_bit_range() {
        assert_eq!(optional_port(&mut arguments(&[])).unwrap(), 0);
        assert_eq!(optional_port(&mut arguments(&["0"])).unwrap(), 0);
        assert_eq!(optional_port(&mut arguments(&["65535"])).unwrap(), 65535);
        for invalid in ["-1", "65536", "not-a-port"] {
            assert!(optional_port(&mut arguments(&[invalid])).is_err());
        }
    }
}
