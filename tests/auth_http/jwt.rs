use super::*;
use ring::{rand::SystemRandom, signature};
use std::time::UNIX_EPOCH;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, pem::PemObject};

fn encoded(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::new();
    let mut bits = 0u32;
    let mut count = 0;
    for byte in bytes {
        bits = bits << 8 | u32::from(*byte);
        count += 8;
        while count >= 6 {
            count -= 6;
            output.push(alphabet[((bits >> count) & 63) as usize] as char);
        }
    }
    if count != 0 {
        output.push(alphabet[((bits << (6 - count)) & 63) as usize] as char);
    }
    output
}

fn bearer(issuer: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({"iss":issuer,"sub":issuer,
        "scope":"https://www.googleapis.com/auth/devstorage.full_control", "iat":now - 60,"exp":now + 60});
    let message = format!(
        "{}.{}",
        encoded(br#"{"alg":"RS256","typ":"JWT"}"#),
        encoded(claims.to_string().as_bytes())
    );
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
    format!("Authorization: Bearer {message}.{}\r\n", encoded(&bytes))
}

#[test]
fn strict_jwt_bearer_upload_download_and_iam_denial_match_opaque_tokens() {
    let (_config, _data, server) = fixture();
    let admin = bearer("admin@example.invalid");
    let viewer = bearer("viewer@example.invalid");
    let creator = bearer("creator@example.invalid");
    server
        .upload(&admin, "iam-bucket", "jwt-object", b"jwt bytes")
        .ok();
    let path = "/storage/v1/b/iam-bucket/o/jwt-object?alt=media";
    assert_eq!(server.get(&viewer, path).ok().body, b"jwt bytes");
    assert_eq!(
        server.get(&viewer, "/iam-bucket/jwt-object").ok().body,
        b"jwt bytes"
    );
    server
        .upload(&viewer, "iam-bucket", "denied", b"no")
        .forbidden();
    server
        .upload(&creator, "iam-bucket", "jwt-object", b"no")
        .forbidden();
    server.get(&creator, path).forbidden();
    server
        .get(&bearer("no-roles@example.invalid"), path)
        .forbidden();
    server
        .get(&bearer("unknown@example.invalid"), path)
        .invalid_credentials();
    assert_eq!(server.get(ADMIN, path).ok().body, b"jwt bytes");
    server.get("", path).login_required();
    let mut tampered = admin.into_bytes();
    let signature = tampered.iter().rposition(|b| *b == b'.').unwrap() + 1;
    tampered[signature] = if tampered[signature] == b'A' {
        b'B'
    } else {
        b'A'
    };
    server
        .get(std::str::from_utf8(&tampered).unwrap(), path)
        .invalid_credentials();
    assert_eq!(server.get(ADMIN, path).ok().body, b"jwt bytes");
}

#[test]
fn jwt_public_principals_pap_and_preflight_remain_independent_of_authentication() {
    let (_config, _data, server) = fixture();
    let no_roles = bearer("no-roles@example.invalid");
    let admin = bearer("admin@example.invalid");
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer", "allAuthenticatedUsers")]}),
    )
    .ok();
    server
        .get("", "/storage/v1/b/iam-bucket/o/object?alt=media")
        .login_required();
    assert_eq!(
        server.get(&no_roles, "/iam-bucket/object").ok().body,
        b"hello world"
    );
    server.request("PATCH", "/storage/v1/b/iam-bucket", &admin,
        br#"{"cors":[{"origin":["http://localhost:3000"],"method":["PUT"],"responseHeader":["Content-Type","Content-MD5","Cache-Control"]}]}"#).ok();
    let preflight = server.request("OPTIONS", "/iam-bucket/object",
        "Origin: http://localhost:3000\r\nAccess-Control-Request-Method: PUT\r\nAccess-Control-Request-Headers: content-type,content-md5,cache-control\r\n", b"");
    preflight.ok();
    assert_eq!(
        preflight.headers["access-control-allow-origin"],
        "http://localhost:3000"
    );
    server
        .upload(&no_roles, "iam-bucket", "object", b"no")
        .forbidden();
    server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket",
            &admin,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .ok();
    server
        .get(&no_roles, "/storage/v1/b/iam-bucket/o/object?alt=media")
        .forbidden();
    assert_eq!(
        server.get(&admin, "/iam-bucket/object").ok().body,
        b"hello world"
    );
    let url = sign(
        &server,
        &Method::GET,
        "/iam-bucket/object",
        "admin@example.invalid",
        SystemTime::now(),
        HeaderMap::new(),
    );
    assert_eq!(send(&server, "GET", &url, "").ok().body, b"hello world");
    server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket",
            &admin,
            br#"{"iamConfiguration":{"publicAccessPrevention":"inherited"}}"#,
        )
        .ok();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer", "allUsers")]}),
    )
    .ok();
    assert_eq!(
        server.get("", "/iam-bucket/object").ok().body,
        b"hello world"
    );
    server
        .get(
            &bearer("unregistered@example.invalid"),
            "/storage/v1/b/iam-bucket/o/object?alt=media",
        )
        .invalid_credentials();
}

#[test]
fn jwt_key_rotation_takes_effect_at_restart_and_keeps_native_and_legacy_profiles() {
    for profile in ["native", "legacy-emulator"] {
        let config = TestDir::new();
        let data = TestDir::new();
        let mut value = registry();
        value["identities"][0]["public_keys"] = json!([PUBLIC]);
        let path = config.0.join("identities.json");
        std::fs::write(&path, value.to_string()).unwrap();
        let options = ["--compat-profile", profile];
        let mut server = Server::strict_with(&data, &path, &options);
        server
            .create_bucket(ADMIN, "project-a", "jwt-rotation")
            .ok();
        let jwt = bearer("admin@example.invalid");
        server
            .upload(&jwt, "jwt-rotation", "object", b"persisted")
            .ok();
        // Registry changes are deliberate restart operations, never untrusted token key URLs.
        value["identities"][0]
            .as_object_mut()
            .unwrap()
            .remove("public_keys");
        std::fs::write(&path, value.to_string()).unwrap();
        assert_eq!(
            server.get(&jwt, "/jwt-rotation/object").ok().body,
            b"persisted"
        );
        server.stop();
        let mut server = Server::strict_with(&data, &path, &options);
        server
            .get(&jwt, "/storage/v1/b/jwt-rotation/o/object?alt=media")
            .invalid_credentials();
        assert_eq!(
            server.get(ADMIN, "/jwt-rotation/object").ok().body,
            b"persisted"
        );
        value["identities"][0]["public_keys"] = json!([PUBLIC]);
        std::fs::write(&path, value.to_string()).unwrap();
        server.stop();
        let server = Server::strict_with(&data, &path, &options);
        assert_eq!(
            server.get(&jwt, "/jwt-rotation/object").ok().body,
            b"persisted"
        );
    }
}
