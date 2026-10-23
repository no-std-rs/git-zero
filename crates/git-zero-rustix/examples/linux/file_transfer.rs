//! File-to-socket fast path for this process's immutable encoded files.
//! The host ignores SIGPIPE before accepting requests; sendfile has no
//! MSG_NOSIGNAL flag. Generated responses still use SocketWriter.
use git_zero::{Error, io::Write};
use git_zero_rustix::SocketWriter;
use rustix::{fd::AsFd, io::Errno};

#[derive(Default)]
pub(super) struct Counts {
    pub sendfile_bytes: u64,
    pub buffered_file_bytes: u64,
}

pub(super) fn send(
    socket: &impl AsFd,
    file: &impl AsFd,
    mut offset: u64,
    length: u64,
    scratch: &mut [u8],
    automatic: bool,
) -> git_zero::Result<Counts> {
    let end = offset
        .checked_add(length)
        .filter(|&end| end <= i64::MAX as u64)
        .ok_or(Error::InvalidObject)?;
    let mut counts = Counts::default();
    if automatic {
        while offset < end {
            // Linux caps a single transfer; loop on short writes and keep an
            // explicit offset so neither path changes the shared file cursor.
            let count = (end - offset).min(0x7fff_f000) as usize;
            match rustix::fs::sendfile(socket, file, Some(&mut offset), count) {
                Ok(0) => return Err(Error::Backend), // Truncation, not success.
                Ok(sent) => counts.sendfile_bytes += sent as u64,
                Err(Errno::INTR) => continue,
                Err(Errno::NOSYS | Errno::INVAL | Errno::OPNOTSUPP) => break,
                Err(_) => return Err(Error::Backend),
            }
        }
    }
    let mut output = SocketWriter(socket.as_fd());
    while offset < end {
        let length = (end - offset).min(scratch.len() as u64) as usize;
        if length == 0 {
            return Err(Error::BufferTooSmall);
        }
        let read = match rustix::io::pread(file, &mut scratch[..length], offset) {
            Ok(0) => return Err(Error::Backend),
            Ok(read) => read,
            Err(Errno::INTR) => continue,
            Err(_) => return Err(Error::Backend),
        };
        output.write_all(&scratch[..read])?;
        offset += read as u64;
        counts.buffered_file_bytes += read as u64;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::CString, io::Read, os::unix::net::UnixStream};

    fn transfer(path: &str, offset: u64, length: u64, automatic: bool) -> (Vec<u8>, Counts) {
        let path = CString::new(path).unwrap();
        let file = rustix::fs::open(
            path.as_c_str(),
            rustix::fs::OFlags::RDONLY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let (writer, mut reader) = UnixStream::pair().unwrap();
        writer
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        // Drain while sending: small writes consume socket-buffer metadata too,
        // so even a short file can fill the queue before its last byte is sent.
        let received = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            bytes
        });
        // SAFETY: these borrowed descriptors remain valid for the transfer.
        let socket =
            unsafe { rustix::fd::BorrowedFd::borrow_raw(std::os::fd::AsRawFd::as_raw_fd(&writer)) };
        let counts = send(&socket, &file, offset, length, &mut [0; 7], automatic).unwrap();
        assert_eq!(
            rustix::fs::seek(&file, rustix::fs::SeekFrom::Current(0)).unwrap(),
            0
        );
        drop(writer);
        (received.join().unwrap(), counts)
    }

    #[test]
    fn file_ranges_and_forced_buffered_transfers_match() {
        let path =
            std::env::temp_dir().join(format!("git-zero-file-transfer-{}.bin", std::process::id()));
        let expected: Vec<u8> = (0..=255).cycle().take(256 * 1024).collect();
        std::fs::write(&path, &expected).unwrap();
        let length = (expected.len() - 11) as u64;
        for automatic in [true, false] {
            let (bytes, counts) = transfer(path.to_str().unwrap(), 11, length, automatic);
            assert_eq!(bytes, expected[11..]);
            assert_eq!(counts.sendfile_bytes + counts.buffered_file_bytes, length);
            assert_eq!(counts.sendfile_bytes > 0, automatic);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_proc_file_uses_positional_read_fallback() {
        let expected = std::fs::read("/proc/self/cmdline").unwrap();
        let (bytes, counts) = transfer("/proc/self/cmdline", 0, expected.len() as u64, true);
        assert_eq!(bytes, expected);
        assert_eq!(counts.sendfile_bytes, 0);
        assert_eq!(counts.buffered_file_bytes, expected.len() as u64);
    }
}
