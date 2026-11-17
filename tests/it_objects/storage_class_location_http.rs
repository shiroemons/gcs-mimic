//! The legacy REGIONAL and MULTI_REGIONAL classes against the bucket location, following what
//! Cloud Storage recorded on 2026-11-17 (cases `bkt-gaps-legacy-class-*`).

use crate::common;
use common::TestDir;
use common::effects::Server;
use serde_json::{Value, json};

/// Cloud Storage's refusal of a class the location cannot hold, naming the number of project
/// `local`.
fn refusal(class: &str, location: &str) -> String {
    format!(
        "Project 902648055030 may not create storageClass {class} buckets with locationConstraint {location}."
    )
}

fn send(server: &Server, method: &str, path: &str, body: &Value) -> (u16, Value) {
    let reply = server.request(method, path, "", body.to_string().as_bytes());
    (reply.status, reply.json())
}

fn get(server: &Server, path: &str) -> (u16, Value) {
    let reply = server.request("GET", path, "", b"");
    (reply.status, reply.json())
}

#[track_caller]
fn assert_refused((status, body): (u16, Value), class: &str, location: &str) {
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["errors"][0]["reason"], "invalid", "{body}");
    assert_eq!(body["error"]["message"], refusal(class, location), "{body}");
}

fn create(server: &Server, name: &str, location: &str, class: &str) -> (u16, Value) {
    send(
        server,
        "POST",
        "/storage/v1/b?project=local",
        &json!({"name":name,"location":location,"storageClass":class}),
    )
}

/// A regional bucket whose default class is REGIONAL, and a STANDARD multi-region bucket.
fn setup() -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    assert_eq!(
        create(&server, "regional", "us-central1", "REGIONAL").0,
        200
    );
    assert_eq!(create(&server, "multi", "US", "STANDARD").0, 200);
    (dir, server)
}

fn upload(server: &Server, bucket: &str, metadata: &Value) -> (u16, Value) {
    let reply = server.multipart_in(bucket, "boundary", &metadata.to_string(), b"x", "");
    (reply.status, reply.json())
}

#[test]
fn a_bucket_class_must_fit_the_location_type() {
    let (_dir, server) = setup();
    let regional = get(&server, "/storage/v1/b/regional").1;
    assert_eq!(
        (&regional["storageClass"], &regional["locationType"]),
        (&json!("REGIONAL"), &json!("region"))
    );
    assert_refused(
        create(&server, "refused-us", "US", "REGIONAL"),
        "REGIONAL",
        "US",
    );
    assert_refused(
        create(&server, "refused-nam4", "nam4", "REGIONAL"),
        "REGIONAL",
        "NAM4",
    );
    assert_refused(
        create(&server, "refused-region", "us-central1", "MULTI_REGIONAL"),
        "MULTI_REGIONAL",
        "US-CENTRAL1",
    );
    for method in ["PATCH", "PUT"] {
        for (bucket, class, location) in [
            ("multi", "REGIONAL", "US"),
            ("regional", "MULTI_REGIONAL", "US-CENTRAL1"),
        ] {
            let path = format!("/storage/v1/b/{bucket}");
            assert_refused(
                send(&server, method, &path, &json!({"storageClass":class})),
                class,
                location,
            );
        }
    }
    for (bucket, class) in [
        ("regional", "DURABLE_REDUCED_AVAILABILITY"),
        ("regional", "REGIONAL"),
        ("multi", "MULTI_REGIONAL"),
    ] {
        let path = format!("/storage/v1/b/{bucket}");
        let (status, body) = send(&server, "PATCH", &path, &json!({"storageClass":class}));
        assert_eq!((status, &body["storageClass"]), (200, &json!(class)));
    }
}

#[test]
fn an_object_class_must_fit_the_bucket_location() {
    let (_dir, server) = setup();
    let (status, inherited) = upload(&server, "regional", &json!({"name":"inherited"}));
    assert_eq!(
        (status, &inherited["storageClass"]),
        (200, &json!("REGIONAL"))
    );
    assert_refused(
        upload(
            &server,
            "multi",
            &json!({"name":"o","storageClass":"REGIONAL"}),
        ),
        "REGIONAL",
        "US",
    );
    assert_refused(
        upload(
            &server,
            "regional",
            &json!({"name":"o","storageClass":"MULTI_REGIONAL"}),
        ),
        "MULTI_REGIONAL",
        "US-CENTRAL1",
    );
    // A resumable upload is refused when it starts, before any byte is sent.
    assert_refused(
        send(
            &server,
            "POST",
            "/upload/storage/v1/b/multi/o?uploadType=resumable&name=o",
            &json!({"storageClass":"REGIONAL"}),
        ),
        "REGIONAL",
        "US",
    );
    assert_eq!(upload(&server, "multi", &json!({"name":"plain"})).0, 200);
    assert_refused(
        send(
            &server,
            "POST",
            "/storage/v1/b/multi/o/plain/rewriteTo/b/multi/o/rewritten",
            &json!({"storageClass":"REGIONAL"}),
        ),
        "REGIONAL",
        "US",
    );
    let (status, rewritten) = send(
        &server,
        "POST",
        "/storage/v1/b/regional/o/inherited/rewriteTo/b/regional/o/standard",
        &json!({"storageClass":"STANDARD"}),
    );
    assert_eq!(
        (status, &rewritten["resource"]["storageClass"]),
        (200, &json!("STANDARD"))
    );
    for object in ["o", "rewritten"] {
        let path = format!("/storage/v1/b/multi/o/{object}");
        assert_eq!(get(&server, &path).0, 404, "{object}");
    }
}

#[test]
fn an_xml_put_names_the_conflict_in_its_details() {
    let (_dir, server) = setup();
    let refused = server.request(
        "PUT",
        "/multi/object",
        "x-goog-storage-class: REGIONAL\r\n",
        b"x",
    );
    assert_eq!(refused.status, 400);
    let body = String::from_utf8(refused.body).unwrap();
    for element in [
        "<Code>InvalidLocationConstraintStorageClassCombination</Code>".to_owned(),
        "<Message>The combination of location and storage class specified is not supported for your project. In particular, regional locations do not support Multi-Regional Storage, and multi-regional locations do not support Regional Storage.</Message>".to_owned(),
        format!("<Details>{}</Details>", refusal("REGIONAL", "US")),
    ] {
        assert!(body.contains(&element), "{body}");
    }
    server
        .request(
            "PUT",
            "/regional/object",
            "x-goog-storage-class: REGIONAL\r\n",
            b"x",
        )
        .expect(200);
    let head = server.request("HEAD", "/regional/object", "", b"");
    assert_eq!(head.headers["x-goog-storage-class"], "REGIONAL");
}

#[test]
fn compose_components_must_share_the_destination_class() {
    let (_dir, server) = setup();
    assert_eq!(
        upload(
            &server,
            "regional",
            &json!({"name":"s","storageClass":"STANDARD"})
        )
        .0,
        200
    );
    assert_eq!(upload(&server, "regional", &json!({"name":"d"})).0, 200);
    // Without a named class the destination takes the first component's.
    let (status, body) = send(
        &server,
        "POST",
        "/storage/v1/b/regional/o/composed/compose",
        &json!({"sourceObjects":[{"name":"s"},{"name":"d"}]}),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["errors"][0]["reason"], "invalid");
    assert_eq!(
        body["error"]["message"],
        "Cannot compose object with destination storage class STANDARD from source components that have different storage classes (REGIONAL)."
    );
}

#[test]
fn a_lifecycle_class_must_fit_the_bucket_location() {
    let (_dir, server) = setup();
    let set_class = |class: &str| json!({"action":{"type":"SetStorageClass","storageClass":class},"condition":{"matchesStorageClass":["DURABLE_REDUCED_AVAILABILITY"]}});
    let matching = |class: &str| json!({"action":{"type":"Delete"},"condition":{"age":3650,"matchesStorageClass":[class]}});
    for (bucket, rule, class, location) in [
        ("multi", set_class("REGIONAL"), "REGIONAL", "US"),
        ("multi", matching("REGIONAL"), "REGIONAL", "US"),
        (
            "regional",
            set_class("MULTI_REGIONAL"),
            "MULTI_REGIONAL",
            "US-CENTRAL1",
        ),
        (
            "regional",
            matching("MULTI_REGIONAL"),
            "MULTI_REGIONAL",
            "US-CENTRAL1",
        ),
    ] {
        let path = format!("/storage/v1/b/{bucket}");
        assert_refused(
            send(
                &server,
                "PATCH",
                &path,
                &json!({"lifecycle":{"rule":[rule]}}),
            ),
            class,
            location,
        );
        assert!(get(&server, &path).1.get("lifecycle").is_none(), "{bucket}");
    }
    for (bucket, rule) in [
        ("multi", set_class("MULTI_REGIONAL")),
        ("regional", set_class("REGIONAL")),
        ("regional", matching("REGIONAL")),
    ] {
        let path = format!("/storage/v1/b/{bucket}");
        let lifecycle = json!({"rule":[rule]});
        let (status, body) = send(&server, "PATCH", &path, &json!({"lifecycle":lifecycle}));
        assert_eq!((status, &body["lifecycle"]), (200, &lifecycle), "{body}");
    }
}
