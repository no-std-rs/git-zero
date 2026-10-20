//! Wire interoperability only: a fixture peer validates a known three-object
//! push and reports its result. This is not a persistent repository host.
use super::{Bytes, Event, LIMITS, capabilities, read_control};
use git_zero::{
    ObjectFormat, ObjectId,
    http::receive::{self, Capabilities, Report},
    pack::{DecodeLimits, DecodeStatus, Decoder},
    zlib::{InflateLimits, WINDOW_SIZE},
};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(root: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Receive Test")
        .env("GIT_AUTHOR_EMAIL", "receive@example.invalid")
        .env("GIT_COMMITTER_NAME", "Receive Test")
        .env("GIT_COMMITTER_EMAIL", "receive@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn checked_git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let output = git(root, args, input);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn source(root: &Path, format: ObjectFormat) -> ObjectId {
    fs::create_dir(root).unwrap();
    checked_git(
        root,
        &[
            "init",
            "--bare",
            &format!("--object-format={}", format.name()),
        ],
        b"",
    );
    let blob = checked_git(
        root,
        &["hash-object", "-w", "--stdin"],
        b"our receive-pack protocol\n",
    );
    let tree = checked_git(
        root,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let commit = checked_git(root, &["commit-tree", &tree, "-m", "wire fixture"], b"");
    ObjectId::from_hex(commit.as_bytes()).unwrap()
}

struct Request {
    target: String,
    body: Vec<u8>,
}

fn request(stream: &mut BufReader<TcpStream>) -> Request {
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    let target = line.split_whitespace().nth(1).unwrap().to_owned();
    let mut length = 0;
    let mut chunked = false;
    let mut expect = false;
    loop {
        line.clear();
        stream.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap(),
            "transfer-encoding" => chunked = value.trim().eq_ignore_ascii_case("chunked"),
            "expect" => expect = value.trim().eq_ignore_ascii_case("100-continue"),
            _ => {}
        }
    }
    if expect {
        stream
            .get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .unwrap();
    }
    let body = if chunked {
        chunks(stream)
    } else {
        assert!(length < 1_048_576);
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).unwrap();
        bytes
    };
    Request { target, body }
}

fn chunks(stream: &mut BufReader<TcpStream>) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        let length = usize::from_str_radix(line.trim().split(';').next().unwrap(), 16).unwrap();
        if length == 0 {
            loop {
                line.clear();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    return bytes;
                }
            }
        }
        let end = bytes.len() + length;
        assert!(end < 1_048_576);
        let start = bytes.len();
        bytes.resize(end, 0);
        stream.read_exact(&mut bytes[start..]).unwrap();
        let mut crlf = [0; 2];
        stream.read_exact(&mut crlf).unwrap();
        assert_eq!(crlf, *b"\r\n");
    }
}

fn respond(stream: &mut TcpStream, media: &str, body: &[u8]) {
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-cache\r\n\r\n", body.len()).unwrap();
    stream.write_all(body).unwrap();
}

fn known_pack(bytes: &[u8], format: ObjectFormat, commit: ObjectId) {
    let mut window = [0; WINDOW_SIZE];
    let mut decoder = Decoder::new(
        format,
        &mut window,
        DecodeLimits {
            max_pack_bytes: 1_048_576,
            max_objects: 3,
            inflate: InflateLimits {
                max_output: 1_048_576,
                max_ratio: 4096,
                ratio_slack: 1024,
            },
        },
    );
    let mut offset = 0;
    let mut ids = Vec::new();
    loop {
        let end = (offset + 11).min(bytes.len());
        let step = decoder
            .update(&bytes[offset..end], &mut [0; 17], end == bytes.len())
            .unwrap();
        offset += step.consumed;
        match step.status {
            DecodeStatus::EntryEnd { object_id, .. } => ids.push(object_id.unwrap()),
            DecodeStatus::Finished(_) => break,
            _ => {}
        }
    }
    assert_eq!(offset, bytes.len());
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&commit));
}

fn peer(listener: TcpListener, format: ObjectFormat, commit: ObjectId) {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < deadline,
            "Git did not complete the protocol exchange"
        );
        let (stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(error) => panic!("{error}"),
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        let request = request(&mut stream);
        let mut body = Bytes(vec![]);
        if request.target == "/repo.git/info/refs?service=git-receive-pack" {
            receive::write_advertisement(format, capabilities(), [], &mut body).unwrap();
            respond(
                stream.get_mut(),
                "application/x-git-receive-pack-advertisement",
                &body.0,
            );
            continue;
        }
        assert_eq!(request.target, "/repo.git/git-receive-pack");
        if request.body == b"0000" {
            respond(
                stream.get_mut(),
                "application/x-git-receive-pack-result",
                b"",
            );
            continue;
        }
        let (events, offset, selected) =
            read_control(&request.body, format, capabilities(), LIMITS, 7).unwrap();
        assert_eq!(
            events,
            [
                Event::Command(format.zero_id(), commit, b"refs/heads/main".to_vec()),
                Event::Options,
                Event::Option(b"ci.skip".to_vec()),
                Event::Option(vec![]),
                Event::End(true)
            ]
        );
        assert!(selected.contains(Capabilities::ATOMIC));
        assert!(selected.contains(Capabilities::REPORT_STATUS_V2));
        known_pack(&request.body[offset..], format, commit);
        receive::write_progress(&mut body, selected, b"validated known fixture\n").unwrap();
        let mut report = Report::new(&mut body, selected, Ok(())).unwrap();
        report.reference(b"refs/heads/main", Ok(())).unwrap();
        report.finish().unwrap();
        respond(
            stream.get_mut(),
            "application/x-git-receive-pack-result",
            &body.0,
        );
        return;
    }
}

#[test]
fn ordinary_git_push_negotiates_sha1_sha256_atomic_options_and_chunked_http() {
    let fixture =
        Fixture(std::env::temp_dir().join(format!("git-zero-receive-wire-{}", std::process::id())));
    fs::create_dir(&fixture.0).unwrap();
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let root = fixture.0.join(format.name());
        let commit = source(&root, format);
        for version in [0, 1, 2] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
            let server = thread::spawn(move || peer(listener, format, commit));
            let output = git(
                &root,
                &[
                    "-c",
                    &format!("protocol.version={version}"),
                    "-c",
                    if version == 2 {
                        "http.postBuffer=0"
                    } else {
                        "http.postBuffer=1048576"
                    },
                    "push",
                    "--atomic",
                    "--push-option=ci.skip",
                    "--push-option=",
                    &url,
                    &format!("{commit}:refs/heads/main"),
                ],
                b"",
            );
            server.join().unwrap();
            assert!(
                output.status.success(),
                "{} v{version}: {}",
                format.name(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
