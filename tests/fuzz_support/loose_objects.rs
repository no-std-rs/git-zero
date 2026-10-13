use super::MAX_INPUT;
use git_zero::{Error, Object, ObjectId, ObjectInfo};
use sha1::{Digest, Sha1};

/// Use the actual SHA-1 to reach header/length validation after mutations.
pub fn loose(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let id = ObjectId(Sha1::digest(data).into());
    let _ = ObjectInfo::from_header(data);
    match Object::from_loose(data, id) {
        Ok(object) => {
            let mut header = [0; 32];
            let header = object.header(&mut header).unwrap();
            assert!(data.starts_with(header));
            assert_eq!(&data[header.len()..], object.data);
            assert_eq!(object.id(), id);
            let mut wrong = id;
            wrong.0[0] ^= 1;
            assert!(matches!(
                Object::from_loose(data, wrong),
                Err(Error::InvalidObject)
            ));
        }
        Err(error) => assert_eq!(error, Error::InvalidObject),
    }
}
