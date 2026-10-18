//! Linux-only watchdog and bounded diagnostic executor. Runtime GO is separate.
#[path = "fetch-phase-supervisor/replay.rs"]
mod replay;
#[path = "fetch-phase-supervisor/runtime.rs"]
mod runtime;
use git_zero_host::{benchmark_cost::interleaved_schedule, hash};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const OUTPUT_CAP: usize = 128 * 1024;
const STAGE1: &str = "a827df032e87374737b1ac7e36fb518b507e8411";
const IMMUTABLE: &str =
    "/tmp/git-zero-fetch-diagnostics-immutable/stage1-a827df032e87374737b1ac7e36fb518b507e8411-v1";

// Root approved these three necessary Linux supervisor-only calls. No Native,
// library, hook, dependency, timeout or transport-policy change is involved.
#[cfg(target_os = "linux")]
mod linux {
    use std::{
        ffi::{c_int, c_ulong},
        io,
    };
    unsafe extern "C" {
        fn prctl(option: c_int, ...) -> c_int;
        fn kill(pid: c_int, signal: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    }
    pub fn subreaper() -> io::Result<()> {
        // SAFETY: Linux PR_SET_CHILD_SUBREAPER=36 accepts unsigned-long arguments;
        // no pointers. This dedicated process owns only one fresh case at a time.
        if unsafe { prctl(36, 1 as c_ulong, 0 as c_ulong, 0 as c_ulong, 0 as c_ulong) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub fn signal_validated_group(group: i32, signal: i32) -> io::Result<bool> {
        assert!(group > 1 && matches!(signal, 0 | 15 | 9));
        // SAFETY: caller has freshly verified generation-stamped owned group
        // members and excluded the supervisor's group. Negative PID means PGID.
        let result = unsafe { kill(-group, signal) };
        if result == 0 {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(3) {
            Ok(false)
        } else {
            Err(e)
        } // ESRCH differs from EPERM.
    }
    pub fn reap_adopted_exact(pid: i32) -> io::Result<Option<i32>> {
        assert!(pid > 1);
        let mut status: c_int = 0;
        // SAFETY: exact PID was just verified as our adopted child in the owned
        // group. WNOHANG=1 cannot block; initialized writable C-int status.
        let result = unsafe { waitpid(pid, &mut status, 1) };
        if result == pid {
            return Ok(Some(status));
        }
        if result == 0 {
            return Ok(None);
        }
        let e = io::Error::last_os_error();
        // EINTR is surfaced to the bounded polling loop, not an unbounded retry.
        Err(e)
    }
}
#[cfg(not(target_os = "linux"))]
compile_error!("fetch-phase-supervisor requires Linux subreaper and /proc semantics");

fn save(root: &Path, name: &str, value: &Value) -> io::Result<()> {
    fs::write(root.join(name), serde_json::to_vec_pretty(value)?)
}
#[derive(Clone)]
struct Process {
    pid: i32,
    parent: i32,
    group: i32,
    start: u64,
    state: String,
    term_ignored: bool,
}
fn process(pid: i32) -> io::Result<Process> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = stat
        .rsplit_once(") ")
        .ok_or_else(|| io::Error::other("bad proc stat"))?
        .1
        .split_whitespace()
        .collect();
    let number = |i: usize| {
        fields
            .get(i)
            .ok_or_else(|| io::Error::other("short proc stat"))?
            .parse()
            .map_err(io::Error::other)
    };
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let mask = status
        .lines()
        .find_map(|l| l.strip_prefix("SigIgn:"))
        .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
        .unwrap_or(0);
    Ok(Process {
        pid,
        parent: number(1)? as i32,
        group: number(2)? as i32,
        start: number(19)?,
        state: fields[0].into(),
        term_ignored: mask & (1 << 14) != 0,
    })
}
fn group_members(group: i32) -> io::Result<Vec<Process>> {
    let mut rows = Vec::new();
    for entry in fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        // An exit between directory enumeration and read is ordinary; access
        // errors on a known owned PID are handled again before any signal.
        if let Ok(p) = process(pid) {
            if p.group == group {
                rows.push(p);
            }
        }
    }
    Ok(rows)
}
fn process_row(p: &Process) -> Value {
    json!({"pid":p.pid,"parent":p.parent,"pgid":p.group,"start_ticks":p.start,"state":p.state,"term_ignored":p.term_ignored})
}
#[derive(Default)]
struct Capture {
    prefix: Vec<u8>,
    total: u64,
    done: bool,
    error: Option<String>,
}
fn drainer(mut pipe: impl Read + Send + 'static) -> (Arc<Mutex<Capture>>, JoinHandle<()>) {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let shared = capture.clone();
    let handle = thread::spawn(move || {
        let mut buf = [0; 8192];
        loop {
            let read = pipe.read(&mut buf);
            let mut state = shared.lock().unwrap();
            match read {
                Ok(0) => {
                    state.done = true;
                    break;
                }
                Ok(n) => {
                    let keep = n.min(OUTPUT_CAP - state.prefix.len());
                    state.prefix.extend_from_slice(&buf[..keep]);
                    state.total += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    state.error = Some(format!("{:?}", e.kind()));
                    state.done = true;
                    break;
                }
            }
        }
    });
    (capture, handle)
}
fn capture_row(capture: &Arc<Mutex<Capture>>) -> Value {
    let c = capture.lock().unwrap();
    json!({"bytes":c.total,"retained_bytes":c.prefix.len(),"truncated":c.total>OUTPUT_CAP as u64,"drain_complete":c.done,"error_kind":c.error,"prefix_sha256":hash(&c.prefix)})
}
// Only current group members with a verified current parent chain, or exact
// children adopted by this dedicated subreaper, may extend the ownership map.
fn register_owned(members: &[Process], parent: i32, known: &mut BTreeMap<i32, u64>) -> bool {
    if members
        .iter()
        .any(|p| known.get(&p.pid).is_some_and(|s| *s != p.start))
    {
        return false;
    }
    for _ in 0..=members.len() {
        let before = known.len();
        for p in members {
            let current_parent = members
                .iter()
                .any(|q| q.pid == p.parent && known.get(&q.pid) == Some(&q.start));
            if p.parent == parent || current_parent {
                known.entry(p.pid).or_insert(p.start);
            }
        }
        if known.len() == before {
            break;
        }
    }
    members.iter().all(|p| known.get(&p.pid) == Some(&p.start))
}
// Covers every early Result error after spawn. A failure never bypasses owned
// cleanup or gains another grace period after the absolute hard end. This guard
// is used only while returning/unwinding, so no Child::try_wait follows its reap.
struct CleanupOnError {
    root: PathBuf,
    group: i32,
    parent: i32,
    parent_group: i32,
    hard_end: Instant,
    grace: Duration,
    known: Rc<RefCell<BTreeMap<i32, u64>>>,
    armed: bool,
    captures: Option<(Arc<Mutex<Capture>>, Arc<Mutex<Capture>>)>,
    out_thread: Option<JoinHandle<()>>,
    err_thread: Option<JoinHandle<()>>,
}
impl Drop for CleanupOnError {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let end = self.hard_end.min(Instant::now() + self.grace);
        let mut reaped = Vec::new();
        let mut signals = Vec::new();
        let mut ownership_lost = false;
        let mut kill_sent = false;
        while Instant::now() < end && self.group > 1 && self.group != self.parent_group {
            let Ok(members) = group_members(self.group) else {
                ownership_lost = true;
                break;
            };
            if !register_owned(&members, self.parent, &mut self.known.borrow_mut()) {
                ownership_lost = true;
                break;
            }
            if members.is_empty() {
                break;
            }
            if !kill_sent {
                let result = linux::signal_validated_group(self.group, 9);
                signals.push(json!({"signal":9,"result":result.as_ref().ok(),"errno":result.as_ref().err().and_then(|e|e.raw_os_error())}));
                kill_sent = true;
            }
            for p in members {
                if p.parent == self.parent && self.known.borrow().get(&p.pid) == Some(&p.start) {
                    match linux::reap_adopted_exact(p.pid) {
                        Ok(Some(status)) => reaped.push(
                            json!({"pid":p.pid,"start_ticks":p.start,"raw_wait_status":status}),
                        ),
                        Ok(None) => {}
                        Err(e) if e.raw_os_error() == Some(4) => {}
                        Err(_) => {}
                    }
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        let mut joins = true;
        for handle in [&mut self.out_thread, &mut self.err_thread] {
            while !handle.as_ref().is_none_or(|h| h.is_finished()) && Instant::now() < end {
                thread::sleep(Duration::from_millis(5));
            }
            if handle.as_ref().is_some_and(|h| h.is_finished()) {
                joins &= handle.take().unwrap().join().is_ok();
            } else if handle.is_some() {
                joins = false;
            }
        }
        let remaining = group_members(self.group).ok();
        let captures = self.captures.as_ref().map(|(out, err)| {
            let _ = fs::write(self.root.join("stdout.prefix"), &out.lock().unwrap().prefix);
            let _ = fs::write(self.root.join("stderr.prefix"), &err.lock().unwrap().prefix);
            json!({"stdout":capture_row(out),"stderr":capture_row(err)})
        });
        let complete = !ownership_lost && remaining.as_ref().is_some_and(|r| r.is_empty()) && joins;
        let _ = save(
            &self.root,
            "cleanup-on-error.json",
            &json!({"owned_pgid":self.group,"cleanup_complete":complete,
            "ownership_lost":ownership_lost,"remaining_owned_processes":remaining.map(|r|r.iter().map(process_row).collect::<Vec<_>>()),
            "reaped_exact_children":reaped,"signals":signals,"pipe_joins_complete":joins,"partial_capture":captures,
            "counters_after_failure":null,"counter_receipts_complete":false,"cpu_ms":null}),
        );
    }
}
fn canonical_git_environment(command: &mut Command) {
    // Remove unknown inherited Git variables. Never copy environment values to
    // logs, and never put client Trace2/curl flags into the Native parent.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|k| k.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
}
fn inherited_environment_receipt(pid: i32) -> io::Result<Value> {
    let bytes = fs::read(format!("/proc/{pid}/environ"))?;
    let mut names = Vec::new();
    let mut invalid = Vec::new();
    for variable in bytes.split(|b| *b == 0) {
        let Some(at) = variable.iter().position(|b| *b == b'=') else {
            continue;
        };
        let key = String::from_utf8_lossy(&variable[..at]);
        if !key.starts_with("GIT_") {
            continue;
        }
        names.push(key.to_string());
        let valid = if matches!(&*key, "GIT_CONFIG_COUNT" | "GIT_TERMINAL_PROMPT") {
            &variable[at + 1..] == b"0"
        } else {
            key.starts_with("GIT_ZERO_")
                || matches!(
                    &*key,
                    "GIT_CONFIG_NOSYSTEM"
                        | "GIT_CONFIG_GLOBAL"
                        | "GIT_DIR"
                        | "GIT_WORK_TREE"
                        | "GIT_PROTOCOL"
                        | "GIT_USER_AGENT"
                        | "GIT_PAGER"
                )
        };
        if !valid {
            invalid.push(key.to_string());
        }
    }
    names.sort();
    invalid.sort();
    Ok(
        json!({"pid":pid,"source":format!("/proc/{pid}/environ"),"git_environment_names":names,"invalid_git_names":invalid,"identity_environment_eligible":!bytes.is_empty() && !names.is_empty() && invalid.is_empty(),"raw_environment_bytes":bytes.len(),"raw_environment_sha256":hash(&bytes),"values_recorded":false}),
    )
}

// Watchdog runs outside the case group and never reads case sockets or joins a
// pending pipe-drainer. Case setup/HTTP/Git/observer/cleanup code runs INSIDE the
// killable group. Caller selects only reviewed internal workers, no arbitrary
// command/PID/signal CLI. All descendants in this case path inherit this group.
fn supervise(
    root: &Path,
    mode: &str,
    deadline: Duration,
    cleanup_grace: Duration,
) -> Result<Value> {
    let started = Instant::now();
    fs::create_dir(root)?;
    let mut command = if mode == "executor" {
        let mut c = Command::new(std::env::current_exe()?);
        c.arg("--internal-executor").arg(root);
        c
    } else {
        // Fixed proof-only wrapper makes the controlled child TERM-resistant.
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "trap '' TERM; exec \"$@\"", "resistant-proof"])
            .arg(std::env::current_exe()?)
            .arg("--internal-worker")
            .arg(root)
            .arg(mode);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    canonical_git_environment(&mut command);
    let parent = process(std::process::id() as i32)?;
    let mut child: Child = command.spawn()?;
    let group = child.id() as i32;
    let known = Rc::new(RefCell::new(BTreeMap::new()));
    let mut guard = CleanupOnError {
        root: root.into(),
        group,
        parent: parent.pid,
        parent_group: parent.group,
        hard_end: started + deadline + cleanup_grace,
        grace: cleanup_grace,
        known: known.clone(),
        armed: true,
        captures: None,
        out_thread: None,
        err_thread: None,
    };
    if group <= 1 || group == parent.group {
        return Err("invalid owned group".into());
    }
    let leader = process(group)?;
    if leader.parent != parent.pid || leader.group != group {
        return Err("case group ownership failed".into());
    }
    known.borrow_mut().insert(group, leader.start);
    let (out, out_thread) = drainer(child.stdout.take().unwrap());
    let (err, err_thread) = drainer(child.stderr.take().unwrap());
    guard.out_thread = Some(out_thread);
    guard.err_thread = Some(err_thread);
    guard.captures = Some((out.clone(), err.clone()));
    let mut leader_status = None;
    let mut expired = false;
    let mut cleanup_started = None;
    let mut term_at = None;
    let mut kill_sent = false;
    let mut signal_errors = Vec::new();
    let mut reaped = Vec::new();
    let mut observed = Vec::new();
    let mut orphan_adopted = false;
    let mut pipe_block_after_exit = false;
    let mut ownership_lost = false;
    loop {
        let elapsed = started.elapsed();
        let members = group_members(group)?;
        if !register_owned(&members, parent.pid, &mut known.borrow_mut()) {
            ownership_lost = true;
        }
        for p in &members {
            if !observed.iter().any(|r: &Value| r["pid"] == p.pid) {
                observed.push(process_row(p));
            }
        }
        if leader_status.is_none() {
            leader_status = child.try_wait()?;
        }
        if leader_status.is_some() {
            if !guard.out_thread.as_ref().is_some_and(|t| t.is_finished())
                || !guard.err_thread.as_ref().is_some_and(|t| t.is_finished())
            {
                pipe_block_after_exit = true;
            }
            for p in &members {
                if p.pid == group || p.parent != parent.pid {
                    continue;
                }
                if known.borrow().get(&p.pid) != Some(&p.start) {
                    ownership_lost = true;
                    continue;
                }
                orphan_adopted = true;
                match linux::reap_adopted_exact(p.pid) {
                    Ok(Some(status))=>reaped.push(json!({"pid":p.pid,"start_ticks":p.start,"raw_wait_status":status,"signal":status&127})),
                    Ok(None)=>{},Err(e) if e.raw_os_error()==Some(4)=>{},Err(e)=>signal_errors.push(json!({"operation":"waitpid-exact","pid":p.pid,"errno":e.raw_os_error()})),
                }
            }
        }
        let drained = guard.out_thread.as_ref().is_some_and(|t| t.is_finished())
            && guard.err_thread.as_ref().is_some_and(|t| t.is_finished());
        if leader_status.is_some() && drained && group_members(group)?.is_empty() {
            break;
        }
        if mode == "supervisor-error" && root.join("partial-progress.json").exists() {
            return Err("intentional supervisor failure after owned child setup".into());
        }
        if elapsed >= deadline && cleanup_started.is_none() {
            expired = true;
            cleanup_started = Some(Instant::now());
        }
        if (leader_status.as_ref().is_some_and(|s| !s.success())
            || (mode == "executor" && leader_status.is_some()))
            && cleanup_started.is_none()
        {
            cleanup_started = Some(Instant::now());
        }
        if let Some(cleanup) = cleanup_started {
            // Fresh generation evidence is required for EVERY signal. No signal
            // after a PID-generation change, and no parent's group accepted.
            let current = group_members(group)?;
            let verified = !ownership_lost
                && !current.is_empty()
                && register_owned(&current, parent.pid, &mut known.borrow_mut());
            if !current.is_empty() && !verified {
                ownership_lost = true;
            }
            let signal = if term_at.is_none() {
                Some(15)
            } else if !kill_sent && cleanup.elapsed() >= Duration::from_millis(250) {
                Some(9)
            } else {
                None
            };
            if let Some(signal) = signal {
                if verified {
                    match linux::signal_validated_group(group,signal) {
                        Ok(found)=>signal_errors.push(json!({"operation":"signal","signal":signal,"group_present":found,"errno":null})),
                        Err(e)=>signal_errors.push(json!({"operation":"signal","signal":signal,"errno":e.raw_os_error(),"kind":format!("{:?}",e.kind())})),
                    }
                    if signal == 15 {
                        term_at = Some(elapsed.as_millis() as u64);
                    } else {
                        kill_sent = true;
                    }
                }
            }
            if cleanup.elapsed() >= cleanup_grace {
                break;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    let mut joins_complete = true;
    for thread in [&mut guard.out_thread, &mut guard.err_thread] {
        if thread.as_ref().is_some_and(|t| t.is_finished()) {
            if thread.take().unwrap().join().is_err() {
                joins_complete = false;
            }
        } else {
            joins_complete = false;
        } // Never block past cleanup grace in join.
    }
    let remaining = group_members(group)?;
    fs::write(root.join("stdout.prefix"), &out.lock().unwrap().prefix)?;
    fs::write(root.join("stderr.prefix"), &err.lock().unwrap().prefix)?;
    let receipt = json!({"schema":1,"mode":mode,"owned_pgid":group,"parent_outside_case_group":parent.group!=group,
        "deadline_ms":deadline.as_millis(),"cleanup_grace_ms":cleanup_grace.as_millis(),"elapsed_ms":started.elapsed().as_millis(),
        "deadline_expired":expired,"term_sent_at_ms":term_at,"kill_sent":kill_sent,"ownership_lost":ownership_lost,
        "leader_reaped":leader_status.is_some(),"leader_exit_code":leader_status.as_ref().and_then(|s|s.code()),"leader_signal":leader_status.as_ref().and_then(|s|s.signal()),
        "observed_owned_processes":observed,"orphan_adoption_observed":orphan_adopted,"reaped_adopted_descendants":reaped,
        "pipe_drain_blocked_after_leader_exit":pipe_block_after_exit,"pipe_joins_complete":joins_complete,
        "remaining_owned_processes":remaining.iter().map(process_row).collect::<Vec<_>>(),"signals_and_errors":signal_errors,
        "stdout":capture_row(&out),"stderr":capture_row(&err),"counter_receipts_after_timeout":null,"counter_receipts_complete":false,
        "native_cpu_ms":null,"child_cpu_ms":null,"worker_cpu_ms":null,"full_bill_usd":null});
    save(root, "supervision.json", &receipt)?;
    if !remaining.is_empty() || !joins_complete || leader_status.is_none() || ownership_lost {
        return Err("hard cleanup incomplete; no continuation allowed".into());
    }
    guard.armed = false;
    Ok(receipt)
}
fn internal_worker(root: &Path, mode: &str) -> Result<()> {
    if !matches!(
        mode,
        "normal"
            | "socket-stall"
            | "orphan-pipe-stall"
            | "supervisor-error"
            | "leaf-socket"
            | "leaf-pipe"
    ) {
        return Err("unknown internal proof mode".into());
    }
    if !root.is_dir() {
        return Err("missing owned proof directory".into());
    }
    if mode.starts_with("leaf-") {
        let port = if mode == "leaf-socket" { 8853 } else { 8854 };
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let me = process(std::process::id() as i32)?;
        save(root, &format!("{mode}.json"), &process_row(&me))?;
        println!("owned resistant {mode}; stdout held open");
        let (_socket, _) = listener.accept()?;
        loop {
            thread::sleep(Duration::from_millis(50));
        }
    }
    save(
        root,
        "actual-worker-environment.json",
        &inherited_environment_receipt(std::process::id() as i32)?,
    )?;
    if mode == "normal" {
        println!("normal owned child completed");
        return Ok(());
    }
    let leaf = if matches!(mode, "socket-stall" | "supervisor-error") {
        "leaf-socket"
    } else {
        "leaf-pipe"
    };
    // No PGID/setsid call: child inherits the dedicated case group and ignored TERM.
    let descendant = Command::new(std::env::current_exe()?)
        .arg("--internal-worker")
        .arg(root)
        .arg(leaf)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let _owned_pid = descendant.id();
    while !root.join(format!("{leaf}.json")).exists() {
        thread::sleep(Duration::from_millis(5));
    }
    io::stdout().write_all(&vec![b'p'; 256 * 1024])?;
    io::stdout().flush()?;
    if mode == "orphan-pipe-stall" {
        save(
            root,
            "partial-progress.json",
            &json!({"stage":"leader_exit_with_descendant_stdout_open","after_counters":null,"complete":false}),
        )?;
        return Ok(()); // Deliberate orphan for the subreaper proof.
    }
    save(
        root,
        "partial-progress.json",
        &json!({"stage":"socket_read_begin","after_counters":null,"complete":false}),
    )?;
    let mut socket = TcpStream::connect(("127.0.0.1", 8853))?;
    // Intentionally no socket timeout: only the OUTSIDE hard supervisor can end
    // this blocked read. This is a synthetic watchdog proof, never a Git echo.
    socket.read_to_end(&mut Vec::new())?;
    Err("stalled socket unexpectedly completed".into())
}
fn cheap_proof(root: &Path) -> Result<()> {
    fs::create_dir(root)?;
    let root = root.canonicalize()?;
    save(
        &root,
        "resources-before.json",
        &replay::supervised_resource_receipt(&root, "preflight-before")?,
    )?;
    let ports = [
        TcpListener::bind(("127.0.0.1", 8853))?,
        TcpListener::bind(("127.0.0.1", 8854))?,
    ];
    drop(ports);
    linux::subreaper()?;
    let mut rows = Vec::new();
    for mode in ["normal", "socket-stall", "orphan-pipe-stall"] {
        let row = supervise(
            &root.join(mode),
            mode,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )?;
        if mode == "normal" {
            if row["deadline_expired"] != false || row["leader_exit_code"] != 0 {
                return Err("normal proof failed".into());
            }
        } else {
            if row["deadline_expired"] != true
                || row["kill_sent"] != true
                || row["orphan_adoption_observed"] != true
                || row["reaped_adopted_descendants"]
                    .as_array()
                    .is_none_or(|a| a.is_empty())
            {
                return Err("resistant descendant deadline/reaping not proven".into());
            }
            let leaf = json_from(root.join(mode).join(if mode == "socket-stall" {
                "leaf-socket.json"
            } else {
                "leaf-pipe.json"
            }))?;
            if leaf["term_ignored"] != true || leaf["pgid"] != row["owned_pgid"] {
                return Err("resistant child/group not proven".into());
            }
            if mode == "orphan-pipe-stall" && row["pipe_drain_blocked_after_leader_exit"] != true {
                return Err("blocked drainer not proven".into());
            }
        }
        if row["elapsed_ms"].as_u64().is_none_or(|n| n > 4500) {
            return Err("cheap hard bound exceeded".into());
        }
        let ports = [
            TcpListener::bind(("127.0.0.1", 8853))?,
            TcpListener::bind(("127.0.0.1", 8854))?,
        ];
        drop(ports);
        rows.push(row);
    }
    let failed_root = root.join("supervisor-error");
    if supervise(
        &failed_root,
        "supervisor-error",
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .is_ok()
    {
        return Err("supervisor error injection did not fail".into());
    }
    let error_cleanup = json_from(failed_root.join("cleanup-on-error.json"))?;
    if error_cleanup["cleanup_complete"] != true
        || error_cleanup["pipe_joins_complete"] != true
        || error_cleanup["reaped_exact_children"]
            .as_array()
            .is_none_or(|r| r.len() < 2)
    {
        return Err("early-error cleanup incomplete".into());
    }
    let ports = [
        TcpListener::bind(("127.0.0.1", 8853))?,
        TcpListener::bind(("127.0.0.1", 8854))?,
    ];
    drop(ports);
    save(&root, "expected-error.json", &error_cleanup)?;
    save(
        &root,
        "proof.json",
        &json!({"pass":true,"linux_only":true,"stage":"supervision only; no Git fixture/replay","stage1_source":STAGE1,
        "cases":rows,"expected_supervisor_error_cleanup":error_cleanup,"owned_ports":[8853,8854],"ports_free":true,"expensive_replays":0,"performance_claim":false,"cpu_ms":null}),
    )?;
    save(
        &root,
        "resources-after.json",
        &replay::supervised_resource_receipt(&root, "preflight-after")?,
    )?;
    println!("hard-supervisor cheap proof PASS: {}", root.display());
    Ok(())
}
fn json_from(path: impl AsRef<Path>) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn seal_case_plans(root: &Path) -> Result<()> {
    fs::create_dir(root)?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let archived = Path::new(IMMUTABLE).join("source/examples/repository-host");
    for relative in ["src/bin/hosting-bench.rs", "src/benchmark_cost.rs"] {
        if fs::read(source.join(relative))? != fs::read(archived.join(relative))? {
            return Err("frozen generator/schedule changed".into());
        }
    }
    let original = "/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r17-r18-screen/run-39113545149/profiles";
    let specs = [
        (
            "64mib-cold",
            "history_no_haves_16",
            "699da46639e28c7a47a94061423878ca591405f75abb10019b8d6895bf2b7b82",
            42usize,
            "38d11e717df6687e26ade7033be62a56a408595c",
            "141c23fa72d6958422cd6699001ddd410cf703a5",
        ),
        (
            "16mib-warm",
            "history_no_haves_32",
            "703883dd612874c8caddde88b301b2d2c0cf66a937ad1ba267642ea151104f57",
            86usize,
            "6d55590087ff6851e7e37884c864b854d1c43f51",
            "66c8fe43b4875bc980a381d59b8dac5aefcf900a",
        ),
    ];
    let mut plans = Vec::new();
    for (profile, key, expected, length, want, tree) in specs {
        let path = Path::new(original)
            .join(profile)
            .join("r18")
            .join(format!("{profile}.json"));
        let bytes = fs::read(&path)?;
        if hash(&bytes) != expected {
            return Err("original report seal mismatch".into());
        }
        let report: Value = serde_json::from_slice(&bytes)?;
        let fixture = &report["fixture"];
        let tips = fixture["generations"]
            .as_array()
            .ok_or("fixture generations missing")?
            .iter()
            .map(|g| {
                g["tip"]
                    .as_str()
                    .ok_or("fixture tip missing")
                    .map(String::from)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let schedule = interleaved_schedule(&tips)?;
        if json!(schedule) != fixture["schedule"]
            || hash(&serde_json::to_vec(&schedule)?) != fixture["schedule_sha256"]
        {
            return Err("frozen schedule differs from sealed report".into());
        }
        let raw = report["raw"]
            .as_array()
            .ok_or("raw rows missing")?
            .iter()
            .filter(|r| r["backend"] == "cloudflare-local" && r["sample"] == 1)
            .collect::<Vec<_>>();
        let position = raw
            .iter()
            .position(|r| r["operation"]["key"] == key)
            .ok_or("target missing")?;
        if position + 1 != length
            || raw[position]["verified_want"] != want
            || raw[position]["verified_tree"] != tree
        {
            return Err("sealed target mismatch".into());
        }
        let prefix = &schedule[..length];
        if !raw
            .iter()
            .take(length)
            .zip(prefix)
            .all(|(r, o)| r["operation"] == *o)
        {
            return Err("original prefix order mismatch".into());
        }
        let history=raw.iter().take(length).map(|r|json!({"operation":r["operation"],"wall_ms":r["wall_ms"],"client_received_pack_bytes":r["client_received_pack_bytes"],"http_response_body_bytes":r["http_response_body_bytes"],"verified_want":r["verified_want"],"verified_tree":r["verified_tree"]})).collect::<Vec<_>>();
        let plan = json!({"schema":1,"status":"SEALED PLAN ONLY; fixture reconstruction and replay NOT RUN","profile":profile,"target_key":key,
            "original":{"report_path":path,"sha256":expected,"ci_run":39113545149u64,"provider":"r18","sample":1,"source":"f1ba8f548507dca1e5ab78e5c5d20d8360ecee21","host_binary_sha256":report["config"]["host_binary_sha256"],"machine":report["machine"],"target":raw[position]["operation"],"want":want,"tree":tree,"packet_trace_retained":false,"request_sha256":null},
            "diagnostic":{"native_source":STAGE1,"approved_native_base":"f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5","provider_source_differs_from_historical_r18":true,"exact_old_provider_reproduction_claim":false,"worker_source_tree":"7f41db4407963ed04e6cbe19556b1416633caf18","worker_index_sha256":"0b3183bb89d7920c9f1d0868895ec13414d74e74e096f8181b30654e418c0bbd","native_hook_changes_allowed":false},
            "original_config":report["config"],"logical_fixture":fixture,"schedule_prefix_including_target":prefix,"prefix_sha256":hash(&serde_json::to_vec(prefix)?),"historical_prefix_context_all_events":history,
            "prefix_state_rules":{"fresh_independent_services_and_native_each_replica":true,"preserve_all_prior_push_read_and_observer_order":true,"retain_all_slow_prefix_events":true,"no_synthetic_cache_seeding":true,"no_forced_r2_or_latency_count":true,"old_warmup_and_sample_identity_not_reused":true},
            "future_reconstruction_gate":{"fixture_generated":false,"reconstructed_wants_verified":null,"reconstructed_trees_verified":null,"mismatch_action":"STOP; do not relabel/reseed fixture","generate_full_32_commit_source_before_prefix":true,"git_source_gc_before_prefix":true,"expected_generations":fixture["generations"],"future_actual_request_hashes":null,"future_actual_pack_hashes":null},
            "client_semantics":{"protocol_version":0,"http_post_buffer":268435456,"fetch_unpack_limit":1,"fetch_keep":true,"nonempty_pack_required":true,"strict_fsck":true,"want_tree_archive_required":true,"persistent_latest_client":true,"fresh_historical_clients":true,"seed_have_locally_outside_operation_timer":true,"trace2_curl_no_data_child_client_only":true,"unknown_git_env_removed":true,"actual_native_proc_environment_each_cohort_required":true},
            "observer_order":{"each_operation":["metrics-before","actual-git-command","metrics-after"],"push_then":["metrics-cache-generation-invalidation-inventory-check"],"fetch_then":["new-pack-byte-count","want-tree","strict-fsck","archive","metrics-cache-storage-check","remove-fresh-client-after-checks"],"failed_observer_action":"save actual headers/status/body; mark receipts incomplete/null; stop"},
            "acceptance":{"applicable_v0_lineage_certificate_preparation_counters_required":true,"report_actual_fast_paths_and_r2_bytes":true,"r19_cache_shape_may_differ":true,"native_cpu_ms":null,"worker_cpu_ms":null,"do_gb_seconds":null,"full_bill_usd":null,"performance_claim":false},
            "resource_limits":{"replicas_max":2,"case_deadline_seconds_including_prefix_setup_io_and_observation":1200,"fixed_cleanup_grace_seconds":5,"raw_prefix_byte_cap_proposed":8589934592u64,"disk_reserve_bytes":2147483648u64,"memory_available_floor_bytes_proposed":8589934592u64,"tmpfs_authorized":false,"large_runtime_authorized":false}});
        let name = format!("{profile}-{key}.json");
        save(root, &name, &plan)?;
        plans.push(json!({"path":root.join(name),"sha256":hash(&serde_json::to_vec_pretty(&plan)?),"operations_including_target":length,"replicas_max":2,"fixture_reconstruction_verified":null}));
    }
    save(
        root,
        "plans.json",
        &json!({"schema":1,"plans":plans,"max_cases_total":4,"max_serial_work_plus_cleanup_seconds":4820,"replay_cli_available":false,"large_generation_performed":false,
        "frozen_generator_sha256":hash(&fs::read(archived.join("src/bin/hosting-bench.rs"))?),"frozen_schedule_sha256":hash(&fs::read(archived.join("src/benchmark_cost.rs"))?),
        "immutable_stage1_relocation_map":Path::new(IMMUTABLE).join("relocation-map.json"),"raw_prefix_location":null,"root_go_required":true}),
    )?;
    println!(
        "sealed two replay plans; no fixture generated: {}",
        root.display()
    );
    Ok(())
}
fn main() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mode = args
        .first()
        .and_then(|a| a.to_str())
        .ok_or("use --cheap-supervisor-proof NEW_DIR or --seal-case-plans NEW_DIR")?;
    let root = PathBuf::from(args.get(1).ok_or("missing output directory")?);
    match mode {
        "--blocked-resource-preflight-proof" if args.len() == 2 => {
            replay::blocked_resource_proof(&root)
        }
        "--internal-blocked-resource-helper" if args.len() == 2 => {
            replay::blocked_resource_helper(&root)
        }
        "--seal-runtime-manifest" if args.len() == 2 => runtime::seal(&root),
        "--tiny-production-proof" if args.len() == 2 => replay::tiny(&root),
        "--execute-reviewed-batch" if args.len() == 2 => replay::batch(&root),
        "--internal-executor" if args.len() == 2 => replay::worker(&root),
        "--cheap-supervisor-proof" if args.len() == 2 => cheap_proof(&root),
        "--seal-case-plans" if args.len() == 2 => seal_case_plans(&root),
        "--internal-worker" if args.len() == 3 => {
            internal_worker(&root, args[2].to_str().ok_or("mode encoding")?)
        }
        _ => Err("unsupported command; expensive replay is not enabled".into()),
    }
}
