//! Buffered selected-pack caching over loopback HTTP; caller-owned synchronous caches.
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/selected_http.rs"]
mod selected_http;

use fixture::{Fixture, core_result};
use git_zero::{
    RefRepository, ValidatedRepository,
    cache::selected::{FixedCache, PackCache},
    http::WantOptions,
};
use git_zero_rustix::{FileRepository, accept, cache::DiskCache, listen, local_port};
use rustix::{
    fd::AsFd,
    fs::{self, Mode, OFlags},
};
use std::{io::Write, path::PathBuf};

struct Options {
    fixture: PathBuf,
    directory: Option<PathBuf>,
    pack_bytes: usize,
    port: u16,
    allow_reachable: bool,
}

impl Options {
    fn parse() -> fixture::Result<Self> {
        let mut arguments = std::env::args().skip(1);
        let (mut fixture, mut directory, mut port, mut pack_bytes) = (None, None, 0, 1_048_576);
        let mut allow_reachable = false;
        while let Some(argument) = arguments.next() {
            if argument == "--allow-reachable-wants" {
                allow_reachable = true;
                continue;
            }
            let value = arguments.next().ok_or("expected an option value")?;
            match argument.as_str() {
                "--fixture" => fixture = Some(value.into()),
                "--cache-directory" => directory = Some(value.into()),
                "--pack-bytes" => pack_bytes = value.parse()?,
                "--port" => port = value.parse()?,
                _ => return Err("unknown option".into()),
            }
        }
        Ok(Self {
            fixture: fixture.ok_or("--fixture is required")?,
            directory,
            pack_bytes,
            port,
            allow_reachable,
        })
    }
}

fn run(options: Options) -> fixture::Result<()> {
    let fixture = Fixture::load(&options.fixture)?;
    let (id, head) = fixture.head()?;
    let references = fixture.references()?;
    let source = FileRepository::new(id, fixture.objects());
    let source = core_result(RefRepository::new(&source, head, &references), "references")?;
    let (mut metadata, mut index) = ([None; 256], [0; 256]);
    let repository = core_result(
        ValidatedRepository::new(
            &source,
            &mut metadata,
            &mut index,
            &mut [0; 512],
            &mut [0; 512],
        ),
        "validation",
    )?;
    let mut storage = core_result(
        selected_http::Storage::new(options.pack_bytes),
        "response storage",
    )?;
    let mut resident = vec![0; options.pack_bytes];
    let wants = if options.allow_reachable {
        WantOptions::all_visible_reachable(64)
    } else {
        WantOptions::STRICT
    };
    if let Some(path) = options.directory {
        let directory = fs::openat(
            fs::CWD,
            path.as_os_str().as_encoded_bytes(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        // This synchronous single producer uses a process-specific staging nonce.
        let mut nonce = [0; 16];
        nonce[..4].copy_from_slice(&std::process::id().to_be_bytes());
        nonce[4..12].copy_from_slice(
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
                .to_be_bytes(),
        );
        serve(
            &repository,
            &mut DiskCache::new(directory.as_fd(), &mut resident, nonce),
            &mut storage,
            options.port,
            wants,
        )
    } else {
        serve(
            &repository,
            &mut FixedCache::new(&mut resident),
            &mut storage,
            options.port,
            wants,
        )
    }
}

fn serve(
    repository: &ValidatedRepository<'_, '_, impl git_zero::Repository>,
    cache: &mut impl PackCache,
    storage: &mut selected_http::Storage,
    port: u16,
    wants: WantOptions,
) -> fixture::Result<()> {
    let listener = core_result(listen(port), "listen")?;
    println!(
        "http://127.0.0.1:{}/repo.git",
        core_result(local_port(&listener), "port")?
    );
    std::io::stdout().flush()?;
    loop {
        let socket = core_result(accept(&listener), "accept")?;
        match storage.respond(&socket, repository, cache, wants) {
            Ok((status, outcome)) => eprintln!("status={status} cache={outcome:?}"),
            Err(error) => eprintln!("transport error: {error:?}"),
        }
    }
}

fn main() {
    if let Err(error) = Options::parse().and_then(run) {
        eprintln!("selected-cache: {error}");
        std::process::exit(1);
    }
}
