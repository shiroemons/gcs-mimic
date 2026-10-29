use super::*;

#[tokio::test]
async fn upload_replay_never_crosses_authentication_profiles_after_restart() {
    let h = Harness::start(1000, false, Some("http://example.invalid")).await;
    let path = "/upload/storage/v1/b/bucket/o?uploadType=media&name=profile&ifGenerationMatch=0";
    let original = h
        .request(
            "POST",
            path,
            "X-Goog-Gcs-Idempotency-Token: profile\r\n",
            b"original",
        )
        .await;
    assert_eq!(original.status, 200);
    h.state.catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&h.dir.0, h.clock.clone())
        .await
        .unwrap();
    let auth = Auth::strict(Registry::parse(br#"{"identities":[{"id":"permissive","tokens":["strict"],"project_roles":{"project":["roles/storage.objectCreator"]}}]}"#).unwrap());
    let mut headers = hyper::HeaderMap::new();
    headers.insert("authorization", "Bearer strict".parse().unwrap());
    let identity = auth
        .authenticate(&headers, None)
        .unwrap()
        .unwrap()
        .principal()
        .unwrap()
        .clone();
    let access = crate::bucket::BucketAccess::Policy {
        identity: Some(identity),
        permission: crate::auth::Permission::ObjectsCreate,
        project: None,
    };
    let mut permit = catalog.admit_object_upload().unwrap();
    let result = catalog
        .check_upload_replay(
            "bucket",
            "profile",
            crate::object::ObjectConditions {
                if_generation_match: Some(0),
                ..Default::default()
            },
            &access,
            Some("profile"),
            &format!("{path}|{}|None", h.base),
            &mut permit,
        )
        .await;
    assert!(matches!(
        result,
        Err(crate::object::ObjectError::InvalidFixedMetadata(_))
    ));
    let (record, mut reader) = catalog
        .open_object_authorized(
            "bucket",
            "profile",
            None,
            Default::default(),
            &crate::bucket::BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(
        record.generation.to_string(),
        original.json()["generation"].as_str().unwrap()
    );
    assert_eq!(
        reader.next_chunk().await.unwrap().unwrap().as_ref(),
        b"original"
    );
    assert!(reader.next_chunk().await.unwrap().is_none());
    drop(permit);
    catalog.close().await.unwrap();
    h.stop().await;
}

#[tokio::test]
async fn upload_replay_capacity_expiry_and_current_permissions() {
    let h = Harness::start(1000, true, Some("http://example.invalid")).await;
    let path = "/upload/storage/v1/b/bucket/o?uploadType=media&name=replay";
    let headers = "Authorization: Bearer creator\r\nX-Goog-Gcs-Idempotency-Token: creator\r\n";
    let original = h.request("POST", path, headers, b"original").await;
    assert_eq!(original.status, 200);
    assert_eq!(
        h.request("POST", path, headers, b"original").await.body,
        original.body
    );
    h.request(
        "POST",
        path,
        "Authorization: Bearer viewer\r\nX-Goog-Gcs-Idempotency-Token: creator\r\n",
        b"original",
    )
    .await
    .error(403, "forbidden");
    h.request(
        "POST",
        path,
        "Authorization: Bearer admin\r\nX-Goog-Gcs-Idempotency-Token: creator\r\n",
        b"original",
    )
    .await
    .error(400, "invalidArgument");
    let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("WITH RECURSIVE entries(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM entries WHERE n < 1023) INSERT INTO upload_replays SELECT CAST(printf('%032d', n) AS BLOB), request, incarnation, created, deadline, fingerprint, completion FROM entries CROSS JOIN (SELECT * FROM upload_replays LIMIT 1)").unwrap();
    drop(connection);
    assert_eq!(
        h.request("POST", path, headers, b"original").await.body,
        original.body
    );
    let next = "Authorization: Bearer admin\r\nX-Goog-Gcs-Idempotency-Token: next\r\n";
    h.request("POST", path, next, b"new")
        .await
        .error(503, "backendError");
    assert_eq!(
        h.request(
            "GET",
            "/storage/v1/b/bucket/o/replay?alt=media",
            "Authorization: Bearer admin\r\n",
            b""
        )
        .await
        .body,
        b"original"
    );
    *h.clock.0.lock().unwrap() += Duration::from_secs(600);
    let replacement = h.request("POST", path, next, b"new").await;
    assert_eq!(replacement.status, 200);
    assert_ne!(
        replacement.json()["generation"],
        original.json()["generation"]
    );
    let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM upload_replays", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(connection);
    *h.clock.0.lock().unwrap() += Duration::from_secs(600);
    let expired_retry = h.request("POST", path, next, b"new").await;
    assert_eq!(expired_retry.status, 200);
    assert_ne!(
        expired_retry.json()["generation"],
        replacement.json()["generation"]
    );
    h.stop().await;
}

#[tokio::test]
async fn upload_replay_full_projection_retains_acl_after_mutation_and_deletion() {
    let h = Harness::start(1000, false, None).await;
    let path = "/upload/storage/v1/b/bucket/o?uploadType=media&name=acl&projection=full&fields=name,generation,acl,owner";
    let token = "X-Goog-Gcs-Idempotency-Token: acl\r\n";
    let original = h.request("POST", path, token, b"data").await;
    assert_eq!(original.status, 200);
    assert!(original.json()["acl"].is_array());
    let changed = h
        .request(
            "POST",
            "/storage/v1/b/bucket/o/acl/acl",
            "Content-Type: application/json\r\n",
            br#"{"entity":"allUsers","role":"READER"}"#,
        )
        .await;
    assert_eq!(changed.status, 200);
    assert_eq!(
        h.request("POST", path, token, b"data").await.body,
        original.body
    );
    assert_eq!(
        h.request("DELETE", "/storage/v1/b/bucket/o/acl", "", b"")
            .await
            .status,
        204
    );
    assert_eq!(
        h.request("POST", path, token, b"data").await.body,
        original.body
    );
    h.request("GET", "/storage/v1/b/bucket/o/acl", "", b"")
        .await
        .error(404, "notFound");
    h.stop().await;
}

#[tokio::test]
async fn upload_replay_full_projection_rechecks_revoked_acl_permission() {
    let h = Harness::start(1000, true, None).await;
    let admin = "Authorization: Bearer admin\r\nContent-Type: application/json\r\n";
    assert_eq!(h.request("PUT", "/storage/v1/b/bucket/iam", admin, br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["user:viewer@example.test"]}]}"#).await.status, 200);
    let path = "/upload/storage/v1/b/bucket/o?uploadType=media&name=revoked&projection=full";
    let headers = "Authorization: Bearer viewer\r\nX-Goog-Gcs-Idempotency-Token: revoke\r\n";
    let original = h.request("POST", path, headers, b"data").await;
    assert_eq!(original.status, 200);
    assert!(original.json()["acl"].is_array());
    assert_eq!(h.request("PUT", "/storage/v1/b/bucket/iam", admin, br#"{"bindings":[{"role":"roles/storage.objectCreator","members":["user:viewer@example.test"]}]}"#).await.status, 200);
    h.request("POST", path, headers, b"data")
        .await
        .error(403, "forbidden");
    assert_eq!(
        h.request(
            "GET",
            "/storage/v1/b/bucket/o/revoked",
            "Authorization: Bearer admin\r\n",
            b""
        )
        .await
        .json()["generation"],
        original.json()["generation"]
    );
    h.stop().await;
}

#[tokio::test]
async fn python_upload_content_type_is_advisory_after_initiation() {
    let h = Harness::start(1000, false, None).await;
    let init = h.initiate("name=python", "Content-Type: application/json\r\nX-Upload-Content-Type: text/advisory\r\nX-Upload-Content-Length: 3\r\n", br#"{"contentType":"text/metadata"}"#).await;
    assert_eq!(init.status, 200);
    let reply = h
        .request(
            "PUT",
            &h.path(&init),
            "Content-Type: text/chunk\r\nX-Upload-Content-Type: text/repeated\r\n",
            b"abc",
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["contentType"], "text/metadata");
    assert_eq!(reply.json()["size"], "3");
    h.stop().await;
}

#[tokio::test]
async fn php_chunk_hashes_persist_and_validate_the_whole_object() {
    for hashes in [
        "md5=rXaC3/oyxEygvmW1+8ETpQ==,crc32c=srP9bA==",
        "md5=AAAAAAAAAAAAAAAAAAAAAA==,crc32c=srP9bA==",
        "md5=rXaC3/oyxEygvmW1+8ETpQ==,crc32c=AAAAAA==",
    ] {
        let h = Harness::start(1024 * 1024, false, None).await;
        let old = h
            .request(
                "POST",
                "/upload/storage/v1/b/bucket/o?uploadType=media&name=php",
                "",
                b"old",
            )
            .await
            .json();
        let init = h.initiate("name=php", "", b"").await;
        let path = h.path(&init);
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes */*\r\nX-Goog-Hash: crc32c=srP9bA==\r\n",
            b"",
        )
        .await
        .error(400, "invalidParameter");
        let header = format!("Content-Range: bytes 0-262143/262145\r\nX-Goog-Hash: {hashes}\r\n");
        assert_eq!(
            h.request("PUT", &path, &header, &vec![0; 262144])
                .await
                .status,
            308
        );
        assert_eq!(
            h.request("PUT", &path, &header, &vec![0; 262144])
                .await
                .status,
            308
        );
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 0-262143/262145\r\nX-Goog-Hash: md5=/////////////////////w==\r\n",
            &vec![0; 262144],
        )
        .await
        .error(400, "invalidParameter");
        let id = path.split("upload_id=").nth(1).unwrap();
        let persisted = h
            .state
            .catalog
            .resumable_session(id, "bucket")
            .await
            .unwrap();
        assert!(persisted.options.expected.md5.is_some());
        assert!(persisted.options.expected.crc32c.is_some());
        // Reopen the catalog to verify declarations survive without a final header.
        h.state.catalog.close().await.unwrap();
        let catalog = Catalog::open_with_clock(&h.dir.0, h.clock.clone())
            .await
            .unwrap();
        let lease = catalog.lock_resumable_session(id, "bucket").await.unwrap();
        let options = lease.session.options.clone();
        let (catalog_wait, _) = tokio::sync::watch::channel(false);
        let chunk = crate::storage::ResumableChunk {
            start: 262144,
            length: 1,
            total: Some(262145),
            finalize: true,
            options,
            catalog_wait,
        };
        let (sender, body) = tokio::sync::mpsc::channel(2);
        sender
            .send(Ok(crate::object::UploadChunk::Data(
                bytes::Bytes::from_static(&[0]),
            )))
            .await
            .unwrap();
        sender
            .send(Ok(crate::object::UploadChunk::End))
            .await
            .unwrap();
        drop(sender);
        let permit = catalog.admit_object_upload().unwrap();
        let result = catalog
            .write_resumable_session(lease, chunk, body, permit, |record| {
                Ok(json!({"generation":record.generation}))
            })
            .await;
        if hashes.starts_with("md5=rXa") && hashes.ends_with("srP9bA==") {
            assert!(result.unwrap().is_some());
        } else {
            assert!(matches!(
                result,
                Err(crate::storage::ResumableError::Object(
                    crate::object::ObjectError::ChecksumMismatch
                ))
            ));
            let record = catalog
                .get_object_authorized(
                    "bucket",
                    "php",
                    None,
                    Default::default(),
                    &crate::bucket::BucketAccess::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(
                record.generation.to_string(),
                old["generation"].as_str().unwrap()
            );
            let (_, mut reader) = catalog
                .open_object_authorized(
                    "bucket",
                    "php",
                    None,
                    Default::default(),
                    &crate::bucket::BucketAccess::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(reader.next_chunk().await.unwrap().unwrap().as_ref(), b"old");
            assert!(reader.next_chunk().await.unwrap().is_none());
        }
        catalog.close().await.unwrap();
        h.stop().await;
    }
}
