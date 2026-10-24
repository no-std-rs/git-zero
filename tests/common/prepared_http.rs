//! Native allocation and lifecycle checks for the same async preparation API.
use super::*;
use git_zero::http::stream::{ResponseEncoder, preparation::PreparedResponse};

fn prepare<'storage>(
    repository: &TestRepository<'_>,
    request: Request<'_>,
    selection: &'storage mut SelectionBuffers<16, 16, 17, 128>,
    control: &'storage mut [u8],
) -> PreparedResponse<'storage> {
    let encoder = allocations::without_allocations(|| {
        ResponseEncoder::with_selection_storage_policy(
            repository,
            request,
            selection.storage(65536),
            WantOptions::STRICT,
            control,
        )
    })
    .unwrap();
    allocations::without_allocations(|| {
        PreparedResponse::new(
            encoder,
            repository,
            &mut [repository.object_format().zero_id(); 16],
        )
    })
    .unwrap()
}

fn generate(prepared: &mut PreparedResponse<'_>, repository: &impl Repository) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut part = [0; 7];
        let count =
            allocations::without_allocations(|| prepared.read_pack(repository, &mut part)).unwrap();
        if count == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&part[..count]);
    }
}

fn drain(encoder: &mut ResponseEncoder<'_>, repository: &impl Repository, chunk: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut part = vec![0; chunk];
    loop {
        let count =
            allocations::without_allocations(|| encoder.read(repository, &mut part)).unwrap();
        if count == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&part[..count]);
    }
}

fn unframe(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut remaining = bytes;
    let mut prefix = Vec::new();
    let mut pack = Vec::new();
    loop {
        if remaining.is_empty() {
            return (prefix, pack);
        }
        if remaining.starts_with(b"PACK") {
            return (prefix, remaining.to_vec());
        }
        let before = remaining;
        match git_zero::pktline::read(&mut remaining).unwrap().unwrap() {
            git_zero::pktline::Packet::Data(data) if data.first() == Some(&1) => {
                pack.extend_from_slice(&data[1..])
            }
            _ if pack.is_empty() => {
                prefix.extend_from_slice(&before[..before.len() - remaining.len()])
            }
            _ => {}
        }
    }
}

#[test]
fn selected_preparation_matches_existing_http_for_every_protocol_and_band_without_allocations() {
    formats(check_protocol_matrix);
}

#[test]
fn prepared_responses_release_request_and_validation_storage_before_attachment() {
    formats(check_request_lifetime);
}

#[test]
fn preparation_rejects_already_read_or_cancelled_encoders_including_empty_output_attempts() {
    formats(check_started_encoder);
}

#[test]
fn negotiation_discovery_and_loose_responses_have_no_pack_key_and_need_no_backend() {
    formats(check_non_pack_responses);
}

#[test]
fn wrong_set_corrupt_and_exhausted_attachments_are_consumed_before_any_output() {
    formats(check_attachment_errors);
}

#[test]
fn failed_generation_does_not_affect_an_independent_preparation_or_a_valid_hit() {
    formats(check_provider_failure);
}

fn check_protocol_matrix(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    for version in 0..=2 {
        for band in ["", " side-band", " side-band-64k"] {
            check_protocol_band(fixture, repository, version, band);
        }
    }
}

fn check_request_lifetime(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let mut selection = SelectionBuffers::new();
    let mut control = [0; 2048];
    let mut prepared = {
        let wire = body(fixture.format, 2, &fixture.tips[..1], &[], &[], true, "");
        prepare(repository, request(&wire, 2), &mut selection, &mut control)
    }; // Request bytes are gone, while selection/control remain pinned.
    let pack = generate(&mut prepared, repository);
    let mut ids = [fixture.format.zero_id(); 16];
    let mut encoder = prepared.attach(&pack, &mut ids, pack.len()).unwrap();
    assert_eq!(unframe(&drain(&mut encoder, repository, 29)).1, pack);
}

fn check_started_encoder(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    for cancel in [false, true] {
        let wanted = body(fixture.format, 0, &fixture.tips[..1], &[], &[], true, "");
        let mut selection = SelectionBuffers::<16, 16, 17, 128>::new();
        let mut control = [0; 2048];
        let mut encoder = ResponseEncoder::with_selection_storage_policy(
            repository,
            request(&wanted, 0),
            selection.storage(65536),
            WantOptions::STRICT,
            &mut control,
        )
        .unwrap();
        if cancel {
            encoder.cancel();
        } else {
            assert_eq!(
                encoder.read(repository, &mut []),
                Err(Error::BufferTooSmall)
            );
        }
        assert!(matches!(
            PreparedResponse::new(encoder, repository, &mut [fixture.format.zero_id(); 16]),
            Err(Error::InvalidRequest)
        ));
    }
}

fn check_non_pack_responses(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    for version in 0..=2 {
        check_negotiation(fixture, repository, version);
    }
    let mut control = [0; 2048];
    let request = Request {
        method: "GET",
        path: "HEAD",
        query: None,
        content_type: None,
        git_protocol: None,
        body: &[],
    };
    let encoder = ResponseEncoder::new(repository, request, &mut control).unwrap();
    let prepared = PreparedResponse::new(encoder, repository, &mut []).unwrap();
    assert_eq!(prepared.key(), None);
    let mut encoder = prepared.without_pack().unwrap();
    assert!(drain(&mut encoder, repository, 1).starts_with(b"ref: "));
}

fn check_attachment_errors(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let first = body(fixture.format, 2, &fixture.tips[..1], &[], &[], true, "");
    let second = body(fixture.format, 2, &fixture.tips[1..], &[], &[], true, "");
    let mut selection = SelectionBuffers::new();
    let mut control = [0; 2048];
    let mut prepared = prepare(repository, request(&first, 2), &mut selection, &mut control);
    let pack = generate(&mut prepared, repository);
    assert!(matches!(
        prepared.without_pack(),
        Err(Error::InvalidRequest)
    ));
    let mut corrupted = pack.clone();
    corrupted[20] ^= 1;
    for (wanted, bytes, limit, expected) in [
        (&second, pack.as_slice(), pack.len(), Error::CorruptPack),
        (&first, corrupted.as_slice(), pack.len(), Error::CorruptPack),
        (
            &first,
            pack.as_slice(),
            pack.len() - 1,
            Error::LimitExceeded,
        ),
    ] {
        let mut selection = SelectionBuffers::new();
        let mut control = [0; 2048];
        let prepared = prepare(repository, request(wanted, 2), &mut selection, &mut control);
        let result = allocations::without_allocations(|| {
            prepared.attach(bytes, &mut [fixture.format.zero_id(); 16], limit)
        });
        assert_eq!(result.err().unwrap(), expected);
    }
}

fn check_provider_failure(fixture: &Fixture<'_>, repository: &TestRepository<'_>) {
    let wanted = body(fixture.format, 2, &fixture.tips[..1], &[], &[], true, "");
    let mut one_selection = SelectionBuffers::new();
    let mut one_control = [0; 2048];
    let mut one = prepare(
        repository,
        request(&wanted, 2),
        &mut one_selection,
        &mut one_control,
    );
    let pack = generate(&mut one, repository);
    let mut two_selection = SelectionBuffers::new();
    let mut two_control = [0; 2048];
    let mut two = prepare(
        repository,
        request(&wanted, 2),
        &mut two_selection,
        &mut two_control,
    );
    fixture.fail_blobs.set(true);
    let mut failed = false;
    for _ in 0..10000 {
        match two.read_pack(repository, &mut [0; 13]) {
            Err(Error::Backend) => {
                failed = true;
                break;
            }
            Ok(0) => panic!("failed provider reached a successful trailer"),
            Ok(_) => {}
            Err(error) => panic!("unexpected {error:?}"),
        }
    }
    assert!(failed);
    let mut encoder = one
        .attach(&pack, &mut [fixture.format.zero_id(); 16], pack.len())
        .unwrap();
    assert_eq!(unframe(&drain(&mut encoder, repository, 31)).1, pack);
    encoder.cancel();
    assert_eq!(
        encoder.read(repository, &mut [0; 3]),
        Err(Error::InvalidRequest)
    );
    fixture.fail_blobs.set(false);
}

fn check_protocol_band(
    fixture: &Fixture<'_>,
    repository: &TestRepository<'_>,
    version: u8,
    band: &str,
) {
    let wanted = body(
        fixture.format,
        version,
        &fixture.tips[..1],
        &[],
        &[],
        true,
        band,
    );
    let mut selection = SelectionBuffers::new();
    let mut control = [0; 2048];
    let mut prepared = prepare(
        repository,
        request(&wanted, version),
        &mut selection,
        &mut control,
    );
    let identity = prepared.key().unwrap();
    let pack = generate(&mut prepared, repository);
    assert_eq!(prepared.key(), Some(identity));
    let mut ids = [fixture.format.zero_id(); 16];
    let mut encoder =
        allocations::without_allocations(|| prepared.attach(&pack, &mut ids, pack.len())).unwrap();
    let actual = drain(&mut encoder, repository, 1);
    let mut resident = [0; CAPACITY];
    let mut client = Client::new(repository, FixedCache::new(&mut resident));
    let expected = client.run(request(&wanted, version)).pack(Outcome::Miss);
    assert_eq!(unframe(&actual), unframe(&expected));
    // Hit attachment does not read provider blob bodies at all.
    let mut selected_hit = SelectionBuffers::new();
    let mut control_hit = [0; 2048];
    let hit = prepare(
        repository,
        request(&wanted, version),
        &mut selected_hit,
        &mut control_hit,
    );
    assert_eq!(hit.key(), Some(identity));
    fixture.fail_blobs.set(true);
    let reads = fixture.blob_reads.get();
    let mut encoder =
        allocations::without_allocations(|| hit.attach(&pack, &mut ids, pack.len())).unwrap();
    assert_eq!(drain(&mut encoder, repository, 17), actual);
    assert_eq!(fixture.blob_reads.get(), reads);
    fixture.fail_blobs.set(false);
}

fn check_negotiation(fixture: &Fixture<'_>, repository: &TestRepository<'_>, version: u8) {
    let wanted = body(
        fixture.format,
        version,
        &fixture.tips[..1],
        &[],
        &[],
        false,
        "",
    );
    let mut selection = SelectionBuffers::new();
    let mut control = [0; 2048];
    let mut prepared = prepare(
        repository,
        request(&wanted, version),
        &mut selection,
        &mut control,
    );
    assert_eq!(prepared.key(), None);
    assert_eq!(
        prepared.read_pack(repository, &mut [0; 7]),
        Err(Error::InvalidRequest)
    );
    let mut encoder = prepared.without_pack().unwrap();
    assert!(unframe(&drain(&mut encoder, repository, 3)).1.is_empty());
}
