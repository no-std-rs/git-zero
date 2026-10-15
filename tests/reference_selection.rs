#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, ObjectId,
    client::{self, Transport},
    http::Request,
};

struct Origin<'a> {
    exchanges: &'a [(&'a str, u16, &'a [u8])],
    next: usize,
}
impl Transport for Origin<'_> {
    fn request(
        &mut self,
        request: Request<'_>,
        response: &mut [u8],
    ) -> git_zero::Result<(u16, usize)> {
        let &(path, status, bytes) = self.exchanges.get(self.next).expect("unexpected request");
        self.next += 1;
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, path);
        assert_eq!(request.query, None);
        assert!(request.body.is_empty());
        let output = response
            .get_mut(..bytes.len())
            .ok_or(Error::BufferTooSmall)?;
        output.copy_from_slice(bytes);
        Ok((status, bytes.len()))
    }
}

#[test]
fn normalizes_branches_but_preserves_head_and_full_refs() {
    let mut storage = [0; 128];
    allocations::without_allocations(|| {
        for (input, expected) in [
            ("main", "refs/heads/main"),
            ("topic/demo", "refs/heads/topic/demo"),
            ("HEAD", "HEAD"),
            ("refs/tags/v1", "refs/tags/v1"),
        ] {
            assert_eq!(client::reference_name(input, &mut storage), Ok(expected));
        }
        for name in ["", "../main", "a..b", "bad name", "refs/heads/", "main\n"] {
            assert!(client::reference_name(name, &mut storage).is_err());
        }
        assert_eq!(
            client::reference_name("main", &mut [0; 4]),
            Err(Error::BufferTooSmall)
        );
    });
}

#[test]
fn bounded_symbolic_refs_percent_encode_paths_without_allocations() {
    for id in [ObjectId::Sha1([7; 20]), ObjectId::Sha256([8; 32])] {
        let hex = format!("{id}\n");
        let exchanges = [
            (
                "HEAD",
                200,
                b"ref: refs/heads/topic/#%\xc3\xa9\n".as_slice(),
            ),
            ("refs/heads/topic/%23%25%C3%A9", 200, hex.as_bytes()),
        ];
        let mut origin = Origin {
            exchanges: &exchanges,
            next: 0,
        };
        let mut response = [0; 256];
        let mut path = [0; 256];
        allocations::without_allocations(|| {
            assert_eq!(
                client::resolve_reference(&mut origin, "HEAD", &mut response, &mut path, 2),
                Ok(id)
            );
        });
        assert_eq!(origin.next, 2);
    }
}

#[test]
fn malformed_missing_and_cyclic_refs_fail_with_bounded_reads() {
    for (body, status, expected) in [
        (
            b"ref: refs/heads/../escape\n".as_slice(),
            200,
            Error::InvalidReference,
        ),
        (b"ref: HEAD\n", 200, Error::InvalidReference),
        (b"garbage\n", 200, Error::InvalidObjectId),
        (b"", 404, Error::ObjectNotFound),
        (b"", 500, Error::Backend),
    ] {
        let exchanges = [("HEAD", status, body)];
        let mut origin = Origin {
            exchanges: &exchanges,
            next: 0,
        };
        assert_eq!(
            client::resolve_reference(&mut origin, "HEAD", &mut [0; 256], &mut [0; 256], 8),
            Err(expected)
        );
        assert_eq!(origin.next, 1);
    }
    let exchanges = [("refs/heads/main", 200, b"ref: refs/heads/main\n".as_slice()); 3];
    let mut origin = Origin {
        exchanges: &exchanges,
        next: 0,
    };
    assert_eq!(
        client::resolve_reference(
            &mut origin,
            "refs/heads/main",
            &mut [0; 256],
            &mut [0; 256],
            3
        ),
        Err(Error::LimitExceeded)
    );
    assert_eq!(origin.next, 3);
    assert_eq!(
        client::resolve_reference(&mut origin, "HEAD", &mut [0; 256], &mut [0; 256], 0),
        Err(Error::LimitExceeded)
    );
    assert_eq!(origin.next, 3);
}
