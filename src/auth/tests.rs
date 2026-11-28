use super::*;
use super::{authenticate::looks_like_jwt, registry::MAX_TOKEN_BYTES};
use crate::object::MapPatch;
use crate::storage::test_fixtures::{PERMISSIVE, live, permissive_upload};
use crate::test_support::TestDir;
use hyper::{HeaderMap, header::AUTHORIZATION};
use serde_json::Value;
use std::assert_matches;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn registry_errors_keep_their_diagnostics_and_sources() {
    use RegistryError::*;
    let cases = [
        Syntax(serde_json::from_slice::<bool>(b"").unwrap_err()),
        Invalid("missing identities".into()),
        InvalidEntry {
            entry: 2,
            reason: "missing id".into(),
        },
        PublicKey {
            entry: 3,
            defect: signature::InvalidPublicKey,
        },
        Project {
            entry: 4,
            project: "bad project".into(),
            defect: InvalidProjectId,
        },
    ];
    for error in cases {
        let expected = match &error {
            Syntax(_) => "EOF while parsing a value at line 1 column 0",
            Invalid(_) => "missing identities",
            InvalidEntry { .. } => "identities[2]: missing id",
            PublicKey { .. } => {
                "identities[3]: public_keys must contain only RSA SubjectPublicKeyInfo PEM (2048..8192 bits)"
            }
            Project { .. } => {
                "identities[4]: project \"bad project\": project must be a nonempty identifier of at most 256 bytes"
            }
        };
        assert_eq!(error.to_string(), expected);
        crate::error::assert_distinct_causes(&error);
        assert!(std::error::Error::source(&error).is_none());
    }
}

#[test]
fn compact_grants_keep_project_membership_and_legacy_upload_snapshots() {
    let registry = Registry::parse(br#"{"identities":[{"id":"hmac","tokens":[],"project_roles":{"project":["roles/storage.hmacKeyAdmin"]}}]}"#).unwrap();
    let identity = &registry.identities[0];
    assert!(identity.has_project_grant("project"));
    assert!(!identity.allows(OBJECTS_GET, "project"));
    let restored = Identity::from_upload_snapshot(
        &serde_json::json!({"id":"legacy","projects":{"project":["storage.objects.create"]}}),
    )
    .unwrap();
    assert!(restored.allows(OBJECTS_CREATE, "project"));
    assert!(!restored.allows(OBJECTS_DELETE, "project"));
    assert_eq!(
        restored.upload_snapshot(),
        serde_json::json!({"id":"legacy","projects":{"project":["storage.objects.create"]}})
    );
    assert!(Permission::parse_testable(BUCKETS_ENABLE_OBJECT_RETENTION.as_str()).is_none());
    assert_eq!(
        BUCKETS_ENABLE_OBJECT_RETENTION,
        Permission::BucketsEnableObjectRetention
    );
}

#[test]
fn compact_grants_preserve_hmac_key_management_permissions() {
    let registry = Registry::parse(br#"{"identities":[{"id":"hmac","tokens":[],"project_roles":{"project":["roles/storage.hmacKeyAdmin"]}}]}"#).unwrap();
    let identity = &registry.identities[0];
    for permission in [
        Permission::HmacKeysCreate,
        Permission::HmacKeysDelete,
        Permission::HmacKeysGet,
        Permission::HmacKeysList,
        Permission::HmacKeysUpdate,
    ] {
        assert!(identity.allows(permission, "project"), "{permission}");
        assert!(Permission::parse_testable(permission.as_str()).is_none());
        let query = format!("permissions={permission}");
        let error = crate::json::test_permissions_query(Some(&query)).unwrap_err();
        assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST);
        assert_eq!(error.reason(), "badRequest");
        let object_uri = format!("/storage/v1/b/bucket/o/object/iam/testPermissions?{query}")
            .parse()
            .unwrap();
        assert!(crate::json::route_object(&hyper::Method::GET, &object_uri).is_none());
    }
}

#[test]
fn jwt_detection_requires_three_segments_and_a_json_header() {
    assert!(looks_like_jwt("eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln"));
    for token in [
        "admin-token",
        "ya29.opaque-oauth-access-token",
        "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0",
        "eyJhbGciOiJSUzI1NiJ9..c2ln",
        "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln.extra",
        "abc.def.ghi",
        "",
    ] {
        assert!(!looks_like_jwt(token), "{token}");
    }
}

#[test]
fn pap_parent_fixture_rejects_malformed_and_repeated_projects() {
    for projects in [
        serde_json::json!(null),
        serde_json::json!(true),
        serde_json::json!({}),
        serde_json::json!([1]),
        serde_json::json!([""]),
        serde_json::json!(["has space"]),
        serde_json::json!(["p", "p"]),
    ] {
        let value = serde_json::json!({"identities":[{"id":"local","tokens":["token"]}],"public_access_prevention_projects":projects});
        assert!(
            Registry::parse(value.to_string().as_bytes())
                .unwrap_err()
                .to_string()
                .contains("public_access_prevention_projects")
        );
    }
}

#[tokio::test]
async fn pap_mutations_require_update_and_set_iam_permissions() {
    use crate::{
        bucket::{BucketAccess, BucketMetadataUpdate, CatalogError, PublicAccessPrevention},
        storage::Catalog,
    };
    let pap = |enforced| {
        if enforced {
            PublicAccessPrevention::Enforced
        } else {
            PublicAccessPrevention::Inherited
        }
    };
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let record = crate::storage::test_fixtures::bucket("pap-permissions", "project");
    let record = catalog.insert_bucket(record).await.unwrap();
    let identity = Identity {
        id: "local".into(),
        acl_entity: None,
        projects: BTreeMap::from([("project".into(), [BUCKETS_UPDATE].into_iter().collect())])
            .into(),
    };
    let access = BucketAccess::Policy {
        identity: Some(identity),
        permission: crate::auth::Permission::BucketsUpdate,
        project: Some("project".into()),
    };
    for replace in [false, true] {
        let update = BucketMetadataUpdate {
            public_access_prevention: Some(crate::bucket::PublicAccessPrevention::Enforced),
            ..Default::default()
        };
        let result = if replace {
            catalog
                .replace_bucket("pap-permissions", update, Default::default(), &access)
                .await
        } else {
            catalog
                .patch_bucket("pap-permissions", update, Default::default(), &access)
                .await
        };
        assert_eq!(
            result,
            Err(CatalogError::Denied(
                access.denied_for(BUCKETS_SET_IAM, "pap-permissions")
            ))
        );
        assert_eq!(
            catalog
                .get_bucket("pap-permissions", PERMISSIVE)
                .await
                .unwrap(),
            Some(record.clone())
        );
    }
    for enforced in [false, true] {
        catalog
            .patch_bucket(
                "pap-permissions",
                BucketMetadataUpdate {
                    public_access_prevention: Some(pap(enforced)),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        for replace in [false, true] {
            for value in [None, Some(enforced), Some(!enforced)] {
                let before = catalog
                    .get_bucket("pap-permissions", PERMISSIVE)
                    .await
                    .unwrap()
                    .unwrap();
                let update = BucketMetadataUpdate {
                    public_access_prevention: value.map(pap),
                    labels: MapPatch::Merge(BTreeMap::from([(
                        "team".into(),
                        Some("storage".into()),
                    )])),
                    ..Default::default()
                };
                let result = if replace {
                    catalog
                        .replace_bucket("pap-permissions", update, Default::default(), &access)
                        .await
                } else {
                    catalog
                        .patch_bucket("pap-permissions", update, Default::default(), &access)
                        .await
                };
                let expected = value.unwrap_or(enforced);
                if expected == enforced {
                    let after = result.unwrap();
                    assert_eq!(after.public_access_prevention, pap(enforced));
                    assert_eq!(after.labels["team"], "storage");
                    assert_eq!(after.metageneration, before.metageneration + 1);
                } else {
                    assert_eq!(
                        result,
                        Err(CatalogError::Denied(
                            access.denied_for(BUCKETS_SET_IAM, "pap-permissions")
                        ))
                    );
                    assert_eq!(
                        catalog
                            .get_bucket("pap-permissions", PERMISSIVE)
                            .await
                            .unwrap(),
                        Some(before)
                    );
                }
            }
        }
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn resource_acl_echo_requires_read_permission_and_changes_require_set_permission() {
    use crate::{
        bucket::{BucketAccess, BucketMetadataUpdate, CatalogError},
        storage::Catalog,
    };
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let record = crate::storage::test_fixtures::bucket("acl-permissions", "project");
    catalog.insert_bucket(record).await.unwrap();
    for default_acl in [false, true] {
        for readable in [false, true] {
            let mut permissions = vec![BUCKETS_UPDATE];
            if readable {
                permissions.push(BUCKETS_GET_IAM);
            }
            let access = BucketAccess::Policy {
                identity: Some(Identity {
                    id: "local".into(),
                    acl_entity: None,
                    projects: BTreeMap::from([(
                        "project".into(),
                        permissions.into_iter().collect(),
                    )])
                    .into(),
                }),
                permission: Permission::BucketsUpdate,
                project: Some("project".into()),
            };
            for replace in [false, true] {
                for same in [false, true] {
                    let before = catalog
                        .get_bucket("acl-permissions", PERMISSIVE)
                        .await
                        .unwrap()
                        .unwrap();
                    let mut entries = crate::acl::project_private("project");
                    entries.reverse();
                    if !same {
                        entries[0].role = crate::acl::AclRole::Owner;
                    }
                    let mut update = BucketMetadataUpdate {
                        labels: MapPatch::Merge(BTreeMap::from([(
                            "changed".into(),
                            Some("yes".into()),
                        )])),
                        ..Default::default()
                    };
                    if default_acl {
                        update.default_object_acl = Some(entries);
                    } else {
                        update.acl = Some(entries);
                    }
                    let conditions = Default::default();
                    let result = if replace {
                        catalog
                            .replace_bucket("acl-permissions", update, conditions, &access)
                            .await
                    } else {
                        catalog
                            .patch_bucket("acl-permissions", update, conditions, &access)
                            .await
                    };
                    if readable && same {
                        let after = result.unwrap();
                        assert_eq!(after.metageneration, before.metageneration + 1);
                        assert_eq!(after.labels["changed"], "yes");
                    } else {
                        // Even a caller who may not read the ACL is refused for writing it.
                        assert_eq!(
                            result,
                            Err(CatalogError::Denied(
                                access.denied_for(BUCKETS_SET_IAM, "acl-permissions")
                            ))
                        );
                        assert_eq!(
                            catalog
                                .get_bucket("acl-permissions", PERMISSIVE)
                                .await
                                .unwrap()
                                .unwrap(),
                            before
                        );
                    }
                }
            }
        }
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn object_resource_acl_echo_and_replacement_keep_read_and_write_permissions_separate() {
    use crate::{
        object::UploadChunk,
        server::{State, Stop, TcpListeners, serve},
        storage::Catalog,
        test_support::TestDir,
        tls::ListenerSecurity,
    };
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn request(
        address: std::net::SocketAddr,
        method: &str,
        body: Value,
        token: &str,
    ) -> (u16, Value) {
        let body = body.to_string();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(format!("{method} /storage/v1/b/acl-permissions/o/object?projection=full HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        let response = std::str::from_utf8(&bytes).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        (
            head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            serde_json::from_str(body).unwrap(),
        )
    }

    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(crate::storage::test_fixtures::bucket(
            "acl-permissions",
            "project",
        ))
        .await
        .unwrap();
    let (sender, body) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(bytes::Bytes::from_static(
            b"original",
        ))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    drop(sender);
    catalog
        .put_object(permissive_upload("acl-permissions", "object", 100), body)
        .await
        .unwrap();
    let mut registry = Registry::parse(br#"{"identities":[{"id":"reader","tokens":["reader"]},{"id":"writer","tokens":["writer"]}]}"#).unwrap();
    Arc::make_mut(&mut registry.identities[0].projects).insert(
        "project".into(),
        [OBJECTS_UPDATE, OBJECTS_GET_IAM].into_iter().collect(),
    );
    Arc::make_mut(&mut registry.identities[1].projects).insert(
        "project".into(),
        [OBJECTS_UPDATE, OBJECTS_SET_IAM].into_iter().collect(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(State {
        api_base_url: format!("http://{address}"),
        ..State::for_test(catalog.clone(), Auth::strict(registry))
    });
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(serve(
        TcpListeners::public(listener, ListenerSecurity::Plain),
        crate::config::ConfigDraft {
            scheme: crate::config::Scheme::Http,
            ..Default::default()
        }
        .build(),
        state,
        Stop::on(async {
            stopped.await.unwrap();
            Ok(())
        }),
    ));
    let target = live("acl-permissions", "object");
    for method in ["PATCH", "PUT"] {
        let before = catalog
            .get_object(target.clone(), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        let mut acl = crate::acl::project_private("project");
        acl.reverse();
        let (status, response) = request(
            address,
            method,
            serde_json::json!({"acl":acl,"contentType":"text/plain"}),
            "reader",
        )
        .await;
        assert_eq!(status, 200, "{response}");
        assert_eq!(
            response["metageneration"],
            (before.metageneration.get() + 1).to_string()
        );
        assert_eq!(response["acl"].as_array().unwrap().len(), 3);
        let unchanged = catalog
            .get_object(target.clone(), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        let (status, response) = request(address, method, serde_json::json!({"acl":[{"entity":"allUsers","role":"READER"}],"contentType":"text/rejected"}), "reader").await;
        assert_eq!(status, 403, "{response}");
        assert_eq!(
            catalog
                .get_object(target.clone(), Default::default(), PERMISSIVE)
                .await
                .unwrap(),
            unchanged
        );
    }
    let replacement = serde_json::json!({"acl":[{"entity":"user-new@example.test","role":"OWNER"}],"contentType":"text/replaced"});
    for method in ["PATCH", "PUT"] {
        let (status, response) = request(address, method, replacement.clone(), "writer").await;
        assert_eq!(status, 200, "{response}");
        assert_eq!(response["contentType"], "text/replaced");
        assert!(response.get("acl").is_none());
        let acl = catalog
            .read_acl(
                crate::acl::AclTarget::Object {
                    bucket: "acl-permissions".into(),
                    name: "object".into(),
                    generation: None,
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let mut entries = acl.entries;
        entries.sort_by(|a, b| a.entity.cmp(&b.entity));
        assert_eq!(
            entries,
            vec![
                crate::acl::AclEntry {
                    entity: "project-owners-198358909953".parse().unwrap(),
                    role: crate::acl::AclRole::Owner
                },
                crate::acl::AclEntry {
                    entity: "user-new@example.test".parse().unwrap(),
                    role: crate::acl::AclRole::Owner
                },
            ]
        );
    }
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    catalog.close().await.unwrap();
}

/// Role order must be name order: stored policies list bindings in the order a map keyed by
/// role yields them, which used to be the names' order.
#[test]
fn role_snapshot_is_sorted_unique_storage_permission_data() {
    let names: Vec<_> = Role::ALL.iter().map(|role| role.as_str()).collect();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "{names:?}");
    assert!(Role::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(Role::ALL.len(), 20);
    for &role in Role::ALL {
        let permissions = role.permissions();
        assert!(role.as_str().starts_with("roles/storage."), "{role}");
        assert!(!permissions.is_empty(), "{role}");
        assert!(
            permissions.windows(2).all(|pair| pair[0] < pair[1]),
            "{role} permissions must be sorted and unique"
        );
        for permission in permissions {
            assert!(permission.starts_with("storage."), "{role}: {permission}");
            assert_eq!(permission.split('.').count(), 3, "{role}: {permission}");
        }
        assert_eq!(role.as_str().parse::<Role>(), Ok(role));
        assert_eq!(role.to_string(), role.as_str());
    }
    for basic in [
        "roles/owner",
        "roles/editor",
        "roles/viewer",
        "roles/storage.",
    ] {
        assert_eq!(basic.parse::<Role>(), Err(roles::UnknownRole), "{basic}");
    }
}

#[test]
fn evaluation_is_driven_by_snapshot_data() {
    // Grant every role in its own project and check each permission only via the data.
    let projects: Vec<_> = (0..Role::ALL.len())
        .map(|index| format!("project-{index}"))
        .collect();
    let project_roles: serde_json::Map<_, _> = Role::ALL
        .iter()
        .zip(&projects)
        .map(|(role, project)| (project.clone(), serde_json::json!([role.as_str()])))
        .collect();
    let registry = Registry::parse(
        serde_json::json!({ "identities": [{
            "id": "user@example.invalid",
            "tokens": ["token"],
            "project_roles": project_roles,
        }]})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let identity = &registry.identities[0];
    for (index, role) in Role::ALL.iter().enumerate() {
        for permission in Permission::ALL {
            assert_eq!(
                identity.allows(*permission, &projects[index]),
                role.permissions().contains(&permission.as_str()),
                "{index} {permission}"
            );
        }
    }
    assert!(!identity.allows(OBJECTS_GET, "unlisted-project"));
    assert!(identity.allows_anywhere(OBJECTS_GET));
    let caller = Caller::Principal(identity);
    assert_eq!(caller.authorize(BUCKETS_GET, None), Ok(()));
    assert_eq!(
        caller.authorize(BUCKETS_GET, Some("unlisted-project")),
        Err(AuthFailure::Denied(Denied::new(
            Some(identity),
            BUCKETS_GET,
            denied::Resource::project(Some("unlisted-project")),
        )))
    );
    assert_eq!(
        Caller::Anonymous.authorize(BUCKETS_GET, None),
        Err(AuthFailure::LoginRequired)
    );
}

#[test]
fn notable_role_consequences_follow_the_snapshot() {
    let has = |role: &str, permission: Permission| role.parse::<Role>().unwrap().grants(permission);
    assert!(!has("roles/storage.objectViewer", BUCKETS_GET));
    assert!(has("roles/storage.objectViewer", OBJECTS_GET));
    assert!(has("roles/storage.objectCreator", OBJECTS_CREATE));
    assert!(!has("roles/storage.objectCreator", OBJECTS_DELETE));
    assert!(!has("roles/storage.objectCreator", OBJECTS_GET));
    assert!(has("roles/storage.editor", BUCKETS_CREATE));
    assert!(!has("roles/storage.editor", BUCKETS_GET));
    assert!(!has("roles/storage.objectAdmin", BUCKETS_CREATE));
}

fn strict() -> Auth {
    Auth::strict(
        Registry::parse(
            br#"{"identities":[{"id":"a@example.invalid","tokens":["tok-a","tok-b"],"project_roles":{"p":["roles/storage.admin"]}}]}"#,
        )
        .unwrap(),
    )
}

/// An upload snapshot keeps the permissions an identity's roles grant, and refuses a role
/// name written in their place.
#[test]
fn upload_snapshots_keep_permissions_and_refuse_role_names() {
    let registry=Registry::parse(br#"{"identities":[{"id":"admin","tokens":["admin-token"],"project_roles":{"owned":["roles/storage.admin"],"other":["roles/storage.objectAdmin","roles/storage.objectViewer"]}}]}"#).unwrap();
    let admin = &registry.identities[0];
    assert!(admin.allows(OBJECTS_DELETE, "owned") && admin.allows(OBJECTS_DELETE, "other"));
    let restored = Identity::from_upload_snapshot(&admin.upload_snapshot()).unwrap();
    assert_eq!(restored.id(), admin.id());
    assert!(restored.allows(OBJECTS_CREATE, "owned"));
    let mut forged = admin.upload_snapshot();
    forged["projects"]["owned"] = serde_json::json!(["roles/storage.admin"]);
    assert!(Identity::from_upload_snapshot(&forged).is_err());
}

fn headers(values: &[&[u8]]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append(
            AUTHORIZATION,
            hyper::header::HeaderValue::from_bytes(value).unwrap(),
        );
    }
    headers
}

#[tokio::test]
async fn strict_caller_resolution() {
    let auth = strict();
    let id = |result: Result<Option<Caller<'_>>, AuthFailure>| match result {
        Ok(Some(Caller::Principal(identity))) => Ok(Some(identity.id().to_owned())),
        Ok(Some(Caller::Anonymous)) => Ok(None),
        Ok(None) => panic!("strict mode must resolve a caller"),
        Err(error) => Err(error),
    };
    assert_eq!(id(auth.authenticate(&headers(&[]), None).await), Ok(None));
    for value in [&b"Bearer tok-a"[..], b"bearer tok-b", b"BEARER   tok-a"] {
        assert_eq!(
            id(auth
                .authenticate(&headers(&[value]), Some("project=p"))
                .await),
            Ok(Some("a@example.invalid".into())),
            "{}",
            String::from_utf8_lossy(value)
        );
    }
    for value in [
        &b"Bearer"[..],
        b"Bearer ",
        b"Bearer tok-c",
        b"Bearer tok-a extra",
        b"Bearer\ttok-a",
        b"Basic dG9rLWE6",
        b"GOOG4-RSA-SHA256 Credential=x",
        b"GOOG4-HMAC-SHA256 Credential=x",
        b"AWS4-HMAC-SHA256 Credential=x",
        b"tok-a",
        b"Bearer \xfftok",
    ] {
        assert_eq!(
            id(auth.authenticate(&headers(&[value]), None).await),
            Err(AuthFailure::InvalidCredentials),
            "{}",
            String::from_utf8_lossy(value)
        );
    }
    assert_eq!(
        id(auth
            .authenticate(&headers(&[b"Bearer tok-a", b"Bearer tok-a"]), None)
            .await),
        Err(AuthFailure::InvalidCredentials)
    );
    for query in [
        "access_token=tok-a",
        "a=b&X-Goog-Signature=00",
        "x-goog-credential=x",
        "X-Goog-Algorithm",
        "Signature=x&GoogleAccessId=y",
        "access%5Ftoken=tok-a",
        "oauth_token=tok-a",
        "X-Amz-Algorithm=AWS4-HMAC-SHA256",
        "x-amz-credential=local",
        "X-Amz-Date=20261031T000000Z",
        "X-Amz-Expires=60",
        "X-Amz-SignedHeaders=host",
        "X-Amz-Signature=00",
    ] {
        assert_eq!(
            id(auth
                .authenticate(&headers(&[b"Bearer tok-a"]), Some(query))
                .await),
            Err(AuthFailure::InvalidCredentials),
            "{query}"
        );
        assert_eq!(
            id(auth.authenticate(&headers(&[]), Some(query)).await),
            Err(AuthFailure::InvalidCredentials),
            "{query}"
        );
    }
    let permissive = Auth::permissive();
    assert_matches!(
        permissive
            .authenticate(&headers(&[b"Basic garbage", b"x"]), Some("access_token=x"))
            .await,
        Ok(None)
    );
}

#[tokio::test]
async fn registered_principal_without_tokens_does_not_gain_bearer_or_jwt_authentication() {
    let registry = Registry::parse(
        br#"{"identities":[{"id":"hmac-only@example.test","tokens":[],"project_roles":{"local":["roles/storage.admin"]}}]}"#,
    )
    .unwrap();
    assert_eq!(registry.len(), 1);
    assert!(registry.tokens.is_empty());
    assert!(registry.signing_keys.is_empty());
    let auth = Auth::strict(registry);
    let anonymous = auth
        .authenticate(&headers(&[]), None)
        .await
        .unwrap()
        .unwrap();
    assert_matches!(anonymous, Caller::Anonymous);
    assert_eq!(anonymous.principal(), Err(AuthFailure::LoginRequired));
    for credential in [
        &b"Bearer"[..],
        b"Bearer ",
        b"Bearer hmac-only@example.test",
        b"Bearer unknown",
        b"Bearer eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJobWFjLW9ubHlAZXhhbXBsZS50ZXN0In0.AA",
    ] {
        assert_matches!(
            auth.authenticate(&headers(&[credential]), None).await,
            Err(AuthFailure::InvalidCredentials)
        );
    }
    assert_matches!(
        auth.authenticate(&headers(&[]), Some("X-Goog-Credential=unknown"))
            .await,
        Err(AuthFailure::InvalidCredentials)
    );
}

#[tokio::test]
async fn the_trusted_listener_changes_only_requests_without_credentials() {
    let id = |result: Result<Option<Caller<'_>>, AuthFailure>| match result {
        Ok(Some(Caller::Principal(identity))) => Ok(Some(identity.id().to_owned())),
        Ok(Some(Caller::Anonymous)) => Ok(None),
        Ok(None) => panic!("strict mode must resolve a caller"),
        Err(error) => Err(error),
    };
    let mut registry = Registry::parse(
        br#"{"identities":[{"id":"app@example.invalid","tokens":["app-token"],"project_roles":{"p":["roles/storage.objectAdmin"]}},{"id":"other@example.invalid","tokens":["other-token"]}]}"#,
    )
    .unwrap();
    registry.add_test_signer(&BTreeSet::new()).unwrap();
    let mut auth = Auth::strict(registry);
    let trusted = ListenerRole::Trusted;
    // Until a principal is named the trusted listener grants nothing more than the public one.
    assert_eq!(
        id(auth.authenticate_on(trusted, &headers(&[]), None).await),
        Ok(None)
    );
    // An unknown name stays unresolved, so requests stay anonymous; configuration reports it.
    auth.trust(&TrustedCaller::Named("missing@example.invalid".into()));
    assert_eq!(auth.trusted_principal(), None);
    assert_eq!(
        id(auth.authenticate_on(trusted, &headers(&[]), None).await),
        Ok(None)
    );
    auth.trust(&TrustedCaller::Named("app@example.invalid".into()));
    assert_eq!(
        id(auth.authenticate_on(trusted, &headers(&[]), None).await),
        Ok(Some("app@example.invalid".into()))
    );
    assert_eq!(
        id(auth.authenticate(&headers(&[]), None).await),
        Ok(None),
        "the public listener keeps anonymous requests anonymous"
    );
    assert_eq!(
        id(auth
            .authenticate_on(trusted, &headers(&[b"Bearer other-token"]), None)
            .await),
        Ok(Some("other@example.invalid".into()))
    );
    for (values, query) in [
        (&[&b"Bearer unknown-token"[..]][..], None),
        (&[b"Bearer"], None),
        (&[b"Basic YXBwOg=="], None),
        (&[b"Bearer app-token", b"Bearer app-token"], None),
        (&[], Some("access_token=app-token")),
        (&[], Some("X-Goog-Credential=unknown")),
    ] {
        assert_eq!(
            id(auth.authenticate_on(trusted, &headers(values), query).await),
            Err(AuthFailure::InvalidCredentials),
            "{values:?} {query:?}"
        );
    }
    auth.trust(&TrustedCaller::Named(test_signer::ID.into()));
    assert_eq!(
        id(auth.authenticate_on(trusted, &headers(&[]), None).await),
        Ok(Some(test_signer::ID.into()))
    );
}

#[test]
fn registry_validation_rejects_invalid_files_without_partial_load() {
    let valid = |identity: &str| format!(r#"{{"identities":[{identity}]}}"#);
    let ok = r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/storage.objectViewer"]}}"#;
    assert_eq!(Registry::parse(valid(ok).as_bytes()).unwrap().len(), 1);
    assert_eq!(
        Registry::parse(valid(r#"{"id":"a","tokens":["t"]}"#).as_bytes())
            .unwrap()
            .len(),
        1
    );
    let long_id = "a".repeat(255);
    let long_token = "t".repeat(MAX_TOKEN_BYTES + 1);
    for (case, document) in [
        ("malformed", "{".to_owned()),
        ("not object", "[]".into()),
        ("missing identities", "{}".into()),
        ("unknown top", r#"{"identities":[],"extra":1}"#.into()),
        ("empty", r#"{"identities":[]}"#.into()),
        (
            "unknown field",
            valid(r#"{"id":"a","tokens":["t"],"role":"x"}"#),
        ),
        ("missing id", valid(r#"{"tokens":["t"]}"#)),
        ("empty id", valid(r#"{"id":"","tokens":["t"]}"#)),
        ("space id", valid(r#"{"id":"a b","tokens":["t"]}"#)),
        (
            "long id",
            valid(&format!(r#"{{"id":"{long_id}","tokens":["t"]}}"#)),
        ),
        (
            "reserved local creator id",
            valid(r#"{"id":"gcs-mimic:local-creator","tokens":["t"]}"#),
        ),
        (
            "reserved namespace",
            valid(r#"{"id":"gcs-mimic:other","tokens":["t"]}"#),
        ),
        (
            "duplicate id",
            valid(r#"{"id":"a","tokens":["t"]},{"id":"a","tokens":["u"]}"#),
        ),
        ("missing tokens", valid(r#"{"id":"a"}"#)),
        ("empty token", valid(r#"{"id":"a","tokens":[""]}"#)),
        ("space token", valid(r#"{"id":"a","tokens":["a b"]}"#)),
        ("non-ascii token", valid(r#"{"id":"a","tokens":["tö"]}"#)),
        (
            "JWT-shaped token",
            valid(r#"{"id":"a","tokens":["a.b.c"]}"#),
        ),
        ("empty JWT segments", valid(r#"{"id":"a","tokens":[".."]}"#)),
        (
            "long token",
            valid(&format!(r#"{{"id":"a","tokens":["{long_token}"]}}"#)),
        ),
        (
            "duplicate token",
            valid(r#"{"id":"a","tokens":["t"]},{"id":"b","tokens":["t"]}"#),
        ),
        (
            "duplicate own token",
            valid(r#"{"id":"a","tokens":["t","t"]}"#),
        ),
        (
            "unknown role",
            valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/storage.nothing"]}}"#),
        ),
        (
            "basic owner",
            valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/owner"]}}"#),
        ),
        (
            "basic viewer",
            valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/viewer"]}}"#),
        ),
        (
            "empty roles",
            valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":[]}}"#),
        ),
        (
            "duplicate role",
            valid(
                r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/storage.admin","roles/storage.admin"]}}"#,
            ),
        ),
        (
            "no roles",
            valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":[]}}"#),
        ),
        (
            "no public keys",
            valid(r#"{"id":"a","tokens":["t"],"public_keys":[]}"#),
        ),
        (
            "invalid project",
            valid(
                r#"{"id":"a","tokens":["t"],"project_roles":{"bad project":["roles/storage.admin"]}}"#,
            ),
        ),
        (
            "empty project",
            valid(r#"{"id":"a","tokens":["t"],"project_roles":{"":["roles/storage.admin"]}}"#),
        ),
    ] {
        // Configuration names the setting that supplied the document; the failure itself
        // names no token.
        let error = Registry::load_principals(
            Some(document.as_bytes()),
            TestSignerChoice::WhenDeclared,
            &TrustedCaller::Anonymous,
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            !error.contains("\"t\""),
            "{case}: tokens must not be echoed"
        );
    }
}

/// Everything the file can name resolves through an index, so counts past the former limits
/// of 256 identities, 16 tokens, 64 projects and 32 roles load and authenticate.
#[tokio::test]
async fn a_registry_past_the_former_count_limits_loads_and_authenticates_its_last_entry() {
    const COUNT: usize = 1000;
    let roles: Vec<_> = Role::ALL.iter().map(|role| role.as_str()).collect();
    let tokens: Vec<_> = (0..COUNT).map(|n| format!("extra-{n}")).collect();
    let projects: serde_json::Map<_, _> = (0..COUNT)
        .map(|n| (format!("project-{n}"), serde_json::json!(roles)))
        .collect();
    let mut identities: Vec<_> = (0..COUNT)
        .map(|n| serde_json::json!({"id": format!("u{n}"), "tokens": [format!("t{n}")]}))
        .collect();
    identities.push(serde_json::json!({
        "id": "last", "tokens": tokens, "project_roles": projects,
    }));
    let parked: Vec<_> = (0..COUNT).map(|n| format!("parked-{n}")).collect();
    let document = serde_json::json!({
        "identities": identities, "public_access_prevention_projects": parked,
    });
    let registry = Registry::parse(document.to_string().as_bytes()).unwrap();
    assert_eq!(registry.len(), COUNT + 1);
    assert_eq!(registry.public_access_prevention_projects.len(), COUNT);
    assert_eq!(registry.identity("u999").unwrap().id(), "u999");
    assert!(registry.identity("unknown").is_none());
    let auth = Auth::strict(registry);
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer extra-999".parse().unwrap());
    let caller = auth
        .authenticate_request(
            &headers,
            None,
            &crate::clock::SystemClock,
            ListenerRole::Public,
        )
        .await
        .unwrap()
        .unwrap();
    let identity = caller.principal().unwrap();
    assert_eq!(identity.id(), "last");
    assert!(identity.allows(OBJECTS_CREATE, "project-999"));
}

/// Stored upload options hold at most 64 KiB, so a principal's other projects must stay out.
#[test]
fn an_upload_snapshot_for_a_project_keeps_only_that_project() {
    let projects: serde_json::Map<_, _> = (0..5000)
        .map(|n| {
            (
                format!("project-{n}"),
                serde_json::json!(["roles/storage.objectAdmin"]),
            )
        })
        .collect();
    let document =
        serde_json::json!({"identities": [{"id":"a","tokens":[],"project_roles":projects}]});
    let registry = Registry::parse(document.to_string().as_bytes()).unwrap();
    let identity = registry.identity("a").unwrap();
    assert!(identity.upload_snapshot().to_string().len() > 64 * 1024);
    let snapshot = identity.upload_snapshot_for("project-7");
    let restored = Identity::from_upload_snapshot(&snapshot).unwrap();
    assert!(restored.allows(OBJECTS_CREATE, "project-7"));
    assert!(!restored.allows(OBJECTS_CREATE, "project-8"));
    assert_eq!(
        identity.upload_snapshot_for("unlisted")["projects"],
        serde_json::json!({})
    );
    let full = Identity::from_upload_snapshot(&identity.upload_snapshot()).unwrap();
    assert!(full.allows(OBJECTS_CREATE, "project-4999"));
}
