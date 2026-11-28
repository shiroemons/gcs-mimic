use crate::{
    auth::{Auth, Registry},
    bucket::BucketRecord,
    clock::{Clock, SystemClock},
    config::{Config, RequestEndpoints},
    server::State,
    service::activity::ObjectActivity,
    storage::{Catalog, admin::LocalAdmin},
    test_support::TestDir,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;

pub(super) struct SignedUploadFixture {
    pub(super) state: Arc<State>,
    pub(super) config: Arc<Config>,
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
        let mut identity = json!({"id":"signer@example.invalid","tokens":[],"project_roles":{"local":["roles/storage.admin"]}});
        if let Some(public_key) = public_key {
            identity["public_keys"] = json!([public_key]);
        }
        let auth = Auth::strict(
            Registry::parse(json!({"identities":[identity]}).to_string().as_bytes()).unwrap(),
        );
        let catalog = Catalog::open_with_options(
            crate::storage::CatalogBacking::Disk(&dir.0),
            crate::storage::CatalogOptions::authenticated(
                clock.clone(),
                LocalAdmin::Disabled,
                &auth,
            ),
        )
        .await
        .unwrap();
        catalog
            .insert_bucket(BucketRecord::for_test("bucket", "local", clock.now()))
            .await
            .unwrap();
        let key = catalog
            .create_hmac_key(&"local".parse().unwrap(), "signer@example.invalid")
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
            config: Arc::new(
                crate::config::ConfigDraft {
                    max_object_bytes: 1024 * 1024,
                    ..Default::default()
                }
                .build(),
            ),
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
        generation: crate::object::Generation::for_test(42),
        metageneration: crate::object::Metageneration::for_test(2),
        content_type: "text/plain".into(),
        size: 5,
        md5: [3; 16],
        crc32c: 123,
        storage_class: crate::bucket::StorageClass::Nearline,
        time_created: now,
        updated: now,
        ..crate::storage::test_fixtures::bare_object_record()
    }
}
