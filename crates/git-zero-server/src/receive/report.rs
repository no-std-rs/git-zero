use super::Capabilities;
use git_zero_core::{Error, Result, io::Write, pktline, repository::validate_ref_name_bytes};

/// A report-status/v2 encoder. Success is a host assertion: construct a report
/// only after pack/graph validation and the authoritative publication outcome.
/// Report v2 uses ordinary statuses when refs have not been rewritten by hooks.
pub struct Report<'a, W: Write> {
    output: &'a mut W,
    encoder: Status,
}

/// Detached report encoder for paged ref outcomes in external storage. Every
/// call accepts a new bounded sink. Discard all output after any sink failure.
pub struct Status {
    selected: Capabilities,
    enabled: bool,
    started: bool,
    finished: bool,
}

impl Status {
    pub fn new(selected: Capabilities) -> Self {
        Self {
            selected,
            enabled: selected.contains(Capabilities::REPORT_STATUS)
                || selected.contains(Capabilities::REPORT_STATUS_V2),
            started: false,
            finished: false,
        }
    }

    pub fn unpack(
        &mut self,
        outcome: core::result::Result<(), &[u8]>,
        output: &mut impl Write,
    ) -> Result<()> {
        if self.started || self.finished {
            return Err(Error::InvalidRequest);
        }
        let message = outcome.err().unwrap_or(b"ok");
        valid_message(message)?;
        if outcome.is_err() && message == b"ok" {
            return Err(Error::InvalidRequest);
        }
        self.started = true;
        if self.enabled {
            pktline::write_parts(
                &mut Band {
                    output,
                    selected: self.selected,
                },
                &[b"unpack ", message, b"\n"],
            )?;
        }
        Ok(())
    }

    pub fn reference(
        &mut self,
        name: &[u8],
        outcome: core::result::Result<(), &[u8]>,
        output: &mut impl Write,
    ) -> Result<()> {
        if !self.started || self.finished {
            return Err(Error::InvalidRequest);
        }
        validate_ref_name_bytes(name)?;
        if let Err(message) = outcome {
            valid_message(message)?;
        }
        if !self.enabled {
            return Ok(());
        }
        let mut band = Band {
            output,
            selected: self.selected,
        };
        match outcome {
            Ok(()) => pktline::write_parts(&mut band, &[b"ok ", name, b"\n"]),
            Err(message) => pktline::write_parts(&mut band, &[b"ng ", name, b" ", message, b"\n"]),
        }
    }

    pub fn finish(&mut self, output: &mut impl Write) -> Result<()> {
        if !self.started || self.finished {
            return Err(Error::InvalidRequest);
        }
        self.finished = true;
        if self.enabled {
            Band {
                output,
                selected: self.selected,
            }
            .write_all(b"0000")?;
        }
        if self.selected.contains(Capabilities::SIDE_BAND_64K) {
            output.write_all(b"0000")?;
        }
        Ok(())
    }
}

impl<'a, W: Write> Report<'a, W> {
    pub fn new(
        output: &'a mut W,
        selected: Capabilities,
        unpack: core::result::Result<(), &[u8]>,
    ) -> Result<Self> {
        let mut encoder = Status::new(selected);
        encoder.unpack(unpack, output)?;
        Ok(Self { output, encoder })
    }

    pub fn reference(
        &mut self,
        name: &[u8],
        outcome: core::result::Result<(), &[u8]>,
    ) -> Result<()> {
        self.encoder.reference(name, outcome, self.output)
    }

    pub fn finish(mut self) -> Result<()> {
        self.encoder.finish(self.output)
    }
}

struct Band<'a, W> {
    output: &'a mut W,
    selected: Capabilities,
}
impl<W: Write> Write for Report<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        Band {
            output: self.output,
            selected: self.encoder.selected,
        }
        .write_all(bytes)
    }
}
impl<W: Write> Write for Band<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        if self.selected.contains(Capabilities::SIDE_BAND_64K) {
            write_band(self.output, 1, bytes)
        } else {
            self.output.write_all(bytes)
        }
    }
}

/// Emit processing messages before the publication outcome is known, without
/// sending an unpack/ref success status. Quiet and non-sideband clients get
/// no bytes. The host starts `Report` only after validation/publication completes.
pub fn write_progress(
    output: &mut impl Write,
    selected: Capabilities,
    message: &[u8],
) -> Result<()> {
    if selected.contains(Capabilities::SIDE_BAND_64K) && !selected.contains(Capabilities::QUIET) {
        write_band(output, 2, message)?;
    }
    Ok(())
}

fn write_band(output: &mut impl Write, channel: u8, bytes: &[u8]) -> Result<()> {
    for chunk in bytes.chunks(pktline::MAX_LENGTH - 5) {
        pktline::write_parts(output, &[&[channel], chunk])?;
    }
    Ok(())
}

fn valid_message(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n')) {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}
