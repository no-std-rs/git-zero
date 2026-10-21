//! Receive-pack wire handling. The host authenticates requests, quarantines
//! objects, resolves deltas, validates graph/ref updates, and publishes them.
//! This module never acknowledges a push or changes refs on its own.
use git_zero_core::{Error, ObjectFormat, Result, io::Write};

mod advertisement;
mod ancestry;
mod control;
mod report;
pub use advertisement::{Advertisement, write_advertisement};
pub use ancestry::Ancestry;
pub use control::{Command, ControlDecoder, ControlEvent, ControlProgress, Limits};
pub use report::{Report, Status, write_progress};

const FLAGS: &[(&[u8], u16)] = &[
    (b"report-status", 1),
    (b"report-status-v2", 2),
    (b"delete-refs", 4),
    (b"ofs-delta", 8),
    (b"atomic", 16),
    (b"push-options", 32),
    (b"side-band-64k", 64),
    (b"quiet", 128),
];

/// Capabilities offered by a host or selected by a client. Hosts must opt in
/// only when their publication/option/progress handling fulfills the promise.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities(u16);

impl Capabilities {
    pub const REPORT_STATUS: Self = Self(1);
    pub const REPORT_STATUS_V2: Self = Self(2);
    pub const DELETE_REFS: Self = Self(4);
    pub const OFS_DELTA: Self = Self(8);
    pub const ATOMIC: Self = Self(16);
    pub const PUSH_OPTIONS: Self = Self(32);
    pub const SIDE_BAND_64K: Self = Self(64);
    pub const QUIET: Self = Self(128);

    pub const fn contains(self, capability: Self) -> bool {
        self.0 & capability.0 == capability.0
    }

    fn parse(bytes: &[u8], format: ObjectFormat, offered: Self) -> Result<Self> {
        let mut selected = 0;
        let mut format_seen = false;
        let mut agent_seen = false;
        for token in bytes
            .split(|&byte| byte == b' ')
            .filter(|token| !token.is_empty())
        {
            if token == format.capability() {
                if format_seen {
                    return Err(Error::InvalidRequest);
                }
                format_seen = true;
                continue;
            }
            if let Some(agent) = token.strip_prefix(b"agent=") {
                if agent_seen
                    || agent.is_empty()
                    || !agent.iter().all(|byte| byte.is_ascii_graphic())
                {
                    return Err(Error::InvalidRequest);
                }
                agent_seen = true;
                continue;
            }
            let flag = FLAGS
                .iter()
                .find_map(|&(name, flag)| (name == token).then_some(flag))
                .ok_or(Error::Unsupported)?;
            if offered.0 & flag == 0 || selected & flag != 0 {
                return Err(Error::InvalidRequest);
            }
            selected |= flag;
        }
        if format == ObjectFormat::Sha256 && !format_seen {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(selected))
    }

    fn write(self, format: ObjectFormat, output: &mut impl Write) -> Result<()> {
        output.write_all(format.capability())?;
        for &(name, flag) in FLAGS {
            if self.0 & flag != 0 {
                output.write_all(b" ")?;
                output.write_all(name)?;
            }
        }
        output.write_all(b" agent=git-zero")
    }
}

impl core::ops::BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
