#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{BlobFilter, Error, FetchOptions, HistoryPolicy, ObjectInfo, ObjectKind};

#[test]
fn external_history_rules_preserve_boundaries_and_relative_common_exclusion() {
    allocations::without_allocations(|| {
        let ordinary = FetchOptions::default();
        assert!(!ordinary.deepens_history(true));
        let history = ordinary.history(true, None).unwrap();
        assert!(history.follows_parents(99, false));
        assert!(!history.follows_parents(1, true));
        assert_eq!(history.parent_depth(99), 1);

        let absolute = FetchOptions {
            depth: Some(2),
            ..ordinary
        };
        assert!(absolute.deepens_history(false));
        let history = absolute.history(true, None).unwrap();
        assert!(history.follows_parents(1, true));
        assert!(!history.follows_parents(2, false));
        assert_eq!(history.parent_depth(1), 2);

        let relative = FetchOptions {
            deepen_relative: true,
            ..absolute
        };
        assert!(!relative.deepens_history(false));
        assert!(relative.deepens_history(true));
        let full = relative.history(false, None).unwrap();
        assert!(full.follows_parents(99, false));
        let unrelated = relative.history(true, None).unwrap();
        assert!(unrelated.follows_parents(99, true));
        let bounded = relative.history(true, Some(3)).unwrap();
        assert!(bounded.follows_parents(4, true));
        assert!(!bounded.follows_parents(5, false));
        assert_eq!(bounded.parent_depth(4), 5);

        let probe = HistoryPolicy::boundary_probe();
        assert!(probe.follows_parents(2, false));
        assert!(!probe.follows_parents(3, true));
        assert_eq!(probe.parent_depth(2), 3);
        assert_eq!(probe.parent_depth(u32::MAX), u32::MAX);
        for depth in [0, i32::MAX as u32 + 1] {
            assert!(matches!(
                FetchOptions {
                    depth: Some(depth),
                    ..ordinary
                }
                .history(false, None),
                Err(Error::InvalidRequest)
            ));
        }
        assert!(matches!(
            relative.history(true, Some(0)),
            Err(Error::InvalidRequest)
        ));
        assert!(matches!(
            relative.history(true, Some(u32::MAX)),
            Err(Error::LimitExceeded)
        ));
    });
}

#[test]
fn blob_filters_keep_explicit_wants_and_exact_size_boundaries() {
    allocations::without_allocations(|| {
        for kind in [
            ObjectKind::Commit,
            ObjectKind::Tree,
            ObjectKind::Blob,
            ObjectKind::Tag,
        ] {
            for size in [0, 7, 8, u64::MAX] {
                let info = ObjectInfo { kind, size };
                for selected in [
                    ObjectKind::Commit,
                    ObjectKind::Tree,
                    ObjectKind::Blob,
                    ObjectKind::Tag,
                ] {
                    assert_eq!(
                        BlobFilter::Type(selected).includes(info, false),
                        kind == selected
                    );
                    assert!(BlobFilter::Type(selected).includes(info, true));
                }
                assert!(BlobFilter::All.includes(info, false));
                assert_eq!(
                    BlobFilter::None.includes(info, false),
                    kind != ObjectKind::Blob
                );
                assert_eq!(
                    BlobFilter::Limit(8).includes(info, false),
                    kind != ObjectKind::Blob || size < 8
                );
                assert!(BlobFilter::None.includes(info, true));
                assert!(BlobFilter::Limit(0).includes(info, true));
            }
        }
    });
}
