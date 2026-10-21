use git_zero::{
    Error, ObjectFormat, ObjectInfo, ObjectKind,
    object::{LinkDecoder, LinkLimits, LinkRelation, LinkStatus},
};
use sha1::{Digest, Sha1};

pub fn check(bytes: &[u8], kind: ObjectKind, format: ObjectFormat) {
    let chunk = usize::from(bytes.first().copied().unwrap_or(0) % 31) + 1;
    assert_eq!(
        scan(bytes, kind, format, chunk),
        scan(bytes, kind, format, bytes.len().max(1))
    );
}

fn scan(
    bytes: &[u8],
    kind: ObjectKind,
    format: ObjectFormat,
    chunk: usize,
) -> git_zero::Result<[u8; 20]> {
    let mut names = [0xa5; 513];
    let mut scanner = LinkDecoder::new(
        ObjectInfo {
            kind,
            size: bytes.len() as u64,
        },
        format,
        &mut names[..512],
        LinkLimits {
            max_body_bytes: super::MAX_INPUT as u64,
            max_links: 128,
        },
    )?;
    let mut digest = Sha1::new();
    let mut offset = 0;
    for _ in 0..bytes.len() * 2 + 3 {
        let end = (offset + chunk).min(bytes.len());
        let result = scanner.update(&bytes[offset..end], end == bytes.len());
        let step = match result {
            Ok(step) => step,
            Err(error) => {
                assert_eq!(scanner.finish(), Err(error));
                assert_eq!(scanner.update(bytes, true).unwrap_err(), error);
                assert_eq!(names[512], 0xa5);
                return Err(error);
            }
        };
        assert!(step.consumed <= end - offset);
        offset += step.consumed;
        match step.status {
            LinkStatus::NeedInput => assert_eq!(offset, end),
            event @ (LinkStatus::Link { .. } | LinkStatus::TreeEntry { .. }) => {
                hash_link(&mut digest, event)
            }
            LinkStatus::Finished => {
                assert_eq!(offset, bytes.len());
                scanner.finish().unwrap();
                assert_eq!(
                    scanner.update(&[], true).unwrap().status,
                    LinkStatus::Finished
                );
                assert_eq!(
                    scanner.update(&[0], true).unwrap_err(),
                    Error::InvalidObject
                );
                assert_eq!(names[512], 0xa5);
                return Ok(digest.finalize().into());
            }
        }
        assert_eq!(scanner.position(), offset as u64);
    }
    panic!("object links failed to make bounded progress")
}

fn hash_link(digest: &mut Sha1, event: LinkStatus<'_>) {
    match event {
        LinkStatus::Link { id, kind, relation } => {
            let relation = match relation {
                LinkRelation::RootTree => 0,
                LinkRelation::Parent => 1,
                LinkRelation::TagTarget => 2,
            };
            digest.update([0, kind as u8, relation]);
            digest.update(id.as_bytes());
        }
        LinkStatus::TreeEntry {
            id,
            mode,
            name,
            kind,
            canonical_mode,
            ..
        } => {
            assert!(name.len() <= 256);
            digest.update([
                1,
                kind.map_or(0, |kind| kind as u8),
                u8::from(canonical_mode),
            ]);
            digest.update(id.as_bytes());
            digest.update(mode.to_be_bytes());
            digest.update((name.len() as u32).to_be_bytes());
            digest.update(name);
        }
        _ => unreachable!("only structural link events have digest records"),
    }
}
