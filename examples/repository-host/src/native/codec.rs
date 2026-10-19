//! Git subprocess codec, protocol framing and disposable repository layout.
use crate::http::{Request, Response};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Service {
    Receive,
    Upload,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Self::Receive => "git-receive-pack",
            Self::Upload => "git-upload-pack",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Route {
    pub(super) service: Option<Service>,
    pub(super) advertise: bool,
    pub(super) push: bool,
}

impl Route {
    pub(super) fn new(request: &Request) -> Self {
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        let requested_service = first_service(query);
        let advertise = request.method == "GET" && path == "/repo.git/info/refs";
        let service = match (request.method.as_str(), path) {
            ("POST", "/repo.git/git-receive-pack") => Some(Service::Receive),
            ("POST", "/repo.git/git-upload-pack") => Some(Service::Upload),
            ("GET", "/repo.git/info/refs") => match requested_service {
                Some("git-receive-pack") => Some(Service::Receive),
                Some("git-upload-pack") => Some(Service::Upload),
                _ => None,
            },
            _ => None,
        };
        let push = (path == "/repo.git/info/refs" && requested_service == Some("git-receive-pack"))
            || (service == Some(Service::Receive) && !advertise);
        Self {
            service,
            advertise,
            push,
        }
    }
}

fn first_service(query: &str) -> Option<&str> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == "service").then_some(value))
}

pub fn command(mut command: Command, input: &[u8]) -> io::Result<Output> {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no child input"))?;
    let result = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output();
        let written = writer
            .join()
            .map_err(|_| io::Error::other("child input thread failed"))?;
        // A rejected Git request can close stdin early. Its exit/status is the
        // useful diagnostic in that case.
        if let Ok(ref output) = output {
            if output.status.success() {
                written?;
            }
        }
        output
    })?;
    if !result.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&result.stderr).into_owned(),
        ));
    }
    Ok(result)
}

pub(super) fn init(repo: &Path) -> io::Result<()> {
    // This is a disposable Git codec view, not the authoritative ref store.
    // Its fixed bare layout needs no hooks, templates or per-push Git process.
    for path in ["objects/pack", "objects/info", "refs"] {
        fs::create_dir_all(repo.join(path))?;
    }
    fs::write(repo.join("HEAD"), b"ref: refs/heads/main\n")?;
    // This disposable codec view has a fixed policy. Write it once instead of
    // spawning a config process for every setting on every quarantined push.
    // Quarantine refs and this view are disposable. Authoritative blob import
    // syncs validated pack/index bytes, then manifest CAS syncs the publication.
    // Do not pay for a second durability transaction on temporary codec refs.
    // Existing compressed entries/deltas are reused; a download never starts
    // a fresh delta search proportional to the size of the requested history.
    fs::write(repo.join("config"), b"[core]\nrepositoryformatversion = 0\nbare = true\nfsync = none\n[http]\nreceivepack = true\n[receive]\nunpackLimit = 0\nfsckObjects = true\ndenyNonFastForwards = true\ndenyDeleteCurrent = ignore\nautogc = false\n[gc]\nauto = 0\n[pack]\nthreads = 0\nwindow = 0\nuseBitmaps = true\n")?;
    Ok(())
}

pub fn refs(repo: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    match fs::read_to_string(repo.join("packed-refs")) {
        Ok(packed) => {
            for line in packed
                .lines()
                .filter(|line| !line.starts_with(['#', '^']) && !line.is_empty())
            {
                let (id, name) = line
                    .split_once(' ')
                    .ok_or_else(|| io::Error::other("invalid packed ref"))?;
                validate_ref(name, id)?;
                result.insert(name.to_string(), id.to_string());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut directories = vec![repo.join("refs")];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(path);
                continue;
            }
            if entry.file_name().to_string_lossy().ends_with(".lock") {
                continue;
            }
            if !kind.is_file() {
                return Err(io::Error::other("nonregular ref"));
            }
            let name = path
                .strip_prefix(repo)
                .map_err(io::Error::other)?
                .to_str()
                .ok_or_else(|| io::Error::other("non UTF-8 ref"))?;
            let id = fs::read_to_string(&path)?;
            let id = id.trim();
            validate_ref(name, id)?;
            result.insert(name.to_string(), id.to_string());
        }
    }
    Ok(result)
}

fn validate_ref(name: &str, id: &str) -> io::Result<()> {
    git_zero_core::repository::validate_ref_name(name)
        .map_err(|_| io::Error::other("invalid ref name"))?;
    if id.len() != 40
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || id.bytes().all(|byte| byte == b'0')
    {
        return Err(io::Error::other("invalid ref object ID"));
    }
    Ok(())
}

pub(super) fn write_refs(repo: &Path, wanted: &BTreeMap<String, String>) -> io::Result<()> {
    let mut packed = String::from("# pack-refs with: sorted\n");
    for (name, id) in wanted {
        validate_ref(name, id)?;
        packed += &format!("{id} {name}\n");
    }
    // Requests are serialized. These derived files need no durability barrier:
    // the manifest was already synced and reconstructs the complete snapshot.
    fs::write(repo.join("packed-refs.next"), packed)?;
    fs::rename(repo.join("packed-refs.next"), repo.join("packed-refs"))?;
    fs::remove_dir_all(repo.join("refs"))?;
    fs::create_dir(repo.join("refs"))?;
    let head = ["refs/heads/main", "refs/heads/master"]
        .into_iter()
        .find(|name| wanted.contains_key(*name))
        .or_else(|| {
            wanted
                .keys()
                .find(|name| name.starts_with("refs/heads/"))
                .map(String::as_str)
        })
        .unwrap_or("refs/heads/main");
    fs::write(repo.join("HEAD"), format!("ref: {head}\n"))?;
    Ok(())
}

pub fn http_backend(repo: &Path, request: &Request) -> io::Result<Response> {
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    if ![
        "/repo.git/info/refs",
        "/repo.git/git-upload-pack",
        "/repo.git/git-receive-pack",
    ]
    .contains(&path)
    {
        return Ok(Response::error(404, "unknown route"));
    }
    let parent = repo
        .parent()
        .ok_or_else(|| io::Error::other("invalid repository path"))?;
    let mut process = Command::new("git");
    process
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", parent)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REQUEST_METHOD", &request.method)
        .env("REMOTE_USER", "local-benchmark")
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env(
            "CONTENT_TYPE",
            request
                .headers
                .get("content-type")
                .map(String::as_str)
                .unwrap_or(""),
        )
        .env("REMOTE_ADDR", "127.0.0.1");
    for (header, variable) in [
        ("git-protocol", "HTTP_GIT_PROTOCOL"),
        ("content-encoding", "HTTP_CONTENT_ENCODING"),
    ] {
        process.env(
            variable,
            request
                .headers
                .get(header)
                .map(String::as_str)
                .unwrap_or(""),
        );
    }
    let output = command(process, &request.body)?.stdout;
    let (boundary, skip) = output
        .windows(4)
        .position(|x| x == b"\r\n\r\n")
        .map(|p| (p, 4))
        .or_else(|| output.windows(2).position(|x| x == b"\n\n").map(|p| (p, 2)))
        .ok_or_else(|| io::Error::other("invalid CGI output"))?;
    let mut response = Response {
        status: 200,
        headers: BTreeMap::new(),
        body: output[boundary + skip..].into(),
    };
    for line in String::from_utf8_lossy(&output[..boundary]).lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid CGI header"))?;
        if name.eq_ignore_ascii_case("status") {
            response.status = value
                .split_whitespace()
                .next()
                .unwrap_or("500")
                .parse()
                .map_err(io::Error::other)?;
        } else if !name.eq_ignore_ascii_case("cache-control") {
            response
                .headers
                .insert(name.to_string(), value.trim().to_string());
        }
    }
    Ok(response)
}

pub(super) fn git_rpc(repo: &Path, request: &Request, route: Route) -> io::Result<Response> {
    let Some(service) = route.service else {
        return if route.advertise {
            http_backend(repo, request)
        } else {
            Ok(Response::error(404, "unknown route"))
        };
    };
    let service = service.name();
    let advertise = route.advertise;
    // Preserve the reference backend's HTTP validation and gzip decoding for
    // encoded requests; the common identity path needs only one Git process.
    if request
        .headers
        .get("content-encoding")
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
        || (!advertise
            && request.headers.get("content-type").map(String::as_str)
                != Some(&format!("application/x-{service}-request")))
    {
        return http_backend(repo, request);
    }
    let mut process = Command::new("git");
    if service == "git-upload-pack" && !advertise && has_haves(&request.body) {
        // A one-entry search creates useful thin deltas against client bases
        // even when incoming self-contained packs stored those objects whole.
        // Full downloads still reuse streams without a fresh delta search.
        process.args(["-c", "pack.window=1"]);
    }
    process
        .arg(service.trim_start_matches("git-"))
        .arg("--stateless-rpc");
    if advertise {
        process.arg("--advertise-refs");
    }
    process.arg(repo).env(
        "GIT_PROTOCOL",
        request
            .headers
            .get("git-protocol")
            .map(String::as_str)
            .unwrap_or(""),
    );
    let mut body = command(process, &request.body)?.stdout;
    if advertise {
        let payload = format!("# service={service}\n");
        let mut prefix = format!("{:04x}{payload}0000", payload.len() + 4).into_bytes();
        prefix.extend_from_slice(&body);
        body = prefix;
    }
    Ok(Response {
        status: 200,
        headers: BTreeMap::from([(
            "Content-Type".into(),
            format!(
                "application/x-{service}-{}",
                if advertise { "advertisement" } else { "result" }
            ),
        )]),
        body: body.into(),
    })
}

fn has_haves(mut body: &[u8]) -> bool {
    use git_zero_core::pktline::{Packet, read};
    while !body.is_empty() {
        // Protocol v2 adds delimiter/response-end packets to v0 framing.
        if body.starts_with(b"0001") || body.starts_with(b"0002") {
            body = &body[4..];
            continue;
        }
        match read(&mut body) {
            Ok(Some(Packet::Data(data))) => {
                if data.strip_prefix(b"have ").is_some_and(|id| {
                    let id = id.strip_suffix(b"\n").unwrap_or(id);
                    id.len() == 40 && id.iter().all(u8::is_ascii_hexdigit)
                }) {
                    return true;
                }
            }
            Ok(Some(Packet::Flush)) => {}
            _ => return false,
        }
    }
    false
}

pub(super) fn incoming_pack(mut body: &[u8]) -> Option<&[u8]> {
    use git_zero_core::pktline::{Packet, read};
    loop {
        match read(&mut body).ok()?? {
            Packet::Data(_) => {}
            Packet::Flush => return body.starts_with(b"PACK").then_some(body),
        }
    }
}

pub(super) fn receive_advertisement(refs: &BTreeMap<String, String>) -> io::Result<Response> {
    fn packet(output: &mut Vec<u8>, text: &str) -> io::Result<()> {
        if text.len() + 4 > 65520 {
            return Err(io::Error::other("ref advertisement packet too long"));
        }
        write!(output, "{:04x}{text}", text.len() + 4)
    }
    let mut body = Vec::new();
    packet(&mut body, "# service=git-receive-pack\n")?;
    body.extend_from_slice(b"0000");
    let capabilities = "report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta object-format=sha1 agent=git-zero-host/0.1";
    if refs.is_empty() {
        packet(
            &mut body,
            &format!("{} capabilities^{{}}\0{capabilities}\n", "0".repeat(40)),
        )?;
    } else {
        for (index, (name, id)) in refs.iter().enumerate() {
            if index == 0 {
                packet(&mut body, &format!("{id} {name}\0{capabilities}\n"))?;
            } else {
                packet(&mut body, &format!("{id} {name}\n"))?;
            }
        }
    }
    body.extend_from_slice(b"0000");
    Ok(Response {
        status: 200,
        headers: BTreeMap::from([(
            "Content-Type".into(),
            "application/x-git-receive-pack-advertisement".into(),
        )]),
        body: body.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_haves_use_packet_boundaries_in_both_protocols() {
        let id = "a".repeat(40);
        let have = format!("have {id}\n");
        let packet = format!("{:04x}{have}", have.len() + 4);
        assert!(has_haves(format!("0000{packet}0000").as_bytes()));
        assert!(has_haves(format!("0001{packet}0002").as_bytes()));
        assert!(!has_haves(format!("have {id}\n").as_bytes()));
        assert!(!has_haves(
            format!("{:04x}x{have}", have.len() + 5).as_bytes()
        ));
        assert!(!has_haves(b"0009have "));
        assert!(!has_haves(&packet.as_bytes()[..packet.len() - 1]));
    }
}
