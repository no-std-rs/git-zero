//! Deadline-aware intake without changing a borrowed descriptor's OS settings.
use core::time::Duration;
use git_zero::{Error, Result};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fd::AsFd,
    net::{RecvFlags, SendFlags, recv, send, sockopt},
    time::{ClockId, clock_gettime},
};

/// Optional total intake and idle deadlines. Disabled defaults preserve the
/// blocking API. A positive read resets idle only; total remains absolute.
/// Zero durations and monotonic-clock arithmetic overflow fail before any read.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestDeadlines {
    pub total: Option<Duration>,
    pub idle: Option<Duration>,
}

/// Optional total response and idle-write-progress deadlines. Each response call
/// gets its own monotonic budget; intake and response budgets are independent.
pub type ResponseDeadlines = RequestDeadlines;

pub(super) struct DeadlineReader<'a, S: AsFd> {
    socket: &'a S,
    deadlines: RequestDeadlines,
    total_end: Option<Duration>,
    progress: Duration,
    socket_timeout: Option<Duration>,
}

fn now() -> Result<Duration> {
    let time = clock_gettime(ClockId::Monotonic);
    Ok(Duration::new(
        time.tv_sec.try_into().map_err(|_| Error::Backend)?,
        time.tv_nsec.try_into().map_err(|_| Error::Backend)?,
    ))
}

fn end(start: Duration, duration: Option<Duration>) -> Result<Option<Duration>> {
    duration
        .map(|value| start.checked_add(value).ok_or(Error::InvalidRequest))
        .transpose()
}

impl<'a, S: AsFd> DeadlineReader<'a, S> {
    pub(super) fn new(socket: &'a S, deadlines: RequestDeadlines) -> Result<Self> {
        Self::configured(socket, deadlines, sockopt::Timeout::Recv)
    }

    fn configured(
        socket: &'a S,
        deadlines: RequestDeadlines,
        option: sockopt::Timeout,
    ) -> Result<Self> {
        if [deadlines.total, deadlines.idle]
            .into_iter()
            .flatten()
            .any(|value| value.is_zero())
        {
            return Err(Error::InvalidRequest);
        }
        let start = now()?;
        let total_end = end(start, deadlines.total)?;
        end(start, deadlines.idle)?;
        let socket_timeout = sockopt::socket_timeout(socket, option).map_err(|_| Error::Backend)?;
        Ok(Self {
            socket,
            deadlines,
            total_end,
            progress: start,
            socket_timeout,
        })
    }

    fn host_end(&self) -> Result<Option<Duration>> {
        Ok(self
            .total_end
            .into_iter()
            .chain(end(self.progress, self.deadlines.idle)?)
            .min())
    }

    pub(super) fn check(&self) -> Result<()> {
        let time = now()?;
        if self.host_end()?.is_some_and(|deadline| time >= deadline) {
            return Err(Error::LimitExceeded);
        }
        Ok(())
    }

    fn ready(&self, flags: PollFlags, call_end: Option<Duration>) -> Result<()> {
        loop {
            let time = now()?;
            let host_end = self.host_end()?;
            if host_end.is_some_and(|deadline| time >= deadline) {
                return Err(Error::LimitExceeded);
            }
            if call_end.is_some_and(|deadline| time >= deadline) {
                return Err(Error::Backend);
            }
            let deadline = host_end.into_iter().chain(call_end).min();
            let remaining = deadline.map(|deadline| deadline.saturating_sub(time));
            // poll on Darwin uses signed milliseconds. Cap long waits and loop
            // against the absolute clock; round sub-ms waits up to avoid spins.
            let wait = remaining.map(|value| {
                let millis = value
                    .as_millis()
                    .saturating_add(u128::from(value.subsec_nanos() % 1_000_000 != 0));
                let millis = millis.clamp(1, i32::MAX as u128) as u64;
                Timespec {
                    tv_sec: (millis / 1000) as _,
                    tv_nsec: ((millis % 1000) * 1_000_000) as _,
                }
            });
            let mut descriptors = [PollFd::new(self.socket, flags)];
            match poll(&mut descriptors, wait.as_ref()) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => return Ok(()),
                Err(_) => return Err(Error::Backend),
            }
        }
    }

    pub(super) fn receive(&mut self, storage: &mut [u8]) -> Result<usize> {
        let call_end = end(now()?, self.socket_timeout)?;
        loop {
            self.ready(PollFlags::IN, call_end)?;
            // Readiness can be stale. DONTWAIT bounds the syscall without
            // mutating O_NONBLOCK or either of the host's socket timeouts.
            match recv(self.socket, &mut *storage, RecvFlags::DONTWAIT) {
                Ok((count, _)) => {
                    self.check()?;
                    if count > 0 {
                        self.progress = now()?;
                    }
                    return Ok(count);
                }
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
                Err(_) => return Err(Error::Backend),
            }
        }
    }
}

/// Uses the same clock and readiness logic with independent send-side settings.
pub(super) struct DeadlineWriter<'a, S: AsFd>(DeadlineReader<'a, S>);

impl<'a, S: AsFd> DeadlineWriter<'a, S> {
    pub(super) fn new(socket: &'a S, deadlines: ResponseDeadlines) -> Result<Self> {
        let state = DeadlineReader::configured(socket, deadlines, sockopt::Timeout::Send)?;
        #[cfg(target_vendor = "apple")]
        sockopt::set_socket_nosigpipe(socket, true).map_err(|_| Error::Backend)?;
        Ok(Self(state))
    }
}

impl<S: AsFd> git_zero::io::Write for DeadlineWriter<'_, S> {
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
        self.0.check()?;
        while !bytes.is_empty() {
            let call_end = end(now()?, self.0.socket_timeout)?;
            let count = loop {
                self.0.ready(PollFlags::OUT, call_end)?;
                #[cfg(target_vendor = "apple")]
                let flags = SendFlags::DONTWAIT;
                #[cfg(not(target_vendor = "apple"))]
                let flags = SendFlags::DONTWAIT | SendFlags::NOSIGNAL;
                match send(self.0.socket, bytes, flags) {
                    Ok(0) => return Err(Error::Backend),
                    Ok(count) => break count,
                    Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
                    Err(_) => return Err(Error::Backend),
                }
            };
            self.0.check()?;
            self.0.progress = now()?;
            bytes = &bytes[count..];
        }
        Ok(())
    }
}
