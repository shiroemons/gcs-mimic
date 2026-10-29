//! Object Retention Lock wire contract over the strict JSON API with an injected clock.

use super::{Harness, Reply};
use serde_json::{Value, json};
use std::time::{Duration, UNIX_EPOCH};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const VIEWER: &str = "Authorization: Bearer viewer\r\n";
/// 2026-01-01T00:00:00Z.
const START: u64 = 1_767_225_600;
const HOUR: &str = "2026-01-01T01:00:00Z";
const TWO_HOURS: &str = "2026-01-01T02:00:00Z";
const HALF_HOUR: &str = "2026-01-01T00:30:00Z";

fn set_clock(h: &Harness, seconds: u64) {
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(seconds);
}

fn retention(mode: &str, until: &str) -> Value {
    json!({"mode": mode, "retainUntilTime": until})
}

async fn create_bucket(h: &Harness, name: &str, extra: Value) -> Value {
    let mut body = json!({"name": name});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let reply = h
        .request(
            "POST",
            "/storage/v1/b?project=project&enableObjectRetention=true",
            ADMIN,
            body.to_string().as_bytes(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.json());
    reply.json()
}

async fn multipart(h: &Harness, bucket: &str, name: &str, metadata: Value, auth: &str) -> Reply {
    let body = format!(
        "--b\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--b\r\nContent-Type: text/plain\r\n\r\ndata\r\n--b--\r\n"
    );
    h.request(
        "POST",
        &format!("/upload/storage/v1/b/{bucket}/o?uploadType=multipart&name={name}"),
        &format!("{auth}Content-Type: multipart/related; boundary=b\r\n"),
        body.as_bytes(),
    )
    .await
}

async fn media(h: &Harness, bucket: &str, name: &str) -> Reply {
    h.request(
        "POST",
        &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
        ADMIN,
        b"replacement",
    )
    .await
}

async fn patch(h: &Harness, path: &str, query: &str, body: Value, auth: &str) -> Reply {
    h.request(
        "PATCH",
        &format!("/storage/v1/b/{path}{query}"),
        auth,
        body.to_string().as_bytes(),
    )
    .await
}

async fn delete(h: &Harness, path: &str) -> Reply {
    h.request("DELETE", &format!("/storage/v1/b/{path}"), ADMIN, b"")
        .await
}

#[tokio::test]
async fn object_retention_bucket_enablement_is_create_only_and_persists() {
    let h = Harness::start(1024, true, None).await;
    set_clock(&h, START);
    let created = create_bucket(&h, "locked-bucket", json!({})).await;
    assert_eq!(created["objectRetention"], json!({"mode":"Enabled"}));
    // An echoed output value is accepted only together with the query parameter.
    let echoed = create_bucket(
        &h,
        "echoed-bucket",
        json!({"objectRetention":{"mode":"Enabled"}}),
    )
    .await;
    assert_eq!(echoed["objectRetention"], json!({"mode":"Enabled"}));
    for (query, body) in [
        (
            "",
            json!({"name":"plain","objectRetention":{"mode":"Enabled"}}),
        ),
        (
            "&enableObjectRetention=true",
            json!({"name":"plain","objectRetention":{"mode":"Disabled"}}),
        ),
    ] {
        h.request(
            "POST",
            &format!("/storage/v1/b?project=project{query}"),
            ADMIN,
            body.to_string().as_bytes(),
        )
        .await
        .error(400, "invalidArgument");
    }
    h.request("GET", "/storage/v1/b/plain", ADMIN, b"")
        .await
        .error(404, "notFound");
    let fetched = h
        .request("GET", "/storage/v1/b/locked-bucket", ADMIN, b"")
        .await
        .json();
    assert_eq!(fetched["objectRetention"], json!({"mode":"Enabled"}));
    let plain = h.request("GET", "/storage/v1/b/bucket", ADMIN, b"").await;
    assert!(plain.json().get("objectRetention").is_none());
    // Output echoes are accepted when they match; enabling later or disabling is rejected.
    for (method, body) in [
        (
            "PATCH",
            json!({"objectRetention":{"mode":"Enabled"},"labels":{"a":"b"}}),
        ),
        ("PATCH", json!({"objectRetention":null})),
        ("PUT", json!({"objectRetention":{}})),
    ] {
        let reply = h
            .request(
                method,
                "/storage/v1/b/locked-bucket",
                ADMIN,
                body.to_string().as_bytes(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.json());
        assert_eq!(reply.json()["objectRetention"], json!({"mode":"Enabled"}));
    }
    let before = h
        .request("GET", "/storage/v1/b/bucket", ADMIN, b"")
        .await
        .json();
    for body in [
        json!({"objectRetention":{"mode":"Enabled"}}),
        json!({"objectRetention":{"mode":"Disabled"}}),
        json!({"objectRetention":true}),
    ] {
        patch(&h, "bucket", "", body, ADMIN)
            .await
            .error(400, "invalidArgument");
    }
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket", ADMIN, b"")
            .await
            .json(),
        before
    );
    h.request(
        "POST",
        "/storage/v1/b?project=project&enableObjectRetention=true",
        VIEWER,
        br#"{"name":"viewer-bucket"}"#,
    )
    .await
    .error(403, "forbidden");
    let dir = h.dir.0.clone();
    let clock = h.clock.clone();
    h.stop.send(()).unwrap();
    h.server.await.unwrap().unwrap();
    h.state.catalog.close().await.unwrap();
    let reopened = crate::storage::Catalog::open_with_clock(&dir, clock)
        .await
        .unwrap();
    assert!(
        reopened
            .get_bucket("locked-bucket")
            .await
            .unwrap()
            .unwrap()
            .object_retention_enabled
    );
    assert!(
        !reopened
            .get_bucket("bucket")
            .await
            .unwrap()
            .unwrap()
            .object_retention_enabled
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn object_retention_schema_migration_and_restart() {
    let h = Harness::start(1024, true, None).await;
    set_clock(&h, START);
    create_bucket(&h, "locked", json!({})).await;
    let created = multipart(
        &h,
        "locked",
        "kept",
        json!({"retention": retention("Locked", HOUR)}),
        ADMIN,
    )
    .await
    .json();
    assert_eq!(
        multipart(&h, "bucket", "legacy", json!({}), ADMIN)
            .await
            .status,
        200
    );
    let dir = h.dir.0.clone();
    let clock = h.clock.clone();
    h.stop.send(()).unwrap();
    h.server.await.unwrap().unwrap();
    h.state.catalog.close().await.unwrap();
    let catalog = crate::storage::Catalog::open_with_clock(&dir, clock.clone())
        .await
        .unwrap();
    let admin = catalog.admin(crate::storage::admin::Origin::Seed);
    let name = |value: &str| crate::storage::admin::ObjectName::new(value).unwrap();
    let bucket = |value: &str| crate::storage::admin::BucketName::new(value).unwrap();
    let kept = admin
        .get_object(&bucket("locked"), &name("kept"), None)
        .await
        .unwrap();
    let retention = kept.retention.unwrap();
    assert_eq!(retention.mode.as_str(), "Locked");
    assert_eq!(
        retention.retain_until,
        UNIX_EPOCH + Duration::from_secs(START + 3600)
    );
    assert_eq!(created["generation"], kept.generation.to_string());
    catalog.close().await.unwrap();
    // Schema 28 catalogs gain the columns with disabled/absent defaults.
    let connection = rusqlite::Connection::open(dir.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_object_retention_schema(&connection);
    connection.pragma_update(None, "user_version", 28).unwrap();
    drop(connection);
    let catalog = crate::storage::Catalog::open_with_clock(&dir, clock)
        .await
        .unwrap();
    let admin = catalog.admin(crate::storage::admin::Origin::Seed);
    for (bucket_name, object) in [("locked", "kept"), ("bucket", "legacy")] {
        let record = admin
            .get_object(&bucket(bucket_name), &name(object), None)
            .await
            .unwrap();
        assert_eq!(record.retention, None);
        assert!(
            !catalog
                .get_bucket(bucket_name)
                .await
                .unwrap()
                .unwrap()
                .object_retention_enabled
        );
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn object_retention_mode_transitions_and_injected_clock_expiry() {
    let h = Harness::start(1024, true, None).await;
    set_clock(&h, START);
    create_bucket(&h, "locked", json!({})).await;
    let created = multipart(
        &h,
        "locked",
        "kept",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.json());
    let created = created.json();
    assert_eq!(
        created["retention"],
        json!({"mode":"Unlocked","retainUntilTime":"2026-01-01T01:00:00.000Z"})
    );
    assert_eq!(
        created["retentionExpirationTime"],
        "2026-01-01T01:00:00.000Z"
    );
    let fetched = h
        .request("GET", "/storage/v1/b/locked/o/kept", ADMIN, b"")
        .await
        .json();
    assert_eq!(fetched["retention"], created["retention"]);
    let listed = h
        .request("GET", "/storage/v1/b/locked/o", ADMIN, b"")
        .await
        .json();
    assert_eq!(listed["items"][0]["retention"], created["retention"]);
    // Retained objects reject deletion and replacement but allow metadata edits.
    delete(&h, "locked/o/kept")
        .await
        .error(403, "retentionPolicyNotMet");
    let generation = created["generation"].as_str().unwrap();
    delete(&h, &format!("locked/o/kept?generation={generation}"))
        .await
        .error(403, "retentionPolicyNotMet");
    media(&h, "locked", "kept")
        .await
        .error(403, "retentionPolicyNotMet");
    let edited = patch(
        &h,
        "locked/o/kept",
        "",
        json!({"metadata":{"k":"v"}}),
        ADMIN,
    )
    .await;
    assert_eq!(edited.status, 200);
    assert_eq!(edited.json()["retention"], created["retention"]);
    // Unlocked: extension is free; shortening, removal and locking need the override.
    for body in [
        json!({"retention": retention("Unlocked", HALF_HOUR)}),
        json!({"retention": null}),
        json!({"retention": {}}),
        json!({"retention": retention("Locked", TWO_HOURS)}),
    ] {
        patch(&h, "locked/o/kept", "", body.clone(), ADMIN)
            .await
            .error(400, "invalidArgument");
        patch(
            &h,
            "locked/o/kept",
            "?overrideUnlockedRetention=false",
            body,
            ADMIN,
        )
        .await
        .error(400, "invalidArgument");
    }
    let extended = patch(
        &h,
        "locked/o/kept",
        "",
        json!({"retention": retention("Unlocked", TWO_HOURS)}),
        ADMIN,
    )
    .await;
    assert_eq!(extended.status, 200);
    assert_eq!(
        extended.json()["retention"]["retainUntilTime"],
        "2026-01-01T02:00:00.000Z"
    );
    let shortened = patch(
        &h,
        "locked/o/kept",
        "?overrideUnlockedRetention=true",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    assert_eq!(shortened.status, 200);
    assert_eq!(
        shortened.json()["retention"]["retainUntilTime"],
        "2026-01-01T01:00:00.000Z"
    );
    let locked = patch(
        &h,
        "locked/o/kept",
        "?overrideUnlockedRetention=true",
        json!({"retention": retention("Locked", HOUR)}),
        ADMIN,
    )
    .await;
    assert_eq!(locked.status, 200);
    assert_eq!(locked.json()["retention"]["mode"], "Locked");
    // Locked: only extension in Locked mode, even with the override.
    for body in [
        json!({"retention": retention("Locked", HALF_HOUR)}),
        json!({"retention": retention("Unlocked", TWO_HOURS)}),
        json!({"retention": null}),
        json!({"retention": {}}),
    ] {
        patch(
            &h,
            "locked/o/kept",
            "?overrideUnlockedRetention=true",
            body,
            ADMIN,
        )
        .await
        .error(400, "invalidArgument");
    }
    let put = h
        .request(
            "PUT",
            "/storage/v1/b/locked/o/kept",
            ADMIN,
            json!({"retention": retention("Locked", TWO_HOURS)})
                .to_string()
                .as_bytes(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.json());
    assert_eq!(
        put.json()["retention"],
        json!({"mode":"Locked","retainUntilTime":"2026-01-01T02:00:00.000Z"})
    );
    // PUT without retention keeps the configuration, like holds.
    let kept = h
        .request("PUT", "/storage/v1/b/locked/o/kept", ADMIN, b"{}")
        .await;
    assert_eq!(kept.json()["retention"], put.json()["retention"]);
    // The protection ends exactly at retainUntilTime.
    set_clock(&h, START + 7199);
    delete(&h, "locked/o/kept")
        .await
        .error(403, "retentionPolicyNotMet");
    set_clock(&h, START + 7200);
    // Locked configurations stay unremovable after expiry.
    patch(
        &h,
        "locked/o/kept",
        "?overrideUnlockedRetention=true",
        json!({"retention": null}),
        ADMIN,
    )
    .await
    .error(400, "invalidArgument");
    media(&h, "locked", "kept").await;
    let replaced = h
        .request("GET", "/storage/v1/b/locked/o/kept", ADMIN, b"")
        .await
        .json();
    assert!(replaced.get("retention").is_none(), "{replaced}");
    assert_eq!(delete(&h, "locked/o/kept").await.status, 204);
    // Ruby removes an Unlocked configuration with an empty retention object.
    set_clock(&h, START);
    multipart(
        &h,
        "locked",
        "removable",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    let removed = patch(
        &h,
        "locked/o/removable",
        "?overrideUnlockedRetention=true",
        json!({"retention": {}}),
        ADMIN,
    )
    .await;
    assert_eq!(removed.status, 200);
    assert!(removed.json().get("retention").is_none());
    assert_eq!(delete(&h, "locked/o/removable").await.status, 204);
    h.stop().await;
}

#[tokio::test]
async fn object_retention_requires_enabled_bucket_and_no_event_hold() {
    let h = Harness::start(1024, true, None).await;
    set_clock(&h, START);
    create_bucket(&h, "locked", json!({})).await;
    multipart(
        &h,
        "bucket",
        "plain",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await
    .error(400, "invalidArgument");
    h.request("GET", "/storage/v1/b/bucket/o/plain", ADMIN, b"")
        .await
        .error(404, "notFound");
    assert_eq!(
        multipart(&h, "bucket", "plain", json!({}), ADMIN)
            .await
            .status,
        200
    );
    patch(
        &h,
        "bucket/o/plain",
        "",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await
    .error(400, "invalidArgument");
    // Removing an absent configuration is a no-op even where the feature is disabled.
    assert_eq!(
        patch(&h, "bucket/o/plain", "", json!({"retention": null}), ADMIN)
            .await
            .status,
        200
    );
    for metadata in [
        json!({"retention": retention("Unlocked", HOUR), "eventBasedHold": true}),
        json!({"retention": retention("Unlocked", "2126-01-03T00:00:00Z")}),
        json!({"retention": {"mode":"Unlocked"}}),
        json!({"retention": retention("GOVERNANCE", HOUR)}),
    ] {
        multipart(&h, "locked", "invalid", metadata, ADMIN)
            .await
            .error(400, "invalidArgument");
    }
    h.request("GET", "/storage/v1/b/locked/o/invalid", ADMIN, b"")
        .await
        .error(404, "notFound");
    let boundary = multipart(
        &h,
        "locked",
        "boundary",
        json!({"retention": retention("Unlocked", "2125-12-31T00:00:00Z")}),
        ADMIN,
    )
    .await;
    assert_eq!(boundary.status, 200, "{}", boundary.json());
    multipart(&h, "locked", "held", json!({"eventBasedHold": true}), ADMIN).await;
    patch(
        &h,
        "locked/o/held",
        "",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await
    .error(400, "invalidArgument");
    let released = patch(
        &h,
        "locked/o/held",
        "",
        json!({"eventBasedHold": false, "retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    assert_eq!(released.status, 200, "{}", released.json());
    // Holds and retention protect independently: the hold reason takes precedence.
    patch(
        &h,
        "locked/o/held",
        "",
        json!({"temporaryHold": true}),
        ADMIN,
    )
    .await;
    delete(&h, "locked/o/held")
        .await
        .error(403, "objectUnderActiveHold");
    patch(
        &h,
        "locked/o/held",
        "",
        json!({"temporaryHold": false}),
        ADMIN,
    )
    .await;
    delete(&h, "locked/o/held")
        .await
        .error(403, "retentionPolicyNotMet");
    // Copy, rewrite and compose do not inherit or accept retention in this slice.
    for (path, body) in [
        (
            "/storage/v1/b/locked/o/held/copyTo/b/locked/o/copied",
            json!({"retention": retention("Unlocked", HOUR)}),
        ),
        (
            "/storage/v1/b/locked/o/held/rewriteTo/b/locked/o/copied",
            json!({"retention": retention("Unlocked", HOUR)}),
        ),
        (
            "/storage/v1/b/locked/o/copied/compose",
            json!({"sourceObjects":[{"name":"held"}],"destination":{"retention": retention("Unlocked", HOUR)}}),
        ),
    ] {
        h.request("POST", path, ADMIN, body.to_string().as_bytes())
            .await
            .error(400, "badRequest");
    }
    let copied = h
        .request(
            "POST",
            "/storage/v1/b/locked/o/held/copyTo/b/locked/o/copied",
            ADMIN,
            b"",
        )
        .await;
    assert_eq!(copied.status, 200, "{}", copied.json());
    assert!(copied.json().get("retention").is_none());
    h.stop().await;
}

#[tokio::test]
async fn object_retention_versioning_soft_delete_and_bucket_policy() {
    let h = Harness::start(1024, true, None).await;
    set_clock(&h, START);
    create_bucket(&h, "versioned", json!({"versioning":{"enabled":true}})).await;
    let first = multipart(
        &h,
        "versioned",
        "doc",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await
    .json();
    // The guide lets a retained live version become noncurrent.
    assert_eq!(media(&h, "versioned", "doc").await.status, 200);
    let generation = first["generation"].as_str().unwrap();
    let noncurrent = h
        .request(
            "GET",
            &format!("/storage/v1/b/versioned/o/doc?generation={generation}"),
            ADMIN,
            b"",
        )
        .await
        .json();
    assert_eq!(noncurrent["retention"], first["retention"]);
    assert!(noncurrent["timeDeleted"].is_string());
    delete(&h, &format!("versioned/o/doc?generation={generation}"))
        .await
        .error(403, "retentionPolicyNotMet");
    multipart(
        &h,
        "versioned",
        "live",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    assert_eq!(delete(&h, "versioned/o/live").await.status, 204);
    // Soft delete still counts as deletion.
    create_bucket(
        &h,
        "soft",
        json!({"softDeletePolicy":{"retentionDurationSeconds":"604800"}}),
    )
    .await;
    multipart(
        &h,
        "soft",
        "doc",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    delete(&h, "soft/o/doc")
        .await
        .error(403, "retentionPolicyNotMet");
    // A bucket policy and object retention both apply; the later deadline is reported.
    create_bucket(
        &h,
        "both",
        json!({"retentionPolicy":{"retentionPeriod":"7200"}}),
    )
    .await;
    let both = multipart(
        &h,
        "both",
        "doc",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await
    .json();
    assert_eq!(both["retentionExpirationTime"], "2026-01-01T02:00:00.000Z");
    let removed = patch(
        &h,
        "both/o/doc",
        "?overrideUnlockedRetention=true",
        json!({"retention": null}),
        ADMIN,
    )
    .await;
    assert_eq!(removed.status, 200);
    set_clock(&h, START + 3600);
    delete(&h, "both/o/doc")
        .await
        .error(403, "retentionPolicyNotMet");
    set_clock(&h, START + 7200);
    assert_eq!(delete(&h, "both/o/doc").await.status, 204);
    // Retained objects keep the bucket non-empty.
    h.request("DELETE", "/storage/v1/b/versioned", ADMIN, b"")
        .await
        .error(409, "conflict");
    h.stop().await;
}

#[tokio::test]
async fn object_retention_resumable_upload_and_strict_permissions() {
    let h = Harness::start(1024, true, None).await;
    set_clock(&h, START);
    create_bucket(&h, "locked", json!({})).await;
    let init = h
        .request(
            "POST",
            "/upload/storage/v1/b/locked/o?uploadType=resumable&name=resumed",
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            json!({"retention": retention("Locked", HOUR)})
                .to_string()
                .as_bytes(),
        )
        .await;
    assert_eq!(init.status, 200);
    let completed = h
        .request(
            "PUT",
            &h.path(&init),
            "Content-Range: bytes 0-3/4\r\n",
            b"data",
        )
        .await;
    assert_eq!(completed.status, 200, "{}", completed.json());
    assert_eq!(
        completed.json()["retention"],
        json!({"mode":"Locked","retainUntilTime":"2026-01-01T01:00:00.000Z"})
    );
    multipart(
        &h,
        "locked",
        "doc",
        json!({"retention": retention("Unlocked", HOUR)}),
        ADMIN,
    )
    .await;
    // objectUser can update metadata but lacks setRetention; legacyBucketOwner adds
    // setRetention without overrideUnlockedRetention.
    let grant = |roles: &[&str]| {
        json!({"bindings": roles.iter().map(|role| json!({"role": role, "members": ["user:viewer@example.test"]})).collect::<Vec<_>>()})
            .to_string()
    };
    let set_policy = async |roles: &[&str]| {
        let reply = h
            .request(
                "PUT",
                "/storage/v1/b/locked/iam",
                &format!("{ADMIN}Content-Type: application/json\r\n"),
                grant(roles).as_bytes(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.json());
    };
    set_policy(&["roles/storage.objectUser"]).await;
    assert_eq!(
        patch(
            &h,
            "locked/o/doc",
            "",
            json!({"metadata":{"k":"v"}}),
            VIEWER
        )
        .await
        .status,
        200
    );
    patch(
        &h,
        "locked/o/doc",
        "",
        json!({"retention": retention("Unlocked", TWO_HOURS)}),
        VIEWER,
    )
    .await
    .error(403, "forbidden");
    multipart(
        &h,
        "locked",
        "viewer",
        json!({"retention": retention("Unlocked", HOUR)}),
        VIEWER,
    )
    .await
    .error(403, "forbidden");
    h.request(
        "POST",
        "/upload/storage/v1/b/locked/o?uploadType=resumable&name=viewer",
        &format!("{VIEWER}Content-Type: application/json\r\n"),
        json!({"retention": retention("Unlocked", HOUR)})
            .to_string()
            .as_bytes(),
    )
    .await
    .error(403, "forbidden");
    set_policy(&[
        "roles/storage.objectUser",
        "roles/storage.legacyBucketOwner",
    ])
    .await;
    let extended = patch(
        &h,
        "locked/o/doc",
        "",
        json!({"retention": retention("Unlocked", TWO_HOURS)}),
        VIEWER,
    )
    .await;
    assert_eq!(extended.status, 200, "{}", extended.json());
    patch(
        &h,
        "locked/o/doc",
        "?overrideUnlockedRetention=true",
        json!({"retention": retention("Unlocked", HOUR)}),
        VIEWER,
    )
    .await
    .error(403, "forbidden");
    assert_eq!(
        patch(
            &h,
            "locked/o/doc",
            "?overrideUnlockedRetention=true",
            json!({"retention": retention("Unlocked", HOUR)}),
            ADMIN,
        )
        .await
        .status,
        200
    );
    h.stop().await;
}
