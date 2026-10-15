//! Bounded raw-fixture loading shared by native reference tests and upstream CGI.
//! Files come from the independent tests/server_fixture.py exporter.
use git_zero::{Head, ObjectId, ObjectKind, Reference};
use git_zero_rustix::FileObject;
use std::{io::Read, path::Path};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const OBJECTS: usize = 256;
pub const OBJECT_BYTES: u64 = 8 * 1024 * 1024;

pub struct Fixture {
    objects: Vec<FileObject>,
    refs: String,
    head: String,
}

impl Fixture {
    pub fn load(root: &Path) -> Result<Self> {
        Ok(Self {
            objects: objects(root)?,
            refs: bounded_text(&root.join("refs.txt"))?,
            head: bounded_text(&root.join("head.txt"))?,
        })
    }

    pub fn objects(&self) -> &[FileObject] {
        &self.objects
    }

    pub fn references(&self) -> Result<Vec<Reference<'_>>> {
        references(&self.refs)
    }

    pub fn head(&self) -> Result<(ObjectId, Head<'_>)> {
        head(&self.head)
    }
}

pub fn core_result<T>(result: git_zero::Result<T>, context: &str) -> Result<T> {
    result.map_err(|error| format!("{context}: {error:?}").into())
}
fn bounded_text(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err("fixture metadata exceeds limit".into());
    }
    Ok(String::from_utf8(bytes)?)
}

fn object_id(text: &str) -> Result<ObjectId> {
    core_result(ObjectId::from_hex(text.as_bytes()), "object ID")
}

fn object_kind(text: &str) -> Result<ObjectKind> {
    match text {
        "blob" => Ok(ObjectKind::Blob),
        "tree" => Ok(ObjectKind::Tree),
        "commit" => Ok(ObjectKind::Commit),
        "tag" => Ok(ObjectKind::Tag),
        _ => Err("unknown fixture object kind".into()),
    }
}

fn objects(root: &Path) -> Result<Vec<FileObject>> {
    let mut result = Vec::new();
    let mut total = 0;
    for row in bounded_text(&root.join("objects.txt"))?.lines() {
        if result.len() == OBJECTS {
            return Err("fixture object count exceeds limit".into());
        }
        let (hex, kind) = row.split_once('\t').ok_or("invalid object row")?;
        let id = object_id(hex)?;
        let path = root.join("raw").join(hex);
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        let object = core_result(
            FileObject::open_at(rustix::fs::CWD, path.as_c_str(), object_kind(kind)?, id),
            "file object",
        )?;
        total += object.info().size;
        if total > OBJECT_BYTES {
            return Err("fixture object bytes exceed limit".into());
        }
        result.push(object);
    }
    Ok(result)
}

fn references(text: &str) -> Result<Vec<Reference<'_>>> {
    let mut result = Vec::new();
    for row in text.lines() {
        if result.len() == 32 {
            return Err("fixture ref count exceeds limit".into());
        }
        let [name, hex, symbolic, peeled]: [_; 4] = row
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "invalid ref row")?;
        result.push(Reference {
            name,
            id: object_id(hex)?,
            symbolic_target: (symbolic != "-").then_some(symbolic),
            peeled: (peeled != "-").then(|| object_id(peeled)).transpose()?,
        });
    }
    Ok(result)
}

fn head(text: &str) -> Result<(ObjectId, Head<'_>)> {
    let mut lines = text.lines();
    let id = object_id(lines.next().ok_or("missing HEAD")?)?;
    let target = lines.next().unwrap_or("");
    let state = if id.is_zero() {
        Head::Unborn(target)
    } else if target.is_empty() {
        Head::Detached(id)
    } else {
        Head::Symbolic { target, id }
    };
    Ok((id, state))
}
