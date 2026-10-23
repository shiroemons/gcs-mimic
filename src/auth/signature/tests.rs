use super::*;
use crate::auth::Registry;
use serde_json::json;

const PRIVATE: &[u8] = include_bytes!("../../../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../../../fixtures/signing/test-only-public.pem");
const SIGNER: &str = "signer@example.invalid";
const DATE: &str = "20261023T120000Z";

struct Fixed(SystemTime);
impl Clock for Fixed {
    fn now(&self) -> SystemTime {
        self.0
    }
}

fn auth() -> Auth {
    Auth::strict(
        Registry::parse(
            &serde_json::to_vec(&json!({"identities":[{
                "id":SIGNER,"tokens":[],"public_keys":[PUBLIC]
            }]}))
            .unwrap(),
        )
        .unwrap(),
    )
}

fn signed(url: &str, method: &Method, headers: HeaderMap) -> String {
    sign_url(
        url,
        method,
        SIGNER,
        PRIVATE,
        60,
        parse_timestamp(DATE).unwrap(),
        headers,
    )
    .unwrap()
}

fn verify(
    url: &str,
    method: &Method,
    mut headers: HeaderMap,
    now: SystemTime,
) -> Result<String, Failure> {
    let uri: Uri = url.parse().unwrap();
    if !headers.contains_key("host") {
        headers.insert("host", uri.authority().unwrap().as_str().parse().unwrap());
    }
    let auth = auth();
    auth.authenticate_signed(method, &uri, &headers, &Fixed(now))
        .map(|caller| caller.unwrap().principal().unwrap().id().to_owned())
}

#[test]
fn signed_time_boundaries_use_the_injected_clock_and_documented_skew() {
    let now = parse_timestamp(DATE).unwrap();
    let url = signed(
        "http://localhost:4443/bucket/object",
        &Method::GET,
        HeaderMap::new(),
    );
    for at in [
        now - Duration::from_secs(900),
        now,
        now + Duration::from_secs(59),
    ] {
        assert_eq!(
            verify(&url, &Method::GET, HeaderMap::new(), at),
            Ok(SIGNER.into())
        );
    }
    assert_eq!(
        verify(
            &url,
            &Method::GET,
            HeaderMap::new(),
            now - Duration::from_secs(901)
        ),
        Err(Failure::NotYetValid)
    );
    assert_eq!(
        verify(
            &url,
            &Method::GET,
            HeaderMap::new(),
            now + Duration::from_secs(60)
        ),
        Err(Failure::Expired)
    );
    for date in [
        "20260229T120000Z",
        "20261301T000000Z",
        "20261023T240000Z",
        "20261023T126000Z",
        "20261023T120060Z",
        "20261023t120000Z",
        "é20261023T1200Z",
    ] {
        assert!(parse_timestamp(date).is_err(), "{date}");
    }
    for date in [
        "00010101T000000Z",
        "19691231T235959Z",
        "20000229T123456Z",
        "99991231T235959Z",
    ] {
        assert_eq!(timestamp(parse_timestamp(date).unwrap()).unwrap(), date);
    }
}

#[test]
fn signed_method_resource_query_host_port_headers_and_hex_are_bound() {
    let now = parse_timestamp(DATE).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-required", "value".parse().unwrap());
    let url = signed(
        "https://localhost:4443/bucket/object?generation=123",
        &Method::GET,
        headers.clone(),
    );
    assert!(verify(&url, &Method::GET, headers.clone(), now).is_ok());
    for changed in [
        url.replace("/bucket/", "/other-bucket/"),
        url.replace("/object?", "/other?"),
        url.replace("generation=123", "generation=124"),
        url.replace("&generation=123", ""),
        format!("{url}&extra=x"),
        url.replace("localhost:4443", "other.invalid:4443"),
        url.replace("localhost:4443", "localhost:4444"),
    ] {
        assert_eq!(
            verify(&changed, &Method::GET, headers.clone(), now),
            Err(Failure::Mismatch),
            "{changed}"
        );
    }
    for method in [Method::HEAD, Method::PUT, Method::DELETE] {
        assert_eq!(
            verify(&url, &method, headers.clone(), now),
            Err(Failure::Mismatch)
        );
    }
    headers.insert("x-required", "changed".parse().unwrap());
    assert_eq!(
        verify(&url, &Method::GET, headers.clone(), now),
        Err(Failure::Mismatch)
    );
    headers.remove("x-required");
    assert_eq!(
        verify(&url, &Method::GET, headers, now),
        Err(Failure::Mismatch)
    );
    let (prefix, signature) = url.rsplit_once('=').unwrap();
    let changed = format!(
        "{prefix}={}{}",
        if &signature[..2] == "00" { "01" } else { "00" },
        &signature[2..]
    );
    let mut headers = HeaderMap::new();
    headers.insert("x-required", "value".parse().unwrap());
    assert_eq!(
        verify(&changed, &Method::GET, headers.clone(), now),
        Err(Failure::Mismatch)
    );
    assert_eq!(
        verify(&format!("{prefix}=zz"), &Method::GET, headers.clone(), now),
        Err(Failure::Malformed)
    );
    assert_eq!(
        verify(&format!("{prefix}=0"), &Method::GET, headers, now),
        Err(Failure::Malformed)
    );
}

#[test]
fn malformed_duplicate_scope_duration_and_unknown_signer_are_rejected() {
    let now = parse_timestamp(DATE).unwrap();
    let url = signed(
        "http://localhost/bucket/object",
        &Method::GET,
        HeaderMap::new(),
    );
    for changed in [
        url.replace("X-Goog-Expires=60", "X-Goog-Expires=604801"),
        url.replace("X-Goog-Expires=60", "X-Goog-Expires=0"),
        url.replace("X-Goog-Expires=60", "X-Goog-Expires=-1"),
        url.replace("X-Goog-Expires=60", "X-Goog-Expires=+60"),
        url.replace("X-Goog-Expires=60", "X-Goog-Expires=18446744073709551616"),
        url.replace("%2F20261023%2F", "%2F20261022%2F"),
        url.replace("%2Fstorage%2F", "%2Fwrong%2F"),
        url.replace("GOOG4-RSA-SHA256", "GOOG4-HMAC-SHA256"),
        url.replace("X-Goog-SignedHeaders=host", "X-Goog-SignedHeaders=Host"),
        url.replace("X-Goog-SignedHeaders=host", "X-Goog-SignedHeaders=range"),
        format!("{url}&X-Goog-Date={DATE}"),
        format!("{url}&%58-Goog-Date={DATE}"),
        format!("{url}&x=%ZZ"),
    ] {
        assert_eq!(
            verify(&changed, &Method::GET, HeaderMap::new(), now),
            Err(Failure::Malformed),
            "{changed}"
        );
    }
    for parameter in PARAMETERS {
        let (base, query) = url.split_once('?').unwrap();
        let query = query
            .split('&')
            .filter(|p| !p.starts_with(&format!("{parameter}=")))
            .collect::<Vec<_>>()
            .join("&");
        assert_eq!(
            verify(
                &format!("{base}?{query}"),
                &Method::GET,
                HeaderMap::new(),
                now
            ),
            Err(Failure::Malformed)
        );
    }
    let unknown = sign_url(
        "http://localhost/bucket/object",
        &Method::GET,
        "unknown@example.invalid",
        PRIVATE,
        60,
        now,
        HeaderMap::new(),
    )
    .unwrap();
    assert_eq!(
        verify(&unknown, &Method::GET, HeaderMap::new(), now),
        Err(Failure::UnknownSigner)
    );
    for expires in [0, 604801, u64::MAX] {
        assert!(
            sign_url(
                "http://localhost/bucket/o",
                &Method::GET,
                SIGNER,
                PRIVATE,
                expires,
                now,
                HeaderMap::new()
            )
            .is_err()
        );
    }
    assert!(
        sign_url(
            "http://localhost/bucket/o",
            &Method::GET,
            SIGNER,
            PRIVATE,
            604800,
            now,
            HeaderMap::new()
        )
        .is_ok()
    );
}

#[test]
fn raw_paths_query_sorting_and_header_normalization_preserve_signed_distinctions() {
    let now = parse_timestamp(DATE).unwrap();
    for path in [
        "/bucket/a%2Fb",
        "/bucket/%C3%A9%E6%97%A5",
        "/bucket/a//b/",
        "/bucket/a%2F..%2Fb",
        "/bucket/.%2Fx",
        "/bucket/%25%2B%20%3F",
    ] {
        let url = signed(
            &format!("http://localhost:8080{path}?z=hello+world&%C3%A9=%2B%25&a=%2F"),
            &Method::HEAD,
            HeaderMap::new(),
        );
        assert!(
            verify(&url, &Method::HEAD, HeaderMap::new(), now).is_ok(),
            "{url}"
        );
        assert!(url.contains("a=%2F&z=hello%20world"));
    }
    let mut headers = HeaderMap::new();
    headers.append("x-reviewer", "  jane\t  doe  ".parse().unwrap());
    headers.append("x-reviewer", " john ".parse().unwrap());
    let url = signed("http://localhost/bucket/o", &Method::GET, headers);
    let mut equivalent = HeaderMap::new();
    equivalent.insert("X-Reviewer", "jane doe,john".parse().unwrap());
    assert!(verify(&url, &Method::GET, equivalent.clone(), now).is_ok());
    equivalent.insert("x-reviewer", "john,jane doe".parse().unwrap());
    assert_eq!(
        verify(&url, &Method::GET, equivalent, now),
        Err(Failure::Mismatch)
    );
    assert_eq!(canonical_path("/bucket/a%2fb").unwrap(), "/bucket/a%2fb");
    assert_eq!(canonical_path("/bucket/a@!b").unwrap(), "/bucket/a%40%21b");
    assert!(canonical_path("/bad%2").is_err());
}

#[test]
fn canonical_headers_match_the_official_published_example() {
    let mut headers = HeaderMap::new();
    headers.insert("host", "storage.googleapis.com".parse().unwrap());
    headers.insert("content-type", "text/plain".parse().unwrap());
    headers.append("x-goog-meta-reviewer", "jane".parse().unwrap());
    headers.append("x-goog-meta-reviewer", "john".parse().unwrap());
    let canonical = canonical_request(
        &Method::GET,
        &"/example-bucket/tabby.jpeg".parse().unwrap(),
        &headers,
        &BTreeMap::new(),
        "content-type;host;x-goog-meta-reviewer",
    )
    .unwrap();
    assert_eq!(
        canonical,
        "GET\n/example-bucket/tabby.jpeg\n\ncontent-type:text/plain\nhost:storage.googleapis.com\nx-goog-meta-reviewer:jane,john\n\ncontent-type;host;x-goog-meta-reviewer\nUNSIGNED-PAYLOAD"
    );
}

#[test]
fn content_sha256_is_bound_when_supplied_and_extension_headers_must_be_signed() {
    let now = parse_timestamp(DATE).unwrap();
    for payload in [
        "UNSIGNED-PAYLOAD",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-content-sha256", payload.parse().unwrap());
        let url = signed("http://localhost/bucket/o", &Method::GET, headers.clone());
        assert!(verify(&url, &Method::GET, headers.clone(), now).is_ok());
        headers.insert("x-goog-content-sha256", "changed".parse().unwrap());
        assert_eq!(
            verify(&url, &Method::GET, headers, now),
            Err(Failure::Malformed)
        );
    }
    let url = signed("http://localhost/bucket/o", &Method::GET, HeaderMap::new());
    let mut headers = HeaderMap::new();
    headers.insert("x-goog-if-generation-match", "1".parse().unwrap());
    assert_eq!(
        verify(&url, &Method::GET, headers, now),
        Err(Failure::Malformed)
    );
    for signed in ["host;host", "x-required;host", "HOST", ""] {
        assert_eq!(
            verify(
                &url.replace(
                    "X-Goog-SignedHeaders=host",
                    &format!("X-Goog-SignedHeaders={signed}")
                ),
                &Method::GET,
                HeaderMap::new(),
                now
            ),
            Err(Failure::Malformed)
        );
    }
}

#[test]
fn registry_accepts_only_bounded_public_rsa_pem_and_rejects_private_material() {
    assert!(PublicKey::parse(PUBLIC).is_ok());
    for pem in [
        String::from_utf8(PRIVATE.to_vec()).unwrap(),
        format!("{PUBLIC}{PUBLIC}"),
        PUBLIC.replace("MIIB", "AAAA"),
        "-----BEGIN PUBLIC KEY-----\nAA==\n-----END PUBLIC KEY-----".into(),
        "x".repeat(4097),
    ] {
        assert!(PublicKey::parse(&pem).is_err());
        let value = json!({"identities":[{"id":SIGNER,"tokens":[],"public_keys":[pem]}]});
        assert!(
            Registry::parse(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .contains("public_keys")
        );
    }
    let value = json!({"identities":[{"id":SIGNER,"tokens":[],"public_keys":vec![PUBLIC;9]}]});
    assert!(Registry::parse(&serde_json::to_vec(&value).unwrap()).is_err());
}
