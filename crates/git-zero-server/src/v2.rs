//! Bounded, stateless protocol v2. Validate the whole command before output.
use crate::upload::{self, PackOutput};
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

pub(crate) fn advertise(format: ObjectFormat, output: &mut impl Write) -> Result<()> {
    for line in [
        b"version 2\n".as_slice(),
        b"agent=git-zero/0.1\n",
        b"ls-refs=unborn\n",
        b"fetch\n",
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
        let mut agent = false;
        let mut object_format = None;
        while !bytes.starts_with(b"0001") {
            let cap = data(&mut bytes)?;
            if let Some(value) = cap.strip_prefix(b"agent=") {
                if agent || value.is_empty() || !value.iter().all(|b| (33..=126).contains(b)) {
                    return Err(Error::InvalidRequest);
                }
                agent = true;
            } else if cap.starts_with(b"object-format=") {
                if object_format.replace(cap).is_some() || cap != format.capability() {
                    return Err(Error::InvalidRequest);
                }
            } else {
                return Err(Error::Unsupported);
            }
        }
        if format == ObjectFormat::Sha256 && object_format.is_none() {
            return Err(Error::InvalidRequest);
        }
        let args = &bytes[4..];
        let mut remaining = args;
        loop {
            match pktline::read(&mut remaining)? {
                Some(Packet::Data(_)) => {}
                Some(Packet::Flush) if remaining.is_empty() => {
                    return Ok(Some(Self { name, args }));
                }
                _ => return Err(Error::InvalidRequest),
            }
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
    args: &'a [u8],
    symrefs: bool,
    peel: bool,
    unborn: bool,
}

impl<'a> Refs<'a> {
    pub fn parse(args: &'a [u8]) -> Result<Self> {
        let mut refs = Self {
            args,
            symrefs: false,
            peel: false,
            unborn: false,
        };
        for arg in arguments(args) {
            match arg {
                b"symrefs" => refs.symrefs = true,
                b"peel" => refs.peel = true,
                b"unborn" => refs.unborn = true,
                _ if arg.starts_with(b"ref-prefix ") && !arg.contains(&0) => {}
                _ => return Err(Error::Unsupported),
            }
        }
        Ok(refs)
    }

    fn matches(&self, name: &str) -> bool {
        let mut prefixes = arguments(self.args)
            .filter_map(|arg| arg.strip_prefix(b"ref-prefix "))
            .peekable();
        prefixes.peek().is_none() || prefixes.any(|prefix| name.as_bytes().starts_with(prefix))
    }

    fn write_ref(
        &self,
        id: Option<ObjectId>,
        name: &str,
        symbolic: Option<&str>,
        peeled: Option<ObjectId>,
        output: &mut impl Write,
    ) -> Result<()> {
        if !self.matches(name) {
            return Ok(());
        }
        let hex = id.map(|id| id.to_hex());
        let id = hex
            .as_ref()
            .map_or(b"unborn".as_slice(), |hex| hex.as_ref());
        let peeled_hex = peeled.filter(|_| self.peel).map(|id| id.to_hex());
        let symbolic = symbolic.filter(|_| self.symrefs);
        pktline::write_parts(
            output,
            &[
                id,
                b" ",
                name.as_bytes(),
                if symbolic.is_some() {
                    b" symref-target:"
                } else {
                    b""
                },
                symbolic.unwrap_or("").as_bytes(),
                if peeled_hex.is_some() {
                    b" peeled:"
                } else {
                    b""
                },
                peeled_hex
                    .as_ref()
                    .map_or(b"".as_slice(), |hex| hex.as_ref()),
                b"\n",
            ],
        )
    }

    pub fn write(&self, repository: &impl Repository, output: &mut impl Write) -> Result<()> {
        let head = repository.head_state();
        if head.id().is_some() || self.unborn {
            let target = match head {
                Head::Symbolic { target, .. } => Some(resolve_reference(repository, target)?.0),
                _ => head.target(),
            };
            self.write_ref(head.id(), "HEAD", target, None, output)?;
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
                name,
                symbolic,
                repository.reference_peeled(index),
                output,
            )?;
        }
        output.write_all(b"0000")
    }
}

pub(crate) struct Fetch<'a> {
    args: &'a [u8],
    pub done: bool,
    pub include_tags: bool,
}

impl<'a> Fetch<'a> {
    pub fn parse(
        args: &'a [u8],
        format: ObjectFormat,
        mut want: impl FnMut(ObjectId) -> Result<()>,
    ) -> Result<Self> {
        let mut fetch = Self {
            args,
            done: false,
            include_tags: false,
        };
        let mut wanted = false;
        for arg in arguments(args) {
            if let Some(hex) = arg.strip_prefix(b"want ") {
                want(upload::id(hex, format)?)?;
                wanted = true;
            } else if let Some(hex) = arg.strip_prefix(b"have ") {
                upload::id(hex, format)?;
            } else {
                match arg {
                    b"done" if !fetch.done => fetch.done = true,
                    b"include-tag" => fetch.include_tags = true,
                    // Full, non-delta packs also satisfy thin-pack/ofs-delta requests.
                    b"thin-pack" | b"ofs-delta" | b"no-progress" => {}
                    _ => return Err(Error::Unsupported),
                }
            }
        }
        if !wanted {
            return Err(Error::InvalidRequest);
        }
        Ok(fetch)
    }

    pub fn common<'r>(
        &'r self,
        repository: &'r impl Repository,
    ) -> impl Iterator<Item = Result<ObjectId>> + 'r {
        arguments(self.args)
            .filter_map(|arg| arg.strip_prefix(b"have "))
            .filter_map(|hex| ObjectId::from_hex(hex).ok())
            .filter_map(move |id| upload::common_commit(repository, id).transpose())
    }

    pub fn acknowledge(&self, repository: &impl Repository, output: &mut impl Write) -> Result<()> {
        if self.done {
            return pktline::write(output, b"packfile\n");
        }
        pktline::write(output, b"acknowledgments\n")?;
        let mut known = false;
        for id in self.common(repository) {
            pktline::write_parts(output, &[b"ACK ", &id?.to_hex(), b"\n"])?;
            known = true;
        }
        if !known {
            pktline::write(output, b"NAK\n")?
        }
        // Wait for done; never claim an early sufficient cut point.
        output.write_all(b"0000")
    }

    pub fn output<'w, W: Write>(&self, output: &'w mut W) -> PackOutput<'w, W> {
        PackOutput::sideband(output)
    }
}
