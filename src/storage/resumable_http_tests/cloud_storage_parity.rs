//! Behaviors recorded from Cloud Storage that differ from a strict reading of the API.

use super::*;

const MEDIA: &str = "/upload/storage/v1/b/bucket/o?uploadType=media&name=object";

#[tokio::test]
async fn media_upload_ignores_declared_hashes_and_stores_the_computed_ones() {
    let h = Harness::start(1000, false, None).await;
    for headers in [
        "X-Goog-Hash: crc32c=AAAAAA==\r\n",
        "X-Goog-Hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n",
        "X-Goog-Hash: crc32c=!!\r\n",
        "X-Goog-Hash: sha1=AAAA\r\n",
        "Content-MD5: AAAAAAAAAAAAAAAAAAAAAA==\r\n",
    ] {
        let reply = h.request("POST", MEDIA, headers, b"hello").await;
        assert_eq!(reply.status, 200, "{headers}");
        let body = reply.json();
        assert_eq!(body["crc32c"], "mnG7TA==");
        assert_eq!(body["md5Hash"], "XUFAKrxLKna5cZ2REBfFkg==");
    }
    h.stop().await;
}

#[tokio::test]
async fn multipart_metadata_hash_mismatch_names_both_digests() {
    let h = Harness::start(1000, false, None).await;
    let body = "--b\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"o\",\"crc32c\":\"AAAAAA==\"}\r\n--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b--";
    let reply = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=multipart",
            "Content-Type: multipart/related; boundary=b\r\n",
            body.as_bytes(),
        )
        .await;
    assert_eq!(reply.status, 400);
    assert_eq!(
        reply.json()["error"]["message"],
        "Provided CRC32C \"AAAAAA==\" doesn't match calculated CRC32C \"mnG7TA==\"."
    );
    h.stop().await;
}

#[tokio::test]
async fn multipart_upload_content_types_must_agree_and_plain_bodies_are_media() {
    let h = Harness::start(1000, false, None).await;
    let mismatch = "--b\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"o\",\"contentType\":\"application/json\"}\r\n--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b--";
    let reply = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=multipart",
            "Content-Type: multipart/related; boundary=b\r\n",
            mismatch.as_bytes(),
        )
        .await;
    assert_eq!(reply.status, 400);
    // The envelope is JSON but labelled text/html, as Cloud Storage sent it.
    assert_eq!(reply.headers["content-type"], "text/html; charset=UTF-8");
    assert!(!reply.headers.contains_key("x-content-type-options"));
    assert!(
        reply.json()["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Content-Type specified in the upload (text/plain) does not match")
    );
    let untyped = "--b\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"untyped\"}\r\n--b\r\n\r\nhello\r\n--b--";
    let reply = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=multipart",
            "Content-Type: multipart/related; boundary=b\r\n",
            untyped.as_bytes(),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert!(reply.json().get("contentType").is_none());
    let plain = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=multipart&name=plain",
            "Content-Type: text/plain\r\n",
            b"hello",
        )
        .await;
    assert_eq!(plain.status, 200);
    assert_eq!(plain.json()["contentType"], "text/plain");
    h.stop().await;
}

#[tokio::test]
async fn media_conditionals_keep_the_representation_headers() {
    let h = Harness::start(1000, false, None).await;
    let created = h
        .request("POST", MEDIA, "Content-Type: text/plain\r\n", b"hello")
        .await
        .json();
    let etag = created["etag"].as_str().unwrap();
    let get = "/storage/v1/b/bucket/o/object?alt=media";
    // If-None-Match wins over a failing If-Match, and the 304 keeps the type and x-goog fields.
    let reply = h
        .request(
            "GET",
            get,
            &format!("If-None-Match: {etag}\r\nIf-Match: \"other\"\r\n"),
            b"",
        )
        .await;
    assert_eq!(reply.status, 304);
    assert_eq!(reply.headers["content-type"], "text/plain");
    assert_eq!(reply.headers["x-goog-stored-content-length"], "5");
    assert!(!reply.headers.contains_key("x-goog-hash"));
    let reply = h.request("GET", get, "If-Match: \"other\"\r\n", b"").await;
    assert_eq!(reply.status, 412);
    assert_eq!(reply.body, b"Precondition Failed");
    // Date validators and If-Range are ignored; several ranges are refused.
    for headers in [
        "If-Modified-Since: Fri, 01 Jan 2100 00:00:00 GMT\r\n",
        "If-Unmodified-Since: Thu, 01 Jan 2015 00:00:00 GMT\r\n",
    ] {
        assert_eq!(h.request("GET", get, headers, b"").await.status, 200);
    }
    let ranged = h
        .request("GET", get, "Range: bytes=1-2\r\nIf-Range: stale\r\n", b"")
        .await;
    assert_eq!((ranged.status, ranged.body.as_slice()), (206, &b"el"[..]));
    let multiple = h.request("GET", get, "Range: bytes=0-1,3-4\r\n", b"").await;
    assert_eq!(multiple.status, 501);
    assert_eq!(multiple.body, b"Multiple Ranges are not supported");
    h.stop().await;
}

#[tokio::test]
async fn metadata_writes_ignore_unknown_members_and_release_holds_on_put() {
    let h = Harness::start(1000, false, None).await;
    h.request("POST", MEDIA, "", b"hello").await;
    let object = "/storage/v1/b/bucket/o/object";
    let json = "Content-Type: application/json\r\n";
    let held = h
        .request(
            "PATCH",
            object,
            json,
            br#"{"temporaryHold":true,"bogus":1}"#,
        )
        .await;
    assert_eq!(held.status, 200);
    assert_eq!(held.json()["temporaryHold"], true);
    // A non-boolean hold and a null customTime change nothing.
    let same = h
        .request(
            "PATCH",
            object,
            json,
            br#"{"temporaryHold":"yes","customTime":null}"#,
        )
        .await;
    assert_eq!(same.status, 200);
    assert_eq!(same.json()["temporaryHold"], true);
    let put = h
        .request("PUT", object, json, br#"{"metadata":{"a":"1"}}"#)
        .await;
    assert_eq!(put.status, 200);
    assert!(put.json().get("temporaryHold").is_none());
    h.stop().await;
}

#[tokio::test]
async fn failed_conditional_media_read_of_a_public_object_is_never_cacheable() {
    let h = Harness::start(1000, false, None).await;
    let created = h
        .request(
            "POST",
            &format!("{MEDIA}&predefinedAcl=publicRead"),
            "Content-Type: text/plain\r\nX-Goog-Gcs-Idempotency-Token: pub\r\n",
            b"hello",
        )
        .await;
    assert_eq!(created.status, 200);
    let get = "/storage/v1/b/bucket/o/object?alt=media";
    let ok = h.request("GET", get, "", b"").await;
    assert_eq!(ok.status, 200);
    assert_eq!(
        ok.headers["cache-control"],
        "public, max-age=3600, must-revalidate"
    );
    let failed = h.request("GET", get, "If-Match: \"other\"\r\n", b"").await;
    assert_eq!(failed.status, 412);
    assert_eq!(
        failed.headers["cache-control"],
        "no-cache, no-store, max-age=0, must-revalidate"
    );
    assert_eq!(failed.headers["pragma"], "no-cache");
    h.stop().await;
}
