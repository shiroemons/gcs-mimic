//! Customer-supplied encryption key behavior over the JSON and XML wire protocols.

use super::{Harness, Reply};
use serde_json::json;

const OLD: [u8; 32] = [1; 32];
const NEW: [u8; 32] = [2; 32];

fn key(prefix: &str, key: &[u8; 32]) -> String {
    crate::csek::key_headers(prefix, key)
        .iter()
        .map(|(name, value)| format!("{name}: {}\r\n", value.to_str().unwrap()))
        .collect()
}

fn destination(value: &[u8; 32]) -> String {
    key(crate::csek::DESTINATION_PREFIX, value)
}

fn source(value: &[u8; 32]) -> String {
    key(crate::csek::SOURCE_PREFIX, value)
}

fn sha256(value: &[u8; 32]) -> String {
    crate::json::base64(&crate::csek::key_sha256(value))
}

async fn upload(h: &Harness, name: &str, headers: &str, body: &[u8]) -> Reply {
    h.request(
        "POST",
        &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
        &format!("Content-Type: text/plain\r\n{headers}"),
        body,
    )
    .await
}

async fn media(h: &Harness, name: &str, headers: &str) -> Reply {
    h.request(
        "GET",
        &format!("/storage/v1/b/bucket/o/{name}?alt=media"),
        headers,
        b"",
    )
    .await
}

fn xml_code(reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status,
        status,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let body = String::from_utf8_lossy(&reply.body);
    assert!(body.contains(&format!("<Code>{code}</Code>")), "{body}");
}

#[tokio::test]
async fn csek_media_upload_requires_its_key_for_data_but_not_metadata() {
    let h = Harness::start(1 << 20, false, None).await;
    let created = upload(&h, "secret", &destination(&OLD), b"secret").await;
    assert_eq!(created.status, 200, "{}", created.json());
    let resource = created.json();
    assert_eq!(
        resource["customerEncryption"],
        json!({"encryptionAlgorithm": "AES256", "keySha256": sha256(&OLD)})
    );
    // The uploader holds the key, so its response includes the plaintext hashes.
    assert!(resource["md5Hash"].is_string() && resource["crc32c"].is_string());

    let metadata = h
        .request("GET", "/storage/v1/b/bucket/o/secret", "", b"")
        .await;
    assert_eq!(metadata.status, 200);
    let metadata = metadata.json();
    assert_eq!(
        metadata["customerEncryption"],
        resource["customerEncryption"]
    );
    assert!(metadata.get("md5Hash").is_none() && metadata.get("crc32c").is_none());
    let keyed = h
        .request(
            "GET",
            "/storage/v1/b/bucket/o/secret",
            &destination(&OLD),
            b"",
        )
        .await
        .json();
    assert_eq!(keyed["md5Hash"], resource["md5Hash"]);
    h.request(
        "GET",
        "/storage/v1/b/bucket/o/secret",
        &destination(&NEW),
        b"",
    )
    .await
    .error(400, "customerEncryptionKeyIsIncorrect");
    let listed = h
        .request("GET", "/storage/v1/b/bucket/o", "", b"")
        .await
        .json();
    assert_eq!(
        listed["items"][0]["customerEncryption"]["keySha256"],
        sha256(&OLD)
    );
    assert!(listed["items"][0].get("md5Hash").is_none());

    media(&h, "secret", "")
        .await
        .error(400, "resourceIsEncryptedWithCustomerEncryptionKey");
    media(&h, "secret", &destination(&NEW))
        .await
        .error(400, "customerEncryptionKeyIsIncorrect");
    let read = media(&h, "secret", &destination(&OLD)).await;
    assert_eq!(read.status, 200);
    assert_eq!(read.body, b"secret");
    assert_eq!(read.headers["x-goog-encryption-algorithm"], "AES256");
    assert_eq!(read.headers["x-goog-encryption-key-sha256"], sha256(&OLD));
    assert!(read.headers["x-goog-hash"].contains("md5="));

    assert_eq!(upload(&h, "plain", "", b"plain").await.status, 200);
    media(&h, "plain", &destination(&OLD))
        .await
        .error(400, "resourceNotEncryptedWithCustomerEncryptionKey");
    // Routes that do not take a key reject it instead of ignoring it.
    h.request(
        "DELETE",
        "/storage/v1/b/bucket/o/secret",
        &destination(&OLD),
        b"",
    )
    .await
    .error(400, "badRequest");
    h.stop().await;
}

#[tokio::test]
async fn csek_invalid_keys_are_rejected_before_any_object_is_created() {
    let h = Harness::start(1 << 20, false, None).await;
    let valid = destination(&OLD);
    let replace = |name: &str, value: &str| {
        valid
            .lines()
            .filter(|line| !line.starts_with(&format!("{name}:")))
            .map(|line| format!("{line}\r\n"))
            .chain((!value.is_empty()).then(|| format!("{name}: {value}\r\n")))
            .collect::<String>()
    };
    for (headers, reason) in [
        (
            replace("x-goog-encryption-algorithm", "AES128"),
            "customerEncryptionAlgorithmIsInvalid",
        ),
        (
            replace("x-goog-encryption-key", ""),
            "customerEncryptionKeyFormatIsInvalid",
        ),
        (
            replace("x-goog-encryption-key", &crate::json::base64(&[1; 16])),
            "customerEncryptionKeyFormatIsInvalid",
        ),
        (
            replace("x-goog-encryption-key-sha256", &sha256(&NEW)),
            "customerEncryptionKeySha256IsInvalid",
        ),
        (
            replace("x-goog-encryption-key-sha256", ""),
            "customerEncryptionKeySha256IsInvalid",
        ),
    ] {
        upload(&h, "rejected", &headers, b"data")
            .await
            .error(400, reason);
    }
    h.request("GET", "/storage/v1/b/bucket/o/rejected", "", b"")
        .await
        .error(404, "notFound");
    h.stop().await;
}

#[tokio::test]
async fn csek_multipart_and_resumable_uploads_bind_the_key() {
    let h = Harness::start(1 << 20, false, None).await;
    let body = b"--b\r\nContent-Type: application/json\r\n\r\n{\"name\":\"multi\"}\r\n--b\r\nContent-Type: text/plain\r\n\r\nmultipart\r\n--b--\r\n";
    let created = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=multipart",
            &format!(
                "Content-Type: multipart/related; boundary=b\r\n{}",
                destination(&OLD)
            ),
            body,
        )
        .await;
    assert_eq!(created.status, 200, "{}", created.json());
    assert_eq!(
        created.json()["customerEncryption"]["keySha256"],
        sha256(&OLD)
    );
    assert_eq!(
        media(&h, "multi", &destination(&OLD)).await.body,
        b"multipart"
    );
    media(&h, "multi", "")
        .await
        .error(400, "resourceIsEncryptedWithCustomerEncryptionKey");

    let session = h.initiate("name=resumed", &destination(&OLD), b"").await;
    assert_eq!(session.status, 200);
    let path = h.path(&session);
    // A chunk may repeat the session key but cannot replace it.
    h.request("PUT", &path, &destination(&NEW), b"resumed")
        .await
        .error(400, "customerEncryptionKeyIsIncorrect");
    let done = h
        .request("PUT", &path, &destination(&OLD), b"resumed")
        .await;
    assert_eq!(done.status, 200, "{}", done.json());
    assert_eq!(done.json()["customerEncryption"]["keySha256"], sha256(&OLD));
    assert!(done.json()["md5Hash"].is_string());
    assert_eq!(
        media(&h, "resumed", &destination(&OLD)).await.body,
        b"resumed"
    );

    let plain = h.initiate("name=plain", "", b"").await;
    let path = h.path(&plain);
    h.request("PUT", &path, &destination(&OLD), b"plain")
        .await
        .error(400, "resourceNotEncryptedWithCustomerEncryptionKey");
    // Chunks without key headers rely on the key bound at initiation.
    let session = h.initiate("name=bound", &destination(&OLD), b"").await;
    let done = h.request("PUT", &h.path(&session), "", b"bound").await;
    assert_eq!(done.status, 200);
    media(&h, "bound", "")
        .await
        .error(400, "resourceIsEncryptedWithCustomerEncryptionKey");
    h.stop().await;
}

#[tokio::test]
async fn csek_metadata_update_checks_a_supplied_key_before_committing() {
    let h = Harness::start(1 << 20, false, None).await;
    assert_eq!(
        upload(&h, "patched", &destination(&OLD), b"x").await.status,
        200
    );
    h.request(
        "PATCH",
        "/storage/v1/b/bucket/o/patched",
        &destination(&NEW),
        br#"{"metadata":{"k":"wrong"}}"#,
    )
    .await
    .error(400, "customerEncryptionKeyIsIncorrect");
    let unkeyed = h
        .request(
            "PATCH",
            "/storage/v1/b/bucket/o/patched",
            "",
            br#"{"metadata":{"k":"v"}}"#,
        )
        .await;
    assert_eq!(unkeyed.status, 200);
    assert_eq!(unkeyed.json()["metadata"], json!({"k":"v"}));
    assert!(unkeyed.json().get("md5Hash").is_none());
    let keyed = h
        .request(
            "PATCH",
            "/storage/v1/b/bucket/o/patched",
            &destination(&OLD),
            br#"{"contentType":"text/csv"}"#,
        )
        .await
        .json();
    assert_eq!(keyed["metageneration"], "3");
    assert!(keyed["md5Hash"].is_string());
    assert_eq!(keyed["customerEncryption"]["keySha256"], sha256(&OLD));
    h.stop().await;
}

#[tokio::test]
async fn csek_copy_rewrite_and_compose_use_source_and_destination_keys() {
    let h = Harness::start(1 << 20, false, None).await;
    assert_eq!(upload(&h, "a", &destination(&OLD), b"aa").await.status, 200);
    assert_eq!(upload(&h, "b", &destination(&OLD), b"bb").await.status, 200);
    assert_eq!(upload(&h, "p", "", b"pp").await.status, 200);

    let copy = |name: &'static str, headers: String| {
        let h = &h;
        async move {
            h.request(
                "POST",
                &format!("/storage/v1/b/bucket/o/a/copyTo/b/bucket/o/{name}"),
                &headers,
                b"",
            )
            .await
        }
    };
    copy("c", String::new())
        .await
        .error(400, "resourceIsEncryptedWithCustomerEncryptionKey");
    copy("c", source(&NEW))
        .await
        .error(400, "customerEncryptionKeyIsIncorrect");
    let decrypted = copy("c", source(&OLD)).await;
    assert_eq!(decrypted.status, 200, "{}", decrypted.json());
    assert!(decrypted.json().get("customerEncryption").is_none());
    assert_eq!(media(&h, "c", "").await.body, b"aa");
    let reencrypted = copy("d", format!("{}{}", source(&OLD), destination(&NEW))).await;
    assert_eq!(
        reencrypted.json()["customerEncryption"]["keySha256"],
        sha256(&NEW)
    );
    assert_eq!(media(&h, "d", &destination(&NEW)).await.body, b"aa");

    // Key rotation: rewrite an object onto itself with a new destination key.
    let rotate = |headers: String| {
        let h = &h;
        async move {
            h.request(
                "POST",
                "/storage/v1/b/bucket/o/a/rewriteTo/b/bucket/o/a",
                &headers,
                b"",
            )
            .await
        }
    };
    rotate(destination(&NEW))
        .await
        .error(400, "resourceIsEncryptedWithCustomerEncryptionKey");
    let rotated = rotate(format!("{}{}", source(&OLD), destination(&NEW))).await;
    assert_eq!(rotated.status, 200, "{}", rotated.json());
    assert_eq!(rotated.json()["done"], true);
    assert_eq!(
        rotated.json()["resource"]["customerEncryption"]["keySha256"],
        sha256(&NEW)
    );
    media(&h, "a", &destination(&OLD))
        .await
        .error(400, "customerEncryptionKeyIsIncorrect");
    assert_eq!(media(&h, "a", &destination(&NEW)).await.body, b"aa");

    let compose = |sources: serde_json::Value, headers: String| {
        let h = &h;
        async move {
            h.request(
                "POST",
                "/storage/v1/b/bucket/o/composed/compose",
                &format!("Content-Type: application/json\r\n{headers}"),
                json!({"sourceObjects": sources}).to_string().as_bytes(),
            )
            .await
        }
    };
    compose(json!([{"name":"b"},{"name":"b"}]), String::new())
        .await
        .error(400, "resourceIsEncryptedWithCustomerEncryptionKey");
    // Components must share the request key: "a" now uses NEW, "b" still uses OLD.
    compose(json!([{"name":"a"},{"name":"b"}]), destination(&OLD))
        .await
        .error(400, "customerEncryptionKeyIsIncorrect");
    compose(json!([{"name":"b"},{"name":"p"}]), destination(&OLD))
        .await
        .error(400, "resourceNotEncryptedWithCustomerEncryptionKey");
    let composed = compose(json!([{"name":"b"},{"name":"b"}]), destination(&OLD)).await;
    assert_eq!(composed.status, 200, "{}", composed.json());
    assert_eq!(
        composed.json()["customerEncryption"]["keySha256"],
        sha256(&OLD)
    );
    assert!(composed.json()["crc32c"].is_string());
    assert_eq!(
        media(&h, "composed", &destination(&OLD)).await.body,
        b"bbbb"
    );
    h.stop().await;
}

#[tokio::test]
async fn csek_xml_put_get_and_head() {
    let h = Harness::start(1 << 20, false, None).await;
    let put = h
        .request("PUT", "/bucket/xml", &destination(&OLD), b"xml data")
        .await;
    assert_eq!(put.status, 200, "{}", String::from_utf8_lossy(&put.body));
    assert_eq!(put.headers["x-goog-encryption-key-sha256"], sha256(&OLD));
    xml_code(
        &h.request("GET", "/bucket/xml", "", b"").await,
        400,
        "ResourceIsEncryptedWithCustomerEncryptionKey",
    );
    xml_code(
        &h.request("GET", "/bucket/xml", &destination(&NEW), b"")
            .await,
        400,
        "CustomerEncryptionKeyIsIncorrect",
    );
    let read = h
        .request("GET", "/bucket/xml", &destination(&OLD), b"")
        .await;
    assert_eq!(read.status, 200);
    assert_eq!(read.body, b"xml data");
    assert!(read.headers.contains_key("x-goog-hash"));
    // HEAD is a metadata read: allowed without the key, but without plaintext hashes.
    // The harness frames by Content-Length, so a closing connection ends the bodiless reply.
    let head = h
        .request("HEAD", "/bucket/xml", "Connection: close\r\n", b"")
        .await;
    assert_eq!(head.status, 200);
    assert!(!head.headers.contains_key("x-goog-hash"));
    assert_eq!(head.headers["x-goog-encryption-algorithm"], "AES256");
    let mut invalid = destination(&OLD);
    invalid = invalid.replace("AES256", "AES128");
    xml_code(
        &h.request("PUT", "/bucket/bad", &invalid, b"x").await,
        400,
        "CustomerEncryptionAlgorithmIsInvalid",
    );
    // JSON metadata of the XML-written object reports the same fingerprint.
    let metadata = h
        .request("GET", "/storage/v1/b/bucket/o/xml", "", b"")
        .await
        .json();
    assert_eq!(metadata["customerEncryption"]["keySha256"], sha256(&OLD));
    h.stop().await;
}
