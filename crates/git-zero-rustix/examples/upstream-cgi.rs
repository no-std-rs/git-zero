//! Independent CGI test host for unchanged, external Git HTTP fixtures.
//! Apache owns HTTP framing. This is not a production repository importer.
use git_zero::{
    RefRepository, Repository, ValidatedRepository,
    http::{Request, SelectionStorage, WantOptions, stream::ResponseEncoder},
};
use git_zero_rustix::FileRepository;
use std::{io::Write, path::Path};

#[path = "support/fixture.rs"]
mod fixture;
use fixture::{Fixture, OBJECT_BYTES, OBJECTS, Result, core_result};

const BODY_BYTES: u32 = 65_536;

fn request<'a>(args: &'a [String], body: &'a [u8]) -> Request<'a> {
    Request {
        method: &args[1],
        path: &args[2],
        query: (!args[3].is_empty()).then_some(args[3].as_str()),
        git_protocol: (!args[4].is_empty()).then_some(args[4].as_str()),
        content_type: (!args[5].is_empty()).then_some(args[5].as_str()),
        body,
    }
}

fn transmit(
    repository: &impl Repository,
    encoder: &mut ResponseEncoder<'_>,
    request: Request<'_>,
) -> Result<()> {
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
        let count = core_result(encoder.read(repository, &mut staging), "HTTP encoding")?;
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

fn serve(args: &[String]) -> Result<()> {
    let fixture = Fixture::load(Path::new(&args[0]))?;
    let (id, head) = fixture.head()?;
    let refs = fixture.references()?;
    let files = FileRepository::new(id, fixture.objects());
    let repository = core_result(RefRepository::new(&files, head, &refs), "refs")?;
    let mut metadata = [None; OBJECTS];
    let mut by_id = [0; OBJECTS];
    let repository = core_result(
        ValidatedRepository::new(
            &repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 1024],
            &mut [0; 4096],
        ),
        "validation",
    )?;
    let body = fixture::bounded_bytes(std::io::stdin(), BODY_BYTES, "request body exceeds limit")?;
    let request = request(args, &body);
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
    let options = if args.len() == 7 {
        WantOptions::all_visible_reachable(1024)
    } else {
        WantOptions::STRICT
    };
    let mut encoder = core_result(
        ResponseEncoder::with_selection_policy(
            &repository,
            request,
            &mut selection,
            options,
            &mut control,
        ),
        "HTTP preparation",
    )?;
    transmit(&repository, &mut encoder, request)
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = if args.len() == 6 || (args.len() == 7 && args[6] == "--allow-reachable-wants") {
        serve(&args)
    } else {
        Err("usage: upstream-cgi FIXTURE METHOD PATH QUERY PROTOCOL CONTENT_TYPE [--allow-reachable-wants]".into())
    };
    if let Err(error) = result {
        eprintln!("upstream-cgi: {error}");
        std::process::exit(1);
    }
}
