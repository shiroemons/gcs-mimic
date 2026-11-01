//! Rate-limit handling for the live service. Cloud Storage documents these
//! limits (https://cloud.google.com/storage/quotas): bucket creation and
//! deletion about once every two seconds per project, one metadata update per
//! second per bucket, one write per second to the same object name. Beyond a
//! limit the service answers 429. [`Paced`] waits before a request that counts
//! against a limit and, when a 429 comes back anyway, waits and retries.

use super::transport::{Request, Response, Transport};
use std::{cell::Cell, thread::sleep, time::Duration};

type Result<T> = std::result::Result<T, String>;

/// Pause before any request that changes state.
pub(crate) const WRITE_PAUSE: Duration = Duration::from_secs(1);
/// Pause before a bucket creation or deletion.
pub(crate) const BUCKET_PAUSE: Duration = Duration::from_secs(2);
const MAX_RETRIES: u32 = 5;
const FIRST_BACKOFF: Duration = Duration::from_secs(4);

/// What a request costs against the limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cost {
    Read,
    Write,
    BucketLifecycle,
}

/// gRPC status RESOURCE_EXHAUSTED, the gRPC counterpart of HTTP 429.
const GRPC_RESOURCE_EXHAUSTED: u16 = 8;

/// A gRPC request carries the RPC name (`pkg.Service/Method`) as its method.
fn is_grpc(request: &Request) -> bool {
    !request.method.chars().all(|c| c.is_ascii_uppercase())
}

pub(crate) fn cost(request: &Request) -> Cost {
    let grpc = is_grpc(request);
    if grpc {
        let rpc = request.method.rsplit('/').next().unwrap_or_default();
        return match rpc {
            "CreateBucket" | "DeleteBucket" => Cost::BucketLifecycle,
            _ if ["Get", "List", "Query", "Test", "Read"]
                .iter()
                .any(|prefix| rpc.starts_with(prefix)) =>
            {
                Cost::Read
            }
            _ => Cost::Write,
        };
    }
    if matches!(request.method.as_str(), "GET" | "HEAD") {
        return Cost::Read;
    }
    let path = request.target.split('?').next().unwrap_or_default();
    let json_bucket = path == "/storage/v1/b"
        || path
            .strip_prefix("/storage/v1/b/")
            .is_some_and(|rest| !rest.contains('/'));
    let xml_bucket = !path.starts_with("/storage/")
        && !path.starts_with("/upload/")
        && path.trim_matches('/').split('/').count() == 1;
    let lifecycle = match request.method.as_str() {
        "POST" => path == "/storage/v1/b",
        "DELETE" => json_bucket || xml_bucket,
        "PUT" => xml_bucket,
        _ => false,
    };
    if lifecycle {
        Cost::BucketLifecycle
    } else {
        Cost::Write
    }
}

pub(crate) struct Paced<'a> {
    pub(crate) inner: &'a dyn Transport,
    pub(crate) sleeper: fn(Duration),
    retries: Cell<u32>,
}

impl<'a> Paced<'a> {
    pub(crate) fn new(inner: &'a dyn Transport) -> Self {
        Self::with_sleeper(inner, sleep)
    }

    pub(crate) fn with_sleeper(inner: &'a dyn Transport, sleeper: fn(Duration)) -> Self {
        Self {
            inner,
            sleeper,
            retries: Cell::new(0),
        }
    }
}

impl Transport for Paced<'_> {
    fn send(&self, request: &Request) -> Result<Response> {
        match cost(request) {
            Cost::Read => {}
            Cost::Write => (self.sleeper)(WRITE_PAUSE),
            Cost::BucketLifecycle => (self.sleeper)(BUCKET_PAUSE),
        }
        self.retries.set(0);
        let mut backoff = FIRST_BACKOFF;
        loop {
            let response = self.inner.send(request)?;
            let throttled = if is_grpc(request) {
                response.status == GRPC_RESOURCE_EXHAUSTED
            } else {
                response.status == 429
            };
            if !throttled || self.retries.get() >= MAX_RETRIES {
                return Ok(response);
            }
            self.retries.set(self.retries.get() + 1);
            (self.sleeper)(backoff);
            backoff *= 2;
        }
    }

    fn requests(&self) -> u64 {
        self.inner.requests()
    }

    fn take_retries(&self) -> u32 {
        self.retries.replace(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static SLEPT: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
    }

    fn record(duration: Duration) {
        SLEPT.with(|s| s.borrow_mut().push(duration));
    }

    /// Answers 429 a set number of times, then 200.
    struct Throttle {
        remaining: Cell<u32>,
        sent: Cell<u64>,
    }

    impl Transport for Throttle {
        fn send(&self, _: &Request) -> Result<Response> {
            self.sent.set(self.sent.get() + 1);
            let throttled = self.remaining.get() > 0;
            self.remaining.set(self.remaining.get().saturating_sub(1));
            Ok(Response {
                status: if throttled { 429 } else { 200 },
                headers: Vec::new(),
                body: Vec::new(),
            })
        }

        fn requests(&self) -> u64 {
            self.sent.get()
        }
    }

    fn request(method: &str, target: &str) -> Request {
        Request {
            method: method.into(),
            target: target.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn classifies_requests_by_the_limit_they_count_against() {
        assert_eq!(cost(&request("GET", "/storage/v1/b/x")), Cost::Read);
        assert_eq!(cost(&request("PATCH", "/storage/v1/b/x")), Cost::Write);
        assert_eq!(
            cost(&request("POST", "/storage/v1/b?project=p")),
            Cost::BucketLifecycle
        );
        assert_eq!(
            cost(&request("DELETE", "/storage/v1/b/x")),
            Cost::BucketLifecycle
        );
        assert_eq!(cost(&request("DELETE", "/storage/v1/b/x/o/y")), Cost::Write);
        assert_eq!(cost(&request("DELETE", "/bucket")), Cost::BucketLifecycle);
        assert_eq!(cost(&request("DELETE", "/bucket/object")), Cost::Write);
        assert_eq!(
            cost(&request("google.storage.v2.Storage/GetBucket", "")),
            Cost::Read
        );
        assert_eq!(
            cost(&request("google.storage.v2.Storage/UpdateObject", "")),
            Cost::Write
        );
    }

    #[test]
    fn waits_before_writes_and_retries_a_429_counting_the_retries() {
        SLEPT.with(|s| s.borrow_mut().clear());
        let inner = Throttle {
            remaining: Cell::new(2),
            sent: Cell::new(0),
        };
        let paced = Paced::with_sleeper(&inner, record);
        let response = paced.send(&request("DELETE", "/storage/v1/b/x")).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(paced.take_retries(), 2);
        assert_eq!(paced.take_retries(), 0);
        assert_eq!(inner.sent.get(), 3);
        let slept = SLEPT.with(|s| s.borrow().clone());
        assert_eq!(slept, [BUCKET_PAUSE, FIRST_BACKOFF, FIRST_BACKOFF * 2]);
        SLEPT.with(|s| s.borrow_mut().clear());
        paced.send(&request("GET", "/x")).unwrap();
        assert!(SLEPT.with(|s| s.borrow().is_empty()));
    }

    #[test]
    fn retries_a_grpc_resource_exhausted_but_not_an_http_8() {
        struct Status(u16, Cell<u32>);
        impl Transport for Status {
            fn send(&self, _: &Request) -> Result<Response> {
                let left = self.1.get();
                self.1.set(left.saturating_sub(1));
                Ok(Response {
                    status: if left > 0 { self.0 } else { 0 },
                    headers: Vec::new(),
                    body: Vec::new(),
                })
            }
            fn requests(&self) -> u64 {
                0
            }
        }
        let grpc = Status(8, Cell::new(2));
        let paced = Paced::with_sleeper(&grpc, |_| {});
        let response = paced
            .send(&request("google.storage.v2.Storage/GetBucket", ""))
            .unwrap();
        assert_eq!((response.status, paced.take_retries()), (0, 2));
        let http = Status(8, Cell::new(2));
        let paced = Paced::with_sleeper(&http, |_| {});
        assert_eq!(paced.send(&request("GET", "/x")).unwrap().status, 8);
        assert_eq!(paced.take_retries(), 0);
    }
    #[test]
    fn gives_up_after_the_retry_limit_and_returns_the_last_429() {
        let inner = Throttle {
            remaining: Cell::new(100),
            sent: Cell::new(0),
        };
        let paced = Paced::with_sleeper(&inner, |_| {});
        let response = paced.send(&request("PATCH", "/x")).unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(paced.take_retries(), MAX_RETRIES);
        assert_eq!(inner.sent.get(), u64::from(MAX_RETRIES) + 1);
    }
}
