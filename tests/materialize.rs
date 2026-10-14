#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, Object, ObjectId, ObjectKind,
    io::SliceWriter,
    pack::{self, DeltaLimits, MaterializeLimits, MaterializedEntry},
    zlib::{self, InflateLimits},
};
use sha1::{Digest, Sha1};

fn limits() -> MaterializeLimits {
    MaterializeLimits {
        inflate: InflateLimits {
            max_output: 128,
            max_ratio: 1024,
            ratio_slack: 65536,
        },
        delta: DeltaLimits {
            max_output: 128,
            max_instructions: 128,
            max_depth: 64,
        },
        max_passes: 65,
        max_decoded_bytes_per_pass: 4096,
        max_base_probes: 4096,
        max_tree_validation_bytes: 4096,
    }
}

fn append_zlib(bytes: &mut Vec<u8>, body: &[u8]) {
    let mut storage = [0; 256];
    let mut writer = SliceWriter::new(&mut storage);
    zlib::write_parts(&mut writer, &[body]).unwrap();
    bytes.extend_from_slice(writer.as_bytes());
}

fn finish_pack(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.extend_from_slice(&Sha1::digest(&bytes));
    bytes
}

fn shared_forward_base_pack(count: u32) -> Vec<u8> {
    let base = Object {
        kind: ObjectKind::Blob,
        data: b"x",
    };
    let mut bytes = b"PACK\0\0\0\x02".to_vec();
    bytes.extend_from_slice(&count.to_be_bytes());
    for _ in 1..count {
        bytes.push(0x74);
        bytes.extend_from_slice(base.id().as_bytes());
        append_zlib(&mut bytes, b"\x01\x01\x01y");
    }
    bytes.push(0x31);
    append_zlib(&mut bytes, base.data);
    finish_pack(bytes)
}

#[test]
fn shared_forward_bases_stop_at_the_metadata_probe_budget() {
    for count in [64, 32768] {
        let wire = shared_forward_base_pack(count);
        let mut index = vec![MaterializedEntry::default(); count as usize];
        let mut arena = vec![0; count as usize];
        let mut scratch = [0; 128];
        let mut window = [0; 32768];
        allocations::without_allocations(|| {
            let result = pack::materialize(
                &wire,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                MaterializeLimits {
                    max_decoded_bytes_per_pass: u64::from(count) * 4,
                    max_base_probes: if count == 64 {
                        64 * 63
                    } else {
                        u64::from(count)
                    },
                    ..limits()
                },
            );
            if count == 64 {
                assert_eq!(result.unwrap().bytes_used(), 64);
            } else {
                assert!(matches!(result, Err(Error::LimitExceeded)));
            }
        });
    }
}

#[test]
fn tree_validation_work_is_shared_across_imported_objects() {
    let mut tree = b"100644 x\0".to_vec();
    tree.extend_from_slice(&[7; 20]);
    let mut wire = b"PACK\0\0\0\x02\0\0\0\x02".to_vec();
    for _ in 0..2 {
        wire.extend_from_slice(&[0xa0 | (tree.len() as u8 & 15), (tree.len() >> 4) as u8]);
        append_zlib(&mut wire, &tree);
    }
    let wire = finish_pack(wire);
    let mut index = [MaterializedEntry::default(); 2];
    let mut arena = [0; 128];
    let mut scratch = [0; 128];
    let mut window = [0; 32768];
    for budget in [tree.len() as u64, (tree.len() * 2) as u64] {
        let result = pack::materialize(
            &wire,
            &mut index,
            &mut arena,
            &mut scratch,
            &mut window,
            MaterializeLimits {
                max_tree_validation_bytes: budget,
                ..limits()
            },
        );
        if budget == tree.len() as u64 {
            assert!(matches!(result, Err(Error::LimitExceeded)));
        } else {
            assert_eq!(result.unwrap().bytes_used(), tree.len() * 2);
        }
    }
}

fn delta_pack(forward: bool) -> Vec<u8> {
    let base = Object {
        kind: ObjectKind::Blob,
        data: b"hello",
    };
    let mut bytes = b"PACK\0\0\0\x02\0\0\0\x02".to_vec();
    let delta = b"\x05\x05\x05world";
    if forward {
        bytes.push(0x78);
        bytes.extend_from_slice(base.id().as_bytes());
        append_zlib(&mut bytes, delta);
        bytes.push(0x35);
        append_zlib(&mut bytes, base.data);
    } else {
        bytes.push(0x35);
        append_zlib(&mut bytes, base.data);
        let distance = bytes.len() - 12;
        assert!(distance < 128);
        bytes.extend_from_slice(&[0x68, distance as u8]);
        append_zlib(&mut bytes, delta);
    }
    bytes.extend_from_slice(&Sha1::digest(&bytes));
    bytes
}

#[test]
fn forward_reference_and_offset_deltas_become_shared_searchable_objects_without_allocation() {
    for forward in [true, false] {
        let wire = delta_pack(forward);
        let mut index = [MaterializedEntry::default(); 3];
        let mut arena = [0; 128];
        let mut scratch = [0; 128];
        let mut window = [0; 32768];
        allocations::without_allocations(|| {
            let store = pack::materialize(
                &wire,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                limits(),
            )
            .unwrap();
            assert_eq!(store.bytes_used(), 10);
            assert_eq!(store.entries().len(), 2);
            assert!(store.entries()[0].id() < store.entries()[1].id());
            for data in [b"hello".as_slice(), b"world"] {
                let expected = Object {
                    kind: ObjectKind::Blob,
                    data,
                };
                let found = store.find(expected.id()).unwrap();
                assert_eq!(found.kind, expected.kind);
                assert_eq!(found.data, data);
                let entry = pack::find_materialized(store.entries(), expected.id()).unwrap();
                assert_eq!(entry.delta_depth(), u32::from(data == b"world"));
                assert_eq!(entry.kind(), Ok(ObjectKind::Blob));
            }
            assert!(store.find(ObjectId::Sha1([0; 20])).is_none());
            assert!(store.object(2).is_none());
        });
        assert!(index[2].kind().is_err());
    }
}

#[test]
fn pack_corruption_capacity_depth_pass_and_work_limits_fail_and_retry_cleanly() {
    let wire = delta_pack(true);
    let mut index = [MaterializedEntry::default(); 2];
    let mut arena = [0; 128];
    let mut scratch = [0; 128];
    let mut window = [0; 32768];
    let scenarios = [
        MaterializeLimits {
            max_passes: 1,
            ..limits()
        },
        MaterializeLimits {
            max_decoded_bytes_per_pass: 1,
            ..limits()
        },
        MaterializeLimits {
            max_base_probes: 1,
            ..limits()
        },
        MaterializeLimits {
            delta: DeltaLimits {
                max_depth: 0,
                ..limits().delta
            },
            ..limits()
        },
    ];
    for limit in scenarios {
        assert!(matches!(
            pack::materialize(
                &wire,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                limit
            ),
            Err(Error::LimitExceeded)
        ));
        assert!(
            pack::materialize(
                &wire,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                limits()
            )
            .is_ok()
        );
    }
    assert!(matches!(
        pack::materialize(
            &wire,
            &mut index[..1],
            &mut arena,
            &mut scratch,
            &mut window,
            limits()
        ),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        pack::materialize(
            &wire,
            &mut index,
            &mut arena[..2],
            &mut scratch,
            &mut window,
            limits()
        ),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        pack::materialize(
            &wire,
            &mut index,
            &mut arena,
            &mut scratch[..2],
            &mut window,
            limits()
        ),
        Err(Error::BufferTooSmall)
    ));
    let mut corrupt = wire.clone();
    corrupt[12] ^= 1;
    assert!(matches!(
        pack::materialize(
            &corrupt,
            &mut index,
            &mut arena,
            &mut scratch,
            &mut window,
            limits()
        ),
        Err(Error::CorruptPack)
    ));
    let mut absent = wire.clone();
    absent[13..33].fill(0);
    let trailer = absent.len() - 20;
    let checksum = Sha1::digest(&absent[..trailer]);
    absent[trailer..].copy_from_slice(&checksum);
    assert!(matches!(
        pack::materialize(
            &absent,
            &mut index,
            &mut arena,
            &mut scratch,
            &mut window,
            limits()
        ),
        Err(Error::CorruptPack)
    ));
}

#[test]
fn large_indexes_sort_with_duplicates_and_empty_packs_need_no_storage() {
    for count in [0_usize, 1, 2, 32768] {
        let mut wire = b"PACK\0\0\0\x02".to_vec();
        wire.extend_from_slice(&(count as u32).to_be_bytes());
        let mut expected = Vec::new();
        for i in 0..count {
            let body = (((i * 7919) % 10007) as u32).to_be_bytes();
            let object = Object {
                kind: ObjectKind::Blob,
                data: &body,
            };
            expected.push(object.id());
            wire.push(0x34);
            append_zlib(&mut wire, &body);
        }
        wire.extend_from_slice(&Sha1::digest(&wire));
        expected.sort_unstable();
        let mut index = vec![MaterializedEntry::default(); count];
        let mut arena = vec![0; count * 4];
        let mut scratch = [0; 128];
        let mut window = [0; 32768];
        allocations::without_allocations(|| {
            let store = pack::materialize(
                &wire,
                &mut index,
                &mut arena,
                &mut scratch,
                &mut window,
                MaterializeLimits {
                    max_decoded_bytes_per_pass: (count * 4) as u64,
                    ..limits()
                },
            )
            .unwrap();
            assert!(
                store
                    .entries()
                    .iter()
                    .map(|entry| entry.id())
                    .eq(expected.iter().copied())
            );
            assert_eq!(store.bytes_used(), count * 4);
            for id in &expected {
                assert!(store.find(*id).is_some());
            }
        });
    }
}
