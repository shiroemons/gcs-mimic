use super::*;

const BUCKET: &str = "/storage/v1/b/iam-bucket";

fn setting(server: &Server, state: &str) -> Value {
    server
        .request(
            "PATCH",
            // The stored resource reads without ACL fields, which a PATCH adds by default.
            &format!("{BUCKET}?projection=noAcl"),
            ADMIN,
            json!({"iamConfiguration":{"publicAccessPrevention":state}})
                .to_string()
                .as_bytes(),
        )
        .success()
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
            .success()
            .json();
        assert_eq!(
            created["iamConfiguration"],
            json!({"publicAccessPrevention":"enforced","uniformBucketLevelAccess":{"enabled":false},"bucketPolicyOnly":{"enabled":false}})
        );
        assert_eq!(server.get(ADMIN, BUCKET).success().json(), created);
        assert_eq!(server.get(ADMIN, "/storage/v1/b?project=project-a&fields=items(iamConfiguration/publicAccessPrevention)").success().json(),
            json!({"items":[{"iamConfiguration":{"publicAccessPrevention":"enforced"}}]}));
        assert_eq!(
            server
                .get(
                    ADMIN,
                    &format!("{BUCKET}?fields=iamConfiguration/publicAccessPrevention")
                )
                .success()
                .json(),
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}})
        );
        for method in ["PATCH", "PUT"] {
            let before = server.get(ADMIN, BUCKET).success().json();
            let not_match = format!(
                "ifMetagenerationNotMatch={}",
                before["metageneration"].as_str().unwrap()
            );
            for condition in ["ifMetagenerationMatch=0", not_match.as_str()] {
                let reply = server.request(
                    method,
                    &format!("{BUCKET}?{condition}"),
                    ADMIN,
                    br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#,
                );
                // Cloud Storage answers a PATCH whose ifMetagenerationNotMatch names the
                // current value with 304 and no body (observed 2026-10-31); PUT stays 412.
                if method == "PATCH" && condition == not_match {
                    reply.expect(304);
                    assert!(reply.body.is_empty());
                } else {
                    reply.expect(412);
                    assert_eq!(
                        reply.json()["error"]["errors"][0]["reason"],
                        "conditionNotMet"
                    );
                }
                assert_eq!(server.get(ADMIN, BUCKET).success().json(), before);
            }
            let meta = before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap();
            let response = server.request(method, &format!("{BUCKET}?ifMetagenerationMatch={meta}&fields=iamConfiguration,metageneration"), ADMIN,
                br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#).success().json();
            assert_eq!(
                response,
                json!({"iamConfiguration":{"publicAccessPrevention":"inherited","uniformBucketLevelAccess":{"enabled":false},"bucketPolicyOnly":{"enabled":false}},"metageneration":(meta+1).to_string()})
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
            .success()
            .json();
        assert_eq!(
            patched["iamConfiguration"]["publicAccessPrevention"],
            "enforced"
        );
        let replaced = server.request("PUT", BUCKET, ADMIN, b"{}").success().json();
        assert_eq!(
            replaced["iamConfiguration"]["publicAccessPrevention"],
            "enforced"
        );
        let before = setting(&server, "enforced");
        server.stop();
        let (server, _) = Server::start(&data, &args);
        assert_eq!(server.get(ADMIN, BUCKET).success().json(), before);
        server
            .create_bucket(ADMIN, "project-a", "default-pap")
            .success();
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/default-pap")
                .success()
                .json()["iamConfiguration"]["publicAccessPrevention"],
            "inherited"
        );
    }
}

#[test]
fn pap_invalid_and_unsupported_settings_preserve_resources() {
    let (_config, _data, server) = fixture();
    let before = server.get(ADMIN, BUCKET).success().json();
    for method in ["PATCH", "PUT", "POST"] {
        for value in [
            json!(false),
            json!([]),
            json!({"publicAccessPrevention":true}),
            json!({"publicAccessPrevention":"unspecified"}),
            json!({"publicAccessPrevention":""}),
            json!({"publicAccessPrevention":"ENFORCED"}),
            json!({"publicAccessPrevention":"disabled"}),
        ] {
            // Only a POST names a bucket; a body that names another one disagrees with the path.
            let body = if method == "POST" {
                json!({"name":"invalid-pap","iamConfiguration":value})
            } else {
                json!({"iamConfiguration":value})
            };
            let path = if method == "POST" {
                "/storage/v1/b?project=project-a"
            } else {
                BUCKET
            };
            server
                .request(method, path, ADMIN, body.to_string().as_bytes())
                .auth_error(400, "invalid");
            assert_eq!(server.get(ADMIN, BUCKET).success().json(), before);
            server
                .get(ADMIN, "/storage/v1/b/invalid-pap")
                .auth_error(404, "notFound");
        }
        for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
            for value in [json!({"enabled":[]}), json!({"enabled":"false"})] {
                let path = if method == "POST" {
                    "/storage/v1/b?project=project-a"
                } else {
                    BUCKET
                };
                let mut body =
                    json!({"iamConfiguration":{field:value,"publicAccessPrevention":"enforced"}});
                if method == "POST" {
                    body["name"] = json!("invalid-pap");
                }
                server
                    .request(method, path, ADMIN, body.to_string().as_bytes())
                    .auth_error(400, "invalid");
                assert_eq!(server.get(ADMIN, BUCKET).success().json(), before);
            }
        }
    }
    // A null iamConfiguration, or a null publicAccessPrevention inside it, restates nothing
    // (recorded 2026-10-31).
    for value in [Value::Null, json!({"publicAccessPrevention":null})] {
        let patched = server
            .request(
                "PATCH",
                BUCKET,
                ADMIN,
                json!({"iamConfiguration":value}).to_string().as_bytes(),
            )
            .success()
            .json();
        assert_eq!(patched["iamConfiguration"], before["iamConfiguration"]);
    }
}

#[test]
fn pap_disabled_uniform_access_noop_matches_python_client() {
    let (_config, _data, server) = fixture();
    for method in ["POST", "PATCH", "PUT"] {
        let path = if method == "POST" {
            "/storage/v1/b?project=project-a"
        } else {
            BUCKET
        };
        let mut body = json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false},"publicAccessPrevention":"enforced"}});
        if method == "POST" {
            body["name"] = json!("python-pap");
        }
        let result = server
            .request(method, path, ADMIN, body.to_string().as_bytes())
            .success()
            .json();
        assert_eq!(
            result["iamConfiguration"]["publicAccessPrevention"],
            "enforced"
        );
    }
    server.request("PATCH", BUCKET, ADMIN, br#"{"iamConfiguration":{"bucketPolicyOnly":{"enabled":false},"publicAccessPrevention":"inherited"}}"#).success();
    assert_eq!(
        server.get(ADMIN, BUCKET).success().json()["iamConfiguration"]["publicAccessPrevention"],
        "inherited"
    );
}

#[test]
fn pap_resumable_finalization_rechecks_public_initiator() {
    for member in ["allUsers", "allAuthenticatedUsers"] {
        let (_config, _data, server) = fixture();
        iam::set(
            &server,
            ADMIN,
            &json!({"bindings":[iam::binding("roles/storage.objectAdmin",member)]}),
        )
        .success();
        let caller = if member == "allUsers" { "" } else { NO_ROLES };
        let before = server
            .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
            .success()
            .json();
        let admitted = server.request(
            "POST",
            "/upload/storage/v1/b/iam-bucket/o?uploadType=resumable&name=object",
            &format!("{caller}Content-Type: application/json\r\n"),
            b"{}",
        );
        admitted.success();
        let location: Uri = admitted.headers["location"].parse().unwrap();
        let path = location.path_and_query().unwrap().as_str();
        setting(&server, "enforced");
        // The session URI is a capability, but publication still checks its initiating principal.
        server
            .request("PUT", path, "Content-Range: bytes 0-2/3\r\n", b"new")
            .auth_error(403, "forbidden");
        assert_eq!(
            server
                .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
                .success()
                .json(),
            before
        );
        assert_eq!(
            server.get(ADMIN, "/iam-bucket/object").success().body,
            b"hello world"
        );
    }
}

#[test]
fn pap_copy_rewrite_compose_recheck_public_source_and_destination_grants() {
    for member in ["allUsers", "allAuthenticatedUsers"] {
        for enforced_bucket in ["iam-bucket", "destination-bucket"] {
            let (_config, _data, server) = fixture();
            server
                .create_bucket(ADMIN, "project-a", "destination-bucket")
                .success();
            let payload = vec![b'x'; 1048576 + 7];
            let source = server
                .upload(ADMIN, "iam-bucket", "source", &payload)
                .success()
                .json();
            let destination = server
                .upload(ADMIN, "destination-bucket", "destination", b"old")
                .success()
                .json();
            for bucket in ["iam-bucket", "destination-bucket"] {
                server
                    .request(
                        "PUT",
                        &format!("/storage/v1/b/{bucket}/iam"),
                        ADMIN,
                        json!({"bindings":[iam::binding("roles/storage.objectAdmin",member)]})
                            .to_string()
                            .as_bytes(),
                    )
                    .success();
            }
            let caller = if member == "allUsers" { "" } else { NO_ROLES };
            let base = "/storage/v1/b/iam-bucket/o/source";
            let copy = format!("{base}/copyTo/b/destination-bucket/o/destination");
            let rewrite = format!("{base}/rewriteTo/b/destination-bucket/o/destination");
            let initial = server
                .request(
                    "POST",
                    &format!("{rewrite}?maxBytesRewrittenPerCall=1048576"),
                    caller,
                    br#"{"storageClass":"NEARLINE"}"#,
                )
                .success()
                .json();
            assert_eq!(initial["done"], false);
            let token = initial["rewriteToken"].as_str().unwrap();
            server
                .request(
                    "PATCH",
                    &format!("/storage/v1/b/{enforced_bucket}"),
                    ADMIN,
                    br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
                )
                .success();
            let (status, reason) = if caller.is_empty() {
                (401, "required")
            } else {
                (403, "forbidden")
            };
            for path in [
                copy,
                rewrite.clone(),
                format!("{rewrite}?rewriteToken={token}"),
            ] {
                server
                    .request("POST", &path, caller, b"{}")
                    .auth_error(status, reason);
            }
            server.request("POST", &format!("/storage/v1/b/{enforced_bucket}/o/composed/compose"), caller,
                json!({"deleteSourceObjects":true,"sourceObjects":[{"name":if enforced_bucket == "iam-bucket" {"source"} else {"destination"}}]}).to_string().as_bytes()).auth_error(status, reason);
            assert_eq!(
                server
                    .get(ADMIN, "/storage/v1/b/iam-bucket/o/source")
                    .success()
                    .json(),
                source
            );
            assert_eq!(
                server.get(ADMIN, "/iam-bucket/source").success().body,
                payload
            );
            assert_eq!(
                server
                    .get(ADMIN, "/storage/v1/b/destination-bucket/o/destination")
                    .success()
                    .json(),
                destination
            );
            assert_eq!(
                server
                    .get(ADMIN, "/destination-bucket/destination")
                    .success()
                    .body,
                b"old"
            );
            server
                .get(
                    ADMIN,
                    &format!("/storage/v1/b/{enforced_bucket}/o/composed"),
                )
                .auth_error(404, "notFound");
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
            &json!({"bindings":[iam::binding("roles/storage.admin",member)]}),
        )
        .success()
        .json();
        let auth = if member == "allUsers" { "" } else { NO_ROLES };
        assert_eq!(
            server.get(auth, "/iam-bucket/object").success().body,
            b"hello world"
        );
        server
            .upload(auth, "iam-bucket", "public-json", b"public")
            .success();
        server
            .request("PUT", "/iam-bucket/public-xml", auth, b"public")
            .success();
        setting(&server, "enforced");
        server
            .request(
                "PATCH",
                BUCKET,
                auth,
                br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#,
            )
            .auth_error(
                if auth.is_empty() { 401 } else { 403 },
                if auth.is_empty() {
                    "required"
                } else {
                    "forbidden"
                },
            );
        for caller in ["", NO_ROLES] {
            for path in [
                "/storage/v1/b/iam-bucket/o/object",
                "/storage/v1/b/iam-bucket/o/object?alt=media",
                "/storage/v1/b/iam-bucket/o",
            ] {
                let reply = server.get(caller, path);
                reply.auth_error(
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
            server.get(VIEWER, "/iam-bucket/object").success().body,
            b"hello world"
        );
        assert_eq!(
            server
                .get(VIEWER, "/storage/v1/b/iam-bucket/o/object?alt=media")
                .success()
                .headers["cache-control"],
            "no-cache, no-store, max-age=0, must-revalidate"
        );
        server
            .upload(CREATOR, "iam-bucket", "bearer", b"allowed")
            .success();
        assert_eq!(server.get(ADMIN, iam::IAM).success().json(), policy);
        iam::set(&server, ADMIN, &policy).auth_error(412, "conditionNotMet");
        assert_eq!(server.get(ADMIN, iam::IAM).success().json(), policy);
        for public in ["allUsers", "allAuthenticatedUsers"] {
            iam::set(
                &server,
                ADMIN,
                &json!({"bindings":[iam::binding("roles/storage.objectViewer",public)]}),
            )
            .auth_error(412, "conditionNotMet");
        }
        setting(&server, "inherited");
        assert_eq!(
            server.get(auth, "/iam-bucket/object").success().body,
            b"hello world"
        );
        server
            .upload(auth, "iam-bucket", "reactivated", b"again")
            .success();
        assert_eq!(server.get(ADMIN, iam::IAM).success().json(), policy);
        setting(&server, "enforced");
        iam::set(&server,ADMIN,&json!({"bindings":[iam::binding("roles/storage.objectViewer","user:no-roles@example.invalid")]})).success();
        assert_eq!(
            server.get(NO_ROLES, "/iam-bucket/object").success().body,
            b"hello world"
        );
    }
}

#[test]
fn pap_signed_requests_suppress_all_authenticated_users_grants() {
    let (_config, _data, server) = fixture();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectAdmin","allAuthenticatedUsers")]}),
    )
    .success();
    let before = server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
        .success()
        .json();
    let urls: Vec<_> = [Method::GET, Method::HEAD, Method::PUT]
        .into_iter()
        .map(|method| {
            let url = sign(
                &server,
                &method,
                "/iam-bucket/object",
                "no-roles@example.invalid",
                SystemTime::now(),
                HeaderMap::new(),
            );
            (method, url)
        })
        .collect();
    send(&server, "GET", &urls[0].1, "").success();
    setting(&server, "enforced");
    for (method, url) in urls {
        let reply = put::request(
            &server,
            method.as_str(),
            &url,
            "",
            if method == Method::PUT {
                b"denied"
            } else {
                b""
            },
        );
        reply.expect(403);
        if method != Method::HEAD {
            error(&reply, 403, "AccessDenied");
        }
    }
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/iam-bucket/o/object")
            .success()
            .json(),
        before
    );
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/object").success().body,
        b"hello world"
    );
}

#[test]
fn pap_signed_get_head_put_use_signer_grants_and_reject_tampering() {
    let (_config, _data, server) = fixture();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectAdmin","allUsers")]}),
    )
    .success();
    let now = SystemTime::now();
    let public_signer = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "no-roles@example.invalid",
        now,
        HeaderMap::new(),
    );
    send(&server, "GET", &public_signer, "").success();
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
        reply.success();
        assert_eq!(
            reply.body,
            if method == Method::HEAD {
                b"".as_slice()
            } else {
                b"hello world".as_slice()
            }
        );
        send(&server, method.as_str(), &(url + "00"), "").expect(403);
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
        .success()
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
            .success()
            .json(),
        before
    );
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/object").success().body,
        b"signed bytes"
    );
    iam::set(&server,ADMIN,&json!({"bindings":[iam::binding("roles/storage.objectAdmin","user:no-roles@example.invalid")]})).success();
    assert_eq!(
        send(&server, "GET", &public_signer, "").success().body,
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
    .success()
    .json();
    server
        .create_bucket(ADMIN, "project-b", "explicit-pap")
        .success();
    server
        .request(
            "PATCH",
            "/storage/v1/b/explicit-pap",
            ADMIN,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .success();
    server.stop();
    let path = config.0.join("identities.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["public_access_prevention_projects"] = json!(["project-a"]);
    std::fs::write(&path, value.to_string()).unwrap();
    let mut server = Server::strict(&data, &path);
    assert_eq!(
        server.get(ADMIN, BUCKET).success().json()["iamConfiguration"]["publicAccessPrevention"],
        "inherited"
    );
    assert_eq!(server.get(ADMIN, iam::IAM).success().json(), policy);
    server.get("", "/iam-bucket/object").expect(401);
    iam::set(&server, ADMIN, &policy).auth_error(412, "conditionNotMet");
    setting(&server, "enforced");
    setting(&server, "inherited");
    server.get("", "/iam-bucket/object").expect(401);
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "viewer@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    assert_eq!(
        send(&server, "GET", &url, "").success().body,
        b"hello world"
    );
    let url = sign(
        &server,
        &Method::PUT,
        "/iam-bucket/parent-signed",
        "creator@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    put::request(&server, "PUT", &url, "", b"parent signed").success();
    server
        .create_bucket(ADMIN, "project-a", "new-inherited")
        .success();
    server
        .request(
            "PUT",
            "/storage/v1/b/new-inherited/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .auth_error(412, "conditionNotMet");
    server
        .create_bucket(ADMIN, "project-b", "unaffected")
        .success();
    server
        .request(
            "PUT",
            "/storage/v1/b/unaffected/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .success();
    server.stop();
    value["public_access_prevention_projects"] = json!([]);
    std::fs::write(&path, value.to_string()).unwrap();
    let server = Server::strict(&data, &path);
    assert_eq!(
        server.get("", "/iam-bucket/object").success().body,
        b"hello world"
    );
    assert_eq!(server.get(ADMIN, iam::IAM).success().json(), policy);
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/explicit-pap")
            .success()
            .json()["iamConfiguration"]["publicAccessPrevention"],
        "enforced"
    );
    server
        .request(
            "PUT",
            "/storage/v1/b/explicit-pap/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .auth_error(412, "conditionNotMet");
}
