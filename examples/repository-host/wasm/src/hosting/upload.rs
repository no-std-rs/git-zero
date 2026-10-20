//! Frozen bounded upload controls and paged external-catalog wire encoding.
use super::{FRAME_BYTES, Frame, Shared, code, format, id_at, input, kind, output, u32_at, u64_at};
use core::cell::UnsafeCell;
use git_zero::{
    BlobFilter, Error, HistoryPolicy, ObjectFormat, ObjectInfo, ObjectKind, Result,
    http::stored::{
        self, Acknowledgments, Command, ObjectRequest, Objects, Request, ShallowUpdates,
    },
    io::{SliceWriter, Write},
};

const CONTROL_BYTES: usize = 4 * 1024 * 1024;
static CONTROL: Shared<[u8; CONTROL_BYTES]> = Shared(UnsafeCell::new([0; CONTROL_BYTES]));
static UPLOAD: Shared<Option<Upload>> = Shared(UnsafeCell::new(None));

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_version(length: usize, present: u32) -> i32 {
    let value = (|| {
        if present > 1 {
            return Err(Error::InvalidRequest);
        }
        let header = core::str::from_utf8(input(length)?).map_err(|_| Error::InvalidRequest)?;
        stored::protocol_version((present != 0).then_some(header)).map(usize::from)
    })();
    result(value)
}
struct Upload {
    format: ObjectFormat,
    version: u8,
    used: usize,
    parsed: Option<Request<'static>>,
    objects: Option<Objects<'static>>,
    acknowledgments: Option<Acknowledgments>,
    shallow_updates: Option<ShallowUpdates>,
    history: Option<HistoryPolicy>,
    boundary_probe: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_cancel() {
    // SAFETY: Exclusive call releases all CONTROL borrows before reinitializing.
    unsafe {
        *UPLOAD.0.get() = None;
    }
}
fn session() -> Result<&'static mut Upload> {
    // SAFETY: One exclusive synchronous ABI call; no references escape to JS.
    unsafe { (&mut *UPLOAD.0.get()).as_mut() }.ok_or(Error::InvalidRequest)
}
fn result(value: Result<usize>) -> i32 {
    match value {
        Ok(value) => value as i32,
        Err(error) => {
            host_upload_cancel();
            code(error)
        }
    }
}
fn emit(operation: impl FnOnce(&mut SliceWriter<'_>) -> Result<()>) -> i32 {
    let bytes = output();
    let value = {
        let mut writer = SliceWriter::new(&mut bytes[FRAME_BYTES..]);
        operation(&mut writer).map(|()| writer.as_bytes().len())
    };
    match value {
        Ok(length) => Frame::new(bytes).finish(60, 0, length),
        Err(error) => {
            host_upload_cancel();
            code(error)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_init(object_format: u32, version: u32) -> i32 {
    host_upload_cancel();
    let value = (|| {
        let format = format(object_format)?;
        if version > 2 {
            return Err(Error::Unsupported);
        }
        // SAFETY: Exclusive call; prior CONTROL borrows have been dropped.
        unsafe {
            *UPLOAD.0.get() = Some(Upload {
                format,
                version: version as u8,
                used: 0,
                parsed: None,
                objects: None,
                acknowledgments: None,
                shallow_updates: None,
                history: None,
                boundary_probe: false,
            });
        }
        Ok(0)
    })();
    result(value)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_append(length: usize) -> i32 {
    result((|| {
        let bytes = input(length)?;
        let session = session()?;
        if session.parsed.is_some() {
            return Err(Error::InvalidRequest);
        }
        let end = session
            .used
            .checked_add(length)
            .ok_or(Error::LimitExceeded)?;
        if end > CONTROL_BYTES {
            return Err(Error::LimitExceeded);
        }
        // SAFETY: Only this exclusive append accesses CONTROL mutably; parse
        // freezes the arena and forbids further appends while borrows are live.
        unsafe {
            (&mut *CONTROL.0.get())[session.used..end].copy_from_slice(bytes);
        }
        session.used = end;
        Ok(0)
    })())
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_parse() -> i32 {
    let value = (|| {
        let session = session()?;
        if session.parsed.is_some() {
            return Err(Error::InvalidRequest);
        }
        // SAFETY: CONTROL is immutable from parse until cancel; append rejects
        // parsed state, and cancel drops both iterators and request first.
        let bytes = unsafe { &(&*CONTROL.0.get())[..session.used] };
        let parsed = Request::parse(bytes, session.format, session.version)?;
        let mut frame = Frame::new(output());
        let mut flags = match parsed.command() {
            Command::Noop => 0,
            Command::Refs => 1,
            Command::Fetch => 2,
        };
        flags |= u32::from(parsed.band()) << 8;
        flags |= u32::from(parsed.done()) << 16;
        flags |= u32::from(parsed.include_tags()) << 17;
        if let Some(options) = parsed.options() {
            frame.wide(16, u64::from(options.depth.unwrap_or(0)));
            flags |= u32::from(options.deepen_relative) << 18;
            match options.filter {
                BlobFilter::All => {}
                BlobFilter::None => flags |= 1 << 19,
                BlobFilter::Limit(limit) => {
                    flags |= 1 << 20;
                    frame.wide(24, limit);
                }
            }
        }
        frame.word(12, flags);
        session.objects = Some(parsed.objects());
        session.parsed = Some(parsed);
        Ok(frame.finish(70, 0, 0) as usize)
    })();
    result(value)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_next() -> i32 {
    let value = (|| {
        let session = session()?;
        let mut frame = Frame::new(output());
        let object = session
            .objects
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .next();
        let tag = match object {
            Some(ObjectRequest::Want(id)) => {
                frame.id(0, id);
                71
            }
            Some(ObjectRequest::Have(id)) => {
                frame.id(0, id);
                72
            }
            Some(ObjectRequest::Shallow(id)) => {
                frame.id(0, id);
                73
            }
            None => 74,
        };
        Ok(frame.finish(tag, 0, 0) as usize)
    })();
    result(value)
}

/// Input: has-client-boundaries u32@0, nearest boundary distance u32@4
/// (zero means absent). Flags: deepening=1, distance probe needed=2,
/// complete-pack reuse permitted by the request=4, boundary announcements=8.
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_history_init(length: usize) -> i32 {
    let value = (|| {
        let bytes = input(length)?;
        if bytes.len() != 8 || u32_at(bytes, 0)? > 1 {
            return Err(Error::InvalidRequest);
        }
        let has_boundaries = u32_at(bytes, 0)? != 0;
        let nearest = u32_at(bytes, 4)?;
        let session = session()?;
        let options = session
            .parsed
            .as_ref()
            .and_then(Request::options)
            .ok_or(Error::InvalidRequest)?;
        session.history = Some(options.history(has_boundaries, (nearest != 0).then_some(nearest))?);
        let deepening = options.deepens_history(has_boundaries);
        session.boundary_probe = deepening && options.deepen_relative;
        let reuse =
            options.depth.is_none() && matches!(options.filter, BlobFilter::All) && !has_boundaries;
        let announces = session
            .parsed
            .as_ref()
            .ok_or(Error::InvalidRequest)?
            .shallow_updates()?
            .is_enabled();
        let mut frame = Frame::new(output());
        frame.word(
            12,
            u32::from(deepening)
                | (u32::from(session.boundary_probe) << 1)
                | (u32::from(reuse) << 2)
                | (u32::from(announces) << 3),
        );
        Ok(frame.finish(77, 0, 0) as usize)
    })();
    result(value)
}

/// Input: kind u32@0, flags u32@4 (client boundary=1, explicit want=2),
/// size u64@8, shortest commit distance u32@16. Flags: include=1,
/// follow parents=2, shallow=4, unshallow=8; next parent distance u64@16.
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_select(length: usize, probe: u32) -> i32 {
    let value = (|| {
        let bytes = input(length)?;
        if bytes.len() != 20 || probe > 1 {
            return Err(Error::InvalidRequest);
        }
        let flags = u32_at(bytes, 4)?;
        let depth = u32_at(bytes, 16)?;
        if flags > 3 || depth == 0 {
            return Err(Error::InvalidRequest);
        }
        let info = ObjectInfo {
            kind: kind(u32_at(bytes, 0)?)?,
            size: u64_at(bytes, 8)?,
        };
        let session = session()?;
        let history = session.history.ok_or(Error::InvalidRequest)?;
        if probe != 0 && !session.boundary_probe {
            return Err(Error::InvalidRequest);
        }
        let history = if probe != 0 {
            HistoryPolicy::boundary_probe()
        } else {
            history
        };
        let options = session
            .parsed
            .as_ref()
            .and_then(Request::options)
            .ok_or(Error::InvalidRequest)?;
        let boundary = flags & 1 != 0;
        let stop = info.kind == ObjectKind::Commit && !history.follows_parents(depth, boundary);
        let include = options.filter.includes(info, flags & 2 != 0);
        let mut frame = Frame::new(output());
        frame.word(
            12,
            u32::from(include)
                | (u32::from(!stop) << 1)
                | (u32::from(stop) << 2)
                | (u32::from(info.kind == ObjectKind::Commit && boundary && !stop) << 3),
        );
        frame.wide(16, u64::from(history.parent_depth(depth)));
        Ok(frame.finish(76, 0, 0) as usize)
    })();
    result(value)
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_advert(object_format: u32, version: u32, selection: u32) -> i32 {
    emit(|output| {
        let version = u8::try_from(version).map_err(|_| Error::InvalidRequest)?;
        if selection > 1 {
            return Err(Error::InvalidRequest);
        }
        stored::advertise(format(object_format)?, version, selection != 0, output)
    })
}

/// Header: padded ID@0, peeled ID@32, flags@64 (ID/peeled present),
/// name/target lengths u32@68/72, raw name and symbolic target at 80.
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_ref(
    length: usize,
    v2: u32,
    first: u32,
    selection: u32,
    reachable: u32,
) -> i32 {
    emit(|output| {
        let session = session()?;
        if v2 > 1 || first > 1 || selection > 1 || reachable > 1 {
            return Err(Error::InvalidRequest);
        }
        let bytes = input(length)?;
        let flags = u32_at(bytes, 64)?;
        if flags > 3 {
            return Err(Error::InvalidRequest);
        }
        let end = 80usize
            .checked_add(u32_at(bytes, 68)? as usize)
            .ok_or(Error::InvalidRequest)?;
        let target_end = end
            .checked_add(u32_at(bytes, 72)? as usize)
            .ok_or(Error::InvalidRequest)?;
        if bytes.len() != target_end {
            return Err(Error::InvalidRequest);
        }
        let name = bytes.get(80..end).ok_or(Error::InvalidRequest)?;
        let target = bytes.get(end..target_end).ok_or(Error::InvalidRequest)?;
        let target = (!target.is_empty()).then_some(target);
        let id = (flags & 1 != 0)
            .then(|| id_at(bytes, 0, session.format))
            .transpose()?;
        let peeled = (flags & 2 != 0)
            .then(|| id_at(bytes, 32, session.format))
            .transpose()?;
        if v2 != 0 {
            session
                .parsed
                .as_ref()
                .ok_or(Error::InvalidRequest)?
                .reference(id, name, target, peeled, output)
        } else {
            stored::reference(
                session.format,
                id,
                name,
                first != 0,
                target,
                stored::Features {
                    selection: selection != 0,
                    reachable_wants: reachable != 0,
                },
                output,
            )?;
            if let Some(peeled) = peeled {
                stored::peeled_reference(session.format, peeled, name, output)?;
            }
            Ok(())
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_flush() -> i32 {
    emit(|output| output.write_all(b"0000"))
}

/// Frame bytes from one already authenticated complete pack, including its
/// original header/trailer, without hashing or reconstructing it again.
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_pack_chunk(length: usize, band: u32) -> i32 {
    emit(|output| {
        let band = u8::try_from(band).map_err(|_| Error::InvalidRequest)?;
        stored::pack_chunk(band, input(length)?, output)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_ack_start() -> i32 {
    emit(|output| {
        let session = session()?;
        if session.acknowledgments.is_some() {
            return Err(Error::InvalidRequest);
        }
        let mut acknowledgments = session
            .parsed
            .as_ref()
            .ok_or(Error::InvalidRequest)?
            .acknowledgments()?;
        acknowledgments.start(output)?;
        session.acknowledgments = Some(acknowledgments);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn host_upload_shallow_init() -> i32 {
    result((|| {
        let session = session()?;
        if session.shallow_updates.is_some() {
            return Err(Error::InvalidRequest);
        }
        session.shallow_updates = Some(
            session
                .parsed
                .as_ref()
                .ok_or(Error::InvalidRequest)?
                .shallow_updates()?,
        );
        Ok(0)
    })())
}
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_shallow_change(length: usize, unshallow: u32) -> i32 {
    emit(|output| {
        let session = session()?;
        let bytes = input(length)?;
        if unshallow > 1 || bytes.len() != session.format.id_length() {
            return Err(Error::InvalidRequest);
        }
        let id = id_at(bytes, 0, session.format)?;
        session
            .shallow_updates
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .change(id, unshallow != 0, output)
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_shallow_finish() -> i32 {
    emit(|output| {
        session()?
            .shallow_updates
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .finish(output)
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_ack_common(length: usize) -> i32 {
    emit(|output| {
        let session = session()?;
        let bytes = input(length)?;
        if bytes.len() != session.format.id_length() {
            return Err(Error::InvalidRequest);
        }
        let id = id_at(bytes, 0, session.format)?;
        session
            .acknowledgments
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .common(id, output)
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn host_upload_ack_finish() -> i32 {
    emit(|output| {
        session()?
            .acknowledgments
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .finish(output)
    })
}
