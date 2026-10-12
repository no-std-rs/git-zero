use super::MAX_INPUT;
use git_zero::{
    Error, ObjectId, Result,
    client::{self, Transport},
    http::Request,
    pack,
};

struct Reply<'a> {
    data: &'a [u8],
    extra: usize,
}

impl Transport for Reply<'_> {
    fn request(&mut self, _: Request<'_>, storage: &mut [u8]) -> Result<(u16, usize)> {
        let destination = storage
            .get_mut(..self.data.len())
            .ok_or(Error::BufferTooSmall)?;
        destination.copy_from_slice(self.data);
        Ok((200, self.data.len() + self.extra))
    }
}

/// Exercise discovery, HEAD lookup, pack framing, and transport size checks.
///
/// The input is one transport body, reused for each request with HTTP status 200.
/// Separate 4096-byte buffers keep borrowed discovery data stable during HEAD
/// lookup; a 128-byte request buffer bounds fetch construction. Successful packs
/// must pass framing/checksum validation. Final requests deliberately report a
/// body larger than the supplied storage and must fail before indexing it.
pub fn responses(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let mut storage = [0; MAX_INPUT];
    let mut response = [0; MAX_INPUT];
    let mut request = [0; 128];
    let mut transport = Reply { data, extra: 0 };
    if let Ok(discovery) = client::discover(&mut transport, &mut storage) {
        assert!(discovery.references().count() <= data.len());
        let _ = discovery.head();
        let _ = client::read_head(&mut transport, &discovery, &mut response);
    }
    let id = ObjectId([1; 20]);
    if let Ok(bytes) = client::fetch_pack(&mut transport, id, &mut request, &mut response) {
        assert!(bytes.len() <= data.len());
        pack::validate(bytes).unwrap();
    }
    let _ = pack::validate(data);
    // A transport may overreport its body size; every client entrypoint must
    // reject the size before indexing the caller's storage.
    transport.extra = MAX_INPUT + 1;
    let errors = [
        client::discover(&mut transport, &mut storage).err(),
        client::fetch_pack(&mut transport, id, &mut request, &mut response).err(),
        client::fetch_loose(&mut transport, id, &mut response).err(),
    ];
    assert_eq!(errors, [Some(Error::BufferTooSmall); 3]);
}
