//! Allocator-free TLS 1.3 handshake over caller-owned kernel sockets/buffers.
//! This example deliberately skips certificate and hostname verification.
use core::ffi::c_void;
use embedded_io::{ErrorKind, ErrorType, Read, Write};
use embedded_tls::{
    Aes128GcmSha256, TlsConfig, TlsContext, TlsError, UnsecureProvider, blocking::TlsConnection,
};
use rand_core::{CryptoRng, RngCore};

const RECORD_SIZE: usize = 16640;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SocketIo {
    context: *mut c_void,
    receive: unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> isize,
    send: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> isize,
    random: unsafe extern "C" fn(*mut u8, usize),
}

#[derive(Debug)]
pub struct IoError;
impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("kernel socket I/O")
    }
}
impl core::error::Error for IoError {}
impl embedded_io::Error for IoError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}
impl ErrorType for SocketIo {
    type Error = IoError;
}
impl Read for SocketIo {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, IoError> {
        // SAFETY: Linux supplies a live context and callbacks with this ABI.
        let count = unsafe { (self.receive)(self.context, bytes.as_mut_ptr(), bytes.len()) };
        if count < 0 || count as usize > bytes.len() {
            Err(IoError)
        } else {
            Ok(count as usize)
        }
    }
}
impl Write for SocketIo {
    fn write(&mut self, bytes: &[u8]) -> Result<usize, IoError> {
        // SAFETY: Linux supplies a live context and callbacks with this ABI.
        let count = unsafe { (self.send)(self.context, bytes.as_ptr(), bytes.len()) };
        if count < 0 || count as usize > bytes.len() {
            Err(IoError)
        } else {
            Ok(count as usize)
        }
    }
    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}
impl RngCore for SocketIo {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        self.fill_bytes(&mut bytes);
        u32::from_ne_bytes(bytes)
    }
    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.fill_bytes(&mut bytes);
        u64::from_ne_bytes(bytes)
    }
    fn fill_bytes(&mut self, bytes: &mut [u8]) {
        // SAFETY: The callback fills this live output after CRNG initialization.
        unsafe { (self.random)(bytes.as_mut_ptr(), bytes.len()) };
    }
    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(bytes);
        Ok(())
    }
}
impl CryptoRng for SocketIo {}

struct Session {
    connection: TlsConnection<'static, SocketIo, Aes128GcmSha256>,
    opened: bool,
}

#[inline(never)]
unsafe fn initialize(
    state: *mut c_void,
    io: SocketIo,
    read: &'static mut [u8],
    write: &'static mut [u8],
) -> &'static mut Session {
    // SAFETY: The FFI caller owns aligned state and all buffers for this lifetime.
    unsafe {
        let session = state.cast::<Session>();
        session.write(Session {
            connection: TlsConnection::new(io, read, write),
            opened: false,
        });
        &mut *session
    }
}

#[inline(never)]
fn handshake(session: &mut Session, host: &str, io: SocketIo) -> Result<(), TlsError> {
    let config = TlsConfig::new()
        .with_server_name(host)
        .with_alpn(&[b"http/1.1"]);
    // Certificate verification is intentionally omitted for this example.
    let provider = UnsecureProvider::new::<Aes128GcmSha256>(io);
    session.connection.open(TlsContext::new(&config, provider))
}

#[unsafe(no_mangle)]
pub extern "C" fn gitfs_tls_size() -> usize {
    core::mem::size_of::<Session>()
}

/// Perform a TLS handshake in caller-owned storage, without certificate checks.
/// # Safety
/// State has gitfs_tls_size bytes aligned to 16; read/write have 16640 disjoint
/// writable bytes. Host and IO callbacks stay valid through the session.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_tls_open(
    state: *mut c_void,
    read: *mut u8,
    write: *mut u8,
    io: SocketIo,
    host: *const u8,
    host_length: usize,
) -> i32 {
    // SAFETY: All ranges are live, disjoint allocations owned by the C transport.
    let (read, write, host) = unsafe {
        (
            core::slice::from_raw_parts_mut(read, RECORD_SIZE),
            core::slice::from_raw_parts_mut(write, RECORD_SIZE),
            core::slice::from_raw_parts(host, host_length),
        )
    };
    let Ok(host) = core::str::from_utf8(host) else {
        return -22;
    };
    // SAFETY: State is suitably aligned and its buffers outlive the session.
    let session = unsafe { initialize(state, io, read, write) };
    match handshake(session, host, io) {
        Ok(()) => {
            session.opened = true;
            0
        }
        Err(_) => -71,
    }
}

/// Read encrypted application traffic after a completed handshake.
/// # Safety
/// State is an initialized session; output is disjoint and writable for length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_tls_read(
    state: *mut c_void,
    output: *mut u8,
    length: usize,
) -> isize {
    // SAFETY: The C transport serializes access to live state and buffers.
    let (session, output) = unsafe {
        (
            &mut *state.cast::<Session>(),
            core::slice::from_raw_parts_mut(output, length),
        )
    };
    if !session.opened {
        return -71;
    }
    match session.connection.read(output) {
        Ok(length) => length as isize,
        Err(TlsError::ConnectionClosed) => 0,
        Err(_) => -5,
    }
}

/// Encrypt and flush application bytes after a completed handshake.
/// # Safety
/// State is an initialized session; input is disjoint and valid for length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gitfs_tls_write(
    state: *mut c_void,
    input: *const u8,
    length: usize,
) -> isize {
    // SAFETY: The C transport serializes access to live state and buffers.
    let (session, input) = unsafe {
        (
            &mut *state.cast::<Session>(),
            core::slice::from_raw_parts(input, length),
        )
    };
    if !session.opened {
        return -71;
    }
    match session
        .connection
        .write(input)
        .and_then(|count| session.connection.flush().map(|()| count))
    {
        Ok(length) => length as isize,
        Err(_) => -5,
    }
}
