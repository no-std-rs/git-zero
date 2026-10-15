#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/pull.rs"]
mod pull;
#[path = "common/streamed.rs"]
mod streamed;
use git_zero::{Error, File, ObjectFormat, ObjectId, Repository, Snapshot, io::SliceWriter, pack};
use sha2::Digest;

#[test]
fn checksum_is_available_only_after_the_complete_trailer() {
    check_checksum_visibility(ObjectFormat::Sha1);
    check_checksum_visibility(ObjectFormat::Sha256);
}

fn check_checksum_visibility(format: ObjectFormat) {
    let repository = git_zero::EmptyRepository::new_with_format("refs/heads/main", format).unwrap();
    let mut encoder = pack::Encoder::new(&repository, None).unwrap();
    let mut header = [0; 12];
    assert_eq!(encoder.read(&repository, &mut header).unwrap(), 12);
    assert_eq!(encoder.checksum(), None);
    let mut trailer = [0; 32];
    let last = format.id_length() - 1;
    assert_eq!(
        encoder.read(&repository, &mut trailer[..last]).unwrap(),
        last
    );
    assert_eq!(encoder.checksum(), None);
    assert_eq!(
        encoder
            .read(&repository, &mut trailer[last..last + 1])
            .unwrap(),
        1
    );
    let expected = match format {
        ObjectFormat::Sha1 => ObjectId::Sha1(sha1::Sha1::digest(header).into()),
        ObjectFormat::Sha256 => ObjectId::Sha256(sha2::Sha256::digest(header).into()),
    };
    assert_eq!(encoder.checksum(), Some(expected));
    assert_eq!(&trailer[..format.id_length()], expected.as_bytes());
    assert_eq!(encoder.read(&repository, &mut []).unwrap(), 0);
    assert_eq!(encoder.checksum(), Some(expected));
}
use pull::collect;

#[test]
fn pull_packs_and_loose_objects_match_synchronous_encoders_at_all_block_boundaries() {
    for size in [0, 1, 65534, 65535, 65536, 131071] {
        let repository = streamed::GeneratedRepository::new(size, 29);
        check_generated_pack(&repository, size as usize + 4096);
        for index in 0..repository.object_count() {
            check_generated_loose(&repository, index, size as usize + 4096);
        }
    }
}

fn check_generated_pack(repository: &impl Repository, budget: usize) {
    let mut expected = vec![0; budget];
    let mut writer = SliceWriter::new(&mut expected);
    let checksum = pack::write(repository, &mut writer).unwrap();
    for capacity in [1, 7, 1024, 65536] {
        let mut encoder =
            allocations::without_allocations(|| pack::Encoder::new(repository, None)).unwrap();
        assert_eq!(
            encoder.read(repository, &mut []),
            Err(Error::BufferTooSmall)
        );
        let bytes = collect(|output| encoder.read(repository, output), capacity);
        assert_eq!(bytes, writer.as_bytes());
        assert_eq!(encoder.checksum(), Some(checksum));
        pack::validate(&bytes).unwrap();
    }
}

fn check_generated_loose(repository: &impl Repository, index: usize, budget: usize) {
    let mut expected = vec![0; budget];
    let mut output = SliceWriter::new(&mut expected);
    pack::write_loose_with_scratch(repository, index, &mut [0; 17], &mut output).unwrap();
    for capacity in [1, 7, 1024, 65536] {
        let mut encoder = pack::LooseEncoder::new(repository, index).unwrap();
        assert_eq!(
            collect(|output| encoder.read(repository, output), capacity),
            output.as_bytes()
        );
    }
}

#[test]
fn selected_packs_preserve_format_order_empty_closures_and_validate_before_output() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        check_selected_packs(format);
    }
}

fn check_selected_packs(format: ObjectFormat) {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let files = [File {
        name: "blob",
        contents: b"hello",
    }];
    let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
    for indices in [&[][..], &[2, 0, 1][..], &[1][..]] {
        check_selected_order(&repository, indices);
    }
    assert!(matches!(
        pack::Encoder::new(&repository, Some(&[0, 0])),
        Err(Error::InvalidObject)
    ));
    assert!(matches!(
        pack::Encoder::new(&repository, Some(&[3])),
        Err(Error::ObjectNotFound)
    ));
}

fn check_selected_order(repository: &impl Repository, indices: &[usize]) {
    let mut bytes = [0; 2048];
    let mut output = SliceWriter::new(&mut bytes);
    let checksum =
        pack::write_selected_with_scratch(repository, indices, &mut [0; 13], &mut output).unwrap();
    let mut encoder = pack::Encoder::new(repository, Some(indices)).unwrap();
    assert_eq!(
        collect(|output| encoder.read(repository, output), 3),
        output.as_bytes()
    );
    assert_eq!(encoder.checksum(), Some(checksum));
}
