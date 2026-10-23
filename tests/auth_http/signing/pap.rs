use super::*;

const BUCKET: &str = "/storage/v1/b/iam-bucket";

fn setting(server: &Server, state: &str) -> Value {
    server
        .request(
            "PATCH",
            BUCKET,
            ADMIN,
            json!({"iamConfiguration":{"publicAccessPrevention":state}})
                .to_string()
                .as_bytes(),
        )
        .ok()
        .json()
}

#[test]
fn pap_metadata_insert_patch_update_fields_conditions_and_restart() {
    for profile in ["native", "legacy-emulator"] {
        let config = TestDir::new();
        let identities = identity_file(&config);
        let data = TestDir::new();
        let args = [
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
            "--compat-profile",
            profile,
        ];
        let (mut server, _) = Server::start(&data, &args);
        let create =
            json!({"name":"iam-bucket", "iamConfiguration":{"publicAccessPrevention":"enforced"}});
        let created = server
            .request(
                "POST",
                "/storage/v1/b?project=project-a",
                ADMIN,
                create.to_string().as_bytes(),
            )
            .ok()
            .json();
        assert_eq!(
            created["iamConfiguration"],
            json!({"publicAccessPrevention":"enforced"})
        );
        assert_eq!(server.get(ADMIN, BUCKET).ok().json(), created);
        assert_eq!(server.get(ADMIN, "/storage/v1/b?project=project-a&fields=items(iamConfiguration/publicAccessPrevention)").ok().json(),
            json!({"items":[{"iamConfiguration":{"publicAccessPrevention":"enforced"}}]}));
        assert_eq!(
            server
                .get(
                    ADMIN,
                    &format!("{BUCKET}?fields=iamConfiguration/publicAccessPrevention")
                )
                .ok()
                .json(),
            json!({"iamConfiguration":created["iamConfiguration"]})
        );
        for method in ["PATCH", "PUT"] {
            let before = server.get(ADMIN, BUCKET).ok().json();
            for condition in [
                "ifMetagenerationMatch=0",
                &format!(
                    "ifMetagenerationNotMatch={}",
                    before["metageneration"].as_str().unwrap()
                ),
            ] {
                let reply = server.request(
                    method,
                    &format!("{BUCKET}?{condition}"),
                    ADMIN,
                    br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#,
                );
                assert_eq!(reply.status, 412);
                assert_eq!(
                    reply.json()["error"]["errors"][0]["reason"],
                    "conditionNotMet"
                );
                assert_eq!(server.get(ADMIN, BUCKET).ok().json(), before);
            }
            let meta = before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap();
            let response = server.request(method, &format!("{BUCKET}?ifMetagenerationMatch={meta}&fields=iamConfiguration,metageneration"), ADMIN,
                br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#).ok().json();
            assert_eq!(
                response,
                json!({"iamConfiguration":{"publicAccessPrevention":"inherited"},"metageneration":(meta+1).to_string()})
            );
            setting(&server, "enforced");
        }
        let patched = server
            .request(
                "PATCH",
                BUCKET,
                ADMIN,
                br#"{"iamConfiguration":{"future":true}}"#,
            )
            .ok()
            .json();
        assert_eq!(
            patched["iamConfiguration"]["publicAccessPrevention"],
            "enforced"
        );
        let replaced = server.request("PUT", BUCKET, ADMIN, b"{}").ok().json();
        assert_eq!(
            replaced["iamConfiguration"]["publicAccessPrevention"],
            "inherited"
        );
        let before = setting(&server, "enforced");
        server.stop();
        let (server, _) = Server::start(&data, &args);
        assert_eq!(server.get(ADMIN, BUCKET).ok().json(), before);
        server.create_bucket(ADMIN, "project-a", "default-pap").ok();
        assert_eq!(
            server.get(ADMIN, "/storage/v1/b/default-pap").ok().json()["iamConfiguration"]["publicAccessPrevention"],
            "inherited"
        );
    }
}

#[test]
fn pap_invalid_and_unsupported_settings_preserve_resources() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, BUCKET).ok().json();
    for method in ["PATCH", "PUT", "POST"] {
        for value in [
            Value::Null,
            json!(false),
            json!([]),
            json!({"publicAccessPrevention":null}),
            json!({"publicAccessPrevention":true}),
            json!({"publicAccessPrevention":"unspecified"}),
            json!({"publicAccessPrevention":""}),
            json!({"publicAccessPrevention":"ENFORCED"}),
            json!({"publicAccessPrevention":"disabled"}),
        ] {
            let body = json!({"name":"invalid-pap","iamConfiguration":value});
            let path = if method == "POST" {
                "/storage/v1/b?project=project-a"
            } else {
                BUCKET
            };
            server
                .request(method, path, ADMIN, body.to_string().as_bytes())
                .error(400, "invalidArgument");
            assert_eq!(server.get(ADMIN, BUCKET).ok().json(), before);
            server
                .get(ADMIN, "/storage/v1/b/invalid-pap")
                .error(404, "notFound");
        }
        for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
            for value in [
                Value::Null,
                json!({}),
                json!({"enabled":false}),
                json!({"enabled":true}),
            ] {
                let path = if method == "POST" {
                    "/storage/v1/b?project=project-a"
                } else {
                    BUCKET
                };
                server.request(method,path,ADMIN,json!({"name":"invalid-pap","iamConfiguration":{field:value,"publicAccessPrevention":"enforced"}}).to_string().as_bytes()).error(501,"notImplemented");
                assert_eq!(server.get(ADMIN, BUCKET).ok().json(), before);
            }
        }
    }
}

#[test]
fn pap_suppresses_public_reads_writes_preserves_bindings_and_reactivates() {
    for member in ["allUsers", "allAuthenticatedUsers"] {
        let (_config, _data, server) = fixture();
        let policy = iam::set(
            &server,
            ADMIN,
            &json!({"bindings":[iam::binding("roles/storage.objectAdmin",member)]}),
        )
        .ok()
        .json();
        let auth = if member == "allUsers" { "" } else { NO_ROLES };
        assert_eq!(
            server.get(auth, "/iam-bucket/object").ok().body,
            b"hello world"
        );
        server
            .upload(auth, "iam-bucket", "public-json", b"public")
            .ok();
        server
            .request("PUT", "/iam-bucket/public-xml", auth, b"public")
            .ok();
        setting(&server, "enforced");
        for caller in ["", NO_ROLES] {
            for path in [
                "/storage/v1/b/iam-bucket/o/object",
                "/storage/v1/b/iam-bucket/o/object?alt=media",
                "/storage/v1/b/iam-bucket/o",
            ] {
                let reply = server.get(caller, path);
                reply.error(
                    if caller.is_empty() { 401 } else { 403 },
                    if caller.is_empty() {
                        "required"
                    } else {
                        "forbidden"
                    },
                );
            }
            for method in ["GET", "HEAD", "PUT"] {
                let reply = server.request(
                    method,
                    "/iam-bucket/object",
                    caller,
                    if method == "PUT" { b"denied" } else { b"" },
                );
                assert_eq!(reply.status, if caller.is_empty() { 401 } else { 403 });
                if method == "HEAD" {
                    assert!(reply.body.is_empty());
                }
            }
            let reply = server.upload(caller, "iam-bucket", "object", b"denied");
            assert_eq!(reply.status, if caller.is_empty() { 401 } else { 403 });
            let reply = server.request("DELETE", "/storage/v1/b/iam-bucket/o/object", caller, b"");
            assert_eq!(reply.status, if caller.is_empty() { 401 } else { 403 });
        }
        assert_eq!(
            server.get(VIEWER, "/iam-bucket/object").ok().body,
            b"hello world"
        );
        assert_eq!(
            server
                .get(VIEWER, "/storage/v1/b/iam-bucket/o/object?alt=media")
                .ok()
                .headers["cache-control"],
            "no-cache"
        );
        server
            .upload(CREATOR, "iam-bucket", "bearer", b"allowed")
            .ok();
        assert_eq!(server.get(ADMIN, iam::IAM).ok().json(), policy);
        iam::set(&server, ADMIN, &policy).error(412, "conditionNotMet");
        assert_eq!(server.get(ADMIN, iam::IAM).ok().json(), policy);
        for public in ["allUsers", "allAuthenticatedUsers"] {
            iam::set(
                &server,
                ADMIN,
                &json!({"bindings":[iam::binding("roles/storage.objectViewer",public)]}),
            )
            .error(412, "conditionNotMet");
        }
        setting(&server, "inherited");
        assert_eq!(
            server.get(auth, "/iam-bucket/object").ok().body,
            b"hello world"
        );
        server
            .upload(auth, "iam-bucket", "reactivated", b"again")
            .ok();
        assert_eq!(server.get(ADMIN, iam::IAM).ok().json(), policy);
        setting(&server, "enforced");
        iam::set(&server,ADMIN,&json!({"bindings":[iam::binding("roles/storage.objectViewer","user:no-roles@example.invalid")]})).ok();
        assert_eq!(
            server.get(NO_ROLES, "/iam-bucket/object").ok().body,
            b"hello world"
        );
    }
}

#[test]
fn pap_signed_get_head_put_use_signer_grants_and_reject_tampering() {
    let (_config, _data, server) = fixture();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectAdmin","allUsers")]}),
    )
    .ok();
    let now = SystemTime::now();
    let public_signer = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "no-roles@example.invalid",
        now,
        HeaderMap::new(),
    );
    send(&server, "GET", &public_signer, "").ok();
    setting(&server, "enforced");
    error(
        &send(&server, "GET", &public_signer, ""),
        403,
        "AccessDenied",
    );
    for method in [Method::GET, Method::HEAD] {
        let url = sign(
            &server,
            &method,
            "/iam-bucket/object",
            "viewer@example.invalid",
            now,
            HeaderMap::new(),
        );
        let reply = send(&server, method.as_str(), &url, "");
        reply.ok();
        assert_eq!(
            reply.body,
            if method == Method::HEAD {
                b"".as_slice()
            } else {
                b"hello world".as_slice()
            }
        );
        assert_eq!(
            send(&server, method.as_str(), &(url + "00"), "").status,
            403
        );
    }
    for (signer, path, status) in [
        ("creator@example.invalid", "/iam-bucket/signed-new", 200),
        ("admin@example.invalid", "/iam-bucket/object", 200),
        ("creator@example.invalid", "/iam-bucket/object", 403),
        ("viewer@example.invalid", "/iam-bucket/denied", 403),
        ("no-roles@example.invalid", "/iam-bucket/denied", 403),
    ] {
        let url = sign(&server, &Method::PUT, path, signer, now, HeaderMap::new());
        assert_eq!(
            put::request(&server, "PUT", &url, "", b"signed bytes").status,
            status
        );
    }
    let before = server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
        .ok()
        .json();
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/object",
        "admin@example.invalid",
        now,
        HeaderMap::new(),
    );
    error(
        &put::request(&server, "PUT", &(url + "00"), "", b"tampered"),
        403,
        "SignatureDoesNotMatch",
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
            .ok()
            .json(),
        before
    );
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/object").ok().body,
        b"signed bytes"
    );
    iam::set(&server,ADMIN,&json!({"bindings":[iam::binding("roles/storage.objectAdmin","user:no-roles@example.invalid")]})).ok();
    assert_eq!(
        send(&server, "GET", &public_signer, "").ok().body,
        b"signed bytes"
    );
}

#[test]
fn pap_parent_fixture_inheritance_restart_and_release() {
    let (config, data, mut server) = fixture();
    let policy = iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectAdmin","allUsers")]}),
    )
    .ok()
    .json();
    server
        .create_bucket(ADMIN, "project-b", "explicit-pap")
        .ok();
    server
        .request(
            "PATCH",
            "/storage/v1/b/explicit-pap",
            ADMIN,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .ok();
    server.stop();
    let path = config.0.join("identities.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["public_access_prevention_projects"] = json!(["project-a"]);
    std::fs::write(&path, value.to_string()).unwrap();
    let mut server = Server::strict(&data, &path);
    assert_eq!(
        server.get(ADMIN, BUCKET).ok().json()["iamConfiguration"]["publicAccessPrevention"],
        "inherited"
    );
    assert_eq!(server.get(ADMIN, iam::IAM).ok().json(), policy);
    assert_eq!(server.get("", "/iam-bucket/object").status, 401);
    iam::set(&server, ADMIN, &policy).error(412, "conditionNotMet");
    setting(&server, "enforced");
    setting(&server, "inherited");
    assert_eq!(server.get("", "/iam-bucket/object").status, 401);
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    assert_eq!(send(&server, "GET", &url, "").ok().body, b"hello world");
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/parent-signed",
        "creator@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    put::request(&server, "PUT", &url, "", b"parent signed").ok();
    server
        .create_bucket(ADMIN, "project-a", "new-inherited")
        .ok();
    server
        .request(
            "PUT",
            "/storage/v1/b/new-inherited/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .error(412, "conditionNotMet");
    server.create_bucket(ADMIN, "project-b", "unaffected").ok();
    server
        .request(
            "PUT",
            "/storage/v1/b/unaffected/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .ok();
    server.stop();
    value["public_access_prevention_projects"] = json!([]);
    std::fs::write(&path, value.to_string()).unwrap();
    let server = Server::strict(&data, &path);
    assert_eq!(
        server.get("", "/iam-bucket/object").ok().body,
        b"hello world"
    );
    assert_eq!(server.get(ADMIN, iam::IAM).ok().json(), policy);
    assert_eq!(
        server.get(ADMIN, "/storage/v1/b/explicit-pap").ok().json()["iamConfiguration"]["publicAccessPrevention"],
        "enforced"
    );
    server
        .request(
            "PUT",
            "/storage/v1/b/explicit-pap/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .error(412, "conditionNotMet");
}
