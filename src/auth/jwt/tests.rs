use super::*;
use crate::auth::{Auth, AuthFailure, Caller};
use hyper::HeaderMap;
use ring::{rand::SystemRandom, signature};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, pem::PemObject};

const PRIVATE: &[u8] = include_bytes!("../../../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../../../fixtures/signing/test-only-public.pem");
const ISSUER: &str = "jwt@example.invalid";
const NOW: u64 = 1_793_361_600;

#[tokio::test]
async fn requests_verify_jwts_on_the_bounded_pool_and_resolve_tokens_without_it() {
    let auth = Auth::strict(registry(&[PUBLIC], &["opaque-token"])).with_verifier_budget(2);
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", token(&claims())).parse().unwrap(),
    );
    let caller = auth
        .authenticate_request(&headers, None, &Fixed(NOW))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(caller.principal().unwrap().id(), ISSUER);
    let bad = token(&claims());
    let mut bad = bad.into_bytes();
    let index = bad.len() - 20;
    bad[index] = if bad[index] == b'A' { b'B' } else { b'A' };
    headers.insert(
        "authorization",
        format!("Bearer {}", String::from_utf8(bad).unwrap())
            .parse()
            .unwrap(),
    );
    assert!(matches!(
        auth.authenticate_request(&headers, None, &Fixed(NOW)).await,
        Err(AuthError::Failure(AuthFailure::InvalidCredentials))
    ));
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    let occupied = auth.verifiers.run(move || {
        started.send(()).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let resolve = async {
        ready.await.unwrap();
        headers.insert("authorization", "Bearer opaque-token".parse().unwrap());
        let caller = tokio::time::timeout(
            Duration::from_secs(1),
            auth.authenticate_request(&headers, None, &Fixed(NOW)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(caller.principal().unwrap().id(), ISSUER);
        assert!(matches!(
            auth.authenticate_request(&HeaderMap::new(), None, &Fixed(NOW))
                .await,
            Ok(Some(Caller::Anonymous))
        ));
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(occupied, resolve);
    result.unwrap();
}

struct Fixed(u64);
impl Clock for Fixed {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.0)
    }
}

fn registry(keys: &[&str], tokens: &[&str]) -> Registry {
    Registry::parse(
        &serde_json::to_vec(&json!({"identities": [{
            "id": ISSUER, "tokens": tokens, "public_keys": keys,
            "project_roles": {"local-project": ["roles/storage.admin"]}
        }]}))
        .unwrap(),
    )
    .unwrap()
}

fn claims() -> Value {
    json!({"iss": ISSUER, "sub": ISSUER, "scope": SCOPE, "iat": NOW - 60, "exp": NOW + 60})
}

fn encoded(bytes: &[u8]) -> String {
    crate::json::base64(bytes)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

fn sign_raw(header: &str, claims: &str) -> String {
    let message = format!(
        "{}.{}",
        encoded(header.as_bytes()),
        encoded(claims.as_bytes())
    );
    let private = PrivateKeyDer::from_pem_slice(PRIVATE).unwrap();
    let key = signature::RsaKeyPair::from_pkcs8(private.secret_der()).unwrap();
    let mut bytes = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        message.as_bytes(),
        &mut bytes,
    )
    .unwrap();
    format!("{message}.{}", encoded(&bytes))
}

fn token(claims: &Value) -> String {
    sign_raw(r#"{"alg":"RS256","typ":"JWT"}"#, &claims.to_string())
}

async fn authenticate<'a>(auth: &'a Auth, token: &str, clock: &dyn Clock) -> Option<&'a Identity> {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    match auth.authenticate_request(&headers, None, clock).await {
        Ok(Some(Caller::Principal(identity))) => Some(identity),
        Ok(_) | Err(AuthError::Failure(_)) => None,
        Err(AuthError::Unavailable(error)) => panic!("credential verifier failed: {error}"),
    }
}

#[tokio::test]
async fn jwt_resolves_registered_keys_and_keeps_opaque_tokens_and_project_permissions() {
    let auth = Auth::strict(registry(&[PUBLIC], &["opaque-token"]));
    for token in [token(&claims()), "opaque-token".into()] {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let caller = auth
            .authenticate_request(&headers, None, &Fixed(NOW))
            .await
            .unwrap()
            .unwrap();
        let identity = caller.principal().unwrap();
        assert_eq!(identity.id(), ISSUER);
        assert!(identity.allows(crate::auth::OBJECTS_CREATE, "local-project"));
        assert!(!identity.allows(crate::auth::OBJECTS_CREATE, "other-project"));
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", token(&claims())).parse().unwrap(),
    );
    assert!(matches!(
        Auth::permissive()
            .authenticate_request(&headers, None, &Fixed(0))
            .await,
        Ok(None)
    ));
}

#[tokio::test]
async fn jwt_rejects_algorithm_issuer_audience_scope_time_and_tampering() {
    let registry = Auth::strict(registry(&[PUBLIC], &[]));
    let valid = token(&claims());
    assert_eq!(
        authenticate(&registry, &valid, &Fixed(NOW))
            .await
            .unwrap()
            .id(),
        ISSUER
    );
    for (field, value) in [
        ("iss", json!("unknown@example.invalid")),
        ("sub", json!("other@example.invalid")),
        ("aud", json!("https://oauth2.googleapis.com/token")),
        ("aud", json!("http://localhost:4443/")),
        ("aud", Value::Null),
        (
            "scope",
            json!("https://www.googleapis.com/auth/devstorage.read_only"),
        ),
        (
            "scope",
            json!(format!("{SCOPE} https://www.googleapis.com/auth/unknown")),
        ),
        ("scope", json!("")),
        ("iat", json!(NOW + 1)),
        ("iat", json!(NOW - 3601)),
        ("iat", json!(-1)),
        ("exp", json!(NOW)),
        ("exp", json!(NOW - 1)),
        ("exp", json!(NOW + 3601)),
        ("exp", json!("1793361660")),
        ("nbf", json!(NOW + 1)),
    ] {
        let mut claims = claims();
        claims[field] = value;
        assert!(
            authenticate(&registry, &token(&claims), &Fixed(NOW))
                .await
                .is_none(),
            "{field}: {claims}"
        );
    }
    for header in [
        r#"{"alg":"none"}"#,
        r#"{"alg":"HS256"}"#,
        r#"{"alg":"RS512"}"#,
        r#"{"alg":"RS256","typ":"invalid"}"#,
        r#"{"alg":"RS256","crit":["unknown"]}"#,
        r#"{"alg":"RS256","alg":"RS256"}"#,
        r#"{"alg":"RS256","kid":""}"#,
    ] {
        assert!(
            authenticate(
                &registry,
                &sign_raw(header, &claims().to_string()),
                &Fixed(NOW)
            )
            .await
            .is_none()
        );
    }
    for field in ["iss", "sub", "scope", "iat", "exp"] {
        let mut claims = claims();
        claims.as_object_mut().unwrap().remove(field);
        assert!(
            authenticate(&registry, &token(&claims), &Fixed(NOW))
                .await
                .is_none()
        );
    }
    let duplicate = format!(
        r#"{{"iss":"{ISSUER}","iss":"{ISSUER}","sub":"{ISSUER}","scope":"{SCOPE}","iat":{},"exp":{}}}"#,
        NOW - 60,
        NOW + 60
    );
    assert!(
        authenticate(
            &registry,
            &sign_raw(r#"{"alg":"RS256"}"#, &duplicate),
            &Fixed(NOW)
        )
        .await
        .is_none()
    );
    let (message, signature) = valid.rsplit_once('.').unwrap();
    let tampered = format!("{message}.A{}", &signature[1..]);
    for token in [
        tampered,
        format!("{message}."),
        format!("{valid}.extra"),
        format!("{valid}="),
        "a".repeat(MAX_TOKEN_BYTES + 1),
    ] {
        assert!(authenticate(&registry, &token, &Fixed(NOW)).await.is_none());
    }
}

#[tokio::test]
async fn jwt_time_boundaries_and_registry_key_rotation_are_explicit() {
    let original = Auth::strict(registry(&[PUBLIC], &[]));
    let valid = token(&claims());
    assert!(
        authenticate(&original, &valid, &Fixed(NOW - 60))
            .await
            .is_some()
    );
    assert!(
        authenticate(&original, &valid, &Fixed(NOW + 59))
            .await
            .is_some()
    );
    assert!(
        authenticate(&original, &valid, &Fixed(NOW + 60))
            .await
            .is_none()
    );
    let mut boundary = claims();
    boundary["iat"] = json!(NOW);
    boundary["exp"] = json!(NOW + 3600);
    boundary["nbf"] = json!(NOW);
    assert!(
        authenticate(&original, &token(&boundary), &Fixed(NOW))
            .await
            .is_some()
    );
    // A different supported modulus proves verification tries the next registered key.
    let other = PUBLIC.replace("0YKIIe5A", "0YKJIe5A");
    assert_ne!(other, PUBLIC);
    let overlap = Auth::strict(registry(&[&other, PUBLIC], &[]));
    let rotated = Auth::strict(registry(&[&other], &[]));
    assert!(authenticate(&overlap, &valid, &Fixed(NOW)).await.is_some());
    assert!(authenticate(&rotated, &valid, &Fixed(NOW)).await.is_none());
    assert!(authenticate(&original, &valid, &Fixed(NOW)).await.is_some());
}

#[tokio::test]
async fn registered_jwt_shaped_tokens_never_bypass_signature_or_scope_checks() {
    let mut claims = claims();
    claims["scope"] = json!("https://www.googleapis.com/auth/devstorage.read_only");
    let token = token(&claims);
    // Registry opaque tokens are capped at 512 bytes; use a short unsigned JWT there.
    let unsigned = "eyJhbGciOiJub25lIn0.e30.c2ln";
    let error = Registry::parse(
        &serde_json::to_vec(&json!({"identities":[{
            "id": ISSUER, "tokens": [unsigned], "public_keys": [PUBLIC]
        }]}))
        .unwrap(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("tokens must not be JWT-shaped"));
    let auth = Auth::strict(registry(&[PUBLIC], &[]));
    for token in [token.as_str(), unsigned] {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        assert!(matches!(
            auth.authenticate_request(&headers, None, &Fixed(NOW)).await,
            Err(AuthError::Failure(AuthFailure::InvalidCredentials))
        ));
    }
    assert!(matches!(
        auth.authenticate_request(&HeaderMap::new(), None, &Fixed(NOW))
            .await,
        Ok(Some(Caller::Anonymous))
    ));
}

#[test]
fn jwt_base64url_rejects_padding_noncanonical_bits_and_other_alphabets() {
    assert_eq!(decode("e30").unwrap(), b"{}");
    assert_eq!(decode("YWJj").unwrap(), b"abc");
    for raw in ["", "a", "e30=", "e31", "YQ+_", "YQ/_", "Y Q", "é"] {
        assert!(decode(raw).is_none(), "{raw}");
    }
}
