use super::{Harness, Reply};
use serde_json::{Value, json};
use std::time::{Duration, UNIX_EPOCH};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

async fn call(h: &Harness, method: &str, path: &str, body: Value) -> Reply {
    h.request(method, path, ADMIN, body.to_string().as_bytes())
        .await
}

async fn policy(h: &Harness, value: Value) -> Value {
    let response = call(
        h,
        "PATCH",
        "/storage/v1/b/bucket",
        json!({"retentionPolicy":value}),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.json());
    response.json()
}

#[tokio::test]
async fn retention_configuration_lock_permissions_and_restart() {
    let h = Harness::start(1024, true, None).await;
    for period in [
        json!("0"),
        json!("3155760000"),
        json!("-1"),
        json!(1.5),
        json!(true),
    ] {
        call(
            &h,
            "PATCH",
            "/storage/v1/b/bucket",
            json!({"retentionPolicy":{"retentionPeriod":period}}),
        )
        .await
        .error(400, "invalidArgument");
    }
    let bucket = policy(&h, json!({"retentionPeriod":"3600", "isLocked":true})).await;
    assert_eq!(bucket["retentionPolicy"]["isLocked"], false);
    assert!(bucket["retentionPolicy"]["effectiveTime"].is_string());
    let fetched = h
        .request("GET", "/storage/v1/b/bucket", ADMIN, b"")
        .await
        .json();
    assert_eq!(fetched["retentionPolicy"], bucket["retentionPolicy"]);
    let listed = h
        .request("GET", "/storage/v1/b?project=project", ADMIN, b"")
        .await
        .json();
    assert_eq!(
        listed["items"][0]["retentionPolicy"],
        bucket["retentionPolicy"]
    );
    for suffix in ["", "/lockRetentionPolicy?ifMetagenerationMatch=2"] {
        h.request(
            if suffix.is_empty() { "PATCH" } else { "POST" },
            &format!("/storage/v1/b/bucket{suffix}"),
            "Authorization: Bearer viewer\r\n",
            if suffix.is_empty() {
                br#"{"retentionPolicy":null}"#
            } else {
                b""
            },
        )
        .await
        .error(403, "forbidden");
    }
    h.request(
        "POST",
        "/storage/v1/b/bucket/lockRetentionPolicy",
        ADMIN,
        b"",
    )
    .await
    .error(400, "required");
    h.request(
        "POST",
        "/storage/v1/b/bucket/lockRetentionPolicy?ifMetagenerationMatch=1",
        ADMIN,
        b"",
    )
    .await
    .error(412, "conditionNotMet");
    let generation = bucket["metageneration"].as_str().unwrap();
    h.request(
        "POST",
        &format!("/storage/v1/b/bucket/lockRetentionPolicy?ifMetagenerationMatch={generation}"),
        "Authorization: Bearer admin\r\nContent-Type: application/x-www-form-urlencoded\r\n",
        b"ifMetagenerationMatch=999",
    )
    .await
    .error(400, "invalidArgument");
    let locked = h
        .request(
            "POST",
            &format!("/storage/v1/b/bucket/lockRetentionPolicy?ifMetagenerationMatch={generation}"),
            "Authorization: Bearer admin\r\nContent-Type: application/x-www-form-urlencoded; charset=UTF-8\r\n",
            format!("ifMetagenerationMatch={generation}").as_bytes(),
        )
        .await;
    assert_eq!(locked.status, 200, "{}", locked.json());
    assert_eq!(locked.json()["retentionPolicy"]["isLocked"], true);
    for value in [Value::Null, json!({}), json!({"retentionPeriod":"3599"})] {
        let error = call(
            &h,
            "PATCH",
            "/storage/v1/b/bucket",
            json!({"retentionPolicy":value}),
        )
        .await;
        error.error(400, "badRequestException");
        let message = error.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            message.contains("locked") && message.contains("extend") && message.contains("3601"),
            "{message}"
        );
    }
    call(&h, "PUT", "/storage/v1/b/bucket", json!({}))
        .await
        .error(400, "badRequestException");
    h.request(
        "PATCH",
        "/storage/v1/b/bucket",
        "Authorization: Bearer viewer\r\n",
        br#"{"retentionPolicy":null}"#,
    )
    .await
    .error(403, "forbidden");
    let extended = policy(&h, json!({"retentionPeriod":"7200"})).await;
    assert_eq!(extended["retentionPolicy"]["isLocked"], true);
    let dir = h.dir.0.clone();
    let clock = h.clock.clone();
    // Stop the listener before reopening the same persistent catalog.
    h.stop.send(()).unwrap();
    h.server.await.unwrap().unwrap();
    h.state.catalog.close().await.unwrap();
    let reopened = crate::storage::Catalog::open_with_clock(&dir, clock)
        .await
        .unwrap();
    let saved = reopened.get_bucket("bucket").await.unwrap().unwrap();
    assert_eq!(
        saved.retention_policy.as_ref().unwrap().retention_period,
        7200
    );
    assert!(saved.retention_policy.unwrap().is_locked);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn retention_insert_patch_put_defaults_and_empty_policy_lock() {
    let h = Harness::start(1024, false, None).await;
    h.request(
        "POST",
        "/storage/v1/b/bucket/lockRetentionPolicy?ifMetagenerationMatch=1",
        ADMIN,
        b"",
    )
    .await
    .error(400, "badRequest");
    let created = call(
        &h,
        "POST",
        "/storage/v1/b?project=project",
        json!({"name":"retention-insert","retentionPolicy":{"retentionPeriod":"3155759999"}}),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.json());
    assert_eq!(
        created.json()["retentionPolicy"]["retentionPeriod"],
        "3155759999"
    );
    policy(&h, json!({"retentionPeriod":"3600"})).await;
    let cleared = policy(&h, Value::Null).await;
    assert!(cleared.get("retentionPolicy").is_none());
    policy(&h, json!({"retentionPeriod":"3600"})).await;
    let replaced = call(
        &h,
        "PUT",
        "/storage/v1/b/bucket",
        json!({"retentionPolicy":{"retentionPeriod":"7200"}}),
    )
    .await;
    assert_eq!(
        replaced.json()["retentionPolicy"]["retentionPeriod"],
        "7200"
    );
    let cleared = call(&h, "PUT", "/storage/v1/b/bucket", json!({})).await;
    assert_eq!(cleared.status, 200);
    assert!(cleared.json().get("retentionPolicy").is_none());
    for value in [Value::Null, json!({})] {
        assert!(policy(&h, value).await.get("retentionPolicy").is_none());
    }
    h.stop().await;
}

#[tokio::test]
async fn retention_all_mutation_paths_expire_with_injected_clock() {
    let h = Harness::start(1024, false, None).await;
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1000);
    for name in [
        "upload", "copy", "rewrite", "compose", "move", "delete", "xml", "source",
    ] {
        assert_eq!(
            h.request(
                "POST",
                &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
                ADMIN,
                b"old"
            )
            .await
            .status,
            200
        );
    }
    policy(&h, json!({"retentionPeriod":"10"})).await;
    let metadata = h
        .request("GET", "/storage/v1/b/bucket/o/source", ADMIN, b"")
        .await
        .json();
    assert_eq!(
        metadata["retentionExpirationTime"],
        "1970-01-01T00:16:50.000Z"
    );
    let operations = [
        (
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=upload",
            json!(null),
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/source/copyTo/b/bucket/o/copy",
            json!({}),
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/rewrite",
            json!({}),
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/compose/compose",
            json!({"sourceObjects":[{"name":"source"}]}),
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/move/moveTo/o/moved",
            json!({}),
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/deleted-sources/compose",
            json!({"sourceObjects":[{"name":"source"}],"deleteSourceObjects":true}),
        ),
        ("DELETE", "/storage/v1/b/bucket/o/delete", Value::Null),
    ];
    for (method, path, body) in &operations {
        let bytes = if *method == "DELETE" || path.contains("/moveTo/") {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        h.request(method, path, ADMIN, &bytes)
            .await
            .error(403, "retentionPolicyNotMet");
    }
    let xml = h
        .request("PUT", "/bucket/xml", "Content-Type: text/plain\r\n", b"new")
        .await;
    assert_eq!(xml.status, 403, "{}", String::from_utf8_lossy(&xml.body));
    assert!(String::from_utf8_lossy(&xml.body).contains("RetentionPolicyNotMet"));
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1010);
    for (method, path, body) in &operations {
        let bytes = if *method == "DELETE" || path.contains("/moveTo/") {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        let reply = h.request(method, path, ADMIN, &bytes).await;
        assert!(
            (200..300).contains(&reply.status),
            "{path}: {}",
            String::from_utf8_lossy(&reply.body)
        );
    }
    assert_eq!(
        h.request("PUT", "/bucket/xml", "Content-Type: text/plain\r\n", b"new")
            .await
            .status,
        200
    );
    h.stop().await;
}

#[tokio::test]
async fn retention_and_both_holds_are_independent() {
    let h = Harness::start(1024, false, None).await;
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1000);
    for field in ["temporaryHold", "eventBasedHold"] {
        let name = format!("held-{field}");
        h.request(
            "POST",
            &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
            ADMIN,
            b"held",
        )
        .await;
        policy(&h, json!({"retentionPeriod":"10"})).await;
        let path = format!("/storage/v1/b/bucket/o/{name}");
        call(&h, "PATCH", &path, json!({field:true})).await;
        h.request("DELETE", &path, ADMIN, b"")
            .await
            .error(403, "objectUnderActiveHold");
        call(&h, "PATCH", &path, json!({field:false})).await;
        h.request("DELETE", &path, ADMIN, b"")
            .await
            .error(403, "retentionPolicyNotMet");
        call(&h, "PATCH", &path, json!({field:true})).await;
        *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1010);
        h.request("DELETE", &path, ADMIN, b"")
            .await
            .error(403, "objectUnderActiveHold");
        if field == "temporaryHold" {
            call(&h, "PATCH", &path, json!({field:false})).await;
            assert_eq!(h.request("DELETE", &path, ADMIN, b"").await.status, 204);
        }
        *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1000);
    }
    h.stop().await;
}
