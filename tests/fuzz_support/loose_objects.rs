use super::MAX_INPUT;
use git_zero::{Error, Object, ObjectId, ObjectInfo};
use sha1::{Digest, Sha1};
use sha2::Sha256;

/// Use both actual hashes to reach header/length validation after mutations.
pub fn loose(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    for id in [
        ObjectId::Sha1(Sha1::digest(data).into()),
        ObjectId::Sha256(Sha256::digest(data).into()),
    ] {
        check(data, id);
    }
}

fn check(data: &[u8], id: ObjectId) {
    let _ = ObjectInfo::from_header(data);
    match Object::from_loose(data, id) {
        Ok(object) => {
            let mut header = [0; 32];
            let header = object.header(&mut header).unwrap();
            assert!(data.starts_with(header));
            assert_eq!(&data[header.len()..], object.data);
            assert_eq!(object.id_with_format(id.format()), id);
            let mut wrong = id;
            match &mut wrong {
                ObjectId::Sha1(bytes) => bytes[0] ^= 1,
                ObjectId::Sha256(bytes) => bytes[0] ^= 1,
            }
            assert!(matches!(
                Object::from_loose(data, wrong),
                Err(Error::InvalidObject)
            ));
        }
        Err(error) => assert_eq!(error, Error::InvalidObject),
    }
}
