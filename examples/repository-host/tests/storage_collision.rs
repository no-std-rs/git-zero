#![cfg(unix)]

use git_zero_host::storage::{BlobStore, Chunk, LocalServices, Manifest, StateStore, StoredPack};
use std::{fs, io, time::SystemTime};

#[test]
fn failed_temporary_creation_preserves_a_published_blob() {
    let root = std::env::temp_dir().join(format!(
        "git-zero-storage-collision-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let services = LocalServices::new(root.clone()).unwrap();
    // This integration-test binary has its own temporary-file counter. The
    // first put uses 0, manifest publication uses 1, and the next put uses 2.
    let key = format!(".write-{}-2", std::process::id());
    let bytes = b"compressed";
    services.blobs.put(&key, bytes).unwrap();
    let published = Manifest {
        generation: 1,
        packs: vec![StoredPack {
            id: "published".into(),
            pack: vec![Chunk {
                key: key.clone(),
                len: bytes.len() as u64,
            }],
            pack_bytes: bytes.len() as u64,
            ..StoredPack::default()
        }],
        ..Manifest::default()
    };
    assert!(services.state.compare_and_swap(0, &published).unwrap());

    let error = services.blobs.put("new", b"another blob").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(services.state.load().unwrap(), published);
    assert_eq!(services.blobs.get(&key).unwrap(), bytes);
    assert_eq!(
        services.blobs.get("new").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    fs::remove_dir_all(root).unwrap();
}
