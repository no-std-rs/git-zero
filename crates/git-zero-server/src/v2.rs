//! Bounded, stateless protocol v2. Validate the whole command before output.
use crate::upload;
use git_zero_core::{
    Error, Head, ObjectFormat, ObjectId, Repository, Result,
    io::Write,
    pktline::{self, Packet},
    repository::resolve_reference,
};

pub(crate) fn serve_refs(
    repository: &impl Repository,
    body: &[u8],
    output: &mut impl Write,
) -> Result<bool> {
    match Command::parse(body, repository.object_format())? {
        None => Ok(true),
        Some(command) if command.name == b"ls-refs" => {
            Refs::parse(command.args)?.write(repository, output)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn advertise(
    format: ObjectFormat,
    output: &mut impl Write,
    selection: bool,
) -> Result<()> {
    for line in [
        b"version 2\n".as_slice(),
        b"agent=git-zero/0.1\n",
        b"ls-refs=unborn\n",
        if selection {
            b"fetch=shallow filter\n"
        } else {
            b"fetch\n"
        },
    ] {
        pktline::write(output, line)?;
    }
    pktline::write_parts(output, &[format.capability(), b"\n"])?;
    output.write_all(b"0000")
}

fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

fn data<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8]> {
    match pktline::read(bytes)? {
        Some(Packet::Data(value)) => Ok(line(value)),
        _ => Err(Error::InvalidRequest),
    }
}

pub(crate) struct Command<'a> {
    pub name: &'a [u8],
    pub args: &'a [u8],
}

impl<'a> Command<'a> {
    pub fn parse(mut bytes: &'a [u8], format: ObjectFormat) -> Result<Option<Self>> {
        if bytes == b"0000" {
            return Ok(None);
        }
        let name = data(&mut bytes)?
            .strip_prefix(b"command=")
            .ok_or(Error::InvalidRequest)?;
        if !matches!(name, b"ls-refs" | b"fetch") {
            return Err(Error::Unsupported);
        }
        let args = command_arguments(bytes, format)?;
        validate_arguments(args)?;
        Ok(Some(Self { name, args }))
    }
}

fn command_arguments(mut bytes: &[u8], format: ObjectFormat) -> Result<&[u8]> {
    let mut agent = false;
    let mut format_seen = false;
    while !bytes.starts_with(b"0001") {
        let cap = data(&mut bytes)?;
        if let Some(value) = cap.strip_prefix(b"agent=") {
            if agent || value.is_empty() || !value.iter().all(|b| (33..=126).contains(b)) {
                return Err(Error::InvalidRequest);
            }
            agent = true;
        } else if cap.starts_with(b"object-format=") {
            if format_seen || cap != format.capability() {
                return Err(Error::InvalidRequest);
            }
            format_seen = true;
        } else {
            return Err(Error::Unsupported);
        }
    }
    if format == ObjectFormat::Sha256 && !format_seen {
        return Err(Error::InvalidRequest);
    }
    Ok(&bytes[4..])
}

fn validate_arguments(mut bytes: &[u8]) -> Result<()> {
    loop {
        match pktline::read(&mut bytes)? {
            Some(Packet::Data(_)) => {}
            Some(Packet::Flush) if bytes.is_empty() => return Ok(()),
            _ => return Err(Error::InvalidRequest),
        }
    }
}

fn arguments(mut bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    core::iter::from_fn(move || match pktline::read(&mut bytes).ok().flatten() {
        Some(Packet::Data(value)) => Some(line(value)),
        _ => None,
    })
}

pub(crate) struct Refs<'a> {
    prefixes: [&'a [u8]; 256],
    prefix_count: usize,
    symrefs: bool,
    peel: bool,
    unborn: bool,
}

struct Discard;
impl Write for Discard {
    fn write_all(&mut self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }
}

fn attribute<'a>(marker: &'a [u8], value: Option<&'a [u8]>) -> [&'a [u8]; 2] {
    match value {
        Some(value) => [marker, value],
        None => [b"", b""],
    }
}

impl<'a> Refs<'a> {
    pub fn parse(args: &'a [u8]) -> Result<Self> {
        let mut refs = Self {
            prefixes: [b""; 256],
            prefix_count: 0,
            symrefs: false,
            peel: false,
            unborn: false,
        };
        for arg in arguments(args) {
            match arg {
                b"symrefs" => refs.symrefs = true,
                b"peel" => refs.peel = true,
                b"unborn" => refs.unborn = true,
                _ if arg.starts_with(b"ref-prefix ") && !arg.contains(&0) => {
                    refs.prefix(&arg[b"ref-prefix ".len()..])?;
                }
                _ => return Err(Error::Unsupported),
            }
        }
        Ok(refs)
    }

    fn matches(&self, name: &[u8]) -> bool {
        let prefixes = &self.prefixes[..self.prefix_count];
        let end = prefixes.partition_point(|prefix| *prefix <= name);
        prefixes.is_empty() || (end > 0 && name.starts_with(prefixes[end - 1]))
    }

    // Keep disjoint prefixes in byte order. Duplicate and contained arguments
    // consume no slots, and each catalog reference needs one binary lookup.
    fn prefix(&mut self, prefix: &'a [u8]) -> Result<()> {
        let index = match self.prefixes[..self.prefix_count].binary_search(&prefix) {
            Ok(_) => return Ok(()),
            Err(index) => index,
        };
        if index > 0 && prefix.starts_with(self.prefixes[index - 1]) {
            return Ok(());
        }
        let end = index
            + self.prefixes[index..self.prefix_count]
                .partition_point(|existing| existing.starts_with(prefix));
        let retained = self.prefix_count - (end - index);
        if retained == self.prefixes.len() {
            return Err(Error::LimitExceeded);
        }
        self.prefixes.copy_within(end..self.prefix_count, index + 1);
        self.prefixes[index] = prefix;
        self.prefix_count = retained + 1;
        Ok(())
    }

    pub(crate) fn write_ref(
        &self,
        id: Option<ObjectId>,
        name: &[u8],
        symbolic: Option<&[u8]>,
        peeled: Option<ObjectId>,
        output: &mut impl Write,
    ) -> Result<()> {
        if id.is_none() && !(self.unborn && self.symrefs) {
            return Ok(());
        }
        if !self.matches(name) {
            return Ok(());
        }
        let hex = id.map(|id| id.to_hex());
        let id = hex
            .as_ref()
            .map_or(b"unborn".as_slice(), |hex| hex.as_ref());
        let peeled_hex = peeled.filter(|_| self.peel).map(|id| id.to_hex());
        let [symbolic_marker, symbolic] =
            attribute(b" symref-target:", symbolic.filter(|_| self.symrefs));
        let [peeled_marker, peeled] =
            attribute(b" peeled:", peeled_hex.as_ref().map(|hex| hex.as_ref()));
        pktline::write_parts(
            output,
            &[
                id,
                b" ",
                name,
                symbolic_marker,
                symbolic,
                peeled_marker,
                peeled,
                b"\n",
            ],
        )
    }

    pub fn write(&self, repository: &impl Repository, output: &mut impl Write) -> Result<()> {
        // Reuse the exact encoder to preflight every selected line, including
        // optional attributes. Valid v0 ref names can exceed v2's packet budget.
        self.write_refs(repository, &mut Discard)
            .map_err(|error| match error {
                Error::InvalidPacket => Error::LimitExceeded,
                error => error,
            })?;
        self.write_refs(repository, output)
    }

    fn write_refs(&self, repository: &impl Repository, output: &mut impl Write) -> Result<()> {
        let head = repository.head_state();
        if head.id().is_some() || (self.unborn && self.symrefs) {
            let target = match head {
                Head::Symbolic { target, .. } => Some(resolve_reference(repository, target)?.0),
                _ => head.target(),
            };
            self.write_ref(head.id(), b"HEAD", target.map(str::as_bytes), None, output)?;
        }
        for index in 0..repository.reference_count() {
            let name = repository
                .reference_name(index)
                .ok_or(Error::InvalidReference)?;
            let symbolic = repository
                .reference_symbolic_target(index)
                .map(|_| resolve_reference(repository, name).map(|resolved| resolved.0))
                .transpose()?;
            self.write_ref(
                repository.reference_id(index),
                name.as_bytes(),
                symbolic.map(str::as_bytes),
                repository.reference_peeled(index),
                output,
            )?;
        }
        output.write_all(b"0000")
    }
}

pub(crate) struct Fetch {
    pub done: bool,
    pub include_tags: bool,
    pub ofs_delta: bool,
}

impl Fetch {
    pub fn parse(
        args: &[u8],
        format: ObjectFormat,
        mut want: impl FnMut(ObjectId) -> Result<()>,
        extensions: &mut crate::fetch::Extensions,
    ) -> Result<Self> {
        let mut fetch = Self {
            done: false,
            include_tags: false,
            ofs_delta: false,
        };
        let mut wanted = false;
        for arg in arguments(args) {
            match crate::fetch::argument_parts(arg) {
                (b"want", Some(hex)) => {
                    want(upload::id(hex, format)?)?;
                    wanted = true;
                }
                (b"have", Some(hex)) => {
                    upload::id(hex, format)?;
                }
                (b"done", None) if !fetch.done => fetch.done = true,
                (b"include-tag", None) => fetch.include_tags = true,
                (b"deepen-relative", None) if !extensions.options.deepen_relative => {
                    extensions.options.deepen_relative = true;
                }
                (b"ofs-delta", None) => fetch.ofs_delta = true,
                // Full packs also satisfy thin-pack requests.
                (b"thin-pack" | b"no-progress", None) => {}
                _ => extensions.argument(arg, format)?,
            }
        }
        if !wanted {
            return Err(Error::InvalidRequest);
        }
        extensions.require_filter_capability(true)?;
        Ok(fetch)
    }
}
