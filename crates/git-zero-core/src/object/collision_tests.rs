use super::*;

// Native Git 2.56 test-tool rejects the raw streams; hash-object accepts their
// canonical blob envelopes. These fixed vectors cover both behaviors.
const VECTORS: &[(&[u8], &str, &str, &str)] = &[
    (
        include_bytes!("test-data/sha-mbles-1.bin"),
        "3ead211681cec93d265c8ac123dd062e105408cebf82fa6e2b126f4f40bcb88c",
        "5a7c30e97646c66422abe0a9793a5fcb9f1cf8d6",
        "5bdd106d095bcf6cd9d22f768c354f3c1a7b507058cc4def6e7dd9d1270cb5d8",
    ),
    (
        include_bytes!("test-data/sha-mbles-2.bin"),
        "208feafe1c6a95c73f662514ac48761f25e1f3b74922521a98d9ce287f4a2197",
        "fe39178400a7ebeedca8ccfd0f3a64ceecdb9cda",
        "1b85d41b0767182fce62d9f17aeb90e090333df20f8816afe25f5c9cdaef8425",
    ),
    (
        include_bytes!("test-data/shattered-prefix.bin"),
        "cac8644dba1a9aef70cc268f3794036a2be5b5107109ad742247858fd1a36990",
        "ef380704685cc8e54de9bc13556d1ff7026ec0cc",
        "53c2fff6e58c1a029bc06c4c6b2cf8b285f6a8e2199ea83184199cdaa96bc227",
    ),
];

#[test]
fn raw_sha1_collision_vectors_fail_after_fragmented_updates_and_cloning() {
    for &(bytes, _, _, _) in VECTORS {
        for size in [1, 7, 64, bytes.len()] {
            let mut hash = ObjectFormat::Sha1.checked_hash();
            for part in bytes.chunks(size) {
                hash.update(part);
            }
            assert_eq!(hash.clone().finish(), Err(Error::InvalidObject));
            assert_eq!(hash.finish(), Err(Error::InvalidObject));
        }
    }
    let mut clean = ObjectFormat::Sha1.checked_hash();
    clean.update(b"hello");
    assert_eq!(
        clean.finish(),
        ObjectId::from_hex(b"aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")
    );
}

#[test]
fn canonical_collision_file_blobs_preserve_exact_native_ids_in_both_formats() {
    for &(bytes, _, sha1, sha256) in VECTORS {
        let object = Object {
            kind: ObjectKind::Blob,
            data: bytes,
        };
        for (format, id) in [(ObjectFormat::Sha1, sha1), (ObjectFormat::Sha256, sha256)] {
            let expected = ObjectId::from_hex(id.as_bytes()).unwrap();
            assert_eq!(object.id_with_format(format), expected);
            for size in [1, 7, 64, bytes.len()] {
                let mut hash = ObjectDigest::new(object.info(), format);
                for part in bytes.chunks(size) {
                    hash.update(part).unwrap();
                }
                assert_eq!(hash.finish(), Ok(expected));
                assert_eq!(hash.clone().finish(), Ok(expected));
            }
        }
    }
}

#[test]
fn raw_sha256_streams_keep_their_standard_digests() {
    for &(bytes, digest, _, _) in VECTORS {
        let mut hash = ObjectFormat::Sha256.checked_hash();
        for part in bytes.chunks(7) {
            hash.update(part);
        }
        assert_eq!(hash.finish(), ObjectId::from_hex(digest.as_bytes()));
    }
}
