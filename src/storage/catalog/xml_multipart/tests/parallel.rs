//! Parts of one upload write side by side; only the same number or the whole upload excludes them.

use super::super::super::fault::Crash;
use super::*;
use crate::{config::Config, hashing::Hasher, test_support::HANG_GUARD, time_limit::TimeLimit};
use bytes::Bytes;
use std::any::Any;
use tokio::task::JoinHandle;

const PART_LIMIT: u64 = 64 * 1024 * 1024;

/// Past `HANG_GUARD`, so a request parked on a holder resumes only when woken, and a test fails
/// on the guard rather than on this budget.
const PATIENT_MILLIS: u64 = 120_000;

async fn patient(clock: Arc<dyn Clock>) -> (TestDir, Catalog) {
    let (dir, catalog) = setup(clock).await;
    catalog.apply_session_limits(&Config {
        catalog_wait_timeout: TimeLimit::from_millis(PATIENT_MILLIS),
        ..Default::default()
    });
    (dir, catalog)
}

/// A part request whose body stops partway, as a slow client's does.
struct OpenPart {
    sender: mpsc::Sender<Result<UploadChunk, io::Error>>,
    request: JoinHandle<Result<Part, Error>>,
}

impl OpenPart {
    async fn start(
        catalog: &Catalog,
        target: &Target,
        number: u16,
        first: impl Into<Bytes>,
    ) -> Self {
        Self::start_within(catalog, target, number, first, PART_LIMIT).await
    }

    async fn start_within(
        catalog: &Catalog,
        target: &Target,
        number: u16,
        first: impl Into<Bytes>,
        max_bytes: u64,
    ) -> Self {
        let lease = catalog
            .lease_xml_part(target.clone(), number, BucketAccess::Permissive)
            .await
            .unwrap();
        let (sender, body) = mpsc::channel(2);
        sender
            .send(Ok(UploadChunk::Data(first.into())))
            .await
            .unwrap();
        let worker = catalog.clone();
        let request = tokio::spawn(async move {
            worker
                .put_xml_part(
                    lease,
                    PartUpload {
                        expected: Default::default(),
                        max_bytes,
                    },
                    body,
                )
                .await
        });
        eventually_async("the part to consume its first chunk", || async {
            sender.capacity() == 2
        })
        .await;
        Self { sender, request }
    }

    async fn finish(self, rest: impl Into<Bytes>) -> Result<Part, Error> {
        self.sender
            .send(Ok(UploadChunk::Data(rest.into())))
            .await
            .unwrap();
        self.sender.send(Ok(UploadChunk::End)).await.unwrap();
        self.request.await.unwrap()
    }

    async fn disconnect(self) -> Result<Part, Error> {
        drop(self.sender);
        self.request.await.unwrap()
    }
}

fn etag_of(bytes: &[u8]) -> String {
    let mut hasher = Hasher::default();
    hasher.update(bytes);
    format!("\"{}\"", crate::hashing::encode_lower(&hasher.finish().md5))
}

async fn numbers(catalog: &Catalog, target: &Target) -> Vec<u16> {
    let (_, parts, _) = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 1000)
        .await
        .unwrap();
    parts.into_iter().map(|part| part.number).collect()
}

fn staged_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir.join("staging")).unwrap().count()
}

async fn complete_parts(
    catalog: &Catalog,
    target: &Target,
    parts: &[(u16, &str)],
) -> Result<ObjectRecord, Error> {
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await?;
    let parts = parts
        .iter()
        .map(|(number, etag)| CompletedPart {
            number: *number,
            etag: (*etag).to_owned(),
        })
        .collect();
    catalog
        .complete_xml_multipart(lease, parts, PART_LIMIT)
        .await
}

/// The failure this design removes: with one lease per upload, a stalled body made every other
/// part wait out the catalog wait and answer 503. This catalog fails a wait at once, so the third
/// part finishing shows that nothing waited, whatever the timing.
#[tokio::test]
async fn a_part_finishes_while_two_other_bodies_stay_stalled() {
    let (dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let filler = |byte| vec![byte; usize::try_from(MIN_PART_BYTES).unwrap()];
    let first = OpenPart::start(&catalog, &target, 1, &b"1"[..]).await;
    let second = OpenPart::start(&catalog, &target, 2, &b"2"[..]).await;
    assert_eq!(catalog.multipart_parts_writing(&target.id), 2);

    let third = put(&catalog, &target, 3, b"third").await.unwrap();

    // A body still streaming is invisible until its commit; each part owns one staging file.
    assert_eq!(numbers(&catalog, &target).await, [3]);
    assert_eq!(staged_files(&dir.0), 3);

    // Finishing out of order still assembles in number order.
    let second = second.finish(filler(b'2')).await.unwrap();
    assert_eq!(numbers(&catalog, &target).await, [2, 3]);
    let first = first.finish(filler(b'1')).await.unwrap();
    assert!(!catalog.has_session_writers());
    complete_parts(
        &catalog,
        &target,
        &[(1, &first.etag), (2, &second.etag), (3, &third.etag)],
    )
    .await
    .unwrap();
    let mut expected = b"1".to_vec();
    expected.extend(filler(b'1'));
    expected.extend(b"2");
    expected.extend(filler(b'2'));
    expected.extend(b"third");
    assert!(bytes(&catalog).await == expected);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_part_number_waits_and_the_last_to_finish_wins() {
    let (dir, catalog) = patient(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let open = OpenPart::start(&catalog, &target, 1, &b"first "[..]).await;
    let same_number = WriterKey::MultipartPart(target.id.clone(), 1);
    assert!(matches!(
        catalog.claim_writer(same_number, KeyOrigin::Client),
        Err(ClaimError::Busy)
    ));
    let (worker, waiting) = (catalog.clone(), target.clone());
    let second = tokio::spawn(async move { put(&worker, &waiting, 1, b"second body").await });
    // Another number passes both of them.
    put(&catalog, &target, 2, b"other").await.unwrap();
    assert!(!second.is_finished());

    let first = open.finish(&b"body"[..]).await.unwrap();
    let second = tokio::time::timeout(HANG_GUARD, second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.etag, etag_of(b"first body"));
    assert_eq!(second.etag, etag_of(b"second body"));
    let (_, parts, _) = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 1)
        .await
        .unwrap();
    assert_eq!(parts[0].etag, second.etag);
    assert_eq!(parts[0].size, 11);
    // The replaced body is gone: one staging file for each stored part.
    assert_eq!(deletion_count(&catalog).await, 0);
    assert_eq!(staged_files(&dir.0), 2);

    assert!(matches!(
        complete(&catalog, &target, &first.etag).await,
        Err(Error::InvalidPart)
    ));
    complete(&catalog, &target, &second.etag).await.unwrap();
    assert_eq!(bytes(&catalog).await, b"second body");
    catalog.close().await.unwrap();
}

#[derive(Clone, Copy)]
enum WholeRequest {
    Complete,
    Abort,
}

impl WholeRequest {
    async fn run(self, catalog: Catalog, target: Target, etag: String) -> Result<(), Error> {
        match self {
            Self::Complete => complete(&catalog, &target, &etag).await.map(|_| ()),
            Self::Abort => {
                let lease = catalog
                    .lease_xml_multipart(target, BucketAccess::Permissive)
                    .await?;
                catalog.abort_xml_multipart(lease).await
            }
        }
    }
}

/// A later part must not overtake the waiting request, or steady part traffic could postpone it
/// past its budget. The later part ends with `NoSuchUpload` only if the request went first.
async fn whole_request_waits_for_the_part_in_flight(request: WholeRequest) -> (TestDir, Catalog) {
    let (dir, catalog) = patient(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let open = OpenPart::start(&catalog, &target, 1, &b"in flight "[..]).await;
    let whole =
        tokio::spawn(request.run(catalog.clone(), target.clone(), etag_of(b"in flight body")));
    eventually_async("the request to take its place in line", || async {
        catalog.multipart_whole_waiters(&target.id) == 1
    })
    .await;
    let (worker, waiting) = (catalog.clone(), target.clone());
    let late = tokio::spawn(async move { put(&worker, &waiting, 2, b"late").await });
    assert!(matches!(
        catalog.claim_writer(
            WriterKey::MultipartPart(target.id.clone(), 3),
            KeyOrigin::Client
        ),
        Err(ClaimError::Busy)
    ));
    assert!(!whole.is_finished());

    open.finish(&b"body"[..]).await.unwrap();
    tokio::time::timeout(HANG_GUARD, whole)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let late = tokio::time::timeout(HANG_GUARD, late)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(late, Err(Error::NoSuchUpload)), "{late:?}");
    assert!(matches!(
        catalog
            .list_xml_parts(target, BucketAccess::Permissive, 0, 1000)
            .await,
        Err(Error::NoSuchUpload)
    ));
    assert!(!catalog.has_session_writers());
    (dir, catalog)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completion_waits_for_the_part_in_flight_and_a_later_part_cannot_overtake_it() {
    let (dir, catalog) = whole_request_waits_for_the_part_in_flight(WholeRequest::Complete).await;
    assert_eq!(bytes(&catalog).await, b"in flight body");
    assert_eq!(staged_files(&dir.0), 0);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_waits_for_the_part_in_flight_and_a_later_part_cannot_overtake_it() {
    let (dir, catalog) = whole_request_waits_for_the_part_in_flight(WholeRequest::Abort).await;
    assert_eq!(deletion_count(&catalog).await, 0);
    assert_eq!(staged_files(&dir.0), 0);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_whole_request_lets_the_parts_it_held_back_continue() {
    let (_dir, catalog) = patient(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let open = OpenPart::start(&catalog, &target, 1, &b"stalled "[..]).await;
    let waiting =
        tokio::spawn(WholeRequest::Abort.run(catalog.clone(), target.clone(), String::new()));
    eventually_async("the request to take its place in line", || async {
        catalog.multipart_whole_waiters(&target.id) == 1
    })
    .await;
    let (worker, later) = (catalog.clone(), target.clone());
    let held_back = tokio::spawn(async move { put(&worker, &later, 2, b"held back").await });
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(!held_back.is_finished());

    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    // Giving up the place in line must wake it; nothing else would before the budget.
    tokio::time::timeout(HANG_GUARD, held_back)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(catalog.multipart_whole_waiters(&target.id), 0);
    open.finish(&b"body"[..]).await.unwrap();
    assert_eq!(numbers(&catalog, &target).await, [1, 2]);
    assert!(!catalog.has_session_writers());
    catalog.close().await.unwrap();
}

#[derive(Clone, Copy, Debug)]
enum Claim {
    Part(u16),
    Whole,
}

impl Claim {
    async fn lease(self, catalog: Catalog, target: Target) -> Result<Box<dyn Any + Send>, Error> {
        match self {
            Self::Part(number) => catalog
                .lease_xml_part(target, number, BucketAccess::Permissive)
                .await
                .map(|lease| Box::new(lease) as Box<dyn Any + Send>),
            Self::Whole => catalog
                .lease_xml_multipart(target, BucketAccess::Permissive)
                .await
                .map(|lease| Box::new(lease) as Box<dyn Any + Send>),
        }
    }
}

/// A conflicting request waits for the holder and takes over; one that outlasts its budget fails
/// retryably, and a whole-upload request gives up its place in line when it does.
#[tokio::test]
async fn a_conflicting_lease_waits_for_the_holder_and_fails_only_past_its_budget() {
    for (holder, contender) in [
        (Claim::Part(1), Claim::Part(1)),
        (Claim::Part(1), Claim::Whole),
        (Claim::Whole, Claim::Part(1)),
        (Claim::Whole, Claim::Whole),
    ] {
        for (budget, expected_busy) in [(60_000, false), (20, true)] {
            let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
            catalog.apply_session_limits(&Config {
                catalog_wait_timeout: TimeLimit::from_millis(budget),
                ..Default::default()
            });
            let target = begin(&catalog).await;
            let held = holder.lease(catalog.clone(), target.clone()).await.unwrap();
            let waiting = tokio::spawn(contender.lease(catalog.clone(), target.clone()));
            if expected_busy {
                let result = waiting.await.unwrap();
                assert!(
                    matches!(result, Err(Error::Busy)),
                    "{holder:?} {contender:?}"
                );
                assert_eq!(catalog.multipart_whole_waiters(&target.id), 0);
                drop(held);
            } else {
                for _ in 0..32 {
                    tokio::task::yield_now().await;
                }
                assert!(!waiting.is_finished(), "{holder:?} {contender:?}");
                drop(held);
                drop(waiting.await.unwrap().unwrap());
            }
            assert!(!catalog.has_session_writers(), "{holder:?} {contender:?}");
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn parts_of_different_numbers_and_uploads_do_not_exclude_each_other() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let (target, other) = (begin(&catalog).await, begin(&catalog).await);
    let first = Claim::Part(1)
        .lease(catalog.clone(), target.clone())
        .await
        .unwrap();
    let second = Claim::Part(2)
        .lease(catalog.clone(), target.clone())
        .await
        .unwrap();
    let elsewhere = Claim::Whole
        .lease(catalog.clone(), other.clone())
        .await
        .unwrap();
    assert_eq!(catalog.multipart_parts_writing(&target.id), 2);
    assert!(matches!(
        Claim::Part(1).lease(catalog.clone(), other).await,
        Err(Error::Busy)
    ));
    // One part leaving is not enough for the whole upload.
    drop(first);
    assert!(matches!(
        Claim::Whole.lease(catalog.clone(), target.clone()).await,
        Err(Error::Busy)
    ));
    drop(second);
    drop(Claim::Whole.lease(catalog.clone(), target).await.unwrap());
    drop(elsewhere);
    assert!(!catalog.has_session_writers());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_part_number_outside_the_range_gets_no_lease() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    for number in [0, 10_001] {
        assert!(matches!(
            Claim::Part(number)
                .lease(catalog.clone(), target.clone())
                .await,
            Err(Error::InvalidPart)
        ));
    }
    assert!(!catalog.has_session_writers());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_disconnected_part_leaves_no_staging_file_and_gives_its_number_back() {
    let (dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let beside = OpenPart::start(&catalog, &target, 2, &b"beside "[..]).await;
    let cut = OpenPart::start(&catalog, &target, 1, &b"cut off"[..]).await;
    assert_eq!(staged_files(&dir.0), 2);

    let result = cut.disconnect().await;
    assert!(
        matches!(result, Err(Error::Object(ObjectError::BodyAborted(_)))),
        "{result:?}"
    );
    assert_eq!(staged_files(&dir.0), 1);
    assert_eq!(catalog.multipart_parts_writing(&target.id), 1);
    assert_eq!(numbers(&catalog, &target).await, Vec::<u16>::new());

    // A retry of that number needs no wait, and the part beside it never noticed.
    put(&catalog, &target, 1, b"retried").await.unwrap();
    beside.finish(&b"part"[..]).await.unwrap();
    assert_eq!(numbers(&catalog, &target).await, [1, 2]);
    assert_eq!(staged_files(&dir.0), 2);
    assert!(!catalog.has_session_writers());
    catalog.close().await.unwrap();
}

const PART_STEPS: [Step; 6] = [
    Step::BodyWrite,
    Step::BodyFlush,
    Step::BodySync,
    Step::Checkpoint,
    Step::BeforeCommit,
    Step::AfterCommit,
];

#[tokio::test]
async fn a_failed_part_leaves_the_part_beside_it_writing_and_both_survive_a_restart() {
    for step in PART_STEPS {
        let (dir, catalog) = setup(Arc::new(SystemClock)).await;
        let target = begin(&catalog).await;
        let beside = OpenPart::start(&catalog, &target, 2, &b"beside "[..]).await;
        catalog.inject(step, 0, Fault::Io(io::ErrorKind::StorageFull));

        let failed = put(&catalog, &target, 1, b"failing").await;
        assert!(
            matches!(failed, Err(Error::Object(ObjectError::Io(_)))),
            "{step:?}: {failed:?}"
        );
        assert_eq!(catalog.pending_faults(), 0, "{step:?}");
        assert_eq!(catalog.multipart_parts_writing(&target.id), 1, "{step:?}");

        beside.finish(&b"part"[..]).await.unwrap();
        // A failure after COMMIT is ambiguous to the client, and the part stays committed.
        let expected: &[u16] = if step == Step::AfterCommit {
            &[1, 2]
        } else {
            &[2]
        };
        assert_eq!(numbers(&catalog, &target).await, expected, "{step:?}");
        assert_eq!(staged_files(&dir.0), expected.len(), "{step:?}");
        catalog.close().await.unwrap();

        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(numbers(&reopened, &target).await, expected, "{step:?}");
        assert_eq!(staged_files(&dir.0), expected.len(), "{step:?}");
        reopened.close().await.unwrap();
    }
}

/// A crash while another body is streaming: the committed parts stay readable, even after a power
/// loss, and recovery removes the body that never committed.
#[tokio::test]
async fn a_crash_beside_a_part_in_flight_keeps_committed_parts_and_drops_that_body() {
    for crash in Crash::ALL {
        for step in PART_STEPS {
            let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
            let target = begin(&catalog).await;
            let kept = put(&catalog, &target, 1, b"kept").await.unwrap();
            let open = OpenPart::start(&catalog, &target, 2, &b"in flight "[..]).await;
            let snapshot = TestDir::new();
            let path = snapshot.0.join("data");
            catalog.inject(step, 0, crash.fault(path.clone()));

            let committed = put(&catalog, &target, 3, b"committed").await.unwrap();
            assert_eq!(catalog.pending_faults(), 0, "{crash:?} {step:?}");

            let reopened = Catalog::open(&path).await.unwrap();
            let expected: &[u16] = if step == Step::AfterCommit {
                &[1, 3]
            } else {
                &[1]
            };
            assert_eq!(
                numbers(&reopened, &target).await,
                expected,
                "{crash:?} {step:?}"
            );
            assert_eq!(staged_files(&path), expected.len(), "{crash:?} {step:?}");
            // The surviving part's bytes are intact, not only its row and its length.
            let (number, etag, bytes_expected) = if step == Step::AfterCommit {
                (3, &committed.etag, b"committed".as_slice())
            } else {
                (1, &kept.etag, b"kept".as_slice())
            };
            complete_parts(&reopened, &target, &[(number, etag)])
                .await
                .unwrap();
            assert_eq!(bytes(&reopened).await, bytes_expected, "{crash:?} {step:?}");
            reopened.close().await.unwrap();

            // The live catalog never crashed, so its stalled part still finishes.
            open.finish(&b"body"[..]).await.unwrap();
            assert_eq!(numbers(&catalog, &target).await, [1, 2, 3]);
            catalog.close().await.unwrap();
        }
    }
}

/// Maintenance claims the whole upload, so any part still writing keeps it, down to the last one.
#[tokio::test]
async fn an_expired_upload_stays_until_its_last_part_in_flight_finishes() {
    let clock = Arc::new(ManualClock::new(
        UNIX_EPOCH + Duration::from_secs(1_000_000),
    ));
    let (dir, catalog) = setup(clock.clone()).await;
    let target = begin(&catalog).await;
    let first = OpenPart::start(&catalog, &target, 1, &b"first"[..]).await;
    let second = OpenPart::start(&catalog, &target, 2, &b"second"[..]).await;
    clock.advance(Duration::from_secs(604800));
    let now = unix_millis(clock.now()).unwrap();

    catalog.maintain_xml_multipart(now).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 1);

    assert!(matches!(
        first.finish(&b""[..]).await,
        Err(Error::NoSuchUpload)
    ));
    catalog.maintain_xml_multipart(now).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 1);

    assert!(matches!(
        second.finish(&b""[..]).await,
        Err(Error::NoSuchUpload)
    ));
    catalog.maintain_xml_multipart(now).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 0);
    assert_eq!(staged_files(&dir.0), 0);
    assert!(!catalog.has_session_writers());
    catalog.close().await.unwrap();
}

/// A deleted bucket's upload waits for its part too, and the pass that stepped over it must leave
/// a later pass looking for orphans, since no new delete will ask again.
#[tokio::test]
async fn a_deleted_bucket_retires_its_upload_after_the_part_in_flight() {
    for recreated in [false, true] {
        let (dir, catalog) = setup(Arc::new(SystemClock)).await;
        let target = begin(&catalog).await;
        let stored = put(&catalog, &target, 1, b"stored").await.unwrap();
        let open = OpenPart::start(&catalog, &target, 2, &b"in flight"[..]).await;
        catalog
            .delete_bucket("bucket", Default::default(), PERMISSIVE)
            .await
            .unwrap();
        if recreated {
            catalog
                .local_admin()
                .unwrap()
                .ensure_bucket(
                    BucketDefinition::default_for(
                        &BucketName::new("bucket").unwrap(),
                        "project",
                        &SystemClock,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
        }
        let now = unix_millis(SystemClock.now()).unwrap();
        catalog.maintain_xml_multipart(now).await.unwrap();
        assert_eq!(upload_count(&catalog).await, 1, "recreated: {recreated}");

        let result = open.finish(&b""[..]).await;
        if recreated {
            assert!(matches!(result, Err(Error::NoSuchUpload)), "{result:?}");
        } else {
            assert!(
                matches!(result, Err(Error::Object(ObjectError::BucketNotFound))),
                "{result:?}"
            );
        }
        catalog.maintain_xml_multipart(now).await.unwrap();
        assert_eq!(upload_count(&catalog).await, 0, "recreated: {recreated}");
        assert_eq!(staged_files(&dir.0), 0, "recreated: {recreated}");
        assert_eq!(deletion_count(&catalog).await, 0, "{}", stored.etag);
        catalog.close().await.unwrap();
    }
}

/// Each body passes the check before it starts, when the other has committed nothing, so only the
/// commit can hold the limit on what an upload retains.
#[tokio::test]
async fn parts_in_flight_cannot_together_pass_the_retained_limit() {
    let (dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    let first = OpenPart::start_within(&catalog, &target, 1, &b"aaaa"[..], 10).await;
    let second = OpenPart::start_within(&catalog, &target, 2, &b"bbbb"[..], 10).await;

    first.finish(&b"aaaa"[..]).await.unwrap();
    let result = second.finish(&b"bbbb"[..]).await;
    assert!(
        matches!(result, Err(Error::Object(ObjectError::TooLarge))),
        "{result:?}"
    );
    assert_eq!(numbers(&catalog, &target).await, [1]);
    assert_eq!(staged_files(&dir.0), 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_replacement_counts_against_the_retained_limit_without_the_part_it_replaces() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let target = begin(&catalog).await;
    OpenPart::start_within(&catalog, &target, 1, &b"aaaaaaaa"[..], 10)
        .await
        .finish(&b""[..])
        .await
        .unwrap();
    let part = OpenPart::start_within(&catalog, &target, 1, &b"bbbbbbbb"[..], 10)
        .await
        .finish(&b""[..])
        .await
        .unwrap();
    assert_eq!(part.etag, etag_of(b"bbbbbbbb"));
    catalog.close().await.unwrap();
}
