mod object_effects_support;
use object_effects_support::{Reply, Server, common::TestDir, request, settings};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Barrier},
    thread,
};

const AUTH: &str = "Authorization: Bearer admin\r\n";

fn setup(strict: bool) -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    (dir, server)
}

fn compose(server: &Server, name: &str, body: Value) -> Reply {
    server.request(
        "POST",
        &format!("/storage/v1/b/test-bucket/o/{name}/compose"),
        AUTH,
        body.to_string().as_bytes(),
    )
}

fn sources(names: &[&str]) -> Value {
    json!({"sourceObjects": names.iter().map(|name| json!({"name": name})).collect::<Vec<_>>()})
}

fn media(server: &Server, name: &str) -> Reply {
    server.request(
        "GET",
        &format!("/storage/v1/b/test-bucket/o/{name}?alt=media"),
        AUTH,
        b"",
    )
}

#[test]
fn compose_order_checksums_metadata_fields_and_restart() {
    let (dir, mut server) = setup(false);
    let first = server.put("a", b"1234").json();
    server.put("b", b"56789").json();
    let body = json!({"sourceObjects": [{"name":"a"}, {"name":"b"}], "destination": {
        "contentType":"application/x-composite", "metadata":{"key":"value"}, "contentLanguage":"en",
        "md5Hash":"ignored", "crc32c":"ignored"
    }});
    let result = compose(&server, "dir%2F%E9%9B%AA%25", body).json();
    assert_eq!(result["name"], "dir/雪%");
    assert_eq!(result["size"], "9");
    assert_eq!(result["componentCount"], 2);
    assert_eq!(result["crc32c"], "4waSgw==");
    assert!(result.get("md5Hash").is_none());
    assert_eq!(result["contentType"], "application/x-composite");
    assert_eq!(result["metadata"]["key"], "value");
    assert_eq!(result["metageneration"], "1");
    assert_ne!(result["generation"], first["generation"]);
    assert!(result.get("owner").is_none() && result.get("acl").is_none());
    let bytes = media(&server, "dir%2F%E9%9B%AA%25");
    assert_eq!(bytes.body, b"123456789");
    assert_eq!(bytes.headers["x-goog-hash"], "crc32c=4waSgw==");
    let xml = server.request("GET", "/test-bucket/dir%2F%E9%9B%AA%25", "", b"");
    assert_eq!(xml.status, 200);
    assert_eq!(xml.body, b"123456789");
    assert!(!xml.headers["x-goog-hash"].contains("md5="));
    let selected = server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/dir%2F%E9%9B%AA%25?fields=componentCount,crc32c,md5Hash",
            AUTH,
            b"",
        )
        .json();
    assert_eq!(selected, json!({"componentCount":2,"crc32c":"4waSgw=="}));
    let listed = server.list("fields=items(name,componentCount,md5Hash)");
    let listed = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "dir/雪%")
        .unwrap();
    assert_eq!(listed["componentCount"], 2);
    assert!(listed.get("md5Hash").is_none());
    let updated = server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/dir%2F%E9%9B%AA%25",
            AUTH,
            br#"{"metadata":{"changed":"yes"}}"#,
        )
        .json();
    assert_eq!(updated["componentCount"], 2);
    assert!(updated.get("md5Hash").is_none());
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("dir%2F%E9%9B%AA%25").json(), updated);
    assert_eq!(media(&server, "dir%2F%E9%9B%AA%25").body, b"123456789");
    assert_eq!(server.get("a").json(), first);
}

#[test]
fn compose_one_empty_repeated_and_nested_sources_exceed_old_component_limit() {
    let (_dir, server) = setup(false);
    server.put("empty", b"").json();
    server.put("a", b"a").json();
    let empty = compose(&server, "empty-composite", sources(&["empty"])).json();
    assert_eq!(empty["componentCount"], 1);
    assert_eq!(empty["crc32c"], "AAAAAA==");
    assert!(empty.get("md5Hash").is_none());
    assert!(media(&server, "empty-composite").body.is_empty());
    let mut count = 1;
    let mut source = "a".to_owned();
    for index in 0..3 {
        let name = format!("nested-{index}");
        let body = json!({"sourceObjects": vec![json!({"name":source}); 32]});
        let object = compose(&server, &name, body).json();
        count *= 32;
        assert_eq!(object["componentCount"], count);
        assert_eq!(object["size"], count.to_string());
        source = name;
    }
    assert_eq!(media(&server, &source).body, vec![b'a'; count]);
    let uploaded = server.put(&source, b"ordinary").json();
    assert!(uploaded.get("componentCount").is_none());
    assert!(uploaded.get("md5Hash").is_some());
}

#[test]
fn compose_source_and_destination_preconditions_preserve_failed_overwrites() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let first = server.put("source", b"old").json();
    let next = server.put("source", b"new").json();
    let old_destination = server.put("destination", b"preserved").json();
    for body in [
        sources(&["missing"]),
        json!({"sourceObjects":[{"name":"source","generation":"1"}]}),
        json!({"sourceObjects":[{"name":"source","objectPreconditions":{"ifGenerationMatch": first["generation"]}}]}),
        json!({"sourceObjects":[{"name":"source","generation": first["generation"],"objectPreconditions":{"ifGenerationMatch":next["generation"]}}]}),
    ] {
        let reply = compose(&server, "destination", body);
        assert!(matches!(reply.status, 404 | 412));
        assert_eq!(server.get("destination").json(), old_destination);
    }
    for query in [
        "ifGenerationMatch=0",
        "ifGenerationMatch=1",
        "ifMetagenerationMatch=2",
    ] {
        let reply = server.request(
            "POST",
            &format!("/storage/v1/b/test-bucket/o/destination/compose?{query}"),
            AUTH,
            sources(&["source"]).to_string().as_bytes(),
        );
        reply.error(412, "conditionNotMet");
        assert_eq!(server.get("destination").json(), old_destination);
    }
    let pinned = compose(&server, "destination", json!({"sourceObjects":[{"name":"source", "generation":first["generation"], "objectPreconditions":{"ifGenerationMatch":first["generation"]}}]})).json();
    assert_eq!(media(&server, "destination").body, b"old");
    assert_ne!(pinned["generation"], old_destination["generation"]);
    assert_eq!(
        server
            .get(&format!(
                "destination?generation={}",
                old_destination["generation"].as_str().unwrap()
            ))
            .json()["size"],
        "9"
    );
}

#[test]
fn compose_hard_deletion_is_atomic_deduplicated_and_preserves_new_destination() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let a_old = server.put("a", b"old").json();
    let a = server.put("a", b"a").json();
    let b = server.put("b", b"b").json();
    let destination = server.put("destination", b"d").json();
    let held = server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/b",
            AUTH,
            br#"{"eventBasedHold":true}"#,
        )
        .json();
    let body = json!({"sourceObjects":[{"name":"a"},{"name":"a"},{"name":"b"},{"name":"destination"}],"deleteSourceObjects":true});
    compose(&server, "destination", body.clone()).error(403, "objectUnderActiveHold");
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(server.get("a").json(), a);
    assert_eq!(server.get("b").json(), held);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/b",
            AUTH,
            br#"{"eventBasedHold":false}"#,
        )
        .json();
    let mut missing = body.clone();
    missing["sourceObjects"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"missing"}));
    compose(&server, "destination", missing).error(404, "notFound");
    assert_eq!(server.get("a").json(), a);
    let result = compose(&server, "destination", body).json();
    assert_eq!(result["componentCount"], 4);
    assert_eq!(media(&server, "destination").body, b"aabd");
    server.get("a").error(404, "notFound");
    server.get("b").error(404, "notFound");
    server
        .get(&format!(
            "a?generation={}",
            a["generation"].as_str().unwrap()
        ))
        .error(404, "notFound");
    server
        .get(&format!(
            "b?generation={}",
            b["generation"].as_str().unwrap()
        ))
        .error(404, "notFound");
    let retained_destination = server
        .get(&format!(
            "destination?generation={}",
            destination["generation"].as_str().unwrap()
        ))
        .json();
    assert_eq!(retained_destination["size"], "1");
    assert!(retained_destination.get("timeDeleted").is_some());
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "/storage/v1/b/test-bucket/o/a?generation={}&alt=media",
                    a_old["generation"].as_str().unwrap()
                ),
                AUTH,
                b""
            )
            .body,
        b"old"
    );
}

#[test]
fn compose_rejects_invalid_or_unsupported_input_without_mutation() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json();
    let destination = server.put("destination", b"preserved").json();
    for body in [
        json!({}),
        json!({"sourceObjects":[]}),
        json!({"sourceObjects":vec![json!({"name":"a"});33]}),
        json!({"sourceObjects":[{"name":"a","generation":-1}]}),
    ] {
        assert_eq!(compose(&server, "destination", body).status, 400);
    }
    for query in [
        "destinationPredefinedAcl=publicRead",
        "kmsKeyName=key",
        "userProject=billed",
        "dropContextGroups=custom",
        "ifGenerationNotMatch=1",
    ] {
        server
            .request(
                "POST",
                &format!("/storage/v1/b/test-bucket/o/destination/compose?{query}"),
                AUTH,
                sources(&["a"]).to_string().as_bytes(),
            )
            .error(501, "notImplemented");
    }
    for destination_body in [
        json!({"acl":[]}),
        json!({"kmsKeyName":"key"}),
        json!({"retention":{}}),
        json!({"contexts":{}}),
        json!({"temporaryHold":true}),
        json!({"storageClass":"NEARLINE"}),
    ] {
        compose(
            &server,
            "destination",
            json!({"sourceObjects":[{"name":"a"}],"destination":destination_body}),
        )
        .error(501, "notImplemented");
    }
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?fields=missingField",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .error(400, "invalidParameter");
    assert_eq!(server.get("destination").json(), destination);
}

#[test]
fn compose_strict_requires_both_read_create_and_delete_for_destructive_cases() {
    let (_dir, server) = setup(true);
    server.put("a", b"a").json();
    for (token, status, reason) in [
        ("", 401, "required"),
        ("viewer", 403, "forbidden"),
        ("creator", 403, "forbidden"),
    ] {
        let auth = if token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {token}\r\n")
        };
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/destination/compose",
                &auth,
                sources(&["a"]).to_string().as_bytes(),
            )
            .error(status, reason);
    }
    server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["user:creator@example.test"]}]}"#).json();
    let auth = "Authorization: Bearer creator\r\n";
    let created = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            auth,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            auth,
            sources(&["a"]).to_string().as_bytes(),
        )
        .error(403, "forbidden");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/deleted/compose",
            auth,
            br#"{"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}"#,
        )
        .error(403, "forbidden");
    assert_eq!(server.get("destination").json(), created);
    assert_eq!(media(&server, "a").body, b"a");
}

#[test]
fn compose_concurrent_create_if_absent_has_one_winner() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json();
    let barrier = Arc::new(Barrier::new(8));
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let address = server.address.clone();
            thread::spawn(move || {
                barrier.wait();
                request(
                    &address,
                    "POST",
                    "/storage/v1/b/test-bucket/o/destination/compose?ifGenerationMatch=0",
                    AUTH,
                    br#"{"sourceObjects":[{"name":"a"}]}"#,
                )
                .status
            })
        })
        .collect();
    let statuses: Vec<_> = requests
        .into_iter()
        .map(|request| request.join().unwrap())
        .collect();
    assert_eq!(statuses.iter().filter(|status| **status == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|status| **status == 412).count(), 7);
    assert_eq!(media(&server, "destination").body, b"a");
}

#[test]
fn compose_storage_class_defaults_and_event_holds() {
    let (dir, server) = setup(false);
    server.put("standard", b"s").json();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            AUTH,
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json();
    server.put("nearline", b"n").json();
    let same_class = compose(&server, "same-class", sources(&["standard"])).json();
    assert_eq!(same_class["storageClass"], "STANDARD");
    let mixed = compose(&server, "mixed", sources(&["standard", "nearline"]));
    assert_eq!(mixed.status, 400);
    server.get("mixed").error(404, "notFound");
    settings(&dir, false, true);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/standard",
            AUTH,
            br#"{"eventBasedHold":true}"#,
        )
        .json();
    let held = compose(&server, "held", sources(&["standard"])).json();
    assert_eq!(held["eventBasedHold"], true);
    compose(&server, "held", sources(&["standard"])).error(403, "objectUnderActiveHold");
    let released = compose(
        &server,
        "released",
        json!({"sourceObjects":[{"name":"standard"}],"destination":{"eventBasedHold":false}}),
    )
    .json();
    assert!(released.get("eventBasedHold").is_none());
}

#[test]
fn compose_method_body_and_selector_boundaries() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json();
    let method = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/destination/compose",
        AUTH,
        b"",
    );
    method.error(405, "methodNotAllowed");
    assert_eq!(method.headers["allow"], "POST");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            AUTH,
            b"{",
        )
        .error(400, "parseError");
    let oversized = vec![b' '; 65537];
    assert_eq!(
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/destination/compose",
                AUTH,
                &oversized
            )
            .status,
        413
    );
    let selected = server
        .request(
            "POST",
        "/storage/v1/b/test-bucket/o/destination/compose?projection=full&fields=name,componentCount,md5Hash",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json();
    assert_eq!(selected, json!({"name":"destination","componentCount":1}));
}
