#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, ObjectFormat, ObjectId,
    client::{self, Transport},
    http::Request,
    io::{SliceWriter, Write},
    pktline,
};

struct Advertisement(Vec<u8>);
impl Transport for Advertisement {
    fn request(&mut self, _: Request<'_>, output: &mut [u8]) -> git_zero::Result<(u16, usize)> {
        output[..self.0.len()].copy_from_slice(&self.0);
        Ok((200, self.0.len()))
    }
}

fn advertisement(
    smart: bool,
    first: ObjectId,
    second: ObjectId,
    capability: &str,
    empty: bool,
) -> Advertisement {
    let name = if empty { "capabilities^{}" } else { "HEAD" };
    if !smart {
        return Advertisement(
            format!("{first}\trefs/heads/a\n{second}\trefs/heads/b\n").into_bytes(),
        );
    }
    let mut bytes = [0; 1024];
    let mut output = SliceWriter::new(&mut bytes);
    pktline::write(&mut output, b"# service=git-upload-pack\n").unwrap();
    output.write_all(b"0000").unwrap();
    pktline::write(
        &mut output,
        format!("{first} {name}\0{capability}\n").as_bytes(),
    )
    .unwrap();
    pktline::write(&mut output, format!("{second} refs/heads/b\n").as_bytes()).unwrap();
    output.write_all(b"0000").unwrap();
    Advertisement(output.as_bytes().to_vec())
}

#[test]
fn discovery_rejects_mixed_id_widths_and_conflicting_capabilities() {
    let sha1 = ObjectId::Sha1([1; 20]);
    let sha256 = ObjectId::Sha256([2; 32]);
    for smart in [false, true] {
        for (first, second) in [(sha1, sha256), (sha256, sha1)] {
            let mut transport = advertisement(smart, first, second, "", false);
            let mut storage = [0; 1024];
            assert_eq!(
                allocations::without_allocations(
                    || client::discover(&mut transport, &mut storage).err()
                ),
                Some(Error::InvalidRequest)
            );
        }
    }
    for (id, capability, empty) in [
        (sha1, "object-format=sha256", false),
        (sha256, "object-format=sha1", false),
        (ObjectFormat::Sha1.zero_id(), "object-format=sha256", true),
        (ObjectFormat::Sha256.zero_id(), "object-format=sha1", true),
    ] {
        let mut transport = advertisement(true, id, id, capability, empty);
        assert_eq!(
            client::discover(&mut transport, &mut [0; 1024]).err(),
            Some(Error::InvalidRequest)
        );
    }
    for (first, second) in [
        (ObjectFormat::Sha1.zero_id(), sha256),
        (ObjectFormat::Sha256.zero_id(), sha1),
    ] {
        let mut transport = advertisement(true, first, second, "", true);
        assert_eq!(
            client::discover(&mut transport, &mut [0; 1024]).err(),
            Some(Error::InvalidRequest)
        );
    }
    let mut unsupported = advertisement(true, sha1, sha1, "object-format=unknown", false);
    assert_eq!(
        client::discover(&mut unsupported, &mut [0; 1024]).err(),
        Some(Error::Unsupported)
    );
}

#[test]
fn discovery_accepts_consistent_formats_with_or_without_capabilities() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let id = ObjectId::from_bytes(format, &[1; 32][..format.id_length()]).unwrap();
        for smart in [false, true] {
            for capability in ["", std::str::from_utf8(format.capability()).unwrap()] {
                let mut transport = advertisement(smart, id, id, capability, false);
                let mut storage = [0; 1024];
                allocations::without_allocations(|| {
                    let discovery = client::discover(&mut transport, &mut storage).unwrap();
                    assert_eq!(discovery.head(), Ok(id));
                    assert!(
                        discovery
                            .references()
                            .all(|reference| reference.unwrap().id.format() == format)
                    );
                });
            }
        }
    }
    // Multiple algorithms may be advertised; the first one encodes the refs.
    let id = ObjectId::Sha1([1; 20]);
    let mut transport = advertisement(
        true,
        id,
        id,
        "object-format=sha1 object-format=sha256",
        false,
    );
    assert_eq!(
        client::discover(&mut transport, &mut [0; 1024])
            .unwrap()
            .head(),
        Ok(id)
    );
}
