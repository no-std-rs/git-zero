use git_zero::{Object, ObjectId, ObjectKind, Repository, io::Write, pack, zlib};
use std::{hint::black_box, path::Path, time::Instant};

struct Objects<'a> {
    head: ObjectId,
    objects: Vec<Object<'a>>,
}

impl Repository for Objects<'_> {
    fn head(&self) -> ObjectId {
        self.head
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).copied()
    }
}

struct Output(Vec<u8>);
impl Write for Output {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

fn core_error(error: git_zero::Error) -> std::io::Error {
    std::io::Error::other(format!("{error:?}"))
}

fn cpu() -> std::io::Result<f64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: the pointer is aligned and exclusively borrowed. A successful
    // getrusage initializes the complete structure before assume_init.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let usage = unsafe { usage.assume_init() };
    Ok(usage.ru_utime.tv_sec as f64
        + usage.ru_stime.tv_sec as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1e6)
}

// Deliberately slow scalar reference, to isolate the old checksum cost from
// copying and the current encoder without relying on OS sampling permissions.
fn scalar_adler(bytes: &[u8]) -> u32 {
    let (mut first, mut second) = (1_u32, 0_u32);
    for &byte in bytes {
        first = (first + u32::from(byte)) % 65521;
        second = (second + first) % 65521;
    }
    (second << 16) | first
}

fn timed(
    name: &str,
    repetitions: usize,
    mut operation: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    operation()?;
    for round in 0..5 {
        let wall = Instant::now();
        let before = cpu()?;
        for _ in 0..repetitions {
            operation()?;
        }
        let elapsed_cpu = cpu()? - before;
        println!(
            "{{\"mode\":\"{name}\",\"round\":{round},\"iterations\":{repetitions},\"cpu_ms_per_graph\":{},\"wall_ms_per_graph\":{}}}",
            elapsed_cpu * 1000.0 / repetitions as f64,
            wall.elapsed().as_secs_f64() * 1000.0 / repetitions as f64
        );
    }
    Ok(())
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: encoder-profile FIXTURE REPETITIONS".into());
    }
    let root = Path::new(&args[1]);
    let repetitions: usize = args[2].parse()?;
    if repetitions == 0 {
        return Err("repetitions must be positive".into());
    }
    let raw = std::fs::read(root.join("objects.raw"))?;
    let manifest = std::fs::read_to_string(root.join("objects.tsv"))?;
    let mut lines = manifest.lines();
    let head = ObjectId::from_hex(lines.next().ok_or("missing manifest HEAD")?.as_bytes())
        .map_err(core_error)?;
    let mut objects = Objects {
        head,
        objects: Vec::new(),
    };
    for line in lines {
        let columns: Vec<_> = line.split('\t').collect();
        if columns.len() != 4 {
            return Err("invalid manifest row".into());
        }
        let offset: usize = columns[2].parse()?;
        let size: usize = columns[3].parse()?;
        let end = offset.checked_add(size).ok_or("object range overflow")?;
        let object = Object {
            kind: match columns[1] {
                "blob" => ObjectKind::Blob,
                "tree" => ObjectKind::Tree,
                "commit" => ObjectKind::Commit,
                "tag" => ObjectKind::Tag,
                _ => return Err("unsupported manifest kind".into()),
            },
            data: raw
                .get(offset..end)
                .ok_or("object range outside raw export")?,
        };
        if object.id() != ObjectId::from_hex(columns[0].as_bytes()).map_err(core_error)? {
            return Err("object body does not match manifest ID".into());
        }
        objects.objects.push(object);
    }
    let blob = Object {
        kind: ObjectKind::Blob,
        data: &raw,
    };
    println!(
        "{{\"event\":\"fixture\",\"objects\":{},\"raw_bytes\":{},\"repetitions\":{repetitions},\"blob_oid\":\"{}\"}}",
        objects.object_count(),
        raw.len(),
        blob.id()
    );
    let mut output = Output(Vec::new());
    timed("copy_to_vec", repetitions, || {
        for object in &objects.objects {
            output.0.clear();
            output
                .write_all(black_box(object.data))
                .map_err(core_error)?;
            black_box(&output.0);
        }
        Ok(())
    })?;
    timed("scalar_adler_reference", repetitions, || {
        black_box(scalar_adler(black_box(&raw)));
        Ok(())
    })?;
    timed("stored_zlib_to_vec", repetitions, || {
        for object in &objects.objects {
            output.0.clear();
            zlib::write_parts(&mut output, &[black_box(object.data)]).map_err(core_error)?;
            black_box(&output.0);
        }
        Ok(())
    })?;
    timed("loose_to_vec", repetitions, || {
        for object in &objects.objects {
            output.0.clear();
            pack::write_loose(black_box(*object), &mut output).map_err(core_error)?;
            black_box(&output.0);
        }
        Ok(())
    })?;
    timed("pack_to_vec", repetitions, || {
        output.0.clear();
        pack::write(&objects, &mut output).map_err(core_error)?;
        black_box(&output.0);
        Ok(())
    })?;
    pack::validate(&output.0).map_err(core_error)?;
    timed("blob_sha1", repetitions, || {
        black_box(black_box(blob).id());
        Ok(())
    })?;
    Ok(())
}
