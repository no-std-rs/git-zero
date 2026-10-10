use git_zero::{
    Error, File, Snapshot,
    client::{self, Protocol},
    http::{self, Response},
    io::SliceWriter,
};
use git_zero_rustix::{HttpTransport, accept, listen, local_port, read_request, respond};
use std::{
    io::{self, Write as _},
    net::SocketAddrV4,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("git-zero: {error:?}");
        std::process::exit(1);
    }
}

fn run() -> git_zero::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let port = args.get(2).map_or(Ok(8080), |port| {
                port.parse().map_err(|_| Error::InvalidRequest)
            })?;
            serve(port)
        }
        Some("fetch") => fetch(args.get(2).ok_or(Error::InvalidRequest)?, false),
        Some("ls-remote") => fetch(args.get(2).ok_or(Error::InvalidRequest)?, true),
        _ => {
            eprintln!(
                "Usage: git-zero serve [port] | ls-remote URL | fetch URL\nfetch writes a smart pack or a dumb loose HEAD object to stdout."
            );
            Err(Error::InvalidRequest)
        }
    }
}

fn serve(port: u16) -> git_zero::Result<()> {
    let files = [File {
        name: "README.md",
        contents: b"Hello from an allocation-free in-memory Git repository.\n",
    }];
    let mut tree_storage = [0; 1024];
    let mut commit_storage = [0; 1024];
    let repository = Snapshot::new(&files, &mut tree_storage, &mut commit_storage)?;
    let listener = listen(port)?;
    // Port 0 is assigned by the OS and reported only after the socket is bound.
    println!("http://127.0.0.1:{}/repo.git", local_port(&listener)?);
    io::stdout().flush().map_err(|_| Error::Backend)?;
    let mut request_storage = [0; 65536];
    let mut response_storage = [0; 131072];
    loop {
        let socket = accept(&listener)?;
        let result = (|| {
            let mut request = read_request(&socket, &mut request_storage)?;
            let Some(path) = request.path.strip_prefix("/repo.git/") else {
                return respond(
                    &socket,
                    Response {
                        status: 404,
                        content_type: "text/plain",
                    },
                    b"Repository not found\n",
                );
            };
            request.path = path;
            let mut output = SliceWriter::new(&mut response_storage);
            let response = http::serve(&repository, request, &mut output)?;
            respond(&socket, response, output.as_bytes())
        })();
        if let Err(error) = result {
            let status = match error {
                Error::BufferTooSmall => 413,
                Error::Backend => 500,
                _ => 400,
            };
            let _ = respond(
                &socket,
                Response {
                    status,
                    content_type: "text/plain",
                },
                b"Request failed\n",
            );
            eprintln!("request: {error:?}");
        }
    }
}

fn fetch(url: &str, list_only: bool) -> git_zero::Result<()> {
    let url = url.strip_prefix("http://").ok_or(Error::Unsupported)?;
    let (authority, path) = url.split_once('/').ok_or(Error::InvalidRequest)?;
    let address: SocketAddrV4 = authority.parse().map_err(|_| Error::Unsupported)?;
    let repository_path = format!("/{path}");
    let mut wire_storage = [0; 262144];
    let mut discovery_storage = [0; 32768];
    let mut response_storage = [0; 262144];
    let mut request_storage = [0; 1024];
    let mut transport = HttpTransport {
        address,
        repository_path: &repository_path,
        wire_storage: &mut wire_storage,
    };
    let discovery = client::discover(&mut transport, &mut discovery_storage)?;
    if list_only {
        for reference in discovery.references() {
            let reference = reference?;
            println!("{}\t{}", reference.id, reference.name);
        }
        return Ok(());
    }
    let head = client::read_head(&mut transport, &discovery, &mut response_storage)?;
    let bytes = match discovery.protocol {
        Protocol::Smart => client::fetch_pack(
            &mut transport,
            head,
            &mut request_storage,
            &mut response_storage,
        )?,
        Protocol::Dumb => client::fetch_loose(&mut transport, head, &mut response_storage)?,
    };
    io::stdout().write_all(bytes).map_err(|_| Error::Backend)
}
