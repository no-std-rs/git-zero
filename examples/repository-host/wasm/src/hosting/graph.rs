use super::{Frame, Shared, code, finish, format, input, kind, output, u32_at, u64_at};
use core::cell::UnsafeCell;
use git_zero::{
    Error, ObjectDigest, ObjectInfo, ObjectKind,
    http::receive::{FsckHeaders, check_tree_entry},
    object::{LinkDecoder, LinkLimits, LinkRelation, LinkStatus},
};

static NAMES: Shared<[u8; 131072]> = Shared(UnsafeCell::new([0; 131072]));
static GRAPH: Shared<Option<Session>> = Shared(UnsafeCell::new(None));
struct Session {
    decoder: LinkDecoder<'static>,
    digest: ObjectDigest,
    headers: Option<FsckHeaders>,
    strict: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn host_graph_cancel() {
    // SAFETY: Exclusive call releases NAMES before reuse.
    unsafe {
        *GRAPH.0.get() = None;
    }
}

/// Init (40 bytes): format/kind u32@0/4; size u64@8; max bytes/links
/// u64@16/24; name scratch capacity u32@32 (two names, at most 131072),
/// strict receive header/tree policy u32@36 (0 or 1).
#[unsafe(no_mangle)]
pub extern "C" fn host_graph_init(length: usize) -> i32 {
    host_graph_cancel();
    let result = initialize(length);
    result.unwrap_or_else(code)
}

/// Tags: 30 need input; 31 typed link; 32 tree entry; 33 canonical ID.
/// Link kind is @12, relation @16. Tree kind is flags low byte (0=gitlink),
/// canonical mode is flags bit 8, zero padding is bit 9, designated blob checks
/// are bits 10/11 (gitmodules/gitattributes), mode is @16, name is payload.
#[unsafe(no_mangle)]
pub extern "C" fn host_graph_feed(length: usize, end_of_input: u32) -> i32 {
    let result = feed(length, end_of_input);
    finish(result, || host_graph_cancel())
}

fn initialize(length: usize) -> git_zero::Result<i32> {
    let bytes = input(length)?;
    if bytes.len() != 40 {
        return Err(Error::InvalidRequest);
    }
    let format = format(u32_at(bytes, 0)?)?;
    let info = ObjectInfo {
        kind: kind(u32_at(bytes, 4)?)?,
        size: u64_at(bytes, 8)?,
    };
    let limits = LinkLimits {
        max_body_bytes: u64_at(bytes, 16)?,
        max_links: u64_at(bytes, 24)?,
    };
    let capacity = u32_at(bytes, 32)? as usize;
    let strict = u32_at(bytes, 36)?;
    if strict > 1 {
        return Err(Error::InvalidRequest);
    }
    if capacity > 131072 {
        return Err(Error::BufferTooSmall);
    }
    // SAFETY: cancel released old names. The session owns NAMES alone;
    // neither scanner nor digest retains INPUT/OUTPUT references.
    unsafe {
        *GRAPH.0.get() = Some(Session {
            decoder: LinkDecoder::new(
                info,
                format,
                &mut (&mut *NAMES.0.get())[..capacity],
                limits,
            )?,
            digest: ObjectDigest::new(info, format),
            headers: if strict == 1 && matches!(info.kind, ObjectKind::Commit | ObjectKind::Tag) {
                Some(FsckHeaders::new(info.kind)?)
            } else {
                None
            },
            strict: strict == 1,
        });
    }
    Ok(0)
}

fn feed(length: usize, end_of_input: u32) -> git_zero::Result<i32> {
    if end_of_input > 1 {
        return Err(Error::InvalidRequest);
    }
    let bytes = input(length)?;
    // SAFETY: Exclusive session access with disjoint INPUT/OUTPUT/NAMES.
    let session = unsafe {
        (&mut *GRAPH.0.get())
            .as_mut()
            .ok_or(Error::InvalidRequest)?
    };
    let step = session.decoder.update(bytes, end_of_input != 0)?;
    let consumed = step.consumed;
    if let Some(headers) = &mut session.headers {
        headers.update(&bytes[..consumed])?;
    }
    session.digest.update(&bytes[..consumed])?;
    let mut frame = Frame::new(output());
    let (tag, written) = match step.status {
        LinkStatus::NeedInput => (30, 0),
        LinkStatus::Link { id, kind, relation } => {
            frame.id(0, id);
            frame.word(12, kind as u32);
            frame.wide(
                16,
                match relation {
                    LinkRelation::RootTree => 0,
                    LinkRelation::Parent => 1,
                    LinkRelation::TagTarget => 2,
                },
            );
            (31, 0)
        }
        LinkStatus::TreeEntry {
            id,
            mode,
            name,
            kind,
            canonical_mode,
            zero_padded,
        } => {
            let checks = if session.strict {
                check_tree_entry(name, mode, zero_padded)?.bits()
            } else {
                0
            };
            frame.id(0, id);
            frame.word(
                12,
                kind.map_or(0, |kind| kind as u32)
                    | (u32::from(canonical_mode) << 8)
                    | (u32::from(zero_padded) << 9)
                    | (u32::from(checks) << 10),
            );
            frame.wide(16, u64::from(mode));
            frame.data(name)?;
            (32, name.len())
        }
        LinkStatus::Finished => {
            session.decoder.finish()?;
            if let Some(headers) = &session.headers {
                headers.finish()?;
            }
            frame.id(0, session.digest.finish()?);
            (33, 0)
        }
    };
    Ok(frame.finish(tag, consumed, written))
}
