//! Local benchmark host. Both backends share HTTP parsing, buffering and writes.
//! Resource observations require Linux; this is not a production server.
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    if let Err(error) = linux::run() {
        eprintln!("benchmark-server: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("benchmark-server resource observations require Linux");
    std::process::exit(1);
}
