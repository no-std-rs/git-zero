//! Bounded immutable raw-object imports, independent of request INPUT storage.
use crate::view::WorkerRepository;
use git_zero::{
    Error, Head, Object, ObjectFormat, ObjectId, ObjectKind, Repository, Result,
    ValidatedRepository,
};

pub(crate) const OBJECTS: usize = 64;
pub(crate) const REFS: usize = 8;
pub(crate) const BODY_CAPACITY: usize = 128 * 1024;
pub(crate) const MANIFEST_CAPACITY: usize = 8192;
const NAME_CAPACITY: usize = 4096;

#[derive(Clone, Copy)]
struct Name {
    start: usize,
    length: usize,
}
impl Name {
    const EMPTY: Self = Self {
        start: 0,
        length: 0,
    };
}

#[derive(Clone, Copy)]
struct Record {
    id: ObjectId,
    kind: ObjectKind,
    start: usize,
    length: usize,
}
impl Record {
    const EMPTY: Self = Self {
        id: ObjectId::Sha1([0; 20]),
        kind: ObjectKind::Blob,
        start: 0,
        length: 0,
    };
}

#[derive(Clone, Copy)]
struct Ref {
    name: Name,
    id: ObjectId,
    symbolic: Option<Name>,
    peeled: Option<ObjectId>,
}
impl Ref {
    const EMPTY: Self = Self {
        name: Name::EMPTY,
        id: ObjectId::Sha1([0; 20]),
        symbolic: None,
        peeled: None,
    };
}

#[derive(Clone, Copy)]
enum StoredHead {
    Unborn(Name),
    Symbolic(Name, ObjectId),
    Detached(ObjectId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Empty,
    Importing,
    Ready,
}

pub(crate) struct Imported {
    bodies: [u8; BODY_CAPACITY],
    names: [u8; NAME_CAPACITY],
    objects: [Record; OBJECTS],
    refs: [Ref; REFS],
    format: ObjectFormat,
    head: StoredHead,
    object_count: usize,
    ref_count: usize,
    names_used: usize,
    required: usize,
    written: usize,
    phase: Phase,
}

impl Imported {
    pub(crate) const fn new() -> Self {
        Self {
            bodies: [0; BODY_CAPACITY],
            names: [0; NAME_CAPACITY],
            objects: [Record::EMPTY; OBJECTS],
            refs: [Ref::EMPTY; REFS],
            format: ObjectFormat::Sha1,
            head: StoredHead::Unborn(Name::EMPTY),
            object_count: 0,
            ref_count: 0,
            names_used: 0,
            required: 0,
            written: 0,
            phase: Phase::Empty,
        }
    }

    pub(crate) fn importing(&self) -> bool {
        self.phase == Phase::Importing
    }

    pub(crate) fn ready(&self) -> bool {
        self.phase == Phase::Ready
    }

    pub(crate) fn reset(&mut self) {
        self.phase = Phase::Empty;
    }

    /// Invalidate the previous import before reading any new manifest bytes.
    pub(crate) fn begin(&mut self, input: &[u8]) -> Result<()> {
        self.phase = Phase::Empty;
        self.names_used = 0;
        self.required = 0;
        self.written = 0;
        if input.len() > MANIFEST_CAPACITY {
            return Err(Error::BufferTooSmall);
        }
        let mut input = Manifest(input);
        if input.take(4)? != b"GZR\0" || input.word()? != 1 || input.word()? != 1 {
            return Err(Error::InvalidRequest);
        }
        self.format = match input.word()? {
            0 => ObjectFormat::Sha1,
            1 => ObjectFormat::Sha256,
            _ => return Err(Error::InvalidRequest),
        };
        self.object_count = input.word()? as usize;
        self.ref_count = input.word()? as usize;
        if self.object_count > OBJECTS || self.ref_count > REFS {
            return Err(Error::BufferTooSmall);
        }
        self.head = match input.word()? {
            0 => StoredHead::Unborn(self.store_name(input.name()?)?),
            1 => {
                let id = input.id(self.format)?;
                StoredHead::Symbolic(self.store_name(input.name()?)?, id)
            }
            2 => StoredHead::Detached(input.id(self.format)?),
            _ => return Err(Error::InvalidRequest),
        };
        for index in 0..self.object_count {
            let kind = match input.word()? {
                1 => ObjectKind::Commit,
                2 => ObjectKind::Tree,
                3 => ObjectKind::Blob,
                4 => ObjectKind::Tag,
                _ => return Err(Error::InvalidRequest),
            };
            let length = input.word()? as usize;
            let start = self.required;
            self.required = start.checked_add(length).ok_or(Error::BufferTooSmall)?;
            if self.required > BODY_CAPACITY {
                return Err(Error::BufferTooSmall);
            }
            self.objects[index] = Record {
                id: input.id(self.format)?,
                kind,
                start,
                length,
            };
        }
        for index in 0..self.ref_count {
            let name = self.store_name(input.name()?)?;
            let id = input.id(self.format)?;
            let symbolic = match input.name()? {
                "" => None,
                value => Some(self.store_name(value)?),
            };
            let peeled = match input.word()? {
                0 => None,
                1 => Some(input.id(self.format)?),
                _ => return Err(Error::InvalidRequest),
            };
            self.refs[index] = Ref {
                name,
                id,
                symbolic,
                peeled,
            };
        }
        if !input.0.is_empty() {
            return Err(Error::InvalidRequest);
        }
        self.phase = Phase::Importing;
        Ok(())
    }

    pub(crate) fn append(&mut self, input: &[u8]) -> Result<()> {
        if !self.importing() {
            return Err(Error::InvalidRequest);
        }
        let end = self
            .written
            .checked_add(input.len())
            .ok_or(Error::BufferTooSmall)?;
        if end > self.required {
            self.phase = Phase::Empty;
            return Err(Error::BufferTooSmall);
        }
        self.bodies[self.written..end].copy_from_slice(input);
        self.written = end;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if !self.importing() {
            return Err(Error::InvalidRequest);
        }
        self.phase = Phase::Empty;
        if self.written != self.required {
            return Err(Error::InvalidRequest);
        }
        ValidatedRepository::new(
            &WorkerRepository::Imported(self),
            &mut [None; OBJECTS],
            &mut [0; OBJECTS],
            &mut [0; 512],
            &mut [0; 512],
        )?;
        self.phase = Phase::Ready;
        Ok(())
    }

    fn store_name(&mut self, value: &str) -> Result<Name> {
        let start = self.names_used;
        let end = start
            .checked_add(value.len())
            .ok_or(Error::BufferTooSmall)?;
        let target = self
            .names
            .get_mut(start..end)
            .ok_or(Error::BufferTooSmall)?;
        target.copy_from_slice(value.as_bytes());
        self.names_used = end;
        Ok(Name {
            start,
            length: value.len(),
        })
    }

    fn name(&self, name: Name) -> &str {
        core::str::from_utf8(&self.names[name.start..name.start + name.length])
            .expect("imported names were validated as UTF-8")
    }

    fn reference(&self, index: usize) -> Option<&Ref> {
        self.refs[..self.ref_count].get(index)
    }
}

impl Repository for Imported {
    fn object_format(&self) -> ObjectFormat {
        self.format
    }
    fn head(&self) -> ObjectId {
        self.head_state().id().unwrap_or(self.format.zero_id())
    }
    fn head_state(&self) -> Head<'_> {
        match self.head {
            StoredHead::Unborn(name) => Head::Unborn(self.name(name)),
            StoredHead::Symbolic(name, id) => Head::Symbolic {
                target: self.name(name),
                id,
            },
            StoredHead::Detached(id) => Head::Detached(id),
        }
    }
    fn head_ref(&self) -> &str {
        self.head_state().target().unwrap_or("")
    }
    fn object_count(&self) -> usize {
        self.object_count
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        let record = self.objects[..self.object_count].get(index)?;
        Some(Object {
            kind: record.kind,
            data: &self.bodies[record.start..record.start + record.length],
        })
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.objects[..self.object_count]
            .get(index)
            .map(|record| record.id)
    }
    fn reference_count(&self) -> usize {
        self.ref_count
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        self.reference(index)
            .map(|reference| self.name(reference.name))
    }
    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        self.reference(index).map(|reference| reference.id)
    }
    fn reference_symbolic_target(&self, index: usize) -> Option<&str> {
        self.reference(index)?.symbolic.map(|name| self.name(name))
    }
    fn reference_peeled(&self, index: usize) -> Option<ObjectId> {
        self.reference(index)?.peeled
    }
}

struct Manifest<'a>(&'a [u8]);
impl<'a> Manifest<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let bytes = self.0.get(..length).ok_or(Error::InvalidRequest)?;
        self.0 = &self.0[length..];
        Ok(bytes)
    }
    fn word(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Error::InvalidRequest)?,
        ))
    }
    fn name(&mut self) -> Result<&'a str> {
        let length = self.word()? as usize;
        core::str::from_utf8(self.take(length)?).map_err(|_| Error::InvalidRequest)
    }
    fn id(&mut self, format: ObjectFormat) -> Result<ObjectId> {
        Ok(match format {
            ObjectFormat::Sha1 => ObjectId::Sha1(
                self.take(20)?
                    .try_into()
                    .map_err(|_| Error::InvalidRequest)?,
            ),
            ObjectFormat::Sha256 => ObjectId::Sha256(
                self.take(32)?
                    .try_into()
                    .map_err(|_| Error::InvalidRequest)?,
            ),
        })
    }
}
