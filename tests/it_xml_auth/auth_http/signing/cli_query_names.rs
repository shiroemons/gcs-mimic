//! V4 URLs in the byte form of `gsutil signurl` and `gcloud storage sign-url`: lowercase names,
//! the signature first and the signed parameters sorted after it. The URLs are built here
//! rather than by the server's own signer, so a shared canonicalization mistake cannot pass.

use super::*;
use gcs_mimic::auth::signature::{encode_query_component, sign_policy, timestamp};
use ring::digest;
use std::time::Duration;

const ALGORITHM: &str = "GOOG4-RSA-SHA256";
const VIEWER: &str = "viewer@example.invalid";
const CREATOR: &str = "creator@example.invalid";
const STORAGE_ADMIN: &str = "admin@example.invalid";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What a URL signs: the request it authorizes and when.
struct Request<'a> {
    method: &'a str,
    path: &'a str,
    signer: &'a str,
    time: SystemTime,
    headers: &'a [(&'a str, &'a str)],
}

/// `request` signed with its query names spelled by `spell`, sorted by the bytes as sent.
fn url_spelled(server: &Server, request: &Request<'_>, spell: impl Fn(&str) -> String) -> String {
    let date = timestamp(request.time).unwrap();
    let scope = format!("{}/auto/storage/goog4_request", &date[..8]);
    let mut signed: Vec<(&str, &str)> = request.headers.to_vec();
    signed.push(("host", &server.address));
    signed.sort_unstable();
    let names = signed
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let mut pairs = [
        ("x-goog-algorithm", ALGORITHM.to_owned()),
        ("x-goog-credential", format!("{}/{scope}", request.signer)),
        ("x-goog-date", date.clone()),
        ("x-goog-expires", "60".to_owned()),
        ("x-goog-signedheaders", names.clone()),
    ]
    .map(|(name, value)| (spell(name), encode_query_component(&value)));
    pairs.sort_unstable();
    let query = pairs
        .map(|(name, value)| format!("{name}={value}"))
        .join("&");
    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let canonical = format!(
        "{}\n{}\n{query}\n{canonical_headers}\n{names}\nUNSIGNED-PAYLOAD",
        request.method, request.path
    );
    let hash = digest::digest(&digest::SHA256, canonical.as_bytes());
    let message = format!("{ALGORITHM}\n{date}\n{scope}\n{}", hex(hash.as_ref()));
    let signature = sign_policy(&message, PRIVATE).unwrap();
    format!(
        "http://{}{}?{}={signature}&{query}",
        server.address,
        request.path,
        spell("x-goog-signature")
    )
}

fn cli_url(server: &Server, request: &Request<'_>) -> String {
    url_spelled(server, request, str::to_owned)
}

fn get_object(time: SystemTime) -> Request<'static> {
    Request {
        method: "GET",
        path: "/iam-bucket/object",
        signer: VIEWER,
        time,
        headers: &[],
    }
}

#[test]
fn cli_shaped_urls_authorize_get_head_put_delete_and_resumable_post() {
    let (_config, _data, server) = fixture();
    let now = SystemTime::now();
    let get = cli_url(&server, &get_object(now));
    assert!(get.contains("?x-goog-signature="));
    assert!(get.contains("&x-goog-algorithm=GOOG4-RSA-SHA256&x-goog-credential="));
    assert_eq!(
        send(&server, "GET", &get, "").success().body,
        b"hello world"
    );

    // The sent order of the parameters is not part of the signature.
    let (base, query) = get.split_once('?').unwrap();
    let (signature, rest) = query.split_once('&').unwrap();
    assert_eq!(
        send(&server, "GET", &format!("{base}?{rest}&{signature}"), "")
            .success()
            .body,
        b"hello world"
    );

    let head = cli_url(
        &server,
        &Request {
            method: "HEAD",
            ..get_object(now)
        },
    );
    send(&server, "HEAD", &head, "").expect(200);

    let put = cli_url(
        &server,
        &Request {
            method: "PUT",
            path: "/iam-bucket/new",
            signer: CREATOR,
            ..get_object(now)
        },
    );
    put::request(&server, "PUT", &put, "", b"uploaded").success();
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/new").success().body,
        b"uploaded"
    );

    let delete = cli_url(
        &server,
        &Request {
            method: "DELETE",
            path: "/iam-bucket/new",
            signer: STORAGE_ADMIN,
            ..get_object(now)
        },
    );
    send(&server, "DELETE", &delete, "").expect(204);
    error(&server.get(ADMIN, "/iam-bucket/new"), 404, "NoSuchKey");

    let start = cli_url(
        &server,
        &Request {
            method: "POST",
            path: "/iam-bucket/resumed",
            signer: CREATOR,
            time: now,
            headers: &[("x-goog-resumable", "start")],
        },
    );
    let reply = put::request(&server, "POST", &start, "x-goog-resumable: start\r\n", b"");
    let session = resumable::session_path(&reply, "/iam-bucket/resumed");
    server
        .request("PUT", &session, "Content-Range: bytes 0-4/5\r\n", b"bytes")
        .expect(200);
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/resumed").success().body,
        b"bytes"
    );
}

#[test]
fn mixed_case_names_verify_when_signed_in_the_spelling_sent() {
    let (_config, _data, server) = fixture();
    let spell = |name: &str| match name {
        "x-goog-credential" => "X-Goog-Credential".to_owned(),
        "x-goog-expires" => "X-GOOG-EXPIRES".to_owned(),
        "x-goog-signature" => "X-Goog-Signature".to_owned(),
        _ => name.to_owned(),
    };
    let url = url_spelled(&server, &get_object(SystemTime::now()), spell);
    assert_eq!(
        send(&server, "GET", &url, "").success().body,
        b"hello world"
    );
}

#[test]
fn cli_shaped_urls_keep_tamper_expiry_and_ambiguity_failures() {
    let (_config, _data, server) = fixture();
    let now = SystemTime::now();
    let url = cli_url(&server, &get_object(now));
    let first = url
        .split_once("x-goog-signature=")
        .unwrap()
        .1
        .chars()
        .next()
        .unwrap();
    let other = if first == 'f' { 'e' } else { 'f' };
    for changed in [
        url.replace("/iam-bucket/object", "/iam-bucket/other"),
        format!("{url}&extra=1"),
        url.replace("x-goog-expires=60", "x-goog-expires=61"),
        // A signer signs the spelling it sends, so changing only a name's case breaks it.
        url.replace("x-goog-date=", "X-Goog-Date="),
        url.replacen(
            &format!("x-goog-signature={first}"),
            &format!("x-goog-signature={other}"),
            1,
        ),
    ] {
        error(
            &send(&server, "GET", &changed, ""),
            403,
            "SignatureDoesNotMatch",
        );
    }
    for method in ["HEAD", "PUT", "DELETE"] {
        send(&server, method, &url, "").expect(403);
    }

    let expired = cli_url(&server, &get_object(now - Duration::from_secs(120)));
    error(&send(&server, "GET", &expired, ""), 400, "ExpiredToken");

    // Cloud Storage's answer to a parameter repeated in another case (recorded 2026-11-09).
    for repeated in [
        format!("{url}&X-Goog-Date=20261023T120000Z"),
        format!("{url}&X-GOOG-SIGNATURE=00"),
    ] {
        error(
            &send(&server, "GET", &repeated, ""),
            400,
            "ExcessHeaderValues",
        );
    }
    error(
        &send(
            &server,
            "GET",
            &format!("{url}&x-amz-date=20261023T120000Z"),
            "",
        ),
        400,
        "InvalidAuthentication",
    );
}
