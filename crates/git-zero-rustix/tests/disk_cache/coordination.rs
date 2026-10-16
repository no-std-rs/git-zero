use super::*;

#[test]
fn coordinated_cold_producers_recheck_after_acquiring_and_publish_once() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = Directory::new();
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let repository = Snapshot::new_with_format(&[], &mut tree, &mut commit, format).unwrap();
        let identity = key(&repository);
        let bytes = encoded(&repository);
        let mut a = [0; 8192];
        let mut b = [0; 8192];
        let mut first = DiskCache::new(directory.fd.as_fd(), &mut a, [1; 16]);
        let mut second = DiskCache::new(directory.fd.as_fd(), &mut b, [2; 16]);
        assert!(first.get(identity).unwrap().is_none());
        assert!(second.get(identity).unwrap().is_none());
        let lease = allocations::without_allocations(|| first.try_lease(identity))
            .unwrap()
            .unwrap();
        assert!(
            allocations::without_allocations(|| second.try_lease(identity))
                .unwrap()
                .is_none()
        );
        request(&repository, &mut first, Outcome::Miss, &bytes);
        allocations::without_allocations(|| drop(lease));
        let lease = allocations::without_allocations(|| second.try_lease(identity))
            .unwrap()
            .unwrap();
        request(&repository, &mut second, Outcome::Hit, &bytes);
        assert_eq!(
            first.statistics().insertions + second.statistics().insertions,
            1
        );
        assert!(second.evict(identity).unwrap());
        assert!(first.try_lease(identity).unwrap().is_none());
        allocations::without_allocations(|| drop(lease));
        assert!(first.try_lease(identity).unwrap().is_some());
        assert_eq!(
            std::fs::metadata(directory.entry(identity).with_extension("lock"))
                .unwrap()
                .len(),
            0
        );
        directory.assert_no_staging();
    }
}

#[test]
fn different_keys_can_be_produced_independently() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let complete = key(&repository);
    let empty = Key::derive(&repository, Selection::Indices(&[]), &mut []).unwrap();
    assert_ne!(complete, empty);
    let mut resident = [0; 1];
    let cache = DiskCache::new(directory.fd.as_fd(), &mut resident, [5; 16]);
    let first = allocations::without_allocations(|| cache.try_lease(complete))
        .unwrap()
        .unwrap();
    let second = allocations::without_allocations(|| cache.try_lease(empty))
        .unwrap()
        .unwrap();
    assert!(cache.try_lease(complete).unwrap().is_none());
    assert!(cache.try_lease(empty).unwrap().is_none());
    drop(first);
    assert!(cache.try_lease(complete).unwrap().is_some());
    assert!(cache.try_lease(empty).unwrap().is_none());
    drop(second);
    assert!(cache.try_lease(empty).unwrap().is_some());
}

#[test]
fn abandoned_and_failed_producers_release_their_guards_for_retry() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let identity = key(&repository);
    let mut resident = [0; 1];
    let mut cache = DiskCache::new(directory.fd.as_fd(), &mut resident, [3; 16]);
    {
        let _lease = cache.try_lease(identity).unwrap().unwrap();
        assert_eq!(cache.put(identity, &[0; 2]), Err(Error::BufferTooSmall));
        assert!(cache.try_lease(identity).unwrap().is_none());
    }
    assert!(cache.try_lease(identity).unwrap().is_some());
    assert!(!directory.entry(identity).exists());
    cache.set_enabled(false);
    let another = Key::derive(&repository, Selection::Indices(&[]), &mut []).unwrap();
    assert!(cache.try_lease(another).unwrap().is_none());
    assert!(!directory.entry(another).with_extension("lock").exists());
    directory.assert_no_staging();
}

#[test]
fn lock_entries_reject_symlinks_directories_and_nonempty_files() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let identity = key(&repository);
    let target = directory.entry(identity).with_extension("lock");
    let outside = directory.path.join("outside");
    std::fs::write(&outside, b"unchanged").unwrap();
    let mut resident = [0; 8192];
    let cache = DiskCache::new(directory.fd.as_fd(), &mut resident, [4; 16]);
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    assert!(matches!(cache.try_lease(identity), Err(Error::Backend)));
    assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
    std::fs::remove_file(&target).unwrap();
    std::fs::create_dir(&target).unwrap();
    assert!(matches!(cache.try_lease(identity), Err(Error::Backend)));
    std::fs::remove_dir(&target).unwrap();
    std::fs::write(&target, b"not a lock").unwrap();
    assert!(matches!(
        cache.try_lease(identity),
        Err(Error::InvalidObject)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"not a lock");
}
