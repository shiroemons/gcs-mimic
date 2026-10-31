mod object_effects_support;

use object_effects_support::{Reply, Server, common::TestDir};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    time::Duration,
};

const CREATE: &str = "/storage/v1/b?project=local";

/// Like the shared helper, but the caller supplies Content-Type (multipart and XML bodies).
fn send(server: &Server, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut response_headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.trim_end().split_once(':').unwrap();
        response_headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    let mut body = Vec::new();
    if response_headers
        .get("transfer-encoding")
        .is_some_and(|value| value == "chunked")
    {
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let size = usize::from_str_radix(line.trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..]).unwrap();
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).unwrap();
        }
    } else {
        reader.read_to_end(&mut body).unwrap();
    }
    Reply {
        status,
        headers: response_headers,
        body,
    }
}

fn create(server: &Server, body: Value) -> Reply {
    server.request("POST", CREATE, "", body.to_string().as_bytes())
}

fn bucket(server: &Server, name: &str) -> Reply {
    server.request("GET", &format!("/storage/v1/b/{name}"), "", b"")
}

fn patch(server: &Server, name: &str, body: Value) -> Reply {
    server.request(
        "PATCH",
        &format!("/storage/v1/b/{name}"),
        "",
        body.to_string().as_bytes(),
    )
}

fn multipart(server: &Server, bucket: &str, name: &str, metadata: Value) -> Reply {
    let mut body = b"--boundary\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n".to_vec();
    body.extend_from_slice(metadata.to_string().as_bytes());
    body.extend_from_slice(
        b"\r\n--boundary\r\nContent-Type: text/plain\r\n\r\nx\r\n--boundary--\r\n",
    );
    send(
        server,
        "POST",
        &format!("/upload/storage/v1/b/{bucket}/o?uploadType=multipart&name={name}"),
        "Content-Type: multipart/related; boundary=boundary\r\n",
        &body,
    )
}

fn resumable(server: &Server, bucket: &str, name: &str, metadata: Value) -> Reply {
    let initiated = server.request(
        "POST",
        &format!("/upload/storage/v1/b/{bucket}/o?uploadType=resumable&name={name}"),
        "",
        metadata.to_string().as_bytes(),
    );
    if initiated.status != 200 {
        return initiated;
    }
    let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
    server.request(
        "PUT",
        uri.path_and_query().unwrap().as_str(),
        "Content-Range: bytes 0-0/1\r\n",
        b"x",
    )
}

/// Server-computed Autoclass times fall within the mutating request. Rendering trims trailing
/// fractional zeroes beyond the third digit, so pad the fraction before comparing UTC
/// RFC 3339 strings lexicographically.
fn assert_between(before: &Value, time: &Value, after: &Value) {
    let (before, time, after) = (comparable(before), comparable(time), comparable(after));
    assert!(
        before <= time && time <= after,
        "{before} <= {time} <= {after}"
    );
}

fn comparable(time: &Value) -> String {
    let time = time.as_str().unwrap();
    let (seconds, fraction) = time
        .strip_suffix('Z')
        .unwrap()
        .split_once('.')
        .unwrap_or((time.strip_suffix('Z').unwrap(), ""));
    format!("{seconds}.{fraction:0<9}Z")
}

fn object(server: &Server, bucket: &str, name: &str) -> Value {
    server
        .request("GET", &format!("/storage/v1/b/{bucket}/o/{name}"), "", b"")
        .json()
}

#[test]
fn bucket_autoclass_create_get_patch_and_restrictions() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);

    let created = create(&server, json!({"name":"auto","autoclass":{"enabled":true}})).json();
    let autoclass = &created["autoclass"];
    assert_eq!(autoclass["enabled"], true);
    assert_eq!(autoclass["terminalStorageClass"], "NEARLINE");
    assert_eq!(autoclass["toggleTime"], created["timeCreated"]);
    assert_eq!(
        autoclass["terminalStorageClassUpdateTime"],
        created["timeCreated"]
    );
    assert_eq!(created["storageClass"], "STANDARD");
    assert_eq!(bucket(&server, "auto").json()["autoclass"], *autoclass);

    let archive = create(
        &server,
        json!({"name":"auto-archive","storageClass":"STANDARD",
               "autoclass":{"enabled":true,"terminalStorageClass":"ARCHIVE","toggleTime":"2000-01-01T00:00:00Z"}}),
    )
    .json();
    assert_eq!(archive["autoclass"]["terminalStorageClass"], "ARCHIVE");
    assert_eq!(
        archive["autoclass"]["toggleTime"], archive["timeCreated"],
        "output-only toggleTime is server computed"
    );

    for body in [
        json!({"name":"rejected","autoclass":{"enabled":true,"terminalStorageClass":"COLDLINE"}}),
        json!({"name":"rejected","autoclass":{"enabled":true,"terminalStorageClass":"STANDARD"}}),
        json!({"name":"rejected","autoclass":{"enabled":true},"storageClass":"NEARLINE"}),
        json!({"name":"rejected","autoclass":{"enabled":true},"lifecycle":{"rule":[
            {"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":30}}]}}),
        json!({"name":"rejected","autoclass":{"enabled":true},"lifecycle":{"rule":[
            {"action":{"type":"Delete"},"condition":{"matchesStorageClass":["STANDARD"]}}]}}),
        json!({"name":"rejected","autoclass":{"enabled":true,"unknown":1}}),
    ] {
        create(&server, body.clone()).error(400, "invalidArgument");
        assert_eq!(bucket(&server, "rejected").status, 404, "{body}");
    }
    // Delete-only lifecycle rules may coexist with Autoclass.
    create(
        &server,
        json!({"name":"auto-delete-rule","autoclass":{"enabled":true},"lifecycle":{"rule":[
            {"action":{"type":"Delete"},"condition":{"age":30}}]}}),
    )
    .json();

    let terminal = patch(
        &server,
        "auto",
        json!({"autoclass":{"terminalStorageClass":"ARCHIVE"}}),
    )
    .json();
    assert_eq!(terminal["autoclass"]["enabled"], true);
    assert_eq!(terminal["autoclass"]["terminalStorageClass"], "ARCHIVE");
    assert_eq!(terminal["autoclass"]["toggleTime"], autoclass["toggleTime"]);
    assert_between(
        &created["updated"],
        &terminal["autoclass"]["terminalStorageClassUpdateTime"],
        &terminal["updated"],
    );

    for body in [
        json!({"storageClass":"NEARLINE"}),
        json!({"lifecycle":{"rule":[{"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}}]}}),
        json!({"autoclass":{"terminalStorageClass":"COLDLINE"}}),
        json!({"autoclass":"enabled"}),
    ] {
        patch(&server, "auto", body.clone()).error(400, "invalidArgument");
        assert_eq!(
            bucket(&server, "auto").json()["metageneration"],
            terminal["metageneration"],
            "{body}"
        );
    }

    // PUT bodies that omit autoclass preserve it, matching the local PUT contract.
    let replaced = server
        .request(
            "PUT",
            "/storage/v1/b/auto",
            "",
            br#"{"labels":{"team":"storage"}}"#,
        )
        .json();
    assert_eq!(replaced["autoclass"], terminal["autoclass"]);

    let disabled = patch(&server, "auto", json!({"autoclass":{"enabled":false}})).json();
    let toggle = &disabled["autoclass"]["toggleTime"];
    assert_eq!(
        disabled["autoclass"],
        json!({"enabled":false,"toggleTime":toggle})
    );
    assert_between(&replaced["updated"], toggle, &disabled["updated"]);
    patch(&server, "auto", json!({"autoclass":{"enabled":true}})).error(400, "invalidArgument");
    patch(
        &server,
        "auto",
        json!({"autoclass":{"terminalStorageClass":"ARCHIVE"}}),
    )
    .error(400, "invalidArgument");
    // Disabled Autoclass lifts the class restriction.
    assert_eq!(
        patch(&server, "auto", json!({"storageClass":"NEARLINE"})).json()["storageClass"],
        "NEARLINE"
    );

    let null = patch(&server, "auto-archive", json!({"autoclass":null})).json();
    assert_eq!(null["autoclass"]["enabled"], false);

    // The configuration survives a restart.
    server.stop();
    let server = Server::start(&dir, false);
    let reloaded = bucket(&server, "auto").json();
    assert_eq!(reloaded["autoclass"], disabled["autoclass"]);
    assert_eq!(
        bucket(&server, "auto-delete-rule").json()["autoclass"]["enabled"],
        true
    );
}

#[test]
fn bucket_autoclass_enable_moves_bucket_and_objects_to_standard() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    create(&server, json!({"name":"cold","storageClass":"NEARLINE"})).json();
    let before = multipart(&server, "cold", "existing", json!({})).json();
    assert_eq!(before["storageClass"], "NEARLINE");
    let archived = multipart(
        &server,
        "cold",
        "archived",
        json!({"storageClass":"ARCHIVE"}),
    )
    .json();
    assert_eq!(archived["storageClass"], "ARCHIVE");

    // An explicit non-STANDARD class in the enabling request is rejected without mutation.
    patch(
        &server,
        "cold",
        json!({"autoclass":{"enabled":true},"storageClass":"COLDLINE"}),
    )
    .error(400, "invalidArgument");
    assert!(bucket(&server, "cold").json().get("autoclass").is_none());
    assert_eq!(
        object(&server, "cold", "existing")["storageClass"],
        "NEARLINE"
    );

    let enabled = patch(&server, "cold", json!({"autoclass":{"enabled":true}})).json();
    assert_eq!(enabled["storageClass"], "STANDARD");
    assert_between(
        &archived["timeCreated"],
        &enabled["autoclass"]["toggleTime"],
        &enabled["updated"],
    );
    for name in ["existing", "archived"] {
        let value = object(&server, "cold", name);
        assert_eq!(value["storageClass"], "STANDARD", "{name}");
        assert_eq!(value["metageneration"], "1", "{name}");
    }
}

#[test]
fn object_storage_class_on_write_json_xml_and_autoclass() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    create(&server, json!({"name":"plain"})).json();
    create(&server, json!({"name":"auto","autoclass":{"enabled":true}})).json();

    for (name, class) in [
        ("cold", "COLDLINE"),
        ("near", "NEARLINE"),
        ("arch", "ARCHIVE"),
        ("std", "STANDARD"),
    ] {
        let written = multipart(&server, "plain", name, json!({"storageClass":class})).json();
        assert_eq!(written["storageClass"], class);
        assert_eq!(object(&server, "plain", name)["storageClass"], class);
    }
    let default = multipart(&server, "plain", "default", json!({"storageClass":null})).json();
    assert_eq!(default["storageClass"], "STANDARD");
    let resumed = resumable(
        &server,
        "plain",
        "resumed",
        json!({"storageClass":"ARCHIVE"}),
    )
    .json();
    assert_eq!(resumed["storageClass"], "ARCHIVE");
    resumable(&server, "plain", "bad", json!({"storageClass":"GLACIER"}))
        .error(400, "invalidArgument");

    // Autoclass stores new objects as STANDARD even when another class is requested.
    let auto = multipart(&server, "auto", "cold", json!({"storageClass":"COLDLINE"})).json();
    assert_eq!(auto["storageClass"], "STANDARD");
    let auto_resumed = resumable(
        &server,
        "auto",
        "resumed",
        json!({"storageClass":"ARCHIVE"}),
    )
    .json();
    assert_eq!(auto_resumed["storageClass"], "STANDARD");
    multipart(&server, "auto", "bad", json!({"storageClass":"GLACIER"}))
        .error(400, "invalidArgument");

    // Rewrite keeps its class change outside Autoclass and ignores it inside.
    let rewritten = server
        .request(
            "POST",
            "/storage/v1/b/plain/o/std/rewriteTo/b/plain/o/rewritten",
            "",
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json();
    assert_eq!(rewritten["resource"]["storageClass"], "NEARLINE");
    let into_auto = server
        .request(
            "POST",
            "/storage/v1/b/plain/o/cold/rewriteTo/b/auto/o/rewritten",
            "",
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json();
    assert_eq!(into_auto["resource"]["storageClass"], "STANDARD");

    // XML PUT accepts x-goog-storage-class and reports it on reads.
    let put = send(
        &server,
        "PUT",
        "/plain/xml-cold",
        "x-goog-storage-class: COLDLINE\r\nContent-Type: text/plain\r\n",
        b"x",
    );
    assert_eq!(put.status, 200, "{}", String::from_utf8_lossy(&put.body));
    assert_eq!(
        object(&server, "plain", "xml-cold")["storageClass"],
        "COLDLINE"
    );
    let head = send(&server, "GET", "/plain/xml-cold", "", b"");
    assert_eq!(head.headers["x-goog-storage-class"], "COLDLINE");
    let auto_put = send(
        &server,
        "PUT",
        "/auto/xml-cold",
        "x-goog-storage-class: ARCHIVE\r\n",
        b"x",
    );
    assert_eq!(
        auto_put.status,
        200,
        "{}",
        String::from_utf8_lossy(&auto_put.body)
    );
    let bad_put = send(
        &server,
        "PUT",
        "/plain/xml-bad",
        "x-goog-storage-class: GLACIER\r\n",
        b"x",
    );
    assert_eq!(bad_put.status, 400);
    assert!(String::from_utf8_lossy(&bad_put.body).contains("InvalidArgument"));
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/plain/o/xml-bad", "", b"")
            .status,
        404
    );
    assert_eq!(
        object(&server, "auto", "xml-cold")["storageClass"],
        "STANDARD"
    );

    // Metadata updates still treat storageClass as output-only.
    patch_object_class(&server).error(400, "badRequest");
    assert_eq!(object(&server, "plain", "cold")["storageClass"], "COLDLINE");
}

fn patch_object_class(server: &Server) -> Reply {
    server.request(
        "PATCH",
        "/storage/v1/b/plain/o/cold",
        "",
        br#"{"storageClass":"ARCHIVE"}"#,
    )
}
