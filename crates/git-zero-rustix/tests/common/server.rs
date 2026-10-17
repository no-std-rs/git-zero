//! Scoped native acceptance server with owned sockets and panic-safe shutdown.
use rustix::fd::OwnedFd;
use std::{
    net::TcpStream,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

struct Stop<'a> {
    stopped: &'a AtomicBool,
    port: u16,
}

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wake an in-flight blocking accept even when the client panics.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// The caller chooses accept options and owns all request/response buffers.
/// Each accepted socket closes after its handler; no socket or thread escapes.
pub fn while_serving<T>(
    port: u16,
    accept: impl FnMut() -> OwnedFd + Send,
    handle: impl FnMut(&OwnedFd) + Send,
    client: impl FnOnce(u16) -> T,
) -> T {
    let stopped = AtomicBool::new(false);
    thread::scope(|scope| {
        let serving = scope.spawn(|| serve(accept, handle, &stopped));
        let result = {
            let _stop = Stop {
                stopped: &stopped,
                port,
            };
            client(port)
        };
        serving.join().unwrap();
        result
    })
}

fn serve(
    mut accept: impl FnMut() -> OwnedFd,
    mut handle: impl FnMut(&OwnedFd),
    stopped: &AtomicBool,
) {
    // The first check also handles a client that finishes before this starts.
    while !stopped.load(Ordering::SeqCst) {
        let socket = accept();
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        handle(&socket);
    }
}
