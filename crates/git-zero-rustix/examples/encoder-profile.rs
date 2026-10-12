//! Isolate encoder CPU from HTTP, process startup and object-file reads.
#[cfg(target_os = "linux")]
#[path = "linux/encoder_profile.rs"]
mod profile;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    profile::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("encoder-profile requires Linux resource measurement");
    std::process::exit(1);
}
