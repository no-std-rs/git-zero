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
    assert!(matches!(
        client::discover(&mut transport, &mut storage),
        Err(Error::BufferTooSmall)
    ));
    assert_eq!(
        client::fetch_pack(&mut transport, id, &mut request, &mut response),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(
        client::fetch_loose(&mut transport, id, &mut response),
        Err(Error::BufferTooSmall)
    );
}
