//! Explicit host permission for wants; packet/body contents never grant access.
#[cfg(feature = "read")]
use git_zero_core::{Error, Repository, Result, io::Write};

/// The host must authenticate and supply one immutable, authorized repository
/// projection. Validation proves its exact reachable union; it does not grant ACLs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WantPolicy {
    #[default]
    AdvertisedOnly,
    /// Permit all object kinds in the supplied validated visible closure.
    AllVisibleReachable,
}

/// Additional request policy, separate from existing selection storage literals.
/// Occurrences include duplicates. Existing wrappers preserve their strict rule
/// without adding a new occurrence limit. Hosts must still bound request bytes.
///
/// This is not a total logical-work limit: metadata scans remain bounded by
/// object/ref capacities, and structural reads by the existing shared graph cap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WantOptions {
    pub policy: WantPolicy,
    pub max_want_lines: usize,
}

impl Default for WantOptions {
    fn default() -> Self {
        Self::STRICT
    }
}

impl WantOptions {
    pub const STRICT: Self = Self {
        policy: WantPolicy::AdvertisedOnly,
        max_want_lines: usize::MAX,
    };

    pub const fn all_visible_reachable(max_want_lines: usize) -> Self {
        Self {
            policy: WantPolicy::AllVisibleReachable,
            max_want_lines,
        }
    }
}

#[cfg(feature = "read")]
pub(crate) const REACHABLE: &[u8] = b" allow-reachable-sha1-in-want";

#[cfg(feature = "read")]
pub(crate) struct Discard;
#[cfg(feature = "read")]
impl Write for Discard {
    fn write_all(&mut self, _: &[u8]) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "read")]
pub(crate) fn info_refs(
    repository: &impl Repository,
    route: &crate::route::Route<'_>,
    output: &mut impl Write,
    options: WantOptions,
) -> Result<crate::Response> {
    if options.policy == WantPolicy::AdvertisedOnly
        || route.service != Some("git-upload-pack")
        || route.version == crate::route::Version::V2
    {
        return crate::serve_info_refs(repository, route, output, true);
    }
    // Reuse the exact packet generator for preflight, including the unborn
    // first-ref carrier and resolved symref. No service/success prefix escapes
    // when the added capability would overflow a packet. Defaults stay intact.
    let ordinary = git_zero_core::repository::UPLOAD_CAPABILITIES;
    let mut capabilities =
        [0; git_zero_core::repository::UPLOAD_CAPABILITIES.len() + REACHABLE.len()];
    capabilities[..ordinary.len()].copy_from_slice(ordinary);
    capabilities[ordinary.len()..].copy_from_slice(REACHABLE);
    crate::advertise_smart_with_capabilities(
        repository,
        route.version,
        &mut Discard,
        true,
        &capabilities,
    )
    .map_err(|error| {
        if error == Error::InvalidPacket {
            Error::LimitExceeded
        } else {
            error
        }
    })?;
    crate::advertise_smart_with_capabilities(
        repository,
        route.version,
        output,
        true,
        &capabilities,
    )?;
    Ok(crate::Response {
        status: 200,
        content_type: "application/x-git-upload-pack-advertisement",
    })
}
