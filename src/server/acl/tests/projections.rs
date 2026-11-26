use super::super::{AclSubjects, BucketVersion, Rendered, add_full_projection};
use crate::{
    acl::{AclRole, AclTarget},
    auth::{Auth, Caller, ListenerRole, Registry},
    config::{CompatProfile, Config, RequestEndpoints},
    server::{ObjectActivity, RequestBody, RequestCtx, State, routing},
    storage::{
        AclChange, Catalog,
        fault::Step,
        test_fixtures::{PERMISSIVE, body, bucket, live, pause_read, permissive_upload},
    },
    test_support::TestDir,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::watch;

async fn get(state: &Arc<State>, uri: &str, token: &str) -> Value {
    let config = Arc::new(Config::default());
    let endpoints = RequestEndpoints::explicit("http://localhost");
    let (activity, _) = watch::channel(ObjectActivity::default());
    let ctx = RequestCtx {
        uri: &crate::percent::EMPTY_REQUEST_URI,
        state,
        config: &config,
        endpoints: &endpoints,
        activity: &activity,
        listener: ListenerRole::Public,
    };
    let request = Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(RequestBody::Buffered(Full::new(Bytes::new())))
        .unwrap();
    let response = routing::respond(request, ctx).await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{uri}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

async fn grant_acl(catalog: &Catalog, target: AclTarget, role: AclRole) {
    catalog
        .change_acl(
            target,
            AclChange::Upsert {
                entity: "user-x@example.test".parse().unwrap(),
                role: Some(role),
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn full_and_no_acl_requests_use_the_expected_catalog_jobs() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for name in ["bucket", "other"] {
        catalog.insert_bucket(bucket(name, "local")).await.unwrap();
    }
    let record = catalog
        .put_object(permissive_upload("bucket", "object", 8), body(vec![]))
        .await
        .unwrap();
    // Pin the generation so both object gets exercise SQL even with a warm live-object cache.
    let object = format!(
        "/storage/v1/b/bucket/o/object?generation={}&",
        record.generation
    );
    let strict = Auth::strict(Registry::parse(br#"{"identities":[{"id":"admin","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}}]}"#).unwrap());
    for (auth, auth_reads) in [(Auth::permissive(), 0), (strict, 1)] {
        let state = Arc::new(State::for_test(catalog.clone(), auth));
        for (path, writes, reads) in [
            ("/storage/v1/b?project=local&", 0, 1),
            ("/storage/v1/b/bucket?", 0, 1 + auth_reads),
            (object.as_str(), 0, 1 + auth_reads),
            ("/storage/v1/b/bucket/o?", 0, 1 + auth_reads),
        ] {
            for (projection, extra) in [("noAcl", 0), ("full", 1)] {
                let uri = format!("{path}projection={projection}");
                let (w, r) = catalog.job_counts();
                let value = get(&state, &uri, "admin").await;
                let items = value
                    .get("items")
                    .map_or(std::slice::from_ref(&value), |items| {
                        items.as_array().unwrap().as_slice()
                    });
                assert!(!items.is_empty());
                for item in items {
                    assert_eq!(item.get("acl").is_some(), projection == "full", "{uri}");
                }
                assert_eq!(
                    catalog.job_counts(),
                    (w + writes, r + reads + extra),
                    "{uri} auth_reads={auth_reads}"
                );
            }
        }
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn full_bucket_list_projects_only_acl_granted_items() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for name in ["hidden", "owned"] {
        catalog.insert_bucket(bucket(name, "local")).await.unwrap();
    }
    let auth = Auth::strict(Registry::parse(br#"{"identities":[{"id":"x@example.test","tokens":["x"],"project_roles":{"local":["roles/storage.bucketViewer"]}}]}"#).unwrap());
    grant_acl(&catalog, AclTarget::Bucket("owned".into()), AclRole::Owner).await;
    let state = Arc::new(State::for_test(catalog.clone(), auth));
    let list = get(&state, "/storage/v1/b?project=local&projection=full", "x").await;
    let [hidden, owned] = list["items"].as_array().unwrap().as_slice() else {
        panic!("expected both listed buckets");
    };
    assert_eq!(hidden["name"], "hidden");
    assert_eq!(owned["name"], "owned");
    for field in ["acl", "defaultObjectAcl"] {
        assert!(hidden.get(field).is_none(), "{field}");
        assert!(owned[field].is_array(), "{field}");
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn deleting_the_bucket_after_an_authorized_object_read_keeps_its_response() {
    let strict = Auth::strict(
        Registry::parse(br#"{"identities":[{"id":"x@example.test","tokens":["x"]}]}"#).unwrap(),
    );
    for auth in [Auth::permissive(), strict] {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let state = Arc::new(State::for_test(catalog.clone(), auth));
        for uri in [
            "/storage/v1/b/bucket/o/object?projection=full",
            "/storage/v1/b/bucket/o?projection=full",
        ] {
            catalog
                .insert_bucket(bucket("bucket", "local"))
                .await
                .unwrap();
            grant_acl(
                &catalog,
                AclTarget::Bucket("bucket".into()),
                AclRole::Reader,
            )
            .await;
            let record = catalog
                .put_object(permissive_upload("bucket", "object", 8), body(vec![]))
                .await
                .unwrap();
            grant_acl(
                &catalog,
                AclTarget::Object {
                    bucket: "bucket".into(),
                    name: "object".into(),
                    generation: Some(record.generation),
                },
                AclRole::Reader,
            )
            .await;
            let (reached, release) = pause_read(&catalog, Step::BeforeObjectAclProjection);
            let response = get(&state, uri, "x");
            tokio::pin!(response);
            tokio::select! {
                value = &mut response => panic!("read finished before bucket deletion: {value}"),
                entered = reached => entered.unwrap(),
            }
            catalog
                .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
                .await
                .unwrap();
            catalog
                .delete_bucket("bucket", Default::default(), PERMISSIVE)
                .await
                .unwrap();
            release.send(()).unwrap();
            let value = response.await;
            let object = if let Some(items) = value.get("items") {
                let [object] = items.as_array().unwrap().as_slice() else {
                    panic!("expected one listed object");
                };
                object
            } else {
                &value
            };
            assert_eq!(object["name"], "object");
            assert_eq!(object["generation"], record.generation.to_string());
            assert!(object.get("acl").is_none());
            assert!(object.get("owner").is_none());
        }
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn deleting_a_listed_bucket_omits_only_its_acl_fields() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for name in ["bucket", "other"] {
        catalog.insert_bucket(bucket(name, "local")).await.unwrap();
    }
    let state = State::for_test(catalog.clone(), Auth::permissive());
    let page = catalog
        .list_buckets_page("local", None, None, 2)
        .await
        .unwrap();
    let base = "http://localhost";
    let value = crate::json::bucket_list_with_profile(
        &page.items,
        None,
        base,
        crate::json::Projection::Full,
        CompatProfile::Native,
    )
    .unwrap();
    let deleted = value["items"][0].clone();
    let mut list = Rendered {
        value,
        subjects: AclSubjects::Buckets {
            project: "local".into(),
            items: page.items.iter().map(BucketVersion::from).collect(),
        },
    };
    catalog
        .delete_bucket("bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    add_full_projection(&state, None, &mut list, base)
        .await
        .unwrap();
    assert_eq!(list.value["items"][0], deleted);
    assert_eq!(list.value["items"][1]["name"], "other");
    assert!(list.value["items"][1]["acl"].is_array());
    assert!(list.value["items"][1]["defaultObjectAcl"].is_array());

    let mut single = Rendered {
        value: deleted.clone(),
        subjects: AclSubjects::Bucket {
            project: "local".into(),
            version: BucketVersion::from(&page.items[0]),
        },
    };
    assert_eq!(
        add_full_projection(&state, None, &mut single, base)
            .await
            .unwrap_err()
            .status(),
        StatusCode::NOT_FOUND
    );
    add_full_projection(&state, Some(Caller::Anonymous), &mut single, base)
        .await
        .unwrap();
    assert_eq!(single.value, deleted);
    catalog.close().await.unwrap();
}
