use git_zero::{
    Error, File, Snapshot, ValidatedRepository,
    client::{self, Protocol},
    http::{self, Response},
};
use git_zero_rustix::{
    ChunkedLimits, HttpTransport, RequestDeadlines, RequestPolicy, accept, listen_on,
    local_address, read_request_chunked, read_request_with_deadlines, respond,
    respond_published_with_deadlines, respond_stream_with_deadlines,
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
                "Usage: git-zero serve [port|IP:port] [--allow-reachable-wants] [--max-header-bytes=N] [--max-header-fields=N] [--max-header-name-bytes=N] [--max-header-value-bytes=N] [--max-wire-bytes=N] [--max-body-bytes=N] [--request-deadline-ms=N] [--idle-deadline-ms=N] [--response-deadline-ms=N] [--response-idle-deadline-ms=N] [--chunked-requests] | ls-remote URL | fetch URL\nIPv6 listen addresses use [IP]:port. The optional serve grant exposes the entire visible snapshot closure. fetch writes a smart pack or a dumb loose HEAD object to stdout."
            );
            Err(Error::InvalidRequest)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServeOptions {
    wants: http::WantOptions,
    admission: RequestPolicy,
    deadlines: RequestDeadlines,
    response_deadlines: RequestDeadlines,
    chunks: Option<ChunkedLimits>,
}

fn serve_options(args: &[String]) -> git_zero::Result<(SocketAddr, ServeOptions)> {
    let mut address = "8080";
    let mut granted = None;
    let mut options = ServeOptions {
        wants: http::WantOptions::STRICT,
        admission: RequestPolicy::default(),
        deadlines: RequestDeadlines::default(),
        response_deadlines: RequestDeadlines::default(),
        chunks: None,
    };
    for (index, argument) in args.iter().enumerate() {
        match argument.as_str() {
            "--chunked-requests" => set_once(&mut options.chunks, ChunkedLimits::default())?,
            "--allow-reachable-wants" => set_once(&mut granted, ())?,
            _ => match argument.split_once('=') {
                Some((name, value)) => serve_limit(&mut options, name, value)?,
                None if index == 0 && !argument.starts_with("--") => address = argument,
                _ => return Err(Error::InvalidRequest),
            },
        }
    }
    if granted.is_some() {
        options.wants = http::WantOptions::all_visible_reachable(1024);
    }
    Ok((listen_address(address)?, options))
}

fn serve_limit(options: &mut ServeOptions, name: &str, value: &str) -> git_zero::Result<()> {
    match name {
        "--request-deadline-ms" => set_deadline(&mut options.deadlines.total, value),
        "--idle-deadline-ms" => set_deadline(&mut options.deadlines.idle, value),
        "--response-deadline-ms" => set_deadline(&mut options.response_deadlines.total, value),
        "--response-idle-deadline-ms" => set_deadline(&mut options.response_deadlines.idle, value),
        "--max-header-bytes" => set_number(&mut options.admission.headers.max_bytes, value),
        "--max-header-fields" => set_number(&mut options.admission.headers.max_fields, value),
        "--max-header-name-bytes" => {
            set_number(&mut options.admission.headers.max_name_bytes, value)
        }
        "--max-header-value-bytes" => {
            set_number(&mut options.admission.headers.max_value_bytes, value)
        }
        "--max-wire-bytes" => set_number(&mut options.admission.request.max_wire_bytes, value),
        "--max-body-bytes" => set_number(&mut options.admission.request.max_body_bytes, value),
        _ => Err(Error::InvalidRequest),
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> git_zero::Result<()> {
    if slot.is_some() {
        return Err(Error::InvalidRequest);
    }
    *slot = Some(value);
    Ok(())
}

fn argument_number<T: core::str::FromStr>(value: &str) -> git_zero::Result<T> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::InvalidRequest);
    }
    value.parse().map_err(|_| Error::InvalidRequest)
}

fn set_number(slot: &mut Option<usize>, value: &str) -> git_zero::Result<()> {
    set_once(slot, argument_number(value)?)
}

fn set_deadline(slot: &mut Option<core::time::Duration>, value: &str) -> git_zero::Result<()> {
    let millis = argument_number(value)?;
    if millis == 0 {
        return Err(Error::InvalidRequest);
    }
    set_once(slot, core::time::Duration::from_millis(millis))
}

fn listen_address(value: &str) -> git_zero::Result<SocketAddr> {
    if let Ok(port) = value.parse() {
        return Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)));
    }
    value.parse().map_err(|_| Error::InvalidRequest)
}

fn serve(address: SocketAddr, options: ServeOptions) -> git_zero::Result<()> {
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
    let mut entries = [git_zero::pack::IndexEntry::EMPTY; 3];
    let publication = http::dumb::Publication::new(
        &repository,
        &mut entries,
        &mut [0; 1024],
        http::dumb::LooseObjects::Enabled,
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
            &publication,
            &socket,
            ConnectionBuffers {
                request: &mut request_storage,
                control: &mut control,
                staging: &mut staging,
                selection: &mut selection,
            },
            options,
        ) {
            eprintln!("response: {error:?}");
        }
    }
}

struct ConnectionBuffers<'a> {
    request: &'a mut [u8],
    control: &'a mut [u8],
    staging: &'a mut [u8],
    selection: &'a mut http::SelectionBuffers<4, 3, 128, 128>,
}

fn serve_connection<R: git_zero::Repository>(
    repository: &ValidatedRepository<'_, '_, R>,
    publication: &http::dumb::Publication<'_, ValidatedRepository<'_, '_, R>>,
    socket: &impl rustix::fd::AsFd,
    buffers: ConnectionBuffers<'_>,
    options: ServeOptions,
) -> git_zero::Result<()> {
    let ConnectionBuffers {
        request: request_storage,
        control,
        staging,
        selection,
    } = buffers;
    let intake = if let Some(chunks) = options.chunks {
        read_request_chunked(
            socket,
            request_storage,
            options.admission,
            options.deadlines,
            chunks,
        )
    } else {
        read_request_with_deadlines(
            socket,
            request_storage,
            options.admission,
            options.deadlines,
        )
    };
    let mut request = match intake {
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
    match publication.prepare(request) {
        Ok(Some(mut encoder)) => {
            return respond_published_with_deadlines(
                socket,
                &mut encoder,
                staging,
                options.response_deadlines,
            );
        }
        Ok(None) => {}
        Err(error) => return reject_request(socket, error),
    }
    let mut encoder = match http::stream::ResponseEncoder::with_selection_storage_policy(
        repository,
        request,
        selection.storage(1_048_576),
        options.wants,
        control,
    ) {
        Ok(encoder) => encoder,
        Err(error) => return reject_request(socket, error),
    };
    // Once headers start, an error closes the socket without a second response.
    respond_stream_with_deadlines(
        socket,
        repository,
        &mut encoder,
        staging,
        options.response_deadlines,
    )
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
    fn serve_header_and_request_quotas_are_explicit_exact_and_unique() {
        let args = [
            "[::1]:0",
            "--max-header-bytes=4096",
            "--max-header-fields=32",
            "--max-header-name-bytes=64",
            "--max-header-value-bytes=1024",
            "--max-wire-bytes=8192",
            "--max-body-bytes=2048",
            "--allow-reachable-wants",
        ]
        .map(str::to_owned);
        let (address, options) = serve_options(&args).unwrap();
        assert_eq!(address, "[::1]:0".parse::<SocketAddr>().unwrap());
        assert_eq!(
            options.wants,
            http::WantOptions::all_visible_reachable(1024)
        );
        assert_eq!(
            options.admission,
            RequestPolicy {
                request: git_zero_rustix::RequestLimits {
                    max_wire_bytes: Some(8192),
                    max_body_bytes: Some(2048)
                },
                headers: git_zero_rustix::HeaderLimits {
                    max_bytes: Some(4096),
                    max_fields: Some(32),
                    max_name_bytes: Some(64),
                    max_value_bytes: Some(1024)
                },
            }
        );
        for args in [
            vec!["--max-header-bytes="],
            vec!["--max-header-bytes=-1"],
            vec!["--max-header-bytes=+1"],
            vec!["--max-header-bytes= 1"],
            vec!["--max-header-bytes=184467440737095516160"],
            vec!["--max-header-bytes=1", "--max-header-bytes=2"],
            vec!["--allow-reachable-wants", "--allow-reachable-wants"],
            vec!["--max-header-fields=2", "0"],
            vec!["--max-unknown=2"],
        ] {
            assert_eq!(
                serve_options(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()),
                Err(Error::InvalidRequest)
            );
        }
        let (_, options) = serve_options(&["--max-header-fields=0".to_owned()]).unwrap();
        assert_eq!(options.admission.headers.max_fields, Some(0));
        assert_eq!(options.wants, http::WantOptions::STRICT);
        assert_eq!(
            serve_options(&[]).unwrap().1.admission,
            RequestPolicy::default()
        );
    }

    #[test]
    fn chunked_intake_requires_an_explicit_unique_flag() {
        assert_eq!(serve_options(&[]).unwrap().1.chunks, None);
        assert_eq!(
            serve_options(&["--chunked-requests".to_owned()])
                .unwrap()
                .1
                .chunks,
            Some(ChunkedLimits::default())
        );
        assert_eq!(
            serve_options(&[
                "--chunked-requests".to_owned(),
                "--chunked-requests".to_owned()
            ]),
            Err(Error::InvalidRequest)
        );
    }

    #[test]
    fn serve_deadline_flags_are_positive_unique_and_independent() {
        let args = [
            "0",
            "--request-deadline-ms=1500",
            "--idle-deadline-ms=250",
            "--response-deadline-ms=2000",
            "--response-idle-deadline-ms=300",
        ]
        .map(str::to_owned);
        let (_, options) = serve_options(&args).unwrap();
        assert_eq!(
            options.deadlines,
            RequestDeadlines {
                total: Some(core::time::Duration::from_millis(1500)),
                idle: Some(core::time::Duration::from_millis(250)),
            }
        );
        assert_eq!(
            options.response_deadlines,
            RequestDeadlines {
                total: Some(core::time::Duration::from_millis(2000)),
                idle: Some(core::time::Duration::from_millis(300)),
            }
        );
        assert_eq!(
            serve_options(&[]).unwrap().1.deadlines,
            RequestDeadlines::default()
        );
        for args in [
            vec!["--response-deadline-ms=0"],
            vec!["--response-idle-deadline-ms=0"],
            vec!["--response-deadline-ms=1", "--response-deadline-ms=2"],
            vec![
                "--response-idle-deadline-ms=1",
                "--response-idle-deadline-ms=2",
            ],
            vec!["--request-deadline-ms=0"],
            vec!["--idle-deadline-ms=0"],
            vec!["--request-deadline-ms=-1"],
            vec!["--idle-deadline-ms=+1"],
            vec!["--request-deadline-ms="],
            vec!["--idle-deadline-ms= 1"],
            vec!["--request-deadline-ms=18446744073709551616"],
            vec!["--request-deadline-ms=1", "--request-deadline-ms=2"],
            vec!["--idle-deadline-ms=1", "--idle-deadline-ms=1"],
        ] {
            assert_eq!(
                serve_options(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()),
                Err(Error::InvalidRequest)
            );
        }
    }

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
                options.wants,
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
