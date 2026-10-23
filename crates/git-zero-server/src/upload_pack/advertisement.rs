//! Upload-pack discovery and reference advertisements for HTTP and protocol v2.
use crate::{Response, route, route::Version, v2};
use git_zero_core::{
    Error, Head, ObjectId, Repository, Result, io::Write, pktline, repository::resolve_reference,
};

pub(crate) fn serve_info_refs(
    repository: &impl Repository,
    route: &route::Route<'_>,
    output: &mut impl Write,
    selection: bool,
) -> Result<Response> {
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    match route.service {
        Some("git-upload-pack") => {
            response.content_type = "application/x-git-upload-pack-advertisement";
            advertise_smart(repository, route.version, output, selection)?;
        }
        None => advertise_references(repository, false, output, super::capabilities(selection))?,
        _ => response.status = 403,
    }
    Ok(response)
}

fn advertise_smart(
    repository: &impl Repository,
    version: Version,
    output: &mut impl Write,
    selection: bool,
) -> Result<()> {
    advertise_smart_with_capabilities(
        repository,
        version,
        output,
        selection,
        super::capabilities(selection),
    )
}

fn advertise_smart_with_capabilities(
    repository: &impl Repository,
    version: Version,
    output: &mut impl Write,
    selection: bool,
    capabilities: &[u8],
) -> Result<()> {
    pktline::write(output, b"# service=git-upload-pack\n")?;
    output.write_all(b"0000")?;
    if version == Version::V2 {
        return v2::advertise(repository.object_format(), output, selection);
    }
    if version == Version::V1 {
        pktline::write(output, b"version 1\n")?;
    }
    if repository.head_state().id().is_some() || repository.reference_count() == 0 {
        advertise_head(repository, output, capabilities)?;
    }
    advertise_references(repository, true, output, capabilities)?;
    output.write_all(b"0000")
}

fn advertise_head(
    repository: &impl Repository,
    output: &mut impl Write,
    capabilities: &[u8],
) -> Result<()> {
    let head = repository.head_state();
    let format = repository.object_format();
    let id = head.id().unwrap_or(format.zero_id()).to_hex();
    let name = if head.id().is_some() {
        b" HEAD\0".as_slice()
    } else {
        b" capabilities^{}\0"
    };
    let target = match head {
        Head::Symbolic { target, .. } => Some(resolve_reference(repository, target)?.0),
        Head::Unborn(target) => Some(target),
        Head::Detached(_) => None,
    };
    let (symref, separator): (&[u8], &[u8]) = match target {
        Some(_) => (b"symref=HEAD:", b" "),
        None => (b"", b""),
    };
    pktline::write_parts(
        output,
        &[
            &id,
            name,
            symref,
            target.unwrap_or("").as_bytes(),
            separator,
            format.capability(),
            capabilities,
            b"\n",
        ],
    )
}

fn advertise_references(
    repository: &impl Repository,
    smart: bool,
    output: &mut impl Write,
    capabilities_bytes: &[u8],
) -> Result<()> {
    for index in 0..repository.reference_count() {
        let name = repository
            .reference_name(index)
            .ok_or(Error::InvalidReference)?;
        let id = repository.reference_id(index).ok_or(Error::InvalidObject)?;
        let capabilities = smart && index == 0 && repository.head_state().id().is_none();
        reference_line(
            id,
            name,
            b"",
            smart,
            if capabilities {
                capabilities_bytes
            } else {
                b""
            },
            output,
        )?;
        if let Some(peeled) = repository.reference_peeled(index) {
            reference_line(peeled, name, b"^{}", smart, b"", output)?;
        }
    }
    Ok(())
}

fn reference_line(
    id: ObjectId,
    name: &str,
    suffix: &[u8],
    smart: bool,
    capabilities: &[u8],
    output: &mut impl Write,
) -> Result<()> {
    let format = id.format();
    let id = id.to_hex();
    if smart {
        let parts: [&[u8]; 3] = if !capabilities.is_empty() {
            [b"\0", format.capability(), capabilities]
        } else {
            [b"", b"", b""]
        };
        pktline::write_parts(
            output,
            &[
                &id,
                b" ",
                name.as_bytes(),
                suffix,
                parts[0],
                parts[1],
                parts[2],
                b"\n",
            ],
        )
    } else {
        output.write_all(&id)?;
        output.write_all(b"\t")?;
        output.write_all(name.as_bytes())?;
        output.write_all(suffix)?;
        output.write_all(b"\n")
    }
}

#[cfg(feature = "read")]
pub(crate) fn serve_revision_info_refs(
    repository: &impl Repository,
    route: &route::Route<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    if route.service != Some("git-upload-pack") {
        return serve_info_refs(repository, route, output, true);
    }
    const EXTRA: &[u8] = b" deepen-since deepen-not";
    let ordinary = git_zero_core::repository::UPLOAD_CAPABILITIES;
    let mut capabilities = [0; git_zero_core::repository::UPLOAD_CAPABILITIES.len() + EXTRA.len()];
    capabilities[..ordinary.len()].copy_from_slice(ordinary);
    capabilities[ordinary.len()..].copy_from_slice(EXTRA);
    serve_capability_refs(repository, route.version, output, &capabilities)
}

#[cfg(feature = "read")]
pub(crate) fn serve_capability_refs(
    repository: &impl Repository,
    version: Version,
    output: &mut impl Write,
    capabilities: &[u8],
) -> Result<Response> {
    advertise_smart_with_capabilities(
        repository,
        version,
        &mut super::policy::Discard,
        true,
        capabilities,
    )
    .map_err(|error| {
        if error == Error::InvalidPacket {
            Error::LimitExceeded
        } else {
            error
        }
    })?;
    advertise_smart_with_capabilities(repository, version, output, true, capabilities)?;
    Ok(Response {
        status: 200,
        content_type: "application/x-git-upload-pack-advertisement",
    })
}
