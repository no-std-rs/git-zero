//! A bounded raw-export host with an immutable verified pack/index generation.
//! Process setup uses std; library encoding, verification and responses do not.
#[path = "support/fixture.rs"]
mod fixture;
use git_zero::{
    Error, RefRepository, RevisionLimits, ValidatedRepository,
    http::{self, dumb::LooseObjects},
    io::Write,
    pack::{self, IndexEntry},
};
use git_zero_rustix::{
    FilePublication, FileRepository, accept, listen, local_port, read_request, respond,
    respond_file_published, respond_stream,
};
use std::{ffi::CString, io::Write as _, path::Path};

struct Output(std::fs::File);
impl Write for Output {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        std::io::Write::write_all(&mut self.0, bytes).map_err(|_| Error::Backend)
    }
}
fn file(path: &Path) -> fixture::Result<rustix::fd::OwnedFd> {
    let path = CString::new(path.as_os_str().as_encoded_bytes())?;
    Ok(rustix::fs::open(
        path.as_c_str(),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| format!("open publication: {error:?}"))?)
}
fn create(path: &Path) -> fixture::Result<Output> {
    Ok(Output(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?,
    ))
}
fn run() -> fixture::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("Usage: packed-dumb RAW_EXPORT NEW_PUBLICATION_DIRECTORY".into());
    }
    let fixture = fixture::Fixture::load(Path::new(&args[1]))?;
    let generation = Path::new(&args[2]);
    // Refuse an existing namespace; failed setup may leave files for diagnosis.
    // No URL/listener is published until every file is complete and verified.
    std::fs::create_dir(generation)?;
    let (id, head) = fixture.head()?;
    let refs = fixture.references()?;
    let files = FileRepository::new(id, fixture.objects());
    let source = fixture::core_result(RefRepository::new(&files, head, &refs), "refs")?;
    let mut objects = [None; fixture::OBJECTS];
    let mut by_id = [0; fixture::OBJECTS];
    let repository = fixture::core_result(
        ValidatedRepository::new(
            &source,
            &mut objects,
            &mut by_id,
            &mut [0; 1024],
            &mut [0; 1024],
        ),
        "graph",
    )?;
    let pack_path = generation.join("complete.pack");
    let index_path = generation.join("complete.idx");
    let mut pack_output = create(&pack_path)?;
    let mut index_output = create(&index_path)?;
    let mut entries = [IndexEntry::EMPTY; fixture::OBJECTS];
    let checksum = fixture::core_result(
        pack::write_indexed_with_scratch(
            &repository,
            None,
            &mut entries,
            &mut [0; 4096],
            &mut pack_output,
            &mut index_output,
        ),
        "pack/index encoding",
    )?;
    pack_output.0.sync_all()?;
    index_output.0.sync_all()?;
    drop(pack_output);
    drop(index_output);
    let named_pack = generation.join(format!("pack-{checksum}.pack"));
    let named_index = generation.join(format!("pack-{checksum}.idx"));
    std::fs::rename(&pack_path, &named_pack)?;
    std::fs::rename(&index_path, &named_index)?;
    // This example's private directory is not served by a filesystem web server.
    // It becomes visible only through the verified immutable library publication.
    let publication = fixture::core_result(
        FilePublication::from_fds(
            &repository,
            &mut entries,
            file(&named_pack)?,
            file(&named_index)?,
            &mut [0; 4096],
            &mut [0; 4096],
            LooseObjects::Disabled,
        ),
        "publication verification",
    )?;
    let listener = fixture::core_result(listen(0), "listen")?;
    let port = fixture::core_result(local_port(&listener), "local port")?;
    println!("http://127.0.0.1:{port}/repo.git");
    std::io::stdout().flush()?;
    let mut request_bytes = [0; 65536];
    let mut control = [0; 8192];
    let mut staging = [0; 4096];
    let mut selection = http::RevisionBuffers::<64, { fixture::OBJECTS }, 1024, 1024>::new();
    loop {
        let socket = fixture::core_result(accept(&listener), "accept")?;
        let result = (|| {
            let mut request = read_request(&socket, &mut request_bytes)?;
            let Some(path) = request.path.strip_prefix("/repo.git/") else {
                return respond(
                    &socket,
                    http::Response {
                        status: 404,
                        content_type: "text/plain",
                    },
                    b"Repository not found\n",
                );
            };
            request.path = path;
            if let Some(mut cursor) = publication.prepare(request)? {
                respond_file_published(&socket, &mut cursor, &mut staging)
            } else {
                let mut cursor = http::stream::ResponseEncoder::with_revision_storage(
                    &repository,
                    request,
                    selection.storage(8 * 1024 * 1024, RevisionLimits { max_exclusions: 8 }),
                    &mut control,
                )?;
                respond_stream(&socket, &repository, &mut cursor, &mut staging)
            }
        })();
        if let Err(error) = result {
            eprintln!("closed failed request: {error:?}");
        }
        // Dropping the accepted descriptor closes every response, including
        // malformed input and interrupted/truncated downloads. No retry headers.
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("packed-dumb: {error}");
        std::process::exit(1);
    }
}
