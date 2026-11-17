use super::AuthProfile::{Permissive, Strict};
use super::{Harness, Reply};
use crate::object::{ObjectError, unix_nanos::UnixNanos};
use crate::storage::test_fixtures::{PERMISSIVE, live};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

async fn call(h: &Harness, method: &str, path: &str, body: Value) -> Reply {
    h.request(method, path, ADMIN, body.to_string().as_bytes())
        .await
}

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

async fn upload(h: &Harness, name: &str) {
    let reply = h
        .request(
            "POST",
            &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
            ADMIN,
            b"data",
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.json());
}

/// Sends a metadata update of `name` and returns the updated resource.
async fn update(h: &Harness, method: &str, name: &str, body: Value) -> Value {
    let reply = call(h, method, &format!("/storage/v1/b/bucket/o/{name}"), body).await;
    assert_eq!(reply.status, 200, "{}", reply.json());
    reply.json()
}

/// A delete and an overwrite of `name` are both refused until `until`, which the denial names.
async fn assert_retained_until(h: &Harness, name: &str, until: SystemTime) {
    let expected = format!(
        "Object 'bucket/{name}' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until {}",
        crate::object::retention::pacific_rfc3339(UnixNanos::from(until))
    );
    let deleted = h
        .request(
            "DELETE",
            &format!("/storage/v1/b/bucket/o/{name}"),
            ADMIN,
            b"",
        )
        .await;
    let overwritten = h
        .request(
            "POST",
            &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
            ADMIN,
            b"new",
        )
        .await;
    for reply in [deleted, overwritten] {
        reply.error(403, "retentionPolicyNotMet");
        assert_eq!(reply.json()["error"]["message"], expected);
    }
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
    let h = Harness::start(1024, Strict, None).await;
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
        .error(400, "invalid");
    }
    let bucket = policy(&h, json!({"retentionPeriod":"3600", "isLocked":true})).await;
    // A policy is reported unlocked by leaving `isLocked` out (recorded 2026-10-31).
    assert!(bucket["retentionPolicy"].get("isLocked").is_none());
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
    // A lock cannot be taken twice, even at the metageneration the first lock produced. The
    // reason is a local choice: the comparison harness refuses every lock request, so Cloud
    // Storage's answer is unrecorded (SPEC-RETENTION-POLICY).
    let relocked = locked.json()["metageneration"].as_str().unwrap().to_owned();
    h.request(
        "POST",
        &format!("/storage/v1/b/bucket/lockRetentionPolicy?ifMetagenerationMatch={relocked}"),
        ADMIN,
        b"",
    )
    .await
    .error(400, "invalid");
    // An empty policy member restates nothing (recorded 2026-10-31); the locked policy stays.
    let unchanged = call(
        &h,
        "PATCH",
        "/storage/v1/b/bucket",
        json!({"retentionPolicy":{}}),
    )
    .await;
    assert_eq!(unchanged.status, 200, "{}", unchanged.json());
    assert_eq!(unchanged.json()["retentionPolicy"]["isLocked"], true);
    for value in [Value::Null, json!({"retentionPeriod":"3599"})] {
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
    let preserved = call(&h, "PUT", "/storage/v1/b/bucket", json!({})).await;
    assert_eq!(preserved.status, 200);
    assert_eq!(
        preserved.json()["retentionPolicy"],
        locked.json()["retentionPolicy"]
    );
    call(
        &h,
        "PUT",
        "/storage/v1/b/bucket",
        json!({"retentionPolicy":null}),
    )
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
    let saved = reopened
        .get_bucket("bucket", PERMISSIVE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.retention_policy.as_ref().unwrap().retention_period,
        7200
    );
    assert!(saved.retention_policy.unwrap().is_locked);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn retention_insert_patch_put_defaults_and_empty_policy_lock() {
    let h = Harness::start(1024, Permissive, None).await;
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
    // A native PUT replaces the resource, so an omitted retention policy is cleared (recorded
    // 2026-10-31).
    let preserved = call(&h, "PUT", "/storage/v1/b/bucket", json!({})).await;
    assert_eq!(preserved.status, 200);
    assert!(preserved.json().get("retentionPolicy").is_none());
    let cleared = call(
        &h,
        "PUT",
        "/storage/v1/b/bucket",
        json!({"retentionPolicy":null}),
    )
    .await;
    assert_eq!(cleared.status, 200);
    assert!(cleared.json().get("retentionPolicy").is_none());
    for value in [Value::Null, json!({})] {
        assert!(policy(&h, value).await.get("retentionPolicy").is_none());
    }
    h.stop().await;
}

/// Cloud Storage reported a new policy's effectiveTime about 60 seconds after the request in its
/// one observation (2026-10-31), yet enforced the policy at once. gcs-mimic reports the request
/// time, so the time it shows is the time protection starts (SPEC-RESPONSE-WIRE-REMAINING).
#[tokio::test]
async fn a_retention_policy_takes_effect_at_its_request_time() {
    let h = Harness::start(1024, Permissive, None).await;
    let effective_time = |bucket: Value| bucket["retentionPolicy"]["effectiveTime"].clone();
    h.clock.set(UNIX_EPOCH + Duration::from_secs(1000));
    let set = policy(&h, json!({"retentionPeriod":"3600"})).await;
    assert_eq!(effective_time(set), "1970-01-01T00:16:40.000Z");
    // Restating the period keeps the time; a new period starts from its own request.
    h.clock.set(UNIX_EPOCH + Duration::from_secs(1010));
    let restated = policy(&h, json!({"retentionPeriod":"3600"})).await;
    assert_eq!(effective_time(restated), "1970-01-01T00:16:40.000Z");
    let extended = policy(&h, json!({"retentionPeriod":"7200"})).await;
    assert_eq!(effective_time(extended), "1970-01-01T00:16:50.000Z");
    h.stop().await;
}

#[tokio::test]
async fn retention_all_mutation_paths_expire_with_injected_clock() {
    let h = Harness::start(1024, Permissive, None).await;
    h.clock.set(UNIX_EPOCH + Duration::from_secs(1000));
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
    h.clock.set(UNIX_EPOCH + Duration::from_secs(1010));
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
    let h = Harness::start(1024, Permissive, None).await;
    h.clock.set(UNIX_EPOCH + Duration::from_secs(1000));
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
            .error(403, "forbidden");
        call(&h, "PATCH", &path, json!({field:false})).await;
        h.request("DELETE", &path, ADMIN, b"")
            .await
            .error(403, "retentionPolicyNotMet");
        call(&h, "PATCH", &path, json!({field:true})).await;
        h.clock.set(UNIX_EPOCH + Duration::from_secs(1010));
        h.request("DELETE", &path, ADMIN, b"")
            .await
            .error(403, "forbidden");
        if field == "temporaryHold" {
            call(&h, "PATCH", &path, json!({field:false})).await;
            assert_eq!(h.request("DELETE", &path, ADMIN, b"").await.status, 204);
        }
        h.clock.set(UNIX_EPOCH + Duration::from_secs(1000));
    }
    h.stop().await;
}

/// "Retention duration of the objects begins from the moment event-based hold transitioned from
/// true to false" (Objects resource, `eventBasedHold`), so an object created long before the
/// period is retained again once its hold is released. While the hold is held the deadline is
/// unknown and omitted (`retention_expire_time` in the pinned v2 and v1 protos). The release
/// survives a restart.
#[tokio::test]
async fn releasing_an_event_based_hold_restarts_the_bucket_retention_period() {
    let h = Harness::start(1024, Permissive, None).await;
    h.clock.set(at(1000));
    policy(&h, json!({"retentionPeriod":"10"})).await;
    upload(&h, "held").await;
    let held = update(&h, "PATCH", "held", json!({"eventBasedHold":true})).await;
    assert!(held.get("retentionExpirationTime").is_none(), "{held}");
    h.clock.set(at(2000));
    let released = update(&h, "PATCH", "held", json!({"eventBasedHold":false})).await;
    assert_eq!(released["eventBasedHold"], false);
    assert_eq!(
        released["retentionExpirationTime"],
        "1970-01-01T00:33:30.000Z"
    );
    assert_retained_until(&h, "held", at(2010)).await;
    // Restating the released hold is not another release.
    h.clock.set(at(2005));
    let restated = update(&h, "PATCH", "held", json!({"eventBasedHold":false})).await;
    assert_eq!(
        restated["retentionExpirationTime"],
        "1970-01-01T00:33:30.000Z"
    );
    h.clock.set(at(2010) - Duration::from_micros(1));
    assert_retained_until(&h, "held", at(2010)).await;
    let dir = h.dir.0.clone();
    let clock = h.clock.clone();
    h.stop.send(()).unwrap();
    h.server.await.unwrap().unwrap();
    h.state.catalog.close().await.unwrap();
    let reopened = crate::storage::Catalog::open_with_clock(&dir, clock.clone())
        .await
        .unwrap();
    let object = reopened
        .get_object(live("bucket", "held"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(object.retention_expiration_time, Some(at(2010)));
    let refused = reopened
        .delete_object(live("bucket", "held"), Default::default(), PERMISSIVE)
        .await;
    assert!(
        matches!(refused, Err(ObjectError::ObjectRetentionNotMet(_))),
        "{refused:?}"
    );
    clock.set(at(2010));
    reopened
        .delete_object(live("bucket", "held"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    reopened.close().await.unwrap();
}

/// A PUT releases the holds it does not name, so dropping a held event-based hold restarts the
/// period like sending false does. Releasing a hold that was never held, or a temporary hold,
/// leaves the period counting from creation.
#[tokio::test]
async fn only_releasing_a_held_event_based_hold_restarts_the_period() {
    let h = Harness::start(1024, Permissive, None).await;
    h.clock.set(at(1000));
    policy(&h, json!({"retentionPeriod":"10"})).await;
    for name in ["put-omits", "put-false", "never-held", "temporary"] {
        upload(&h, name).await;
    }
    for name in ["put-omits", "put-false"] {
        update(&h, "PATCH", name, json!({"eventBasedHold":true})).await;
    }
    update(&h, "PATCH", "temporary", json!({"temporaryHold":true})).await;
    h.clock.set(at(2000));
    for (name, body) in [
        ("put-omits", json!({})),
        ("put-false", json!({"eventBasedHold":false})),
    ] {
        let replaced = update(&h, "PUT", name, body).await;
        assert_eq!(
            replaced["retentionExpirationTime"], "1970-01-01T00:33:30.000Z",
            "{name}"
        );
        assert_retained_until(&h, name, at(2010)).await;
    }
    for (name, body) in [
        ("never-held", json!({"eventBasedHold":false})),
        ("temporary", json!({"temporaryHold":false})),
    ] {
        let released = update(&h, "PATCH", name, body).await;
        assert_eq!(
            released["retentionExpirationTime"], "1970-01-01T00:16:50.000Z",
            "{name}"
        );
        let deleted = h
            .request(
                "DELETE",
                &format!("/storage/v1/b/bucket/o/{name}"),
                ADMIN,
                b"",
            )
            .await;
        assert_eq!(deleted.status, 204, "{name}");
    }
    h.stop().await;
}
