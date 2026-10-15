//! Buffer and callback adaptation; checkout semantics live in git-zero crates.
use crate::{Decoded, TreeEntry, errno, sha1_bytes};
use core::ffi::c_void;
use git_zero::{Error, Object, ObjectId, ObjectKind, checkout, client, http::Request};

#[repr(C)]
pub struct ObjectSlice {
    data: *const u8,
    length: usize,
    kind: u32,
}
#[repr(C)]
pub struct Root {
    id: [u8; 20],
    tree: [u8; 20],
    mode: u32,
    message: u32,
}
type Fetch = unsafe extern "C" fn(*mut c_void, *const u8, *mut ObjectSlice) -> i32;
type Get = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut u8, usize, *mut usize) -> i32;

fn kind(value: u32) -> Result<ObjectKind, Error> {
    match value {
        1 => Ok(ObjectKind::Commit),
        2 => Ok(ObjectKind::Tree),
        3 => Ok(ObjectKind::Blob),
        4 => Ok(ObjectKind::Tag),
        _ => Err(Error::InvalidObject),
    }
}

struct Source {
    context: *mut c_void,
    fetch: Fetch,
    error: i32,
}
impl checkout::ObjectSource for Source {
    fn object(&mut self, id: ObjectId) -> Result<Object<'_>, Error> {
        let id = sha1_bytes(id)?;
        let mut output = ObjectSlice {
            data: core::ptr::null(),
            length: 0,
            kind: 0,
        };
        // SAFETY: The ABI caller guarantees the callback's borrowed slice stays
        // valid until the next fetch. The trait prevents overlapping accesses.
        self.error = unsafe { (self.fetch)(self.context, id.as_ptr(), &mut output) };
        if self.error != 0 {
            return Err(Error::Backend);
        }
        Ok(Object {
            kind: kind(output.kind)?,
            data: unsafe { core::slice::from_raw_parts(output.data, output.length) },
        })
    }
}

/// Resolve a root through the shared bounded tag/commit walker.
/// # Safety
/// ID/result are live disjoint ranges. Fetch returns hash-verified objects of
/// nonnull data valid until the next call; context stays live and locked.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_resolve_checkout(
    context: *mut c_void,
    fetch: Fetch,
    id: *const u8,
    result: *mut Root,
) -> i32 {
    let mut source = Source {
        context,
        fetch,
        error: 0,
    };
    let id = ObjectId::Sha1(unsafe { *id.cast::<[u8; 20]>() });
    let resolved = checkout::resolve_root(&mut source, id, 8).and_then(|root| {
        Ok(Root {
            id: sha1_bytes(root.message.map_or(root.entry.id, |message| message.id))?,
            tree: sha1_bytes(root.entry.id)?,
            mode: root.entry.mode.raw(),
            message: u32::from(root.message.is_some()),
        })
    });
    match resolved {
        Ok(root) => {
            unsafe { *result = root };
            0
        }
        Err(_) if source.error != 0 => source.error,
        Err(Error::LimitExceeded) => -40,
        Err(error) => errno(error),
    }
}

/// Byte-component lookup, returning owned metadata from the shared tree walker.
/// # Safety
/// All inputs and the exclusive output are live, aligned and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_tree_lookup(
    data: *const u8,
    length: usize,
    name: *const u8,
    name_length: usize,
    result: *mut TreeEntry,
) -> i32 {
    let (data, name) = unsafe {
        (
            core::slice::from_raw_parts(data, length),
            core::slice::from_raw_parts(name, name_length),
        )
    };
    match checkout::lookup(data, git_zero::ObjectFormat::Sha1, name) {
        Ok(Some(entry)) => match sha1_bytes(entry.id) {
            Ok(id) => {
                unsafe {
                    *result = TreeEntry {
                        name: name.as_ptr(),
                        name_length,
                        mode: entry.mode.raw(),
                        id,
                    }
                };
                1
            }
            Err(error) => errno(error),
        },
        Ok(None) => 0,
        Err(error) => errno(error),
    }
}

/// Expose exact regular/executable/symlink bytes after shared kind validation.
/// # Safety
/// Data is live; result is aligned, exclusive and disjoint from data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_content_info(
    data: *const u8,
    length: usize,
    object_kind: u32,
    mode: u32,
    result: *mut Decoded,
) -> i32 {
    let data = unsafe { core::slice::from_raw_parts(data, length) };
    let content = (|| {
        let object = Object {
            kind: kind(object_kind)?,
            data,
        };
        checkout::Mode::from_raw(mode)?.contents(object)?;
        Ok(Decoded {
            offset: 0,
            length,
            kind: object_kind,
        })
    })();
    match content {
        Ok(value) => {
            unsafe { *result = value };
            0
        }
        Err(error) => errno(error),
    }
}

/// Normalize a user selector into a NUL-terminated canonical ref name.
/// # Safety
/// All ranges are disjoint/live; output has capacity writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_reference_name(
    name: *const u8,
    length: usize,
    output: *mut u8,
    capacity: usize,
) -> i32 {
    let (name, output) = unsafe {
        (
            core::slice::from_raw_parts(name, length),
            core::slice::from_raw_parts_mut(output, capacity),
        )
    };
    let result = (|| {
        let name = core::str::from_utf8(name).map_err(|_| Error::InvalidReference)?;
        let size = output.len().checked_sub(1).ok_or(Error::BufferTooSmall)?;
        let length = client::reference_name(name, &mut output[..size])?.len();
        output[length] = 0;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => errno(error),
    }
}

struct Transport {
    context: *mut c_void,
    get: Get,
    error: i32,
}
impl client::Transport for Transport {
    fn request(
        &mut self,
        request: Request<'_>,
        response: &mut [u8],
    ) -> Result<(u16, usize), Error> {
        if request.method != "GET" || request.query.is_some() || !request.body.is_empty() {
            return Err(Error::Unsupported);
        }
        let mut length = 0;
        // SAFETY: C owns the live context, copies the path and writes at most
        // response.len bytes; no request data survives this synchronous call.
        self.error = unsafe {
            (self.get)(
                self.context,
                request.path.as_ptr(),
                request.path.len(),
                response.as_mut_ptr(),
                response.len(),
                &mut length,
            )
        };
        if self.error != 0 {
            return Err(Error::Backend);
        }
        Ok((200, length))
    }
}

/// Ordinary-HTTP ref traversal with the shared parser and path encoder.
/// # Safety
/// Ranges are live/disjoint; callback obeys Get's bounded synchronous contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_loose_reference(
    context: *mut c_void,
    get: Get,
    name: *const u8,
    name_length: usize,
    response: *mut u8,
    capacity: usize,
    path: *mut u8,
    path_capacity: usize,
    id: *mut u8,
) -> i32 {
    let (name, response, path) = unsafe {
        (
            core::slice::from_raw_parts(name, name_length),
            core::slice::from_raw_parts_mut(response, capacity),
            core::slice::from_raw_parts_mut(path, path_capacity),
        )
    };
    let mut transport = Transport {
        context,
        get,
        error: 0,
    };
    let resolved = core::str::from_utf8(name)
        .map_err(|_| Error::InvalidReference)
        .and_then(|name| client::resolve_reference(&mut transport, name, response, path, 8))
        .and_then(sha1_bytes);
    match resolved {
        Ok(value) => {
            unsafe { core::ptr::copy_nonoverlapping(value.as_ptr(), id, 20) };
            0
        }
        Err(_) if transport.error != 0 => transport.error,
        Err(Error::LimitExceeded) => -40,
        Err(error) => errno(error),
    }
}
