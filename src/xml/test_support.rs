use crate::{
    auth::{Auth, Registry},
    clock::{Clock, SystemClock},
    config::{Config, RequestEndpoints},
    json,
    server::{ObjectActivity, State},
    storage::{Catalog, admin::LocalAdmin},
    test_support::TestDir,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;

pub(super) struct SignedUploadFixture {
    pub(super) state: Arc<State>,
    pub(super) config: Config,
    pub(super) endpoints: RequestEndpoints,
    pub(super) activity: watch::Sender<ObjectActivity>,
    pub(super) access_id: String,
    pub(super) secret: String,
    pub(super) dir: TestDir,
}

impl SignedUploadFixture {
    pub(super) async fn new(public_key: Option<&str>) -> Self {
        let dir = TestDir::new();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let catalog = Catalog::open_for_server(&dir.0, clock.clone(), LocalAdmin::Disabled)
            .await
            .unwrap();
        let mut identity = json!({"id":"signer@example.invalid","tokens":[],"project_roles":{"local":["roles/storage.admin"]}});
        if let Some(public_key) = public_key {
            identity["public_keys"] = json!([public_key]);
        }
        let auth = Auth::strict(
            Registry::parse(json!({"identities":[identity]}).to_string().as_bytes()).unwrap(),
        );
        catalog.configure_authentication(&auth).await.unwrap();
        catalog
            .insert_bucket(
                json::parse_insert(br#"{"name":"bucket"}"#, "local", clock.now()).unwrap(),
            )
            .await
            .unwrap();
        let key = catalog
            .create_hmac_key("local", "signer@example.invalid")
            .await
            .unwrap();
        let endpoints = RequestEndpoints::explicit("http://localhost");
        let (activity, _) = watch::channel(ObjectActivity::default());
        Self {
            state: Arc::new(State {
                clock,
                api_base_url: endpoints.api_base_url().to_owned(),
                ..State::for_test(catalog, auth)
            }),
            config: Config {
                max_object_bytes: 1024 * 1024,
                ..Config::default()
            },
            endpoints,
            activity,
            access_id: key.record.access_id,
            secret: key.secret,
            dir,
        }
    }
}

/// An object with every field an XML document renders, named so escaping and URL encoding show.
pub(super) fn object_record(
    bucket: &str,
    name: &str,
    now: std::time::SystemTime,
) -> crate::object::ObjectRecord {
    crate::object::ObjectRecord {
        bucket: bucket.into(),
        name: name.into(),
        generation: 42,
        metageneration: 2,
        owner: None,
        content_type: "text/plain".into(),
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: std::collections::BTreeMap::new(),
        size: 5,
        md5: [3; 16],
        md5_availability: Default::default(),
        crc32c: 123,
        component_count: None,
        storage_class: crate::bucket::StorageClass::Nearline,
        time_created: now,
        time_storage_class_updated: None,
        updated: now,
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: crate::object::Hold::Unset,
        temporary_hold: crate::object::Hold::Unset,
        custom_time: None,
        retention_expiration_time: None,
        retention: None,
        customer_key_sha256: None,
    }
}
