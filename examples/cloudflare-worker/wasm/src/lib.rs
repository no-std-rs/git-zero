//! A raw, allocator-free Wasm ABI. JavaScript owns HTTP and runtime metadata.
#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod raw;
#[cfg(target_arch = "wasm32")]
mod view;

#[cfg(target_arch = "wasm32")]
mod abi {
    use crate::{raw, view::WorkerRepository};
    use core::cell::UnsafeCell;
    use git_zero::{
        Entry, EntryKind, Error, ObjectFormat, RevisionLimits, Snapshot, SnapshotBuilder,
        ValidatedRepository,
        http::{
            self, Request, RevisionStorage, SelectionBuffers, WantOptions, WantPolicy,
            stream::ResponseEncoder,
        },
        io::SliceWriter,
    };

    const INPUT_CAPACITY: usize = 65536;
    const OUTPUT_CAPACITY: usize = 131072;

    // A Worker instance is single-threaded. Host calls must be synchronous and
    // non-reentrant. Each host fetch creates its own instance and copies output.
    struct Buffer<const N: usize>(UnsafeCell<[u8; N]>);
    // SAFETY: This module is compiled only for the single-threaded Wasm host ABI.
    unsafe impl<const N: usize> Sync for Buffer<N> {}
    static INPUT: Buffer<INPUT_CAPACITY> = Buffer(UnsafeCell::new([0; INPUT_CAPACITY]));
    static OUTPUT: Buffer<OUTPUT_CAPACITY> = Buffer(UnsafeCell::new([0; OUTPUT_CAPACITY]));

    struct SessionStorage {
        entries: [Entry<'static>; 4],
        tree: [u8; 1024],
        commit: [u8; 1024],
        control: [u8; 8192],
        selection: SelectionBuffers<64, 64, 512, 512>,
        revisions: [u8; 64],
        publication: [git_zero::pack::IndexEntry; raw::OBJECTS],
    }
    struct Exclusive<T>(UnsafeCell<T>);
    // SAFETY: Same synchronous, single-threaded, non-reentrant ABI as Buffer.
    unsafe impl<T> Sync for Exclusive<T> {}
    static SESSION_STORAGE: Exclusive<SessionStorage> =
        Exclusive(UnsafeCell::new(SessionStorage {
            entries: [Entry::EMPTY; 4],
            tree: [0; 1024],
            commit: [0; 1024],
            control: [0; 8192],
            selection: SelectionBuffers::new(),
            revisions: [0; 64],
            publication: [git_zero::pack::IndexEntry::EMPTY; raw::OBJECTS],
        }));
    static PUBLICATION: Exclusive<Option<http::dumb::Publication<'static, raw::Imported>>> =
        Exclusive(UnsafeCell::new(None));
    enum Session {
        Snapshot {
            repository: Snapshot<'static>,
            encoder: ResponseEncoder<'static>,
        },
        Imported {
            repository: &'static raw::Imported,
            encoder: ResponseEncoder<'static>,
        },
        Published {
            encoder: http::dumb::PublishedResponse<'static, raw::Imported>,
        },
    }
    static SESSION: Exclusive<Option<Session>> = Exclusive(UnsafeCell::new(None));
    // Keep the zero arena separate from initialized descriptors so Wasm does not
    // copy 128 KiB of zero data on every instance, including legacy-only hosts.
    static IMPORTED_BODIES: Buffer<{ raw::BODY_CAPACITY }> =
        Buffer(UnsafeCell::new([0; raw::BODY_CAPACITY]));
    // SAFETY: IMPORTED owns the sole reference to this unexported arena. All
    // access follows its phase guards and the exclusive ABI contract above.
    static IMPORTED: Exclusive<raw::Imported> =
        Exclusive(UnsafeCell::new(raw::Imported::new(unsafe {
            &mut *IMPORTED_BODIES.0.get()
        })));

    /// Cancel any active response before obtaining input storage for a new frame.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_cancel() {
        // SAFETY: Exclusive ABI entry; drops all references before buffer reuse.
        unsafe {
            *SESSION.0.get() = None;
            // Drop every cursor before the publication it borrowed, and both
            // before importing new objects or reusing the metadata array.
            *PUBLICATION.0.get() = None;
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn input_ptr() -> *mut u8 {
        stream_cancel();
        INPUT.0.get().cast()
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn input_capacity() -> usize {
        INPUT_CAPACITY
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn output_ptr() -> *const u8 {
        OUTPUT.0.get().cast()
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn output_capacity() -> usize {
        OUTPUT_CAPACITY
    }

    /// Input frame: four u32-LE lengths followed by cf, headers, vars, runtime
    /// JSON; then u32-LE target length, target bytes, and remaining request body.
    /// `method`: 0=GET, 1=POST. Output: u16-LE status, u8 content-type code, body.
    /// Returns output length, or negative error (1=capacity, 2=invalid input).
    #[unsafe(no_mangle)]
    pub extern "C" fn serve(input_length: usize, method: u32) -> i32 {
        serve_with_format(input_length, method, 0)
    }

    /// Same frame and output as `serve`; format: 0=SHA-1, 1=SHA-256.
    #[unsafe(no_mangle)]
    pub extern "C" fn serve_with_format(input_length: usize, method: u32, format: u32) -> i32 {
        serve_frame(input_length, method, format, false)
    }

    /// Extended frame: after target bytes, two u32-LE lengths followed by the
    /// Git-Protocol and Content-Type header values, then the Git request body.
    /// Empty header values mean absent. Other limits and output match `serve`.
    #[unsafe(no_mangle)]
    pub extern "C" fn serve_http(input_length: usize, method: u32, format: u32) -> i32 {
        serve_frame(input_length, method, format, true)
    }

    fn serve_frame(input_length: usize, method: u32, format: u32, http_metadata: bool) -> i32 {
        stream_cancel();
        if input_length > INPUT_CAPACITY {
            return -1;
        }
        let format = match format {
            0 => ObjectFormat::Sha1,
            1 => ObjectFormat::Sha256,
            _ => return -2,
        };
        // SAFETY: All access is bounded, synchronous and exclusive per instance.
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        let output = unsafe { &mut *OUTPUT.0.get() };
        match run(input, method, format, http_metadata, output) {
            Ok(length) => length as i32,
            Err(Error::BufferTooSmall) => -1,
            Err(_) => -2,
        }
    }

    fn take<'a>(input: &mut &'a [u8], length: usize) -> git_zero::Result<&'a [u8]> {
        let result = input.get(..length).ok_or(Error::InvalidRequest)?;
        *input = &input[length..];
        Ok(result)
    }

    fn length(input: &mut &[u8]) -> git_zero::Result<usize> {
        Ok(u32::from_le_bytes(
            take(input, 4)?
                .try_into()
                .map_err(|_| Error::InvalidRequest)?,
        ) as usize)
    }

    fn repository_request<'a>(
        mut input: &'a [u8],
        method: u32,
        format: ObjectFormat,
        http_metadata: bool,
        entries: &'a mut [Entry<'a>],
        tree: &'a mut [u8],
        commit: &'a mut [u8],
    ) -> git_zero::Result<(Snapshot<'a>, Request<'a>)> {
        let lengths = [
            length(&mut input)?,
            length(&mut input)?,
            length(&mut input)?,
            length(&mut input)?,
        ];
        let cf = take(&mut input, lengths[0])?;
        let headers = take(&mut input, lengths[1])?;
        let vars = take(&mut input, lengths[2])?;
        let runtime = take(&mut input, lengths[3])?;
        let request = request(input, method, http_metadata)?;
        let mut builder = SnapshotBuilder::new(entries, tree, commit);
        for (name, contents) in [
            ("cf.json", cf),
            ("headers.json", headers),
            ("vars.json", vars),
            ("runtime.json", runtime),
        ] {
            builder.push(Entry {
                name,
                kind: EntryKind::File(contents),
            })?;
        }
        Ok((builder.finish_with_format(format)?, request))
    }

    fn request(
        mut input: &[u8],
        method: u32,
        http_metadata: bool,
    ) -> git_zero::Result<Request<'_>> {
        let target_length = length(&mut input)?;
        let target = core::str::from_utf8(take(&mut input, target_length)?)
            .map_err(|_| Error::InvalidRequest)?;
        let (git_protocol, content_type) = if http_metadata {
            let protocol_length = length(&mut input)?;
            let media_length = length(&mut input)?;
            let protocol = core::str::from_utf8(take(&mut input, protocol_length)?)
                .map_err(|_| Error::InvalidRequest)?;
            let media = core::str::from_utf8(take(&mut input, media_length)?)
                .map_err(|_| Error::InvalidRequest)?;
            (
                (!protocol.is_empty()).then_some(protocol),
                (!media.is_empty()).then_some(media),
            )
        } else {
            (None, Some("application/x-git-upload-pack-request"))
        };
        let (path, query) = target
            .split_once('?')
            .map_or((target, None), |(path, query)| (path, Some(query)));
        let method = match method {
            0 => "GET",
            1 => "POST",
            _ => return Err(Error::InvalidRequest),
        };
        Ok(Request {
            content_type,
            git_protocol,
            method,
            path,
            query,
            body: input,
        })
    }

    fn run(
        input: &[u8],
        method: u32,
        format: ObjectFormat,
        http_metadata: bool,
        output: &mut [u8],
    ) -> git_zero::Result<usize> {
        let mut tree = [0; 1024];
        let mut commit = [0; 1024];
        let mut entries = [Entry::EMPTY; 4];
        let (repository, request) = repository_request(
            input,
            method,
            format,
            http_metadata,
            &mut entries,
            &mut tree,
            &mut commit,
        )?;
        let mut objects = [None; 6]; // Commit, root tree, up to four distinct blobs.
        let mut by_id = [0; 6];
        let source = WorkerRepository::Snapshot(&repository);
        let repository = ValidatedRepository::new(
            &source,
            &mut objects,
            &mut by_id,
            &mut [0; 128],
            &mut [0; 128],
        )?;
        let mut body = SliceWriter::new(&mut output[3..]);
        let response = http::serve_with_selection(
            &repository,
            request,
            &mut SelectionBuffers::<6, 6, 128, 128>::new().storage(1_048_576),
            &mut body,
        )?;
        let total = body.len() + 3;
        response_header(response, output);
        Ok(total)
    }

    fn response_header(response: http::Response, output: &mut [u8]) {
        output[..2].copy_from_slice(&response.status.to_le_bytes());
        output[2] = match response.content_type {
            "application/x-git-upload-pack-advertisement" => 1,
            "application/x-git-upload-pack-result" => 2,
            "application/x-git-loose-object" => 3,
            "application/x-git-packed-objects" => 4,
            "application/x-git-packed-objects-toc" => 5,
            _ => 0,
        };
    }

    /// Prepare an extended HTTP frame and return three metadata bytes in OUTPUT.
    /// Input must remain immutable until EOF/cancel. Each instance owns one
    /// stream; starting another request replaces it. Errors: -1 capacity, -2 input.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start(input_length: usize, method: u32, format: u32) -> i32 {
        start_frame(input_length, method, format, 0, usize::MAX, None)
    }

    /// Host-granted want policy, never parsed from INPUT. Unknown policy cancels
    /// before rejection. Old starts always remain strict, regardless of history.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start_policy(
        input_length: usize,
        method: u32,
        format: u32,
        policy: u32,
        max_want_lines: u32,
    ) -> i32 {
        start_frame(
            input_length,
            method,
            format,
            policy,
            max_want_lines as usize,
            None,
        )
    }

    #[derive(Clone, Copy)]
    struct RevisionConfig {
        max_exclusions: usize,
        graph_read_limit: u64,
    }

    impl RevisionConfig {
        fn new(max_exclusions: u32, graph_read_limit: u32) -> git_zero::Result<Self> {
            if max_exclusions > 8 || graph_read_limit > 1_048_576 {
                return Err(Error::InvalidRequest);
            }
            Ok(Self {
                max_exclusions: max_exclusions as usize,
                graph_read_limit: graph_read_limit as u64,
            })
        }
    }

    /// Explicit host opt-in; limits are not read from the untrusted HTTP frame.
    /// At most eight exclusions and 1 MiB of graph reads; lower budgets, including
    /// zero, are enforced before output. Existing starts stay revision-disabled.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start_revisions(
        input_length: usize,
        method: u32,
        format: u32,
        policy: u32,
        max_want_lines: u32,
        max_exclusions: u32,
        graph_read_limit: u32,
    ) -> i32 {
        stream_cancel();
        let Ok(revision) = RevisionConfig::new(max_exclusions, graph_read_limit) else {
            return -2;
        };
        start_frame(
            input_length,
            method,
            format,
            policy,
            max_want_lines as usize,
            Some(revision),
        )
    }

    fn want_options(policy: u32, max_want_lines: usize) -> git_zero::Result<WantOptions> {
        Ok(WantOptions {
            policy: match policy {
                0 => WantPolicy::AdvertisedOnly,
                1 => WantPolicy::AllVisibleReachable,
                _ => return Err(Error::InvalidRequest),
            },
            max_want_lines,
        })
    }

    fn start_frame(
        input_length: usize,
        method: u32,
        format: u32,
        policy: u32,
        max_want_lines: usize,
        revision: Option<RevisionConfig>,
    ) -> i32 {
        stream_cancel();
        let options = match want_options(policy, max_want_lines) {
            Ok(options) => options,
            Err(_) => return -2,
        };
        if input_length > INPUT_CAPACITY {
            return -1;
        }
        let format = match format {
            0 => ObjectFormat::Sha1,
            1 => ObjectFormat::Sha256,
            _ => return -2,
        };
        // SAFETY: The cancelled session no longer borrows any storage. These
        // disjoint static fields are exclusively borrowed until cancel/restart.
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        let storage = unsafe { &mut *SESSION_STORAGE.0.get() };
        let session = start_session(input, storage, method, format, options, revision);
        match session {
            Ok(session) => {
                unsafe {
                    *SESSION.0.get() = Some(session);
                }
                3
            }
            Err(Error::BufferTooSmall) => -1,
            Err(Error::LimitExceeded) if options != WantOptions::STRICT || revision.is_some() => -1,
            Err(_) => -2,
        }
    }

    fn start_session(
        input: &'static [u8],
        storage: &'static mut SessionStorage,
        method: u32,
        format: ObjectFormat,
        options: WantOptions,
        revision: Option<RevisionConfig>,
    ) -> git_zero::Result<Session> {
        let (repository, request) = repository_request(
            input,
            method,
            format,
            true,
            &mut storage.entries,
            &mut storage.tree,
            &mut storage.commit,
        )?;
        let mut objects = [None; 6];
        let mut by_id = [0; 6];
        let source = WorkerRepository::Snapshot(&repository);
        let validated = ValidatedRepository::new(
            &source,
            &mut objects,
            &mut by_id,
            &mut [0; 128],
            &mut [0; 128],
        )?;
        let encoder = prepare_response(
            &validated,
            request,
            &mut storage.selection,
            &mut storage.revisions,
            &mut storage.control,
            options,
            revision,
        )?;
        response_header(encoder.response(), unsafe { &mut *OUTPUT.0.get() });
        Ok::<_, Error>(Session::Snapshot {
            repository,
            encoder,
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn repository_capacity() -> usize {
        raw::BODY_CAPACITY
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn repository_manifest_capacity() -> usize {
        raw::MANIFEST_CAPACITY
    }

    /// Begin version-1 raw import from INPUT. Cancels and invalidates the old
    /// repository before parsing. Body chunks follow in manifest object order.
    #[unsafe(no_mangle)]
    pub extern "C" fn repository_begin(input_length: usize) -> i32 {
        stream_cancel();
        // SAFETY: Cancelling drops every borrow before mutable import access.
        let imported = unsafe { &mut *IMPORTED.0.get() };
        if input_length > INPUT_CAPACITY {
            imported.reset();
            return -1;
        }
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        import_result(imported.begin(input))
    }

    /// Append INPUT to the body arena. Writes outside Importing fail without
    /// disturbing an active response; overflows invalidate the partial import.
    #[unsafe(no_mangle)]
    pub extern "C" fn repository_write(input_length: usize) -> i32 {
        // SAFETY: Only Ready repositories can be borrowed by an active stream.
        if !unsafe { (&*IMPORTED.0.get()).importing() } {
            return -2;
        }
        let imported = unsafe { &mut *IMPORTED.0.get() };
        if input_length > INPUT_CAPACITY {
            imported.reset();
            return -1;
        }
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        import_result(imported.append(input))
    }

    /// Authenticate the complete import before making it available to requests.
    #[unsafe(no_mangle)]
    pub extern "C" fn repository_finish() -> i32 {
        // SAFETY: Importing excludes every active borrow of IMPORTED.
        if !unsafe { (&*IMPORTED.0.get()).importing() } {
            return -2;
        }
        import_result(unsafe { (&mut *IMPORTED.0.get()).finish() })
    }

    fn import_result(result: git_zero::Result<()>) -> i32 {
        match result {
            Ok(()) => 0,
            Err(Error::BufferTooSmall) => -1,
            Err(_) => -2,
        }
    }

    /// Request-only extended HTTP frame: target length/bytes, header lengths,
    /// Git-Protocol/Content-Type bytes, then body. Format is fixed by the import.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start_repository(input_length: usize, method: u32) -> i32 {
        start_repository_frame(input_length, method, 0, usize::MAX, None)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start_repository_policy(
        input_length: usize,
        method: u32,
        policy: u32,
        max_want_lines: u32,
    ) -> i32 {
        start_repository_frame(input_length, method, policy, max_want_lines as usize, None)
    }

    /// Imported-repository counterpart of `stream_start_revisions`.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start_repository_revisions(
        input_length: usize,
        method: u32,
        policy: u32,
        max_want_lines: u32,
        max_exclusions: u32,
        graph_read_limit: u32,
    ) -> i32 {
        stream_cancel();
        let Ok(revision) = RevisionConfig::new(max_exclusions, graph_read_limit) else {
            return -2;
        };
        start_repository_frame(
            input_length,
            method,
            policy,
            max_want_lines as usize,
            Some(revision),
        )
    }

    /// Complete stored-pack dumb publication for an authenticated raw import.
    /// loose: 0=packed-only, 1=fall through to ordinary loose-object responses.
    /// Other routes retain strict ordinary selection; smart host options should
    /// use their existing dedicated starts. Publication and cursor borrow only
    /// this instance's immutable import and fixed 64-entry metadata array.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_start_repository_packed(
        input_length: usize,
        method: u32,
        loose: u32,
    ) -> i32 {
        stream_cancel();
        let loose = match loose {
            0 => http::dumb::LooseObjects::Disabled,
            1 => http::dumb::LooseObjects::Enabled,
            _ => return -2,
        };
        if input_length > INPUT_CAPACITY {
            return -1;
        }
        // SAFETY: Cancellation above releases all prior publication/cursor
        // borrows. Ready imports are immutable until repository_begin cancels.
        let imported = unsafe { &*IMPORTED.0.get() };
        if !imported.ready() {
            return -2;
        }
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        let request = match request(input, method, true) {
            Ok(request) => request,
            Err(_) => return -2,
        };
        let storage = unsafe { &mut *SESSION_STORAGE.0.get() };
        let publication = match http::dumb::Publication::new(
            imported,
            &mut storage.publication,
            &mut [0; 512],
            loose,
        ) {
            Ok(publication) => publication,
            Err(Error::BufferTooSmall | Error::LimitExceeded) => return -1,
            Err(_) => return -2,
        };
        // SAFETY: Publication is replaced only after SESSION is cancelled.
        unsafe {
            *PUBLICATION.0.get() = Some(publication);
        }
        let publication = unsafe { (&*PUBLICATION.0.get()).as_ref().unwrap() };
        match publication.prepare(request) {
            Ok(Some(encoder)) => {
                response_header(encoder.response(), unsafe { &mut *OUTPUT.0.get() });
                unsafe {
                    *SESSION.0.get() = Some(Session::Published { encoder });
                }
                3
            }
            Ok(None) => start_repository_frame(input_length, method, 0, usize::MAX, None),
            Err(Error::BufferTooSmall | Error::LimitExceeded) => {
                stream_cancel();
                -1
            }
            Err(_) => {
                stream_cancel();
                -2
            }
        }
    }

    /// Known complete wire length for a currently prepared publication response.
    /// -1 means no publication cursor; all bounded raw imports fit signed i32.
    #[unsafe(no_mangle)]
    pub extern "C" fn publication_length() -> i32 {
        // SAFETY: Read-only, exclusive non-reentrant ABI access.
        match unsafe { &*SESSION.0.get() } {
            Some(Session::Published { encoder }) => i32::try_from(encoder.size()).unwrap_or(-1),
            _ => -1,
        }
    }

    fn start_repository_frame(
        input_length: usize,
        method: u32,
        policy: u32,
        max_want_lines: usize,
        revision: Option<RevisionConfig>,
    ) -> i32 {
        stream_cancel();
        let options = match want_options(policy, max_want_lines) {
            Ok(options) => options,
            Err(_) => return -2,
        };
        if input_length > INPUT_CAPACITY {
            return -1;
        }
        // SAFETY: Ready storage is immutable until repository_begin cancels the
        // session. Neither its bodies nor copied names borrow request INPUT.
        let imported = unsafe { &*IMPORTED.0.get() };
        if !imported.ready() {
            return -2;
        }
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        let storage = unsafe { &mut *SESSION_STORAGE.0.get() };
        match start_imported_session(imported, input, storage, method, options, revision) {
            Ok(session) => {
                unsafe { *SESSION.0.get() = Some(session) };
                3
            }
            Err(Error::BufferTooSmall) => -1,
            Err(Error::LimitExceeded) if options != WantOptions::STRICT || revision.is_some() => -1,
            Err(_) => -2,
        }
    }

    fn start_imported_session(
        repository: &'static raw::Imported,
        input: &'static [u8],
        storage: &'static mut SessionStorage,
        method: u32,
        options: WantOptions,
        revision: Option<RevisionConfig>,
    ) -> git_zero::Result<Session> {
        let mut objects = [None; raw::OBJECTS];
        let mut by_id = [0; raw::OBJECTS];
        let source = WorkerRepository::Imported(repository);
        let validated = ValidatedRepository::new(
            &source,
            &mut objects,
            &mut by_id,
            &mut [0; 512],
            &mut [0; 512],
        )?;
        let encoder = prepare_response(
            &validated,
            request(input, method, true)?,
            &mut storage.selection,
            &mut storage.revisions,
            &mut storage.control,
            options,
            revision,
        )?;
        response_header(encoder.response(), unsafe { &mut *OUTPUT.0.get() });
        Ok(Session::Imported {
            repository,
            encoder,
        })
    }

    fn prepare_response<R: git_zero::Repository + ?Sized>(
        repository: &ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        selection: &'static mut SelectionBuffers<64, 64, 512, 512>,
        states: &'static mut [u8],
        control: &'static mut [u8],
        options: WantOptions,
        revision: Option<RevisionConfig>,
    ) -> git_zero::Result<ResponseEncoder<'static>> {
        if let Some(config) = revision {
            ResponseEncoder::with_revision_storage_policy(
                repository,
                request,
                RevisionStorage {
                    selection: selection.storage(config.graph_read_limit),
                    state: states,
                    limits: RevisionLimits {
                        max_exclusions: config.max_exclusions,
                    },
                },
                options,
                control,
            )
        } else {
            ResponseEncoder::with_selection_storage_policy(
                repository,
                request,
                selection.storage(1_048_576),
                options,
                control,
            )
        }
    }

    /// Produce at most `capacity` body bytes in OUTPUT. 0 means clean EOF.
    /// Invalid capacities leave the cursor intact. A stream error cancels it;
    /// hosts must abort the response. Host pull/cancel callbacks drive all I/O.
    #[unsafe(no_mangle)]
    pub extern "C" fn stream_read(capacity: usize) -> i32 {
        if capacity == 0 || capacity > OUTPUT_CAPACITY {
            return -1;
        }
        // SAFETY: Exclusive, non-reentrant access; OUTPUT never aliases inputs.
        let session = unsafe { &mut *SESSION.0.get() };
        let Some(active) = session else {
            return -2;
        };
        let output = unsafe { &mut (&mut *OUTPUT.0.get())[..capacity] };
        let result = match active {
            Session::Snapshot {
                repository,
                encoder,
            } => encoder.read(&WorkerRepository::Snapshot(repository), output),
            Session::Imported {
                repository,
                encoder,
            } => encoder.read(&WorkerRepository::Imported(repository), output),
            Session::Published { encoder } => encoder.read(output),
        };
        match result {
            Ok(length) => length as i32,
            Err(_) => {
                *session = None;
                -2
            }
        }
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
        core::arch::wasm32::unreachable()
    }
}
