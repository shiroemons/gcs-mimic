use super::*;
use crate::{
    auth::{Auth, Registry},
    clock::{Clock, SystemClock},
    config::{Config, EndpointSnapshot},
    server::{DeadlineWatch, ObjectActivity, RequestBody},
    storage::{
        Catalog,
        fault::{self, Fault, Step},
    },
    test_support::TestDir,
};
use ring::hmac;
use serde_json::json;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::sync::watch;

const PRIVATE: &[u8] = include_bytes!("../../../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../../../fixtures/signing/test-only-public.pem");

#[derive(Clone, Copy)]
enum Variant {
    V4Rsa,
    V4Hmac,
    V2Rsa,
    V2Hmac,
}

struct Fixture {
    state: State,
    config: Config,
    endpoints: EndpointSnapshot,
    activity: watch::Sender<ObjectActivity>,
    access_id: String,
    secret: String,
    dir: TestDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = TestDir::new();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let catalog = Catalog::open_for_server(
            &dir.0,
            clock.clone(),
            crate::storage::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        let auth=Auth::strict(Registry::parse(json!({"identities":[{"id":"signer@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.admin"]}}]}).to_string().as_bytes()).unwrap());
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
        let config = Config {
            max_object_bytes: 1024 * 1024,
            ..Config::default()
        };
        let endpoints = EndpointSnapshot {
            api_base_url: "http://localhost".into(),
            public_base_url: None,
            xml_virtual_host_domains: Vec::new(),
        };
        let (activity, _) = watch::channel(ObjectActivity::default());
        Self {
            state: State {
                ready: AtomicBool::new(true),
                clock,
                catalog,
                api_base_url: "http://localhost".into(),
                default_location: "US".into(),
                auth,
            },
            config,
            endpoints,
            activity,
            access_id: key.record.access_id,
            secret: key.secret,
            dir,
        }
    }

    fn fields(&self, variant: Variant) -> BTreeMap<String, String> {
        let date = signature::timestamp(self.state.clock.now()).unwrap();
        let scope = format!("{}/auto/storage/goog4_request", &date[..8]);
        let mut fields: BTreeMap<String, String> = BTreeMap::from([
            ("key".into(), "object".into()),
            ("x-goog-meta-marker".into(), "old".into()),
        ]);
        match variant {
            Variant::V4Rsa | Variant::V4Hmac => {
                fields.insert(
                    "x-goog-algorithm".into(),
                    if matches!(variant, Variant::V4Rsa) {
                        "GOOG4-RSA-SHA256"
                    } else {
                        "GOOG4-HMAC-SHA256"
                    }
                    .into(),
                );
                fields.insert("x-goog-date".into(), date);
                fields.insert(
                    "x-goog-credential".into(),
                    format!(
                        "{}/{scope}",
                        if matches!(variant, Variant::V4Rsa) {
                            "signer@example.invalid"
                        } else {
                            &self.access_id
                        }
                    ),
                );
            }
            Variant::V2Rsa | Variant::V2Hmac => {}
        }
        let mut conditions = fields
            .iter()
            .map(|(name, value)| json!({name:value}))
            .collect::<Vec<_>>();
        conditions.push(json!({"bucket":"bucket"}));
        let policy=json::base64(json!({"expiration":signature::timestamp(self.state.clock.now()+Duration::from_secs(3600)).unwrap(),"conditions":conditions}).to_string().as_bytes());
        let signature = match variant {
            Variant::V4Rsa => signature::sign_policy(&policy, PRIVATE).unwrap(),
            Variant::V4Hmac => {
                let mut key = hmac::Key::new(
                    hmac::HMAC_SHA256,
                    format!("GOOG4{}", self.secret).as_bytes(),
                );
                for part in scope.split('/') {
                    key = hmac::Key::new(
                        hmac::HMAC_SHA256,
                        hmac::sign(&key, part.as_bytes()).as_ref(),
                    );
                }
                crate::hashing::encode_lower(hmac::sign(&key, policy.as_bytes()).as_ref())
            }
            Variant::V2Rsa => {
                let raw = signature::sign_policy(&policy, PRIVATE).unwrap();
                json::base64(
                    &(0..raw.len())
                        .step_by(2)
                        .map(|index| u8::from_str_radix(&raw[index..index + 2], 16).unwrap())
                        .collect::<Vec<_>>(),
                )
            }
            Variant::V2Hmac => json::base64(
                hmac::sign(
                    &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, self.secret.as_bytes()),
                    policy.as_bytes(),
                )
                .as_ref(),
            ),
        };
        match variant {
            Variant::V4Rsa | Variant::V4Hmac => {
                fields.insert("x-goog-signature".into(), signature);
            }
            Variant::V2Rsa | Variant::V2Hmac => {
                fields.insert(
                    "googleaccessid".into(),
                    if matches!(variant, Variant::V2Rsa) {
                        "signer@example.invalid".into()
                    } else {
                        self.access_id.clone()
                    },
                );
                fields.insert("signature".into(), signature);
            }
        }
        fields.insert("policy".into(), policy);
        fields
    }

    async fn post(&self, variant: Variant, bytes: &[u8]) -> Result<Response<ResponseBody>, Error> {
        let mut body = Vec::new();
        for (name, value) in self.fields(variant) {
            body.extend_from_slice(
                format!(
                    "--fault\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        body.extend_from_slice(b"--fault\r\nContent-Disposition: form-data; name=\"file\"; filename=\"object\"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n--fault--\r\n");
        let request = Request::builder()
            .method("POST")
            .uri("/bucket/")
            .header("content-type", "multipart/form-data; boundary=fault")
            .body(RequestBody::Buffered(Full::new(Bytes::from(body))))
            .unwrap();
        let object = crate::xml::select_object(request.uri(), request.headers(), &self.config)
            .unwrap()
            .unwrap();
        let ctx = RequestCtx {
            state: &self.state,
            config: &self.config,
            endpoints: &self.endpoints,
            activity: &self.activity,
        };
        respond(
            request,
            &object,
            ctx,
            ctx.start_object_deadline(DeadlineWatch::CatalogOnly),
        )
        .await
    }

    async fn resource(&self) -> ObjectRecord {
        self.state
            .catalog
            .get_object("bucket", "object", None, ObjectConditions::default())
            .await
            .unwrap()
    }

    async fn assert_old(&self, before: &ObjectRecord) {
        assert_eq!(&self.resource().await, before);
        let (_, mut reader) = self
            .state
            .catalog
            .open_object("bucket", "object", None, ObjectConditions::default())
            .await
            .unwrap();
        assert_eq!(
            reader.next_chunk().await.unwrap().unwrap(),
            b"old".as_slice()
        );
        assert!(reader.next_chunk().await.unwrap().is_none());
        drop(reader);
        assert_eq!(
            std::fs::read_dir(self.dir.0.join("staging"))
                .unwrap()
                .count(),
            0
        );
        let connection = rusqlite::Connection::open(self.dir.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM objects", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[tokio::test]
async fn policy_variants_keep_old_objects_after_write_flush_and_sync_failures() {
    for variant in [
        Variant::V4Rsa,
        Variant::V4Hmac,
        Variant::V2Rsa,
        Variant::V2Hmac,
    ] {
        let fixture = Fixture::new().await;
        assert_eq!(
            fixture.post(variant, b"old").await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let before = fixture.resource().await;
        for (step, skip) in [
            (Step::BodyWrite, 0),
            (Step::BodyWrite, 1),
            (Step::BodyFlush, 0),
            (Step::BodySync, 0),
        ] {
            fault::arm(&fixture.dir.0, step, skip, Fault::Io(io::ErrorKind::Other));
            let error = fixture
                .post(variant, &vec![b'n'; 256 * 1024])
                .await
                .err()
                .unwrap();
            assert_eq!(
                error.status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{step:?}: {error:?}"
            );
            assert_eq!(fault::pending(&fixture.dir.0), 0, "{step:?}");
            fixture.assert_old(&before).await;
        }
        fixture.state.catalog.close().await.unwrap();
        let reopened = Catalog::open(&fixture.dir.0).await.unwrap();
        assert_eq!(
            reopened
                .get_object("bucket", "object", None, ObjectConditions::default())
                .await
                .unwrap(),
            before
        );
        reopened.close().await.unwrap();
    }
}

#[tokio::test]
async fn hmac_policy_backend_failure_retains_cause_and_never_returns_anonymous() {
    use std::error::Error as _;
    let fixture = Fixture::new().await;
    fixture.state.catalog.close().await.unwrap();
    for variant in [Variant::V4Hmac, Variant::V2Hmac] {
        let fields = fixture.fields(variant);
        let version = Version::select(&fields).unwrap();
        let error = authenticate(&fixture.state, &HeaderMap::new(), &fields, version)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        let failure = match variant {
            Variant::V4Hmac => fixture
                .state
                .auth
                .authenticate_policy(
                    &fixture.state.catalog,
                    &fields["x-goog-algorithm"],
                    &fields["x-goog-credential"],
                    &fields["x-goog-date"],
                    &fields["x-goog-signature"],
                    &fields["policy"],
                )
                .await
                .unwrap_err(),
            Variant::V2Hmac => fixture
                .state
                .auth
                .authenticate_policy_v2(
                    &fixture.state.catalog,
                    &fields["googleaccessid"],
                    &fields["signature"],
                    &fields["policy"],
                )
                .await
                .unwrap_err(),
            _ => unreachable!(),
        };
        assert!(matches!(failure, signature::VerificationError::Catalog(_)));
        assert!(failure.source().is_some());
    }
}
