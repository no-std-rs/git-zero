//! A certificate for one native v0 branch advertisement policy. It retains no
//! refs or objects, and is learned only from an entire ordinary native response.
use super::{MAX_REF_BYTES, MAX_REFS, NATIVE_CONFIG, command, validate_snapshot_refs};
use crate::http::{Request, Response};
use crate::storage::Manifest;
use git_zero_core::pktline::{self, Packet};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io, path::Path, process::Command};

const MAIN: &str = "refs/heads/main";
const CONTENT_TYPE: &str = "application/x-git-upload-pack-advertisement";
const PREFIX: &[u8] = b"# service=git-upload-pack\n";
const MAX_CAPABILITIES: usize = 1024;
const MAX_ADVERTISEMENT: usize = MAX_REF_BYTES + MAX_REFS * 6 + MAX_CAPABILITIES + 128;
// Order and policy must match native Git exactly; only its agent is learned.
const CAPABILITIES: &[u8] = b"multi_ack thin-pack side-band side-band-64k ofs-delta shallow deepen-since deepen-not deepen-relative no-progress include-tag multi_ack_detailed no-done symref=HEAD:refs/heads/main object-format=sha1 agent=";

fn packet(output: &mut Vec<u8>, parts: &[&[u8]]) -> io::Result<()> {
    struct Output<'a>(&'a mut Vec<u8>);
    impl git_zero_core::io::Write for Output<'_> {
        fn write_all(&mut self, bytes: &[u8]) -> git_zero_core::Result<()> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }
    pktline::write_parts(&mut Output(output), parts)
        .map_err(|_| io::Error::other("ref advertisement packet too long"))
}

pub(super) struct Certificate {
    generation: u64,
    identity: [u8; 32],
    capabilities: Box<[u8]>,
}

pub(super) fn eligible_request(request: &Request) -> bool {
    request.method == "GET"
        && request.target == "/repo.git/info/refs?service=git-upload-pack"
        && request.body.is_empty()
        && !request.headers.contains_key("content-type")
        && !request.headers.contains_key("transfer-encoding")
        && request
            .headers
            .get("content-length")
            .is_none_or(|value| value == "0")
        && standard_encoding_and_protocol(request)
}

pub(super) fn eligible_receive(request: &Request) -> bool {
    request.method == "POST"
        && request.target == "/repo.git/git-receive-pack"
        && request.headers.get("content-type").map(String::as_str)
            == Some("application/x-git-receive-pack-request")
        && standard_encoding_and_protocol(request)
}

fn standard_encoding_and_protocol(request: &Request) -> bool {
    request
        .headers
        .get("git-protocol")
        .is_none_or(|value| value.is_empty() || value == "version=0")
        && request
            .headers
            .get("content-encoding")
            .is_none_or(|value| value.eq_ignore_ascii_case("identity"))
}

fn eligible_refs(manifest: &Manifest) -> bool {
    manifest.refs.contains_key(MAIN)
        && manifest
            .refs
            .keys()
            .all(|name| name.starts_with("refs/heads/"))
        && validate_snapshot_refs(&manifest.refs).is_ok()
}

/// Refuse configuration injection even if it happens not to affect the first
/// ref set: a hideRefs rule could otherwise affect a subsequently added branch.
/// The wrapper overwrites its own listed variables, and GIT_ZERO_* is host-only.
fn fixed_environment(name: &str) -> bool {
    !name.starts_with("GIT_")
        || name.starts_with("GIT_ZERO_")
        || matches!(
            name,
            "GIT_CONFIG_NOSYSTEM"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_DIR"
                | "GIT_WORK_TREE"
                | "GIT_PROTOCOL"
                | "GIT_USER_AGENT"
                | "GIT_PAGER"
        )
}

/// Fixed local policy plus the resolved executable and child environment.
/// Ordinary same-generation reconstruction writes the same config and HEAD.
/// Unknown identity or local policy changes always use native Git instead.
pub(super) fn identity(view: &Path) -> Option<[u8; 32]> {
    if fs::read(view.join("config")).ok()?.as_slice() != NATIVE_CONFIG
        || fs::read(view.join("HEAD")).ok()?.as_slice() != b"ref: refs/heads/main\n"
    {
        return None;
    }
    let mut environment: Vec<_> = std::env::vars_os().collect();
    environment.sort();
    let mut digest = Sha256::new();
    for (name, value) in environment {
        if !fixed_environment(name.to_str()?) {
            return None;
        }
        for part in [name.as_encoded_bytes(), value.as_encoded_bytes()] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
    }
    let executable = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|path| path.join("git"))
        .find(|path| path.is_file())?
        .canonicalize()
        .ok()?;
    let metadata = fs::metadata(&executable).ok()?;
    digest.update(executable.as_os_str().as_encoded_bytes());
    digest.update(metadata.len().to_le_bytes());
    digest.update(
        metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
            .to_le_bytes(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            digest.update(value.to_le_bytes());
        }
        if metadata.mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(digest.finalize().into())
}

/// Native advertisement bytes alone cannot distinguish a legacy branch that
/// points directly to a blob. Check types once while bootstrap is already Full;
/// subsequent native branch transactions require commits themselves.
pub(super) fn branch_commits(view: &Path, manifest: &Manifest) -> bool {
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for id in manifest.refs.values() {
        let id = id.to_ascii_lowercase();
        input.extend_from_slice(id.as_bytes());
        input.push(b'\n');
        expected.extend_from_slice(id.as_bytes());
        expected.extend_from_slice(b" commit\n");
    }
    let mut process = Command::new("git");
    process
        .arg("-C")
        .arg(view)
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]);
    command(process, &input).is_ok_and(|output| output.stdout == expected)
}

impl Certificate {
    pub(super) fn learn(
        manifest: &Manifest,
        response: &Response,
        identity: [u8; 32],
    ) -> Option<Self> {
        if !eligible_refs(manifest)
            || response.status != 200
            || response.headers != headers()
            || response.body.len() > MAX_ADVERTISEMENT
        {
            return None;
        }
        let mut input = response.body.as_ref();
        if pktline::read(&mut input).ok()? != Some(Packet::Data(PREFIX))
            || pktline::read(&mut input).ok()? != Some(Packet::Flush)
        {
            return None;
        }
        let Some(Packet::Data(head)) = pktline::read(&mut input).ok()? else {
            return None;
        };
        let nul = head.iter().position(|byte| *byte == 0)?;
        let capabilities = &head[nul + 1..];
        let capabilities = capabilities.strip_suffix(b"\n")?;
        let agent = capabilities.strip_prefix(CAPABILITIES)?;
        if capabilities.len() > MAX_CAPABILITIES
            || agent.is_empty()
            || !agent.iter().all(|byte| (b'!'..=b'~').contains(byte))
        {
            return None;
        }
        for _ in &manifest.refs {
            if !matches!(pktline::read(&mut input).ok()?, Some(Packet::Data(_))) {
                return None;
            }
        }
        if pktline::read(&mut input).ok()? != Some(Packet::Flush) || !input.is_empty() {
            return None;
        }
        let certificate = Self {
            generation: manifest.generation,
            identity,
            capabilities: capabilities.into(),
        };
        // Re-encoding and comparing the ENTIRE bounded body enforces strict
        // framing, normalized OIDs, exact HEAD, sorted complete refs, no peeled
        // or shallow rows, and exactly one terminal flush with no trailing data.
        let expected = certificate.encode(manifest).ok()?;
        (expected.body == response.body).then_some(certificate)
    }

    pub(super) fn matches(&self, manifest: &Manifest, identity: Option<[u8; 32]>) -> bool {
        self.generation == manifest.generation && identity == Some(self.identity)
    }

    pub(super) fn advance(&mut self, previous: &Manifest, next: &Manifest) -> bool {
        if self.generation != previous.generation
            || previous.generation.checked_add(1) != Some(next.generation)
            || !eligible_refs(next)
        {
            return false;
        }
        self.generation = next.generation;
        true
    }

    pub(super) fn weight(&self) -> usize {
        // Include bookkeeping just like ordinary response entries; the only
        // heap allocation is this independently bounded capability template.
        self.capabilities.len() + 256
    }

    pub(super) fn encode(&self, manifest: &Manifest) -> io::Result<Response> {
        if !eligible_refs(manifest) {
            return Err(io::Error::other("uncertified advertisement ref shape"));
        }
        let mut body = Vec::new();
        packet(&mut body, &[PREFIX])?;
        body.extend_from_slice(b"0000");
        let main = manifest.refs[MAIN].to_ascii_lowercase();
        packet(
            &mut body,
            &[main.as_bytes(), b" HEAD\0", &self.capabilities, b"\n"],
        )?;
        for (name, id) in &manifest.refs {
            packet(
                &mut body,
                &[
                    id.to_ascii_lowercase().as_bytes(),
                    b" ",
                    name.as_bytes(),
                    b"\n",
                ],
            )?;
        }
        body.extend_from_slice(b"0000");
        Ok(Response {
            status: 200,
            headers: headers(),
            body: body.into(),
        })
    }
}

fn headers() -> BTreeMap<String, String> {
    BTreeMap::from([("Content-Type".into(), CONTENT_TYPE.into())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_environment_is_not_trusted_as_publication_policy() {
        for name in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG",
            "GIT_NAMESPACE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_SHALLOW_FILE",
            "GIT_REPLACE_REF_BASE",
        ] {
            assert!(!fixed_environment(name), "{name}");
        }
        for name in [
            "GIT_USER_AGENT",
            "GIT_PAGER",
            "GIT_ZERO_CLOUDFLARE_ORIGIN",
            "GIT_CONFIG_GLOBAL",
        ] {
            assert!(fixed_environment(name), "{name}");
        }
    }

    #[test]
    fn packet_and_capability_bounds_reject_oversized_native_data() {
        let mut body = Vec::new();
        assert!(packet(&mut body, &[&vec![b'x'; pktline::MAX_LENGTH - 4]]).is_ok());
        assert!(packet(&mut body, &[&vec![b'x'; pktline::MAX_LENGTH - 3]]).is_err());
        let manifest = Manifest {
            refs: BTreeMap::from([("refs/heads/main".into(), "a".repeat(40))]),
            ..Manifest::default()
        };
        let certificate = Certificate {
            generation: 0,
            identity: [0; 32],
            capabilities: [CAPABILITIES, &vec![b'x'; MAX_CAPABILITIES]]
                .concat()
                .into(),
        };
        let response = certificate.encode(&manifest).unwrap();
        assert!(Certificate::learn(&manifest, &response, [0; 32]).is_none());
        let mut too_long = manifest;
        too_long.refs.insert(
            format!("refs/heads/{}", "x".repeat(pktline::MAX_LENGTH)),
            "a".repeat(40),
        );
        assert!(certificate.encode(&too_long).is_err());
    }
}
