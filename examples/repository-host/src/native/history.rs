//! Ephemeral lineage for strictly classified cold v0 reads. No part of this
//! certificate is serialized, supplied by a client, or inferred from pack names.
use super::{Manifest, advertisement};
use crate::http::Request;
use git_zero_core::pktline::{self, Packet};
use sha2::{Digest, Sha256};

const MAX_BYTES: usize = 32 * 1024;
const MAX_REQUEST: usize = 128 * 1024;
const MAX_COMMANDS: usize = 2048;

pub(super) struct Certificate {
    generation: u64,
    binding: [u8; 32],
    identity: [u8; 32],
    floors: Vec<u64>,
    stamps: Vec<u64>,
}

pub(super) struct Projection {
    pub(super) cutoff: u64,
    pub(super) binding: [u8; 32],
    pub(super) packs: Vec<usize>,
}

/// Hash the complete ordered live descriptor graph and exact sorted refs.
/// Retirement debt is deliberately excluded: confirmed deletion of non-live
/// chunks cannot change the object universe or ref-value witnesses.
fn binding(manifest: &Manifest) -> [u8; 32] {
    fn field(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    let mut hash = Sha256::new();
    hash.update(b"git-zero-ephemeral-lineage-v1");
    hash.update(manifest.generation.to_le_bytes());
    hash.update((manifest.packs.len() as u64).to_le_bytes());
    for pack in &manifest.packs {
        field(&mut hash, pack.id.as_bytes());
        hash.update(pack.pack_bytes.to_le_bytes());
        hash.update(pack.index_bytes.to_le_bytes());
        hash.update(pack.level.to_le_bytes());
        for chunks in [&pack.pack, &pack.index, &pack.bundle] {
            hash.update((chunks.len() as u64).to_le_bytes());
            for chunk in chunks {
                field(&mut hash, chunk.key.as_bytes());
                hash.update(chunk.len.to_le_bytes());
            }
        }
    }
    hash.update((manifest.refs.len() as u64).to_le_bytes());
    for (name, id) in &manifest.refs {
        field(&mut hash, name.as_bytes());
        field(&mut hash, id.as_bytes());
    }
    hash.finalize().into()
}

fn branch_refs(manifest: &Manifest) -> bool {
    manifest
        .refs
        .keys()
        .all(|name| name.starts_with("refs/heads/"))
}

impl Certificate {
    /// This bound must be charged BEFORE allocating a simultaneous candidate.
    pub(super) fn allocation_bound(packs: usize, refs: usize) -> Option<usize> {
        let bytes = packs
            .checked_add(refs)?
            .checked_mul(size_of::<u64>())?
            .checked_add(size_of::<Self>())?;
        (bytes <= MAX_BYTES).then_some(bytes)
    }

    pub(super) fn bootstrap(manifest: &Manifest, identity: [u8; 32]) -> Option<Self> {
        Self::allocation_bound(manifest.packs.len(), manifest.refs.len())?;
        if !branch_refs(manifest) {
            return None;
        }
        Some(Self {
            generation: manifest.generation,
            binding: binding(manifest),
            identity,
            // Never derive earlier introduction from physical ordering.
            floors: vec![0; manifest.packs.len()],
            stamps: vec![manifest.generation; manifest.refs.len()],
        })
    }

    pub(super) fn weight(&self) -> usize {
        size_of::<Self>() + (self.floors.capacity() + self.stamps.capacity()) * size_of::<u64>()
    }

    pub(super) fn matches(&self, manifest: &Manifest, identity: Option<[u8; 32]>) -> bool {
        identity == Some(self.identity)
            && self.generation == manifest.generation
            && self.binding == binding(manifest)
            && self.floors.len() == manifest.packs.len()
            && self.stamps.len() == manifest.refs.len()
    }

    pub(super) fn floor(&self, index: usize) -> u64 {
        self.floors[index]
    }

    /// Called only for this process's confirmed strict native transaction. The
    /// floors come through the actual compaction plan, including EVERY input.
    pub(super) fn advance(
        &self,
        previous: &Manifest,
        next: &Manifest,
        identity: Option<[u8; 32]>,
        floors: Vec<u64>,
    ) -> Option<Self> {
        Self::allocation_bound(floors.capacity(), next.refs.len())?;
        if !self.matches(previous, identity)
            || previous.generation.checked_add(1) != Some(next.generation)
            || floors.len() != next.packs.len()
            || floors.iter().any(|floor| *floor > next.generation)
            || !branch_refs(next)
        {
            return None;
        }
        // One merge walk; no retained names, OIDs or lookup maps.
        let mut old = previous.refs.iter().zip(&self.stamps).peekable();
        let stamps = next
            .refs
            .iter()
            .map(|(name, id)| {
                while old.peek().is_some_and(|((prior, _), _)| *prior < name) {
                    old.next();
                }
                match old.peek() {
                    Some(((prior, value), stamp)) if *prior == name && *value == id => **stamp,
                    _ => next.generation,
                }
            })
            .collect();
        Some(Self {
            generation: next.generation,
            binding: binding(next),
            identity: self.identity,
            floors,
            stamps,
        })
    }

    pub(super) fn projected_refs(
        &self,
        manifest: &Manifest,
        cutoff: u64,
    ) -> std::collections::BTreeMap<String, String> {
        manifest
            .refs
            .iter()
            .zip(&self.stamps)
            .filter(|(_, stamp)| **stamp <= cutoff)
            .map(|((name, id), _)| (name.clone(), id.clone()))
            .collect()
    }

    /// Every want AND have must name an exact currently published ref value.
    /// Maximum alias stamps are deliberately conservative. Client OIDs never
    /// create provenance; unknown grammar/state selects ordinary Full handling.
    pub(super) fn plan(
        &self,
        request: &Request,
        manifest: &Manifest,
        identity: Option<[u8; 32]>,
    ) -> Option<Projection> {
        self.selection(request, manifest, identity)
            .filter(|plan| plan.packs.len() < manifest.packs.len())
    }

    // The gateway must use exactly the same parser and selection, including
    // the ordinary all-pack outcome. No parser/closure logic lives in Worker.
    pub(super) fn selection(
        &self,
        request: &Request,
        manifest: &Manifest,
        identity: Option<[u8; 32]>,
    ) -> Option<Projection> {
        if !self.matches(manifest, identity)
            || !branch_refs(manifest)
            || request.method != "POST"
            || request.target != "/repo.git/git-upload-pack"
            || request.headers.get("content-type").map(String::as_str)
                != Some("application/x-git-upload-pack-request")
            || !advertisement::standard_encoding_and_protocol(request)
            || request.headers.contains_key("transfer-encoding")
            || request.body.len() > MAX_REQUEST
        {
            return None;
        }
        let mut input = request.body.as_slice();
        let mut wants = 0;
        let mut commands = 0;
        let mut have_phase = false;
        let mut terminal = false;
        let mut cutoff = 0;
        while let Some(packet) = pktline::read(&mut input).ok()? {
            match packet {
                Packet::Flush if !have_phase && wants > 0 => have_phase = true,
                Packet::Flush if have_phase => {
                    terminal = true;
                    break;
                }
                Packet::Data(b"done\n") if have_phase => {
                    terminal = true;
                    break;
                }
                Packet::Data(data) => {
                    let line = data.strip_suffix(b"\n")?;
                    let (oid, capabilities) = if !have_phase {
                        let want = line.strip_prefix(b"want ")?;
                        let split = want
                            .iter()
                            .position(|byte| *byte == b' ')
                            .unwrap_or(want.len());
                        let (id, rest) = want.split_at(split);
                        if wants != 0 && !rest.is_empty() {
                            return None;
                        }
                        wants += 1;
                        (id, rest)
                    } else {
                        (line.strip_prefix(b"have ")?, b"".as_slice())
                    };
                    if oid.len() != 40 || !oid.iter().all(u8::is_ascii_hexdigit) {
                        return None;
                    }
                    if !capabilities.is_empty() {
                        for capability in
                            capabilities.strip_prefix(b" ")?.split(|byte| *byte == b' ')
                        {
                            if !matches!(
                                capability,
                                b"multi_ack"
                                    | b"multi_ack_detailed"
                                    | b"thin-pack"
                                    | b"side-band"
                                    | b"side-band-64k"
                                    | b"ofs-delta"
                                    | b"no-progress"
                                    | b"no-done"
                                    // Git echoes these advertised capabilities
                                    // even on ordinary, non-shallow fetches.
                                    // Actual deepen commands remain unsupported.
                                    | b"deepen-since"
                                    | b"deepen-not"
                                    | b"object-format=sha1"
                            ) && !(capability.starts_with(b"agent=")
                                && capability.len() > 6
                                && capability[6..]
                                    .iter()
                                    .all(|byte| (b'!'..=b'~').contains(byte)))
                            {
                                return None;
                            }
                        }
                    }
                    let stamp = manifest
                        .refs
                        .values()
                        .zip(&self.stamps)
                        .filter(|(value, _)| value.as_bytes().eq_ignore_ascii_case(oid))
                        .map(|(_, stamp)| *stamp)
                        .max()?;
                    cutoff = cutoff.max(stamp);
                    commands += 1;
                    if commands > MAX_COMMANDS {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        if wants == 0 || !have_phase || !terminal || !input.is_empty() {
            return None;
        }
        let packs: Vec<_> = self
            .floors
            .iter()
            .enumerate()
            .filter_map(|(index, floor)| (*floor <= cutoff).then_some(index))
            .collect();
        // An all-pack selection gains nothing; retain the exact full ref view.
        Some(Projection {
            cutoff,
            binding: self.binding,
            packs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StoredPack;
    use std::collections::BTreeMap;

    fn snapshot(generation: u64, refs: &[(&str, char)], packs: usize) -> Manifest {
        Manifest {
            generation,
            refs: refs
                .iter()
                .map(|(name, id)| (format!("refs/heads/{name}"), id.to_string().repeat(40)))
                .collect(),
            packs: (0..packs)
                .map(|i| StoredPack {
                    id: i.to_string(),
                    ..StoredPack::default()
                })
                .collect(),
            ..Manifest::default()
        }
    }
    fn request(commands: &[&str]) -> Request {
        let mut body = Vec::new();
        for line in commands {
            if line.is_empty() {
                body.extend_from_slice(b"0000");
            } else {
                body.extend_from_slice(format!("{:04x}{line}", line.len() + 4).as_bytes());
            }
        }
        Request {
            method: "POST".into(),
            target: "/repo.git/git-upload-pack".into(),
            headers: BTreeMap::from([(
                "content-type".into(),
                "application/x-git-upload-pack-request".into(),
            )]),
            body,
        }
    }

    #[test]
    fn private_binding_covers_every_live_descriptor_but_not_retirement_debt() {
        let original = snapshot(3, &[("main", 'a')], 2);
        let certificate = Certificate::bootstrap(&original, [7; 32]).unwrap();
        let mut debt = original.clone();
        debt.garbage.push("irrelevant confirmed debt".into());
        assert!(certificate.matches(&debt, Some([7; 32])));
        for variant in 0..12 {
            let mut changed = original.clone();
            match variant {
                0 => changed.generation += 1,
                1 => changed.packs.swap(0, 1),
                2 => changed.packs[0].id.push('x'),
                3 => changed.packs[0].pack_bytes += 1,
                4 => changed.packs[0].index_bytes += 1,
                5 => changed.packs[0].level += 1,
                6 => changed.packs[0].pack.push(crate::storage::Chunk {
                    key: "x".into(),
                    len: 1,
                }),
                7 => changed.packs[0].index.push(crate::storage::Chunk {
                    key: "x".into(),
                    len: 1,
                }),
                8 => changed.packs[0].bundle.push(crate::storage::Chunk {
                    key: "x".into(),
                    len: 1,
                }),
                9 => {
                    changed
                        .refs
                        .insert("refs/heads/alias".into(), "a".repeat(40));
                }
                10 => {
                    changed
                        .refs
                        .insert("refs/heads/main".into(), "b".repeat(40));
                }
                11 => changed.packs.clear(),
                _ => unreachable!(),
            }
            assert!(!certificate.matches(&changed, Some([7; 32])), "{variant}");
        }
        assert!(!certificate.matches(&original, Some([8; 32])));
        assert!(!certificate.matches(&original, None));
    }

    #[test]
    fn ref_stamps_follow_exact_current_names_and_values_through_no_pack_publications() {
        let first = snapshot(3, &[("main", 'a'), ("old", 'b')], 1);
        let proof = Certificate::bootstrap(&first, [1; 32]).unwrap();
        assert_eq!(proof.floors, [0]);
        let next = snapshot(4, &[("alias", 'b'), ("main", 'a'), ("old", 'b')], 1);
        let proof = proof
            .advance(&first, &next, Some([1; 32]), vec![0])
            .unwrap();
        assert_eq!(proof.stamps, [4, 3, 3]);
        let deleted = snapshot(5, &[("alias", 'b'), ("main", 'c')], 2);
        let proof = proof
            .advance(&next, &deleted, Some([1; 32]), vec![0, 5])
            .unwrap();
        assert_eq!(proof.stamps, [4, 5]);
        let readded = snapshot(6, &[("alias", 'b'), ("main", 'c'), ("old", 'b')], 2);
        let proof = proof
            .advance(&deleted, &readded, Some([1; 32]), vec![0, 5])
            .unwrap();
        assert_eq!(proof.stamps, [4, 5, 6]);
        // The newly re-added alias of b expands the cutoff to six.
        let want = format!("want {}\n", "b".repeat(40));
        assert!(
            proof
                .plan(&request(&[&want, "", "done\n"]), &readded, Some([1; 32]))
                .is_none()
        );
        let over = snapshot(u64::MAX, &[("main", 'a')], 0);
        let proof = Certificate::bootstrap(&over, [1; 32]).unwrap();
        assert!(
            proof
                .advance(
                    &over,
                    &snapshot(0, &[("main", 'a')], 0),
                    Some([1; 32]),
                    vec![]
                )
                .is_none()
        );
        assert!(Certificate::allocation_bound(usize::MAX, 1).is_none());
        assert!(Certificate::allocation_bound(4096, 1).is_none());
        assert!(Certificate::bootstrap(&snapshot(1, &[("main", 'a')], 4096), [1; 32]).is_none());
    }

    #[test]
    fn bounded_complete_v0_negotiation_uses_maximum_want_and_have_witness() {
        let prior = snapshot(1, &[("main", 'a'), ("old", 'b')], 1);
        let proof = Certificate::bootstrap(&prior, [1; 32]).unwrap();
        let current = snapshot(2, &[("main", 'c'), ("old", 'b')], 2);
        let proof = proof
            .advance(&prior, &current, Some([1; 32]), vec![0, 2])
            .unwrap();
        let want = format!(
            "want {} multi_ack_detailed no-done side-band-64k thin-pack no-progress ofs-delta deepen-since deepen-not agent=git/2.53.0-Linux\n",
            "b".repeat(40)
        );
        let have = format!("have {}\n", "B".repeat(40));
        let plan = proof
            .plan(&request(&[&want, "", "done\n"]), &current, Some([1; 32]))
            .unwrap();
        assert_eq!(plan.cutoff, 1);
        assert_eq!(plan.packs, [0]);
        for last in ["done\n", ""] {
            let plan = proof
                .plan(&request(&[&want, "", &have, last]), &current, Some([1; 32]))
                .unwrap();
            assert_eq!(plan.cutoff, 1);
            assert_eq!(plan.packs, [0]);
        }
        let newer = format!("have {}\n", "c".repeat(40));
        assert!(
            proof
                .plan(
                    &request(&[&want, "", &newer, "done\n"]),
                    &current,
                    Some([1; 32])
                )
                .is_none()
        );
        let unknown = format!("have {}\n", "d".repeat(40));
        let unknown_want = format!("want {}\n", "d".repeat(40));
        for lines in [
            vec![&want[..], "", &unknown, "done\n"],
            vec![&unknown_want, "", "done\n"],
            vec![&want, ""],
            vec![&want, "", "done\n", ""],
            vec!["", "done\n"],
            vec![&want, "", "have malformed\n", "done\n"],
            vec![&want, "", "shallow x\n", "done\n"],
        ] {
            assert!(
                proof
                    .plan(&request(&lines), &current, Some([1; 32]))
                    .is_none(),
                "{lines:?}"
            );
        }
        for capability in [
            "include-tag",
            "filter",
            "shallow",
            "deepen-relative",
            "deepen-since=1",
            "deepen-not=refs/heads/old",
            "allow-any-sha1-in-want",
            "unknown",
            "agent=",
            "",
            "agent=a\tb",
        ] {
            let want = format!("want {} {capability}\n", "b".repeat(40));
            assert!(
                proof
                    .plan(&request(&[&want, "", "done\n"]), &current, Some([1; 32]))
                    .is_none(),
                "{capability}"
            );
        }
        // Capability acknowledgement does not authorize shallow command
        // packets in either phase of the bounded request grammar.
        for command in [
            format!("shallow {}\n", "b".repeat(40)),
            "deepen 1\n".into(),
            "deepen-since 1735689600\n".into(),
            "deepen-not refs/heads/old\n".into(),
            "filter blob:none\n".into(),
        ] {
            for lines in [
                vec![want.as_str(), &command, "", "done\n"],
                vec![want.as_str(), "", &command, "done\n"],
            ] {
                assert!(
                    proof
                        .plan(&request(&lines), &current, Some([1; 32]))
                        .is_none(),
                    "{lines:?}"
                );
            }
        }
        let valid = request(&[&want, "", "done\n"]);
        for (header, value) in [
            ("git-protocol", "version=2"),
            ("content-encoding", "gzip"),
            ("transfer-encoding", "chunked"),
            ("content-type", "application/x-other"),
        ] {
            let mut changed = valid.clone();
            changed.headers.insert(header.into(), value.into());
            assert!(proof.plan(&changed, &current, Some([1; 32])).is_none());
        }
        let mut huge = valid.clone();
        huge.body.resize(MAX_REQUEST + 1, 0);
        assert!(proof.plan(&huge, &current, Some([1; 32])).is_none());
        let mut truncated = valid.clone();
        truncated.body.pop();
        assert!(proof.plan(&truncated, &current, Some([1; 32])).is_none());
        let have = format!("have {}\n", "b".repeat(40));
        let mut lines = vec![want.as_str(), ""];
        lines.extend(std::iter::repeat_n(have.as_str(), MAX_COMMANDS));
        lines.push("done\n");
        assert!(
            proof
                .plan(&request(&lines), &current, Some([1; 32]))
                .is_none()
        );
    }
}
