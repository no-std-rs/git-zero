//! Opt-in, bounded wall-time observations. Never used for billing or CPU.
//! A missing ledger/field is incomplete evidence, never a zero measurement.
use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_LEDGER: u64 = 16 * 1024 * 1024;
const MAX_STAGES: usize = 24;
static LEDGER: OnceLock<Option<Ledger>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
thread_local! { static REQUEST: RefCell<Option<String>> = const { RefCell::new(None) }; }

struct Ledger {
    start: Instant,
    boot_id: Option<String>,
    boot_ns: Option<u64>,
    calibration_ns: u64,
    file: Mutex<(File, u64, bool)>,
}

fn ledger() -> Option<&'static Ledger> {
    LEDGER
        .get_or_init(|| {
            let path = std::env::var_os("GIT_ZERO_PHASE_LEDGER")?;
            let before = Instant::now();
            // Safe Linux boot-clock anchor, quantized to 10 ms. Relative stages use
            // Instant; cross-process placement is an estimate with explicit error.
            let boot_ns = fs::read_to_string("/proc/uptime")
                .ok()
                .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
                .map(|s| (s * 1_000_000_000.) as u64);
            let start = Instant::now();
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()?;
            let existing = file.metadata().ok()?.len();
            Some(Ledger {
                start,
                boot_ns,
                calibration_ns: 10_000_000 + start.duration_since(before).as_nanos() as u64,
                boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                    .ok()
                    .map(|s| s.trim().into()),
                file: Mutex::new((file, existing, false)),
            })
        })
        .as_ref()
}

pub struct RequestScope(Option<String>);
impl RequestScope {
    pub fn enter() -> Self {
        let id = ledger().map(|_| {
            format!(
                "{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )
        });
        Self(REQUEST.with(|r| r.replace(id)))
    }
}
impl Drop for RequestScope {
    fn drop(&mut self) {
        REQUEST.with(|r| r.replace(self.0.take()));
    }
}

struct Record {
    id: u64,
    kind: &'static str,
    request: Option<String>,
    start_ns: u64,
    stages: Vec<Value>,
    fields: Map<String, Value>,
    outcome: &'static str,
    counts: Option<Counts>,
}

pub struct Span(Option<Record>);
impl Span {
    pub fn new(kind: &'static str) -> Self {
        Self(ledger().map(|l| Record {
            id: SEQUENCE.fetch_add(1, Ordering::Relaxed),
            kind,
            request: REQUEST.with(|r| r.borrow().clone()),
            start_ns: l.start.elapsed().as_nanos() as u64,
            stages: Vec::new(),
            fields: Map::new(),
            outcome: "incomplete",
            counts: None,
        }))
    }
    pub fn mark(&mut self, stage: &'static str) {
        if let Some(r) = &mut self.0 {
            if r.stages.len() < MAX_STAGES {
                r.stages.push(json!({"stage":stage,"process_ns":ledger().unwrap().start.elapsed().as_nanos() as u64,
                    "wire_read_bytes":r.counts.as_ref().map(|c| c.0.read.get()),
                    "wire_written_bytes":r.counts.as_ref().map(|c| c.0.written.get())}));
            } else {
                r.fields.insert("stages_truncated".into(), json!(true));
            }
        }
    }
    pub fn counts(&mut self, counts: &Counts) {
        if let Some(r) = &mut self.0 {
            r.counts = Some(counts.clone());
        }
    }
    pub fn field(&mut self, name: &str, value: impl Into<Value>) {
        if let Some(r) = &mut self.0 {
            r.fields.insert(name.into(), value.into());
        }
    }
    pub fn body(&mut self, prefix: &str, bytes: &[u8]) {
        if self.0.is_some() {
            self.field(&format!("{prefix}_bytes"), bytes.len());
            self.field(&format!("{prefix}_sha256"), crate::hash(bytes));
        }
    }
    pub fn error(&mut self, error: &io::Error) {
        self.field("error_kind", format!("{:?}", error.kind()));
        self.field("raw_os_error", json!(error.raw_os_error()));
        if let Some(r) = &mut self.0 {
            r.outcome = "error";
            r.fields.insert(
                "error_stage".into(),
                r.stages
                    .last()
                    .and_then(|s| s.get("stage"))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
    }
    pub fn finish<T>(&mut self, result: &io::Result<T>) {
        if let Err(e) = result {
            self.error(e);
        }
        if let Some(r) = &mut self.0 {
            if r.outcome != "error" {
                r.outcome = if result.is_ok() { "complete" } else { "error" };
            }
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(counts) = self.0.as_ref().and_then(|r| r.counts.clone()) {
            counts.save(self);
        }
        let Some(r) = self.0.take() else {
            return;
        };
        let l = ledger().unwrap();
        let row = json!({"schema":1,"pid":std::process::id(),"record":r.id,"request":r.request,
            "kind":r.kind,"process_start_ns":r.start_ns,"process_end_ns":l.start.elapsed().as_nanos() as u64,
            "boot_id":l.boot_id,"process_boot_anchor_ns":l.boot_ns,
            "boot_anchor_uncertainty_ns":l.calibration_ns,"stages":r.stages,
            "fields":r.fields,"outcome":r.outcome,"server_cpu_ms":null,"child_cpu_ms":null});
        let Ok(mut bytes) = serde_json::to_vec(&row) else {
            return;
        };
        bytes.push(b'\n');
        if let Ok(mut file) = l.file.lock() {
            if file.2 {
                return;
            }
            if file.1 + bytes.len() as u64 > MAX_LEDGER {
                let _ = file
                    .0
                    .write_all(b"{\"ledger_truncated\":true,\"complete\":false}\n");
                file.2 = true;
            } else if file.0.write_all(&bytes).is_ok() {
                file.1 += bytes.len() as u64;
            } else {
                file.2 = true;
                eprintln!("phase ledger write failed; diagnostics incomplete");
            }
        }
    }
}

#[derive(Default, Clone)]
pub struct Counts(Rc<Counters>);
#[derive(Default)]
struct Counters {
    read: Cell<u64>,
    written: Cell<u64>,
}
impl Counts {
    #[cfg(test)]
    pub fn written(&self) -> u64 {
        self.0.written.get()
    }
    pub fn save(&self, span: &mut Span) {
        span.field("wire_read_bytes", self.0.read.get());
        span.field("wire_written_bytes", self.0.written.get());
    }
}
pub struct Observed<T> {
    pub inner: T,
    pub counts: Counts,
}
impl<T> Observed<T> {
    pub fn new(inner: T, counts: Counts) -> Self {
        Self { inner, counts }
    }
}
impl<T: Read> Read for Observed<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(bytes)?;
        self.counts.0.read.set(self.counts.0.read.get() + n as u64);
        Ok(n)
    }
}
impl<T: Write> Write for Observed<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.counts
            .0
            .written
            .set(self.counts.0.written.get() + n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Observe one synchronous operation, preserving its exact result and errors.
pub fn observe<T>(kind: &'static str, f: impl FnOnce(&mut Span) -> io::Result<T>) -> io::Result<T> {
    let mut span = Span::new(kind);
    let result = f(&mut span);
    if result.is_ok() {
        span.mark("complete");
    }
    span.finish(&result);
    result
}
