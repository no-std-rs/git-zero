use git_zero::{
    Error, File, Snapshot, ValidatedRepository,
    client::{self, Protocol},
    http::{self, Response},
};
use git_zero_rustix::{
    HttpTransport, accept, listen_on, local_address, read_request, respond, respond_stream,
};
use std::{
    io::{self, Write as _},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
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
            let (address, options) = serve_options(&args[2..])?;
            serve(address, options)
        }
        Some("fetch") => fetch(args.get(2).ok_or(Error::InvalidRequest)?, false),
        Some("ls-remote") => fetch(args.get(2).ok_or(Error::InvalidRequest)?, true),
        _ => {
            eprintln!(
                "Usage: git-zero serve [port|IP:port] [--allow-reachable-wants] | ls-remote URL | fetch URL\nIPv6 listen addresses use [IP]:port. The optional serve grant exposes the entire visible snapshot closure. fetch writes a smart pack or a dumb loose HEAD object to stdout."
            );
            Err(Error::InvalidRequest)
        }
    }
}

fn serve_options(args: &[String]) -> git_zero::Result<(SocketAddr, http::WantOptions)> {
    let (address, granted) = match args {
        [] => ("8080", false),
        [flag] if flag == "--allow-reachable-wants" => ("8080", true),
        [address] => (address.as_str(), false),
        [address, flag] if flag == "--allow-reachable-wants" => (address.as_str(), true),
        _ => return Err(Error::InvalidRequest),
    };
    let options = if granted {
        http::WantOptions::all_visible_reachable(1024)
    } else {
        http::WantOptions::STRICT
    };
    Ok((listen_address(address)?, options))
}

fn listen_address(value: &str) -> git_zero::Result<SocketAddr> {
    if let Ok(port) = value.parse() {
        return Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)));
    }
    value.parse().map_err(|_| Error::InvalidRequest)
}

fn serve(address: SocketAddr, options: http::WantOptions) -> git_zero::Result<()> {
    let files = [File {
        name: "README.md",
        contents: b"Hello from an allocation-free in-memory Git repository.\n",
    }];
    let mut tree_storage = [0; 1024];
    let mut commit_storage = [0; 1024];
    let repository = Snapshot::new(&files, &mut tree_storage, &mut commit_storage)?;
    let mut objects = [None; 3]; // Commit, tree, and the README blob.
    let mut by_id = [0; 3];
    let repository = ValidatedRepository::new(
        &repository,
        &mut objects,
        &mut by_id,
        &mut [0; 128],
        &mut [0; 128],
    )?;
    let listener = listen_on(address)?;
    // Port 0 is assigned by the OS and reported only after the socket is bound.
    println!("http://{}/repo.git", local_address(&listener)?);
    io::stdout().flush().map_err(|_| Error::Backend)?;
    let mut request_storage = [0; 65536];
    let mut control = [0; 8192];
    let mut staging = [0; 1024];
    let mut selection = http::SelectionBuffers::<4, 3, 128, 128>::new();
    loop {
        let socket = accept(&listener)?;
        if let Err(error) = serve_connection(
            &repository,
            &socket,
            &mut request_storage,
            &mut control,
            &mut staging,
            &mut selection,
            options,
        ) {
            eprintln!("response: {error:?}");
        }
    }
}

fn serve_connection(
    repository: &ValidatedRepository<'_, '_, impl git_zero::Repository>,
    socket: &impl rustix::fd::AsFd,
    request_storage: &mut [u8],
    control: &mut [u8],
    staging: &mut [u8],
    selection: &mut http::SelectionBuffers<4, 3, 128, 128>,
    options: http::WantOptions,
) -> git_zero::Result<()> {
    let mut request = match read_request(socket, request_storage) {
        Ok(request) => request,
        Err(error) => return reject_request(socket, error),
    };
    let Some(path) = request.path.strip_prefix("/repo.git/") else {
        return respond(
            socket,
            Response {
                status: 404,
                content_type: "text/plain",
            },
            b"Repository not found\n",
        );
    };
    request.path = path;
    let mut encoder = match http::stream::ResponseEncoder::with_selection_storage_policy(
        repository,
        request,
        selection.storage(1_048_576),
        options,
        control,
    ) {
        Ok(encoder) => encoder,
        Err(error) => return reject_request(socket, error),
    };
    // Once headers start, an error closes the socket without a second response.
    respond_stream(socket, repository, &mut encoder, staging)
}

fn reject_request(socket: &impl rustix::fd::AsFd, error: Error) -> git_zero::Result<()> {
    eprintln!("request: {error:?}");
    let status = match error {
        Error::BufferTooSmall | Error::LimitExceeded => 413,
        Error::Backend => 500,
        _ => 400,
    };
    respond(
        socket,
        Response {
            status,
            content_type: "text/plain",
        },
        b"Request failed\n",
    )
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
        if discovery.protocol == Protocol::Dumb {
            match client::read_head(&mut transport, &discovery, &mut response_storage) {
                Ok(head) => println!("{head}\tHEAD"),
                Err(Error::ObjectNotFound) => {} // Empty/unborn remote.
                Err(error) => return Err(error),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_arguments_keep_the_grant_explicit_with_optional_default_address() {
        for (args, address, granted) in [
            (vec![], "127.0.0.1:8080", false),
            (vec!["--allow-reachable-wants"], "127.0.0.1:8080", true),
            (vec!["0"], "127.0.0.1:0", false),
            (vec!["[::1]:0", "--allow-reachable-wants"], "[::1]:0", true),
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            let (actual, options) = serve_options(&args).unwrap();
            assert_eq!(actual, address.parse::<SocketAddr>().unwrap());
            assert_eq!(
                options,
                if granted {
                    http::WantOptions::all_visible_reachable(1024)
                } else {
                    http::WantOptions::STRICT
                }
            );
        }
        for args in [
            vec!["--unknown"],
            vec!["0", "--unknown"],
            vec!["--allow-reachable-wants", "0"],
            vec!["0", "--allow-reachable-wants", "extra"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            assert_eq!(serve_options(&args), Err(Error::InvalidRequest));
        }
    }
}
