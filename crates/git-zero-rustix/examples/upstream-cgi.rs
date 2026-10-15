//! Independent CGI test host for unchanged, external Git HTTP fixtures.
//! Apache owns HTTP framing. This is not a production repository importer.
use git_zero::{
    Head, ObjectId, ObjectKind, RefRepository, Reference, ValidatedRepository,
    http::{Request, SelectionStorage, stream::ResponseEncoder},
};
use git_zero_rustix::{FileObject, FileRepository};
use std::{
    io::{Read, Write},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const OBJECTS: usize = 256;
const BODY_BYTES: usize = 65_536;
const OBJECT_BYTES: u64 = 8 * 1024 * 1024;

fn bounded_text(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err("fixture metadata exceeds limit".into());
    }
    Ok(String::from_utf8(bytes)?)
}

fn object_id(text: &str) -> Result<ObjectId> {
    ObjectId::from_hex(text.as_bytes()).map_err(|error| format!("object ID: {error:?}").into())
}

fn object_kind(text: &str) -> Result<ObjectKind> {
    match text {
        "blob" => Ok(ObjectKind::Blob),
        "tree" => Ok(ObjectKind::Tree),
        "commit" => Ok(ObjectKind::Commit),
        "tag" => Ok(ObjectKind::Tag),
        _ => Err("unknown fixture object kind".into()),
    }
}

fn objects(root: &Path) -> Result<Vec<FileObject>> {
    let mut result = Vec::new();
    let mut total = 0;
    for row in bounded_text(&root.join("objects.txt"))?.lines() {
        if result.len() == OBJECTS {
            return Err("fixture object count exceeds limit".into());
        }
        let (hex, kind) = row.split_once('\t').ok_or("invalid object row")?;
        let id = object_id(hex)?;
        let path = root.join("raw").join(hex);
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        let fd = rustix::fs::open(
            path.as_c_str(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let size = rustix::fs::fstat(&fd)
            .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?
            .st_size;
        total += u64::try_from(size)?;
        if total > OBJECT_BYTES {
            return Err("fixture object bytes exceed limit".into());
        }
        result.push(
            FileObject::from_fd(fd, object_kind(kind)?, id)
                .map_err(|error| format!("file object: {error:?}"))?,
        );
    }
    Ok(result)
}

fn references(text: &str) -> Result<Vec<Reference<'_>>> {
    let mut result = Vec::new();
    for row in text.lines() {
        if result.len() == 32 {
            return Err("fixture ref count exceeds limit".into());
        }
        let fields: Vec<_> = row.split('\t').collect();
        if fields.len() != 4 {
            return Err("invalid ref row".into());
        }
        result.push(Reference {
            name: fields[0],
            id: object_id(fields[1])?,
            symbolic_target: (fields[2] != "-").then_some(fields[2]),
            peeled: (fields[3] != "-")
                .then(|| object_id(fields[3]))
                .transpose()?,
        });
    }
    Ok(result)
}

fn serve(args: &[String]) -> Result<()> {
    let root = Path::new(&args[0]);
    let objects = objects(root)?;
    let head_text = bounded_text(&root.join("head.txt"))?;
    let mut lines = head_text.lines();
    let id = object_id(lines.next().ok_or("missing HEAD")?)?;
    let target = lines.next().unwrap_or("");
    let head = if id.is_zero() {
        Head::Unborn(target)
    } else if target.is_empty() {
        Head::Detached(id)
    } else {
        Head::Symbolic { target, id }
    };
    let refs_text = bounded_text(&root.join("refs.txt"))?;
    let refs = references(&refs_text)?;
    let files = FileRepository::new(id, &objects);
    let repository =
        RefRepository::new(&files, head, &refs).map_err(|error| format!("refs: {error:?}"))?;
    let mut metadata = [None; OBJECTS];
    let mut by_id = [0; OBJECTS];
    let repository = ValidatedRepository::new(
        &repository,
        &mut metadata,
        &mut by_id,
        &mut [0; 1024],
        &mut [0; 4096],
    )
    .map_err(|error| format!("validation: {error:?}"))?;
    let mut body = Vec::new();
    std::io::stdin()
        .take((BODY_BYTES + 1) as u64)
        .read_to_end(&mut body)?;
    if body.len() > BODY_BYTES {
        return Err("request body exceeds limit".into());
    }
    let request = Request {
        method: &args[1],
        path: &args[2],
        query: (!args[3].is_empty()).then_some(args[3].as_str()),
        git_protocol: (!args[4].is_empty()).then_some(args[4].as_str()),
        content_type: (!args[5].is_empty()).then_some(args[5].as_str()),
        body: &body,
    };
    let mut wants = [id; 32];
    let mut selected = [None; OBJECTS];
    let mut indices = [0; OBJECTS];
    let mut read = [0; 1024];
    let mut parse = [0; 4096];
    let mut selection = SelectionStorage {
        wants: &mut wants,
        objects: &mut selected,
        indices: &mut indices,
        read: &mut read,
        parse: &mut parse,
        graph_read_limit: OBJECT_BYTES,
    };
    let mut control = [0; 65_536];
    let mut encoder =
        ResponseEncoder::with_selection(&repository, request, &mut selection, &mut control)
            .map_err(|error| format!("HTTP preparation: {error:?}"))?;
    let response = encoder.response();
    if response.status != 200 {
        return Err(format!("unexpected CGI status {}", response.status).into());
    }
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    write!(
        output,
        "Status: 200 OK\r\nPragma: no-cache\r\nCache-Control: no-cache, max-age=0, must-revalidate\r\nContent-Type: {}\r\n\r\n",
        response.content_type
    )?;
    let mut staging = [0; 4096];
    let mut total = 0;
    loop {
        let count = encoder
            .read(&repository, &mut staging)
            .map_err(|error| format!("HTTP encoding: {error:?}"))?;
        if count == 0 {
            break;
        }
        total += count;
        if total > OBJECT_BYTES as usize {
            return Err("response exceeds limit".into());
        }
        output.write_all(&staging[..count])?;
    }
    output.flush()?;
    eprintln!(
        "git-zero-upstream-cgi: served {} {}",
        request.method, request.path
    );
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = if args.len() == 6 {
        serve(&args)
    } else {
        Err("usage: upstream-cgi FIXTURE METHOD PATH QUERY PROTOCOL CONTENT_TYPE".into())
    };
    if let Err(error) = result {
        eprintln!("upstream-cgi: {error}");
        std::process::exit(1);
    }
}
