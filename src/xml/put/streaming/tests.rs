use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::{
    auth::signature::{STREAMING_PAYLOAD, timestamp},
    server::{DeadlineWatch, RequestBody, RequestCtx},
    storage::{
        Catalog,
        fault::{self, Fault, Step},
    },
    xml::test_support::SignedUploadFixture as Fixture,
};
use http_body_util::Full;
use hyper::Request;
use ring::hmac;
use std::io;

fn sign(key: &hmac::Key, message: &str) -> String {
    crate::hashing::encode_lower(hmac::sign(key, message.as_bytes()).as_ref())
}

impl Fixture {
    fn request(&self, bytes: &[u8]) -> Request<RequestBody> {
        let date = timestamp(self.state.clock.now()).unwrap();
        let scope = format!("{}/auto/s3/aws4_request", &date[..8]);
        let mut key = hmac::Key::new(hmac::HMAC_SHA256, format!("AWS4{}", self.secret).as_bytes());
        for part in scope.split('/') {
            key = hmac::Key::new(
                hmac::HMAC_SHA256,
                hmac::sign(&key, part.as_bytes()).as_ref(),
            );
        }
        let canonical = format!(
            "PUT\n/bucket/object\n\ncontent-encoding:aws-chunked\nhost:localhost\nx-amz-content-sha256:{STREAMING_PAYLOAD}\nx-amz-date:{date}\nx-amz-decoded-content-length:{}\n\ncontent-encoding;host;x-amz-content-sha256;x-amz-date;x-amz-decoded-content-length\n{STREAMING_PAYLOAD}",
            bytes.len()
        );
        let mut previous = sign(
            &key,
            &format!(
                "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
                crate::hashing::encode_lower(
                    digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref()
                )
            ),
        );
        let seed = previous.clone();
        let empty = crate::hashing::encode_lower(digest::digest(&digest::SHA256, b"").as_ref());
        let mut body = Vec::new();
        for bytes in [bytes, b"".as_slice()] {
            previous = sign(
                &key,
                &format!(
                    "AWS4-HMAC-SHA256-PAYLOAD\n{date}\n{scope}\n{previous}\n{empty}\n{}",
                    crate::hashing::encode_lower(digest::digest(&digest::SHA256, bytes).as_ref())
                ),
            );
            body.extend_from_slice(
                format!("{:x};chunk-signature={previous}\r\n", bytes.len()).as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        Request::builder().method("PUT").uri("/bucket/object")
            .header("host","localhost").header("content-encoding","aws-chunked")
            .header("x-amz-content-sha256",STREAMING_PAYLOAD).header("x-amz-date",&date)
            .header("x-amz-decoded-content-length",bytes.len()).header("content-length",body.len())
            .header("authorization",format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=content-encoding;host;x-amz-content-sha256;x-amz-date;x-amz-decoded-content-length, Signature={seed}",self.access_id))
            .body(RequestBody::Buffered(Full::new(Bytes::from(body)))).unwrap()
    }

    async fn upload(&self, bytes: &[u8]) -> Result<Response<ResponseBody>, Error> {
        let request = self.request(bytes);
        let (activity, _) = tokio::sync::watch::channel(Default::default());
        let ctx = RequestCtx {
            state: &self.state,
            config: &self.config,
            endpoints: &self.endpoints,
            activity: &activity,
        };
        let (caller, streaming) =
            crate::xml::authenticate_upload_target(crate::xml::Authentication {
                auth: &self.state.auth,
                catalog: &self.state.catalog,
                method: request.method(),
                uri: request.uri(),
                headers: request.headers(),
                clock: self.state.clock.as_ref(),
                resource: &crate::auth::signature::Resource::PathStyle,
            })
            .await?;
        let operation = super::super::super::route(
            request.method(),
            &self.config,
            super::super::super::select_object(request.uri(), request.headers(), &self.config)?,
            request.headers(),
        )
        .unwrap()?;
        super::super::respond(
            request,
            operation,
            ctx.object(
                caller,
                ctx.start_object_deadline(DeadlineWatch::CatalogOnly),
            ),
            streaming,
        )
        .await
    }
}

#[tokio::test]
async fn signed_chunk_upload_write_tail_flush_and_sync_failures_preserve_resource_and_restart() {
    let fixture = Fixture::new(None).await;
    assert_eq!(
        fixture.upload(b"old").await.unwrap().status(),
        StatusCode::OK
    );
    let before = fixture
        .state
        .catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    for (step, skip) in [
        (Step::BodyWrite, 0),
        (Step::BodyWrite, 1),
        (Step::BodyFlush, 0),
        (Step::BodySync, 0),
    ] {
        fault::arm(&fixture.dir.0, step, skip, Fault::Io(io::ErrorKind::Other));
        let failure = fixture.upload(&vec![b'n'; 256 * 1024]).await.err().unwrap();
        assert_eq!(
            failure.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{step:?}: {failure:?}"
        );
        assert_eq!(fault::pending(&fixture.dir.0), 0, "{step:?}");
        assert_eq!(
            fixture
                .state
                .catalog
                .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
                .await
                .unwrap(),
            before
        );
        let (_, mut reader) = fixture
            .state
            .catalog
            .open_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(
            reader.next_chunk().await.unwrap().unwrap(),
            b"old".as_slice()
        );
        assert!(reader.next_chunk().await.unwrap().is_none());
        drop(reader);
        assert_eq!(
            std::fs::read_dir(fixture.dir.0.join("staging"))
                .unwrap()
                .count(),
            0
        );
        let connection = rusqlite::Connection::open(fixture.dir.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM objects", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    fixture.state.catalog.close().await.unwrap();
    let reopened = Catalog::open(&fixture.dir.0).await.unwrap();
    assert_eq!(
        reopened
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap(),
        before
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn streaming_key_lookup_failure_keeps_its_backend_cause() {
    use std::error::Error as _;
    let fixture = Fixture::new(None).await;
    fixture.state.catalog.close().await.unwrap();
    let error = fixture.upload(b"no publication").await.err().unwrap();
    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Verification retains the typed catalog error even when that error has no nested source.
    let request = fixture.request(b"no publication");
    let failure = fixture
        .state
        .auth
        .authenticate_streaming(
            &fixture.state.catalog,
            request.method(),
            request.uri(),
            request.headers(),
            fixture.state.clock.as_ref(),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(
        failure,
        crate::auth::signature::VerificationError::Catalog(_)
    ));
    assert!(failure.source().is_some());
}
