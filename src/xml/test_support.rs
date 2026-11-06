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
    pub(super) state: State,
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
            state: State {
                clock,
                api_base_url: endpoints.api_base_url().to_owned(),
                ..State::for_test(catalog, auth)
            },
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
