use super::{Harness, Reply};
use serde_json::{Value, json};
use std::time::{Duration, UNIX_EPOCH};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

async fn resource(h: &Harness, method: &str, path: &str, body: Value) -> Reply {
    h.request(method, path, ADMIN, body.to_string().as_bytes())
        .await
}

async fn policy(h: &Harness, seconds: u64) -> Value {
    let response = resource(
        h,
        "PATCH",
        "/storage/v1/b/bucket",
        json!({"softDeletePolicy":{"retentionDurationSeconds":seconds.to_string()}}),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.json());
    response.json()
}

async fn upload(h: &Harness, name: &str, bytes: &[u8]) -> Value {
    let response = h
        .request(
            "POST",
            &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
            ADMIN,
            bytes,
        )
        .await;
    assert_eq!(response.status, 200, "{}", response.json());
    response.json()
}

async fn retained(h: &Harness) -> Vec<Value> {
    let response = h
        .request("GET", "/storage/v1/b/bucket/o?softDeleted=true", ADMIN, b"")
        .await;
    assert_eq!(response.status, 200, "{}", response.json());
    response.json()["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn soft_delete_configuration_defaults_boundaries_and_permissions() {
    let h = Harness::start(1024, true, None).await;
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1000);
    for seconds in [None, Some(0)] {
        if let Some(seconds) = seconds {
            policy(&h, seconds).await;
        }
        let default = upload(&h, "permanent", b"permanent").await;
        assert!(retained(&h).await.is_empty());
        assert_eq!(
            h.request("DELETE", "/storage/v1/b/bucket/o/permanent", ADMIN, b"")
                .await
                .status,
            204
        );
        assert!(retained(&h).await.is_empty());
        for (name, generation) in [
            ("permanent", default["generation"].as_str().unwrap()),
            ("missing", "1"),
        ] {
            for (method, path) in [
                (
                    "GET",
                    format!(
                        "/storage/v1/b/bucket/o/{name}?softDeleted=true&generation={generation}"
                    ),
                ),
                (
                    "POST",
                    format!("/storage/v1/b/bucket/o/{name}/restore?generation={generation}"),
                ),
            ] {
                h.request(method, &path, ADMIN, b"")
                    .await
                    .error(404, "notFound");
            }
        }
    }
    let configured = policy(&h, 604800).await;
    let effective = configured["softDeletePolicy"]["effectiveTime"].clone();
    for invalid in [
        json!(-1),
        json!(1),
        json!(604799),
        json!(7776001),
        json!(1.5),
        json!(true),
        json!("abc"),
    ] {
        for method in ["PATCH", "PUT"] {
            resource(
                &h,
                method,
                "/storage/v1/b/bucket",
                json!({"softDeletePolicy":{"retentionDurationSeconds":invalid}}),
            )
            .await
            .error(400, "invalidArgument");
        }
        resource(&h, "POST", "/storage/v1/b?project=project", json!({"name":"invalid-policy", "softDeletePolicy":{"retentionDurationSeconds":invalid}})).await.error(400, "invalidArgument");
    }
    for (method, input) in [
        ("PATCH", json!({})),
        ("PATCH", Value::Null),
        ("PUT", json!({})),
        ("PUT", Value::Null),
    ] {
        let response = resource(
            &h,
            method,
            "/storage/v1/b/bucket",
            json!({"softDeletePolicy":input}),
        )
        .await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.json()["softDeletePolicy"],
            configured["softDeletePolicy"]
        );
    }
    *h.clock.0.lock().unwrap() += Duration::from_secs(1);
    let increased = policy(&h, 7776000).await;
    assert_ne!(increased["softDeletePolicy"]["effectiveTime"], effective);
    let decreased = policy(&h, 604800).await;
    assert_eq!(
        decreased["softDeletePolicy"]["effectiveTime"],
        increased["softDeletePolicy"]["effectiveTime"]
    );
    let disabled = policy(&h, 0).await;
    assert_eq!(
        disabled["softDeletePolicy"]["retentionDurationSeconds"],
        "0"
    );
    for (name, input) in [
        ("empty-policy", json!({})),
        ("null-policy", Value::Null),
        (
            "enabled-policy",
            json!({"retentionDurationSeconds":"604800"}),
        ),
    ] {
        let response = resource(
            &h,
            "POST",
            "/storage/v1/b?project=project",
            json!({"name":name,"softDeletePolicy":input}),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.json());
        assert_eq!(
            response.json().get("softDeletePolicy").is_some(),
            name == "enabled-policy"
        );
    }
    let fetched = h
        .request("GET", "/storage/v1/b/bucket", ADMIN, b"")
        .await
        .json();
    assert_eq!(fetched["softDeletePolicy"], disabled["softDeletePolicy"]);
    let listed = h
        .request("GET", "/storage/v1/b?project=project", ADMIN, b"")
        .await
        .json();
    assert_eq!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "bucket")
            .unwrap()["softDeletePolicy"],
        disabled["softDeletePolicy"]
    );
    h.request(
        "PATCH",
        "/storage/v1/b/bucket",
        "Authorization: Bearer viewer\r\n",
        br#"{"softDeletePolicy":{"retentionDurationSeconds":"604800"}}"#,
    )
    .await
    .error(403, "forbidden");
    policy(&h, 604800).await;
    upload(&h, "auth", b"auth").await;
    assert_eq!(
        h.request("DELETE", "/storage/v1/b/bucket/o/auth", ADMIN, b"")
            .await
            .status,
        204
    );
    let generation = retained(&h).await[0]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    for path in [
        "/storage/v1/b/bucket/o?softDeleted=true".to_owned(),
        format!("/storage/v1/b/bucket/o/auth?generation={generation}&softDeleted=true"),
    ] {
        h.request("GET", &path, "Authorization: Bearer creator\r\n", b"")
            .await
            .error(403, "forbidden");
        assert_eq!(
            h.request("GET", &path, "Authorization: Bearer viewer\r\n", b"")
                .await
                .status,
            200
        );
    }
    for token in ["creator", "viewer"] {
        h.request(
            "POST",
            &format!("/storage/v1/b/bucket/o/auth/restore?generation={generation}"),
            &format!("Authorization: Bearer {token}\r\n"),
            b"",
        )
        .await
        .error(403, "forbidden");
    }
    h.stop().await;
}

#[tokio::test]
async fn soft_delete_restore_conditions_collisions_and_restart() {
    let h = Harness::start(1024, false, None).await;
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1000);
    policy(&h, 604800).await;
    let original = upload(&h, "history%2Fitem", b"original").await;
    let generation = original["generation"].as_str().unwrap();
    let metadata = resource(
        &h,
        "PATCH",
        "/storage/v1/b/bucket/o/history%2Fitem",
        json!({"metadata":{"recover":"yes"},"contentDisposition":"attachment"}),
    )
    .await
    .json();
    assert_eq!(metadata["metageneration"], "2");
    assert_eq!(
        h.request(
            "DELETE",
            "/storage/v1/b/bucket/o/history%2Fitem",
            ADMIN,
            b""
        )
        .await
        .status,
        204
    );
    let get = format!("/storage/v1/b/bucket/o/history%2Fitem?generation={generation}");
    h.request("GET", &get, ADMIN, b"")
        .await
        .error(404, "notFound");
    h.request("GET", &format!("{get}&alt=media"), ADMIN, b"")
        .await
        .error(404, "notFound");
    for path in [
        "/storage/v1/b/bucket/o/history%2Fitem?softDeleted=true".to_owned(),
        format!("{get}&softDeleted=true&alt=media"),
        "/storage/v1/b/bucket/o?softDeleted=true&versions=true".into(),
    ] {
        h.request("GET", &path, ADMIN, b"")
            .await
            .error(400, "invalidArgument");
    }
    let saved = h
        .request("GET", &format!("{get}&softDeleted=true"), ADMIN, b"")
        .await
        .json();
    assert_eq!(saved["softDeleteTime"], "1970-01-01T00:16:40.000Z");
    assert_eq!(saved["hardDeleteTime"], "1970-01-08T00:16:40.000Z");
    let restore = format!("/storage/v1/b/bucket/o/history%2Fitem/restore?generation={generation}");
    for query in [
        "copySourceAcl=true",
        "restoreToken=token",
        "projection=full",
    ] {
        h.request("POST", &format!("{restore}&{query}"), ADMIN, b"")
            .await
            .error(400, "badRequest");
    }
    h.request(
        "POST",
        "/storage/v1/b/bucket/o/history%2Fitem/restore",
        ADMIN,
        b"",
    )
    .await
    .error(400, "required");
    h.request(
        "POST",
        &format!("{restore}&ifGenerationMatch=1&ifGenerationNotMatch=2"),
        ADMIN,
        b"",
    )
    .await
    .error(400, "invalidArgument");
    h.request("POST", &restore, ADMIN, b"{}")
        .await
        .error(400, "invalidArgument");
    for body in [
        "generation=1".to_owned(),
        format!("generation={generation}&extra=true"),
    ] {
        h.request(
            "POST",
            &restore,
            "Content-Type: application/x-www-form-urlencoded\r\n",
            body.as_bytes(),
        )
        .await
        .error(400, "invalidArgument");
    }
    h.request(
        "POST",
        "/storage/v1/b/bucket/o/missing/restore?generation=1",
        ADMIN,
        b"",
    )
    .await
    .error(404, "notFound");
    let live = upload(&h, "history%2Fitem", b"replacement").await;
    for condition in [
        "ifGenerationMatch=0".into(),
        format!("ifGenerationMatch={generation}"),
        "ifGenerationNotMatch=0&ifMetagenerationMatch=2".into(),
        "ifMetagenerationNotMatch=1".into(),
    ] {
        h.request("POST", &format!("{restore}&{condition}"), ADMIN, b"")
            .await
            .error(412, "conditionNotMet");
    }
    let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_restored_insert BEFORE INSERT ON objects BEGIN SELECT RAISE(ABORT,'publication rejected'); END;").unwrap();
    h.request("POST", &restore, ADMIN, b"")
        .await
        .error(503, "backendError");
    connection
        .execute_batch("DROP TRIGGER reject_restored_insert;")
        .unwrap();
    assert_eq!(
        h.request(
            "GET",
            "/storage/v1/b/bucket/o/history%2Fitem?alt=media",
            ADMIN,
            b""
        )
        .await
        .body,
        b"replacement"
    );
    assert_eq!(retained(&h).await.len(), 1);
    drop(connection);
    *h.clock.0.lock().unwrap() += Duration::from_secs(1);
    let restored = h
        .request(
            "POST",
            &format!(
                "{restore}&copySourceAcl=false&ifGenerationMatch={}&ifMetagenerationMatch=1",
                live["generation"].as_str().unwrap()
            ),
            ADMIN,
            b"",
        )
        .await;
    assert_eq!(restored.status, 200, "{}", restored.json());
    let restored = restored.json();
    assert_ne!(restored["generation"], original["generation"]);
    assert_eq!(restored["metageneration"], "1");
    assert_eq!(restored["metadata"], json!({"recover":"yes"}));
    assert_eq!(restored["contentDisposition"], "attachment");
    assert_eq!(restored["timeCreated"], "1970-01-01T00:16:41.000Z");
    assert!(restored.get("softDeleteTime").is_none() && restored.get("hardDeleteTime").is_none());
    assert_eq!(
        h.request(
            "GET",
            "/storage/v1/b/bucket/o/history%2Fitem?alt=media",
            ADMIN,
            b""
        )
        .await
        .body,
        b"original"
    );
    assert_eq!(retained(&h).await.len(), 2);
    h.request(
        "POST",
        &format!(
            "/storage/v1/b/bucket/o/history%2Fitem/restore?generation={}",
            restored["generation"].as_str().unwrap()
        ),
        ADMIN,
        b"",
    )
    .await
    .error(412, "objectNotSoftDeleted");
    policy(&h, 0).await;
    let dir = h.dir.0.clone();
    let clock = h.clock.clone();
    h.stop.send(()).unwrap();
    h.server.await.unwrap().unwrap();
    h.state.catalog.close().await.unwrap();
    let reopened = crate::storage::Catalog::open_with_clock(&dir, clock)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .get_bucket("bucket")
            .await
            .unwrap()
            .unwrap()
            .soft_delete_policy
            .unwrap()
            .retention_duration_seconds,
        0
    );
    let retained = reopened
        .get_soft_deleted_object_authorized(
            "bucket",
            "history/item",
            generation.parse().unwrap(),
            Default::default(),
            &crate::bucket::BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(retained.generation.to_string(), generation);
    let restored = reopened
        .restore_object_authorized(
            "bucket",
            "history/item",
            retained.generation,
            Default::default(),
            &crate::bucket::BucketAccess::Permissive,
            &crate::bucket::BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert!(restored.generation > retained.generation);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn soft_delete_all_overwrite_paths_and_versioned_deletion() {
    let h = Harness::start(1024, false, None).await;
    policy(&h, 604800).await;
    for name in [
        "upload",
        "copy",
        "rewrite",
        "compose",
        "move",
        "xml",
        "source",
        "move-source",
    ] {
        upload(&h, name, b"old").await;
    }
    for (method, path, body) in [
        (
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=upload",
            &b"new"[..],
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/source/copyTo/b/bucket/o/copy",
            &b""[..],
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/rewrite",
            &b""[..],
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/compose/compose",
            &br#"{"sourceObjects":[{"name":"source"}]}"#[..],
        ),
        (
            "POST",
            "/storage/v1/b/bucket/o/move-source/moveTo/o/move",
            &b""[..],
        ),
        ("PUT", "/bucket/xml", &b"new"[..]),
    ] {
        let response = h.request(method, path, ADMIN, body).await;
        assert_eq!(
            response.status,
            200,
            "{path}: {}",
            String::from_utf8_lossy(&response.body)
        );
    }
    let deleted = retained(&h).await;
    assert_eq!(
        deleted
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["compose", "copy", "move", "rewrite", "upload", "xml"]
    );
    assert_eq!(
        h.request("DELETE", "/bucket/source", ADMIN, b"")
            .await
            .status,
        204
    );
    assert_eq!(retained(&h).await.len(), 7);
    resource(
        &h,
        "PATCH",
        "/storage/v1/b/bucket",
        json!({"versioning":{"enabled":true}}),
    )
    .await;
    let old = upload(&h, "versioned", b"version one").await;
    upload(&h, "versioned", b"version two").await;
    let path = format!(
        "/storage/v1/b/bucket/o/versioned?generation={}",
        old["generation"].as_str().unwrap()
    );
    assert_eq!(h.request("GET", &path, ADMIN, b"").await.status, 200);
    assert_eq!(h.request("DELETE", &path, ADMIN, b"").await.status, 204);
    h.request("GET", &path, ADMIN, b"")
        .await
        .error(404, "notFound");
    assert_eq!(
        h.request("GET", &format!("{path}&softDeleted=true"), ADMIN, b"")
            .await
            .status,
        200
    );
    let versions = h
        .request(
            "GET",
            "/storage/v1/b/bucket/o?versions=true&prefix=versioned",
            ADMIN,
            b"",
        )
        .await
        .json();
    assert_eq!(versions["items"].as_array().unwrap().len(), 1);
    let restored = h
        .request(
            "POST",
            &format!(
                "/storage/v1/b/bucket/o/versioned/restore?generation={}",
                old["generation"].as_str().unwrap()
            ),
            ADMIN,
            b"",
        )
        .await;
    assert_eq!(restored.status, 200, "{}", restored.json());
    let versions = h
        .request(
            "GET",
            "/storage/v1/b/bucket/o?versions=true&prefix=versioned",
            ADMIN,
            b"",
        )
        .await
        .json();
    assert_eq!(versions["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        retained(&h)
            .await
            .iter()
            .filter(|o| o["name"] == "versioned")
            .count(),
        1
    );
    h.stop().await;
}

#[tokio::test]
async fn soft_delete_filters_paging_expiry_holds_and_bucket_delete() {
    let h = Harness::start(1024, false, None).await;
    *h.clock.0.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1000);
    policy(&h, 604800).await;
    for name in ["a.txt", "a.txt", "dir%2Fb.txt", "z.log"] {
        upload(&h, name, b"bytes").await;
        assert_eq!(
            h.request(
                "DELETE",
                &format!("/storage/v1/b/bucket/o/{name}"),
                ADMIN,
                b""
            )
            .await
            .status,
            204
        );
    }
    let original_deadline = retained(&h).await[0]["hardDeleteTime"].clone();
    policy(&h, 1209600).await;
    assert_eq!(retained(&h).await[0]["hardDeleteTime"], original_deadline);
    let first = h
        .request(
            "GET",
            "/storage/v1/b/bucket/o?softDeleted=true&maxResults=1&prefix=a&matchGlob=*.txt",
            ADMIN,
            b"",
        )
        .await
        .json();
    let token = first["nextPageToken"].as_str().unwrap();
    let second = h.request("GET", &format!("/storage/v1/b/bucket/o?softDeleted=true&maxResults=1&prefix=a&matchGlob=*.txt&pageToken={token}"), ADMIN, b"").await.json();
    assert_ne!(
        first["items"][0]["generation"],
        second["items"][0]["generation"]
    );
    assert!(second.get("nextPageToken").is_none());
    h.request(
        "GET",
        &format!("/storage/v1/b/bucket/o?maxResults=1&prefix=a&matchGlob=*.txt&pageToken={token}"),
        ADMIN,
        b"",
    )
    .await
    .error(400, "invalidParameter");
    let grouped = h
        .request(
            "GET",
            "/storage/v1/b/bucket/o?softDeleted=true&delimiter=/&prefix=dir/",
            ADMIN,
            b"",
        )
        .await
        .json();
    assert_eq!(grouped["items"].as_array().unwrap().len(), 1);
    let grouped = h
        .request(
            "GET",
            "/storage/v1/b/bucket/o?softDeleted=true&delimiter=/&matchGlob=**/",
            ADMIN,
            b"",
        )
        .await
        .json();
    assert_eq!(grouped["prefixes"], json!(["dir/"]));
    let held = upload(&h, "held", b"held").await;
    resource(
        &h,
        "PATCH",
        "/storage/v1/b/bucket/o/held",
        json!({"temporaryHold":true}),
    )
    .await;
    h.request("DELETE", "/storage/v1/b/bucket/o/held", ADMIN, b"")
        .await
        .error(403, "objectUnderActiveHold");
    resource(
        &h,
        "PATCH",
        "/storage/v1/b/bucket/o/held",
        json!({"temporaryHold":false}),
    )
    .await;
    resource(
        &h,
        "PATCH",
        "/storage/v1/b/bucket",
        json!({"retentionPolicy":{"retentionPeriod":"3600"}}),
    )
    .await;
    h.request("DELETE", "/storage/v1/b/bucket/o/held", ADMIN, b"")
        .await
        .error(403, "retentionPolicyNotMet");
    *h.clock.0.lock().unwrap() += Duration::from_secs(604800);
    let expired = first["items"][0]["generation"].as_str().unwrap();
    h.request(
        "GET",
        &format!("/storage/v1/b/bucket/o/a.txt?softDeleted=true&generation={expired}"),
        ADMIN,
        b"",
    )
    .await
    .error(404, "notFound");
    h.request(
        "POST",
        &format!("/storage/v1/b/bucket/o/a.txt/restore?generation={expired}"),
        ADMIN,
        b"",
    )
    .await
    .error(404, "notFound");
    assert!(retained(&h).await.is_empty());
    assert_eq!(
        h.request("DELETE", "/storage/v1/b/bucket/o/held", ADMIN, b"")
            .await
            .status,
        204
    );
    assert_ne!(held["generation"], Value::Null);
    assert_eq!(retained(&h).await.len(), 1);
    assert_eq!(
        h.request("DELETE", "/storage/v1/b/bucket", ADMIN, b"")
            .await
            .status,
        204
    );
    assert!(
        h.state
            .catalog
            .get_bucket("bucket")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("blobs"))
            .unwrap()
            .flat_map(|s| std::fs::read_dir(s.unwrap().path()).unwrap())
            .count(),
        0
    );
    h.stop().await;
}

#[tokio::test]
async fn soft_delete_concurrent_restore_create_if_absent_has_one_winner() {
    let h = Harness::start(1024, false, None).await;
    policy(&h, 604800).await;
    let original = upload(&h, "race", b"bytes").await;
    assert_eq!(
        h.request("DELETE", "/storage/v1/b/bucket/o/race", ADMIN, b"")
            .await
            .status,
        204
    );
    let path = format!(
        "/storage/v1/b/bucket/o/race/restore?generation={}&ifGenerationMatch=0",
        original["generation"].as_str().unwrap()
    );
    let (one, two) = tokio::join!(
        h.request("POST", &path, ADMIN, b""),
        h.request("POST", &path, ADMIN, b"")
    );
    let mut statuses = [one.status, two.status];
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    assert_eq!(retained(&h).await.len(), 1);
    h.stop().await;
}
