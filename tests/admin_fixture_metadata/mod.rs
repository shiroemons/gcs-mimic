use super::*;

#[test]
fn direct_fixture_acl_holds_and_retention_use_shared_atomic_validation() {
    let dir = TestDir::new();
    let options = [
        "--admin-enabled",
        "true",
        "--api-base-url",
        "http://fixture.invalid",
    ];
    let server = Server::start(&dir, &options);
    server.populate();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/notificationConfigs",
            "",
            br#"{"topic":"projects/local/topics/events"}"#,
        )
        .json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let notifications = || {
        connection
            .query_row("SELECT count(*) FROM notification_outbox", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    let acl = json!([{"entity":"allUsers","role":"READER"}]);
    let resource = server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","acl":acl,
        "temporaryHold":true, "eventBasedHold":true}),
            b"protected",
            "",
        )
        .json();
    assert_eq!(resource["temporaryHold"], true);
    assert_eq!(resource["eventBasedHold"], true);
    let full = server
        .request("GET", &format!("{OBJECT}?projection=full"), "", b"")
        .json();
    assert_eq!(full["acl"][0]["entity"], "allUsers");
    assert_eq!(
        server
            .fixture(
                json!({"bucket":"test-bucket","name":"value"}),
                b"blocked",
                ""
            )
            .status,
        403
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"protected"
    );
    server
        .request(
            "PATCH",
            OBJECT,
            "",
            br#"{"temporaryHold":false,"eventBasedHold":false}"#,
        )
        .json();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "",
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .json();
    let before = server.request("GET", OBJECT, "", b"").json();
    let queued = notifications();
    assert_eq!(
        server
            .fixture(
                json!({"bucket":"test-bucket","name":"value","acl":acl}),
                b"denied",
                ""
            )
            .status,
        403
    );
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    assert_eq!(notifications(), queued);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "",
            br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#,
        )
        .json();
    assert_eq!(
        server
            .fixture(
                json!({"bucket":"test-bucket","name":"value","acl":[]}),
                b"denied",
                ""
            )
            .status,
        400
    );
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local&enableObjectRetention=true",
            "",
            br#"{"name":"retained-bucket"}"#,
        )
        .json();
    let retained = server
        .fixture(
            json!({"bucket":"retained-bucket","name":"value", "retention":{
        "mode":"Locked", "retainUntilTime":"2090-01-01T00:00:00Z"}}),
            b"retained",
            "",
        )
        .json();
    assert_eq!(retained["retention"]["mode"], "Locked");
    assert_eq!(
        server
            .fixture(
                json!({"bucket":"retained-bucket","name":"value"}),
                b"denied",
                ""
            )
            .status,
        403
    );
    drop(server);
    let server = Server::start(&dir, &options);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/retained-bucket/o/value", "", b"")
            .json(),
        retained
    );
}
