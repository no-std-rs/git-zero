//! A raw, allocator-free Wasm ABI. JavaScript owns HTTP and runtime metadata.
#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod abi {
    use core::cell::UnsafeCell;
    use git_zero::{
        Entry, EntryKind, Error, SnapshotBuilder, ValidatedRepository,
        http::{self, Request},
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

    #[unsafe(no_mangle)]
    pub extern "C" fn input_ptr() -> *mut u8 {
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
        if input_length > INPUT_CAPACITY {
            return -1;
        }
        // SAFETY: All access is bounded, synchronous and exclusive per instance.
        let input = unsafe { &(&*INPUT.0.get())[..input_length] };
        let output = unsafe { &mut *OUTPUT.0.get() };
        match run(input, method, output) {
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

    fn run(mut input: &[u8], method: u32, output: &mut [u8]) -> git_zero::Result<usize> {
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
        let target_length = length(&mut input)?;
        let target = core::str::from_utf8(take(&mut input, target_length)?)
            .map_err(|_| Error::InvalidRequest)?;
        let (path, query) = target
            .split_once('?')
            .map_or((target, None), |(path, query)| (path, Some(query)));
        let method = match method {
            0 => "GET",
            1 => "POST",
            _ => return Err(Error::InvalidRequest),
        };
        let mut tree = [0; 1024];
        let mut commit = [0; 1024];
        let mut entries = [Entry::EMPTY; 4];
        let mut builder = SnapshotBuilder::new(&mut entries, &mut tree, &mut commit);
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
        let repository = builder.finish()?;
        let mut objects = [None; 6]; // Commit, root tree, up to four distinct blobs.
        let mut by_id = [0; 6];
        let repository = ValidatedRepository::new(
            &repository,
            &mut objects,
            &mut by_id,
            &mut [0; 128],
            &mut [0; 128],
        )?;
        let mut body = SliceWriter::new(&mut output[3..]);
        let response = http::serve(
            &repository,
            Request {
                method,
                path,
                query,
                body: input,
            },
            &mut body,
        )?;
        let total = body.len() + 3;
        output[..2].copy_from_slice(&response.status.to_le_bytes());
        output[2] = match response.content_type {
            "application/x-git-upload-pack-advertisement" => 1,
            "application/x-git-upload-pack-result" => 2,
            "application/x-git-loose-object" => 3,
            _ => 0,
        };
        Ok(total)
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
        core::arch::wasm32::unreachable()
    }
}
