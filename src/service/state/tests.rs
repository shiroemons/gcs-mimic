use super::*;

#[test]
fn work_pools_scale_with_blocking_threads_and_do_not_share_permits() {
    for (threads, expected) in [(1, 1), (8, 2), (16, 4)] {
        let config = crate::config::ConfigDraft {
            max_blocking_threads: threads,
            ..Default::default()
        }
        .build();
        let pools = WorkPools::new(&config);
        assert_eq!(pools.gzip_decoders.available_permits(), expected);
        assert_eq!(pools.unauthenticated_gzip_decoders.available_permits(), 1);
        assert_eq!(pools.batches.available_permits(), 2);
        let other = WorkPools::new(&config);
        assert!(!Arc::ptr_eq(&pools.gzip_decoders, &other.gzip_decoders));
        assert!(!Arc::ptr_eq(
            &pools.unauthenticated_gzip_decoders,
            &other.unauthenticated_gzip_decoders
        ));
        assert!(!Arc::ptr_eq(&pools.batches, &other.batches));
    }
}
