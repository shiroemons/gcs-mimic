use super::*;

thread_local! {
    pub(super) static POOL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[test]
fn cancelling_a_queued_frame_read_releases_the_file_before_a_worker_is_free() {
    use std::task::{Context, Waker};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || blocked.recv());
        let dir = crate::test_support::TestDir::new();
        let file = Arc::new(File::create(dir.0.join("blob")).unwrap());
        let descriptor = Arc::downgrade(&file);
        let mut reader = ObjectReader::bounded_for_test(file, 1);
        let mut read = Box::pin(async move { reader.next_frame(FrameLimit::Grpc).await });
        assert!(
            read.as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(read);
        assert!(descriptor.upgrade().is_none());
        // A cached file would still upgrade if its queued read escaped cancellation.
        let cached = Arc::new(File::create(dir.0.join("cached")).unwrap());
        let mut reader = ObjectReader::bounded_for_test(cached.clone(), 1);
        let mut queued = Box::pin(reader.next_frame(FrameLimit::Grpc));
        assert!(
            queued
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(queued);
        release.send(()).unwrap();
        worker.await.unwrap().unwrap();
        assert_eq!(
            tokio::task::spawn_blocking(|| POOL_READS.get())
                .await
                .unwrap(),
            0
        );
        drop(cached);
    });
}

/// A small object or a gRPC chunk must hold no more than its own bytes, and no read may outgrow
/// the largest buffer.
#[test]
fn only_a_read_of_a_whole_pooled_size_takes_a_pooled_buffer() {
    let taken = |capacity| {
        let buffer = ReadBuffer::take(capacity);
        (buffer.bytes.len(), buffer.pool.is_some())
    };
    assert_eq!(taken(1), (1, false));
    assert_eq!(taken(CHUNK_SIZE), (CHUNK_SIZE, false));
    assert_eq!(taken(256 * 1024), (256 * 1024, true));
    assert_eq!(taken(256 * 1024 + 1), (256 * 1024 + 1, false));
    assert_eq!(taken(1024 * 1024), (1024 * 1024, true));
    assert_eq!(taken(4 * 1024 * 1024), (1024 * 1024, true));
}

/// Bounds what the pool retains, however many reads were in flight at once. The pool is this
/// test's own, because the shared ones change under other tests.
#[test]
fn a_finished_buffer_returns_to_its_pool_up_to_the_limit() {
    static POOL: BufferPool = BufferPool::new(16, 2);
    let taken: Vec<_> = (0..3)
        .map(|_| ReadBuffer {
            bytes: vec![0; POOL.buffer_bytes],
            filled: 0,
            pool: Some(&POOL),
        })
        .collect();
    assert!(POOL.spare().is_empty());
    drop(taken);
    assert_eq!(POOL.spare().len(), 2);
}

/// Only the filled start of a reused buffer may reach a response, never another object's bytes.
#[test]
fn a_reused_buffer_shows_only_what_this_read_filled() {
    let dir = crate::test_support::TestDir::new();
    let long = dir.0.join("long");
    let short = dir.0.join("short");
    let pooled = SMALL_BUFFERS.buffer_bytes;
    fs::write(&long, vec![b'x'; pooled]).unwrap();
    fs::write(&short, b"short").unwrap();
    drop(read_chunk(&File::open(long).unwrap(), 0, pooled).unwrap());
    let bytes = read_chunk(&File::open(short).unwrap(), 0, pooled).unwrap();
    assert_eq!(bytes, b"short"[..]);
}
