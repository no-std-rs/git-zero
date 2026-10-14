use git_zero::{
    Error, ObjectId, ObjectInfo, ObjectKind, Repository, ValidatedRepository,
    http::{self, Request, Response},
    io::Write as CoreWrite,
    pack,
};
use git_zero_rustix::{
    FileObject, FileRepository, accept, listen, local_port, read_request, respond, respond_headers,
};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Instant,
};

mod file_transfer;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Usage(libc::rusage);

impl Usage {
    fn read(who: libc::c_int) -> Result<Self> {
        let mut value = std::mem::MaybeUninit::uninit();
        // SAFETY: getrusage initializes the complete struct on success. The
        // pointer is valid, correctly aligned and exclusively borrowed.
        if unsafe { libc::getrusage(who, value.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(unsafe { value.assume_init() }))
    }

    fn cpu(&self) -> f64 {
        let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        seconds(self.0.ru_utime) + seconds(self.0.ru_stime)
    }
}

fn resident_memory() -> Result<(u64, u64)> {
    // /proc measures the post-exec address space, avoiding the launcher's
    // inherited ru_maxrss floor. Values are KiB on Linux.
    let status = std::fs::read_to_string("/proc/self/status")?;
    let field = |name: &str| -> Result<u64> {
        Ok(status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .ok_or("missing resident memory counter")?
            .split_ascii_whitespace()
            .next()
            .ok_or("empty resident memory counter")?
            .parse()?)
    };
    Ok((field("VmRSS:")?, field("VmHWM:")?))
}

struct Body {
    bytes: Vec<u8>,
    limit: usize,
}

impl CoreWrite for Body {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(Error::BufferTooSmall);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

// Every exit path kills/reaps an unfinished CGI child, including pipe errors.
struct CgiChild(Child);
impl Drop for CgiChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn git_response(
    root: &Path,
    backend: &str,
    request: Request<'_>,
    body: &mut Body,
) -> Result<Response> {
    let mut child = CgiChild(
        Command::new(backend)
            .env("GIT_PROJECT_ROOT", root)
            .env("GIT_HTTP_EXPORT_ALL", "1")
            .env("PATH_INFO", format!("/repo.git/{}", request.path))
            .env("QUERY_STRING", request.query.unwrap_or(""))
            .env("REQUEST_METHOD", request.method)
            .env("CONTENT_TYPE", "application/x-git-upload-pack-request")
            .env("CONTENT_LENGTH", request.body.len().to_string())
            .env("REMOTE_ADDR", "127.0.0.1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    child
        .0
        .stdin
        .take()
        .ok_or("missing CGI stdin")?
        .write_all(request.body)?;
    child
        .0
        .stdout
        .take()
        .ok_or("missing CGI stdout")?
        .take(
            body.limit
                .checked_add(8193)
                .ok_or("response limit overflow")? as u64,
        )
        .read_to_end(&mut body.bytes)?;
    if body.bytes.len() > body.limit + 8192 {
        return Err("CGI output exceeds response budget".into());
    }
    if !child.0.wait()?.success() {
        return Err("git-http-backend failed".into());
    }
    let (response, offset) = cgi_headers(&body.bytes)?;
    body.bytes.drain(..offset);
    if body.bytes.len() > body.limit {
        return Err("CGI response exceeds response budget".into());
    }
    Ok(response)
}

fn cgi_headers(bytes: &[u8]) -> Result<(Response, usize)> {
    let end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .ok_or("missing CGI headers")?;
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    for line in std::str::from_utf8(&bytes[..end])?.split("\r\n") {
        let (name, value) = line.split_once(':').ok_or("invalid CGI header")?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("status") {
            response.status = value
                .split_ascii_whitespace()
                .next()
                .ok_or("empty CGI status")?
                .parse()?;
        } else if name.eq_ignore_ascii_case("content-type") {
            response.content_type = match value {
                "application/x-git-upload-pack-advertisement" => {
                    "application/x-git-upload-pack-advertisement"
                }
                "application/x-git-upload-pack-result" => "application/x-git-upload-pack-result",
                "application/x-git-loose-object" => "application/x-git-loose-object",
                "application/x-git-packed-objects" => "application/x-git-packed-objects",
                "text/plain" | "text/plain; charset=utf-8" => "text/plain; charset=utf-8",
                _ => return Err("unexpected CGI content type".into()),
            };
        }
    }
    Ok((response, end + 4))
}

fn open_file(path: &Path) -> rustix::io::Result<rustix::fd::OwnedFd> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| rustix::io::Errno::INVAL)?;
    rustix::fs::open(
        path.as_c_str(),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
}

fn git_static_response(
    root: &Path,
    request: Request<'_>,
) -> Result<(Response, Option<rustix::fd::OwnedFd>)> {
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    if request.method != "GET" {
        response.status = 405;
        return Ok((response, None));
    }
    let path = request.path;
    let known = matches!(
        path,
        "HEAD" | "info/refs" | "objects/info/packs" | "refs/heads/main"
    );
    let object = path.strip_prefix("objects/").is_some_and(|hex| {
        let bytes = hex.as_bytes();
        bytes.len() == 41
            && bytes[2] == b'/'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 2 || b.is_ascii_hexdigit())
    });
    if !known && !object {
        response.status = 404;
        return Ok((response, None));
    }
    // Dumb HTTP has no Git server process: serve Git's existing loose files
    // using this host, rather than charging Git a CGI launch for every object.
    let file = match open_file(&root.join("repo.git").join(path)) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) => {
            response.status = 404;
            return Ok((response, None));
        }
        Err(error) => return Err(format!("{error:?}").into()),
    };
    if object {
        response.content_type = "application/x-git-loose-object";
    }
    Ok((response, Some(file)))
}

fn cache_loose(
    root: &Path,
    repository: &impl Repository,
    scratch: &mut [u8],
    limit: usize,
) -> Result<PathBuf> {
    let directory = root.join("zero-loose");
    std::fs::create_dir(&directory)?;
    let mut body = Body {
        bytes: Vec::new(),
        limit,
    };
    for index in 0..repository.object_count() {
        body.bytes.clear();
        pack::write_loose_with_scratch(repository, index, scratch, &mut body)
            .map_err(|error| format!("{error:?}"))?;
        let id = repository
            .object_id(index)
            .ok_or("missing cached object ID")?;
        std::fs::write(directory.join(id.to_string()), &body.bytes)?;
    }
    Ok(directory)
}

fn cached_loose_response(
    directory: &Path,
    request: Request<'_>,
    repository: &impl Repository,
) -> Result<Option<rustix::fd::OwnedFd>> {
    let Some(path) = request.path.strip_prefix("objects/") else {
        return Ok(None);
    };
    let path = path.as_bytes();
    if request.method != "GET" || path.len() != 41 || path[2] != b'/' {
        return Ok(None);
    }
    let mut hex = [0; 40];
    hex[..2].copy_from_slice(&path[..2]);
    hex[2..].copy_from_slice(&path[3..]);
    let Ok(id) = ObjectId::from_hex(&hex) else {
        return Ok(None);
    };
    if repository.find_index(id).is_none() {
        return Ok(None);
    }
    Ok(Some(
        open_file(&directory.join(id.to_string())).map_err(|error| format!("{error:?}"))?,
    ))
}

fn objects(root: &Path) -> Result<(ObjectId, Vec<FileObject>)> {
    let manifest = std::fs::read_to_string(root.join("objects.tsv"))?;
    let mut lines = manifest.lines();
    let head = ObjectId::from_hex(lines.next().ok_or("missing manifest HEAD")?.as_bytes())
        .map_err(|e| format!("{e:?}"))?;
    let path = std::ffi::CString::new(root.join("objects.raw").as_os_str().as_encoded_bytes())?;
    let data = rustix::fs::open(
        path.as_c_str(),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| format!("{e:?}"))?;
    let mut objects = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 {
            return Err("invalid manifest row".into());
        }
        let id = ObjectId::from_hex(fields[0].as_bytes()).map_err(|e| format!("{e:?}"))?;
        let kind = match fields[1] {
            "commit" => ObjectKind::Commit,
            "tree" => ObjectKind::Tree,
            "blob" => ObjectKind::Blob,
            _ => return Err("unsupported manifest object kind".into()),
        };
        let info = ObjectInfo {
            kind,
            size: fields[3].parse()?,
        };
        objects.push(
            FileObject::from_range(
                rustix::io::fcntl_dupfd_cloexec(&data, 0).map_err(|e| format!("{e:?}"))?,
                fields[2].parse()?,
                info,
                id,
            )
            .map_err(|e| format!("{e:?}"))?,
        );
    }
    Ok((head, objects))
}

pub fn run() -> Result<()> {
    let started = Instant::now();
    let initial = Usage::read(libc::RUSAGE_SELF)?;
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 9 {
        return Err("usage: benchmark-server FIXTURE GIT_HTTP_BACKEND git|zero|both smart|dumb METRICS_JSONL RESPONSE_LIMIT auto|buffered generated|cached".into());
    }
    let root = Path::new(&args[1]);
    let enabled = args[3].as_str();
    if !matches!(enabled, "git" | "zero" | "both")
        || !matches!(args[4].as_str(), "smart" | "dumb")
        || !matches!(args[7].as_str(), "auto" | "buffered")
        || !matches!(args[8].as_str(), "generated" | "cached")
    {
        return Err("invalid backend or protocol".into());
    }
    let limit: usize = args[6].parse()?;
    // SAFETY: SIG_IGN is a valid signal disposition. This dedicated host owns
    // its process policy; sendfile lacks MSG_NOSIGNAL. Broken sockets must
    // report EPIPE rather than terminate the host during transfer or fallback.
    if unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) } == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error().into());
    }
    // Backend-only processes keep unused Rust repository state out of Git's RSS.
    let (head, object_files) = if enabled == "git" {
        (ObjectId::Sha1([0; 20]), Vec::new())
    } else {
        objects(root)?
    };
    let raw = FileRepository::new(head, &object_files);
    let mut metadata = vec![None; object_files.len()];
    let mut indexes = vec![0; object_files.len()];
    let repository = if enabled == "git" {
        None
    } else {
        Some(
            ValidatedRepository::new(
                &raw,
                &mut metadata,
                &mut indexes,
                &mut [0; 65536],
                &mut [0; 4096],
            )
            .map_err(|e| format!("{e:?}"))?,
        )
    };
    let mut metrics = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&args[5])?;
    let mut request_storage = vec![0; 65536];
    let mut scratch = vec![0; 65536];
    let cache = if args[8] == "cached" && args[4] == "dumb" {
        repository
            .as_ref()
            .map(|repository| {
                cache_loose(
                    Path::new(&args[5])
                        .parent()
                        .ok_or("missing metrics parent")?,
                    repository,
                    &mut scratch,
                    limit,
                )
            })
            .transpose()?
    } else {
        None
    };
    let socket = listen(0).map_err(|e| format!("{e:?}"))?;
    let (rss, peak) = resident_memory()?;
    writeln!(
        metrics,
        "{{\"event\":\"ready\",\"pid\":{},\"startup_wall_s\":{},\"startup_cpu_s\":{},\"frontend_rss_kib\":{rss},\"frontend_peak_rss_kib\":{peak}}}",
        std::process::id(),
        started.elapsed().as_secs_f64(),
        Usage::read(libc::RUSAGE_SELF)?.cpu() - initial.cpu()
    )?;
    println!(
        "http://127.0.0.1:{}",
        local_port(&socket).map_err(|e| format!("{e:?}"))?
    );
    std::io::stdout().flush()?;
    loop {
        let client = accept(&socket).map_err(|e| format!("{e:?}"))?;
        let self_before = Usage::read(libc::RUSAGE_SELF)?;
        let children_before = Usage::read(libc::RUSAGE_CHILDREN)?;
        let start = Instant::now();
        let request = read_request(&client, &mut request_storage).map_err(|e| format!("{e:?}"))?;
        // A control barrier lets the runner read complete metric records after
        // a Git client returns before the final server bookkeeping finishes.
        if request.method == "GET" && request.path == "/__barrier" {
            respond(
                &client,
                Response {
                    status: 204,
                    content_type: "text/plain",
                },
                b"",
            )
            .map_err(|e| format!("{e:?}"))?;
            continue;
        }
        let (backend, path) = if let Some(path) = request.path.strip_prefix("/git/repo.git/") {
            ("git", path)
        } else if let Some(path) = request.path.strip_prefix("/zero/repo.git/") {
            ("zero", path)
        } else {
            return Err("unexpected benchmark route".into());
        };
        if enabled != "both" && enabled != backend {
            return Err("backend disabled in this process".into());
        }
        // A dumb-only host serves static info/refs even when the client probes
        // with a smart service query. Apply the same policy to both backends.
        let request = Request {
            path,
            query: if args[4] == "dumb" {
                None
            } else {
                request.query
            },
            ..request
        };
        let mut body = Body {
            bytes: Vec::new(),
            limit,
        };
        let mut file = None;
        let response = if backend == "git" && args[4] == "dumb" {
            let (response, source) = git_static_response(root, request)?;
            file = source;
            response
        } else if backend == "git" {
            git_response(root, &args[2], request, &mut body)?
        } else {
            let repository = repository.as_ref().ok_or("missing Rust repository")?;
            file = cache
                .as_ref()
                .map(|directory| cached_loose_response(directory, request, repository))
                .transpose()?
                .flatten();
            if file.is_some() {
                Response {
                    status: 200,
                    content_type: "application/x-git-loose-object",
                }
            } else {
                http::serve_with_scratch(repository, request, &mut scratch, &mut body)
                    .map_err(|e| format!("{e:?}"))?
            }
        };
        let (response_bytes, counts) = if let Some(file) = file {
            let length = rustix::fs::fstat(&file)
                .map_err(|e| format!("{e:?}"))?
                .st_size;
            let length: u64 = length.try_into()?;
            if length > limit as u64 {
                return Err("static response exceeds response budget".into());
            }
            respond_headers(&client, response, length).map_err(|e| format!("{e:?}"))?;
            let counts =
                file_transfer::send(&client, &file, 0, length, &mut scratch, args[7] == "auto")
                    .map_err(|e| format!("{e:?}"))?;
            (length, counts)
        } else {
            respond(&client, response, &body.bytes).map_err(|e| format!("{e:?}"))?;
            (body.bytes.len() as u64, file_transfer::Counts::default())
        };
        let wall = start.elapsed().as_secs_f64();
        let self_after = Usage::read(libc::RUSAGE_SELF)?;
        let children_after = Usage::read(libc::RUSAGE_CHILDREN)?;
        let (rss, peak) = resident_memory()?;
        // Request wall/CPU include read, dispatch, CGI children and response
        // writes. Sampling/reporting runs afterwards, outside these intervals.
        writeln!(
            metrics,
            "{{\"event\":\"request\",\"backend\":\"{backend}\",\"status\":{},\"response_bytes\":{},\"sendfile_bytes\":{},\"buffered_file_bytes\":{},\"wall_s\":{wall},\"frontend_cpu_s\":{},\"backend_children_cpu_s\":{},\"frontend_rss_kib\":{rss},\"frontend_peak_rss_kib\":{peak},\"backend_child_peak_rss_kib\":{},\"minor_faults\":{},\"major_faults\":{}}}",
            response.status,
            response_bytes,
            counts.sendfile_bytes,
            counts.buffered_file_bytes,
            self_after.cpu() - self_before.cpu(),
            children_after.cpu() - children_before.cpu(),
            children_after.0.ru_maxrss,
            self_after.0.ru_minflt - self_before.0.ru_minflt + children_after.0.ru_minflt
                - children_before.0.ru_minflt,
            self_after.0.ru_majflt - self_before.0.ru_majflt + children_after.0.ru_majflt
                - children_before.0.ru_majflt
        )?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cgi_status_and_binary_body_are_separate() {
        let bytes = b"Status: 404 Not Found\r\nContent-Type: text/plain\r\n\r\n\x00\xff";
        let (response, offset) = cgi_headers(bytes).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(&bytes[offset..], b"\x00\xff");
        assert!(cgi_headers(b"Content-Type: arbitrary\r\n\r\n").is_err());
        assert!(cgi_headers(b"incomplete\r\n").is_err());
    }
}
