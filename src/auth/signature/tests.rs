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

fn v2_signed(
    method: &str,
    path: &str,
    expires: &str,
    md5: &str,
    mime: &str,
    extension: &str,
) -> String {
    let message = format!("{method}\n{md5}\n{mime}\n{expires}\n{extension}{path}");
    let key = PrivateKeyDer::from_pem_slice(PRIVATE).unwrap();
    let key = signature::RsaKeyPair::from_pkcs8(key.secret_der()).unwrap();
    let mut bytes = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        message.as_bytes(),
        &mut bytes,
    )
    .unwrap();
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for shift in [18, 12, 6, 0] {
            encoded.push(
                if (shift == 0 && chunk.len() < 3) || (shift == 6 && chunk.len() < 2) {
                    '='
                } else {
                    alphabet[((value >> shift) & 63) as usize] as char
                },
            );
        }
    }
    format!(
        "http://localhost:4443{path}?GoogleAccessId={}&Expires={expires}&Signature={}",
        encode_query_component(SIGNER),
        encode_query_component(&encoded)
    )
}

#[test]
fn v2_rsa_binds_method_literal_path_md5_type_and_canonical_extension_headers() {
    let now = UNIX_EPOCH + Duration::from_secs(1000);
    let path = "/bucket/a%20b+%25//object";
    let url = v2_signed(
        "PUT",
        path,
        "1060",
        "digest",
        "text/plain",
        "x-goog-meta-a:one two,three\nx-goog-meta-z:last\n",
    );
    let mut headers = HeaderMap::new();
    headers.insert("content-md5", "digest".parse().unwrap());
    headers.insert("content-type", "text/plain".parse().unwrap());
    headers.append("x-goog-meta-a", " one\t two ".parse().unwrap());
    headers.append("x-goog-meta-a", "three".parse().unwrap());
    headers.insert("x-goog-meta-z", "last".parse().unwrap());
    assert_eq!(
        verify(&url, &Method::PUT, headers.clone(), now),
        Ok(SIGNER.into())
    );
    for name in ["content-md5", "content-type", "x-goog-meta-a"] {
        let mut changed = headers.clone();
        changed.remove(name);
        assert_eq!(
            verify(&url, &Method::PUT, changed, now),
            Err(Failure::Mismatch)
        );
        let mut changed = headers.clone();
        changed.insert(name, "changed".parse().unwrap());
        assert_eq!(
            verify(&url, &Method::PUT, changed, now),
            Err(Failure::Mismatch)
        );
    }
    assert_eq!(
        verify(&url, &Method::GET, headers.clone(), now),
        Err(Failure::Mismatch)
    );
    assert_eq!(
        verify(
            &url.replace("%20", "%21"),
            &Method::PUT,
            headers.clone(),
            now
        ),
        Err(Failure::Mismatch)
    );
    assert_eq!(
        verify(
            &url.replace("signer%40", "unknown%40"),
            &Method::PUT,
            headers,
            now
        ),
        Err(Failure::UnknownSigner)
    );
    let get = v2_signed("GET", path, "1060", "", "", "");
    for invalid in ["AA=A", "AAAA=", "A===", "AB==", "!!!!", "éé", ""] {
        assert!(super::v2::decode_signature(invalid).is_err(), "{invalid}");
    }
    assert_eq!(
        verify(
            &format!("{get}&response-content-type=text/plain"),
            &Method::GET,
            HeaderMap::new(),
            now
        ),
        Ok(SIGNER.into())
    );
}

#[tokio::test]
async fn v2_structure_permissive_credentials_and_injected_expiry() {
    use http_body_util::BodyExt;
    let now = UNIX_EPOCH + Duration::from_secs(1000);
    let url = v2_signed("GET", "/bucket/object", "1060", "", "", "");
    for parameter in V2_PARAMETERS {
        let uri: Uri = url.parse().unwrap();
        let query = uri
            .query()
            .unwrap()
            .split('&')
            .filter(|pair| !pair.starts_with(&format!("{parameter}=")))
            .collect::<Vec<_>>()
            .join("&");
        for auth in [auth(), Auth::default()] {
            for bad in [
                format!("http://localhost/bucket/object?{query}"),
                format!("{url}&{parameter}=again"),
                format!("{url}&X-Goog-Date=20261023T120000Z"),
            ] {
                assert!(is_signed(bad.parse::<Uri>().unwrap().query()));
                assert!(matches!(
                    auth.authenticate_signed(
                        &Method::GET,
                        &bad.parse().unwrap(),
                        &HeaderMap::new(),
                        &Fixed(now)
                    ),
                    Err(Failure::Malformed)
                ));
            }
        }
    }
    for bad in ["-1", "abc", "%2B1", "18446744073709551616"] {
        assert_eq!(
            verify(
                &url.replace("Expires=1060", &format!("Expires={bad}")),
                &Method::GET,
                HeaderMap::new(),
                now
            ),
            Err(Failure::Malformed)
        );
    }
    assert_eq!(
        verify(
            &url,
            &Method::GET,
            HeaderMap::new(),
            now + Duration::from_secs(59)
        ),
        Ok(SIGNER.into())
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
    let expired = v2_signed("GET", "/bucket/object", "0", "", "", "");
    let invalid: Uri =
        "http://localhost/bucket/object?GoogleAccessId=unregistered&Expires=0&Signature=ignored"
            .parse()
            .unwrap();
    assert!(
        Auth::default()
            .authenticate_signed(&Method::GET, &invalid, &HeaderMap::new(), &Fixed(now))
            .unwrap()
            .is_none()
    );
    let auth = auth();
    let error = crate::xml::authenticate(
        &auth,
        &Method::GET,
        &expired.parse().unwrap(),
        &HeaderMap::new(),
        &Fixed(now),
    )
    .err()
    .unwrap();
    let response = error.response(false);
    assert_eq!(response.status(), hyper::StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("<Code>ExpiredToken</Code>"));
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
        // Windows SystemTime starts in 1601, so year 1 is rejected there as unrepresentable.
        if cfg!(windows) && date.starts_with("0001") {
            assert!(parse_timestamp(date).is_err(), "{date}");
            continue;
        }
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
fn canonical_query_sorts_decoded_names_before_encoding() {
    let mut headers = HeaderMap::new();
    headers.insert("host", "localhost:4443".parse().unwrap());
    let query = BTreeMap::from([
        ("a".into(), "a b".into()),
        ("+".into(), "+".into()),
        ("é".into(), "日".into()),
    ]);
    let canonical = canonical_request(
        &Method::GET,
        &"/bucket/o?a=a+b&%2B=%2B&%C3%A9=%E6%97%A5".parse().unwrap(),
        &headers,
        &query,
        "host",
    )
    .unwrap();
    assert_eq!(
        canonical,
        "GET\n/bucket/o\n%2B=%2B&a=a%20b&%C3%A9=%E6%97%A5\nhost:localhost:4443\n\nhost\nUNSIGNED-PAYLOAD"
    );
}

#[test]
fn signer_normalizes_default_ports_and_verifier_rejects_conflicting_absolute_authority() {
    let now = parse_timestamp(DATE).unwrap();
    for (base, expected) in [
        ("http://localhost:80", "http://localhost/"),
        ("https://localhost:443", "https://localhost/"),
        ("http://[::1]:80", "http://[::1]/"),
    ] {
        let url = signed(&format!("{base}/bucket/o"), &Method::GET, HeaderMap::new());
        assert!(url.starts_with(expected), "{url}");
        assert!(verify(&url, &Method::GET, HeaderMap::new(), now).is_ok());
        let mut headers = HeaderMap::new();
        headers.insert("host", base.split_once("://").unwrap().1.parse().unwrap());
        assert_eq!(
            verify(&url, &Method::GET, headers, now),
            Err(Failure::Mismatch)
        );
    }
    let url = signed("http://localhost/bucket/o", &Method::GET, HeaderMap::new());
    let mut headers = HeaderMap::new();
    headers.insert("host", "localhost".parse().unwrap());
    assert_eq!(
        verify(
            &url.replace("http://localhost/", "http://other.invalid/"),
            &Method::GET,
            headers,
            now
        ),
        Err(Failure::Mismatch)
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
        let mut canonical_headers = headers.clone();
        canonical_headers.insert("host", "localhost".parse().unwrap());
        let canonical = canonical_request(
            &Method::PUT,
            &"http://localhost/bucket/o".parse().unwrap(),
            &canonical_headers,
            &BTreeMap::new(),
            "host;x-goog-content-sha256",
        )
        .unwrap();
        assert_eq!(
            canonical,
            format!(
                "PUT\n/bucket/o\n\nhost:localhost\nx-goog-content-sha256:{payload}\n\nhost;x-goog-content-sha256\nUNSIGNED-PAYLOAD"
            )
        );
        let url = signed("http://localhost/bucket/o", &Method::GET, headers.clone());
        assert!(verify(&url, &Method::GET, headers.clone(), now).is_ok());
        headers.insert("x-goog-content-sha256", "0".repeat(64).parse().unwrap());
        assert_eq!(
            verify(&url, &Method::GET, headers.clone(), now),
            Err(Failure::Mismatch)
        );
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
