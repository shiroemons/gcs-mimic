use crate::common;
use common::{
    TestDir,
    process::{Server, http_command, write_request},
};
use hyper::Uri;
use std::{net::TcpStream, time::Duration};

fn request(
    server: &Server,
    method: &str,
    target: &str,
    host: &str,
    headers: &str,
    body: &[u8],
) -> common::http::Reply {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write_request(
        &mut stream,
        method,
        target,
        host,
        &format!("Connection: close\r\n{headers}"),
        body,
    );
    common::http::receive_eof(stream, common::http::Body::for_method(method)).into_last()
}

#[test]
fn signing_cli_urls_verify_through_the_live_xml_route() {
    let dir = TestDir::new();
    let server = Server::launch(http_command(&dir.0).args([
        "--auth-mode",
        "strict",
        "--identity-file",
        "config/identities.json",
        "--xml-virtual-host-domains",
        "localhost",
    ]));
    let admin = "Authorization: Bearer local-admin-token\r\nContent-Type: application/json\r\n";
    request(
        &server,
        "POST",
        "/storage/v1/b?project=local-project",
        &server.address,
        admin,
        br#"{"name":"example-bucket"}"#,
    )
    .expect(200);
    request(
        &server,
        "POST",
        "/upload/storage/v1/b/example-bucket/o?uploadType=media&name=%C3%A9%2Fa%20b",
        &server.address,
        admin,
        b"signed bytes",
    )
    .expect(200);
    for virtual_host in [false, true] {
        let base = if virtual_host {
            format!(
                "http://LOCALHOST:{}/",
                server.address.rsplit_once(':').unwrap().1
            )
        } else {
            format!("http://{}", server.address)
        };
        let config = dir.0.join("signing.json");
        std::fs::write(
            &config,
            serde_json::json!({"public_base_url":base}).to_string(),
        )
        .unwrap();
        let mut command = common::server_command();
        command.args([
            "url",
            "sign",
            "--bucket",
            "example-bucket",
            "--object",
            "é/a b",
            "--method",
            "GET",
            "--expires",
            "15m",
            "--signer",
            "local-viewer@example.invalid",
            "--private-key",
            "fixtures/signing/test-only-private.pem",
            "--header",
            "x-reviewer: jane doe",
            "--header",
            "x-reviewer:john",
        ]);
        if virtual_host {
            command
                .arg("--config")
                .arg(&config)
                .args(["--style", "virtual-hosted"]);
        } else {
            command.args(["--base-url", &base]);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let uri: Uri = text.trim().parse().unwrap();
        let host = uri.authority().unwrap().as_str();
        if virtual_host {
            assert_eq!(
                host,
                format!(
                    "example-bucket.localhost:{}",
                    server.address.rsplit_once(':').unwrap().1
                )
            );
        }
        let target = uri.path_and_query().unwrap().as_str();
        let headers = "x-reviewer: jane doe,john\r\n";
        let reply = request(&server, "GET", target, host, headers, b"");
        reply.expect(200);
        assert_eq!(reply.body, b"signed bytes");
        request(
            &server,
            "GET",
            target,
            &format!("{}:1", uri.authority().unwrap().host()),
            headers,
            b"",
        )
        .expect(403);
        request(&server, "GET", target, host, "x-reviewer: changed\r\n", b"").expect(403);
        request(
            &server,
            "GET",
            &format!("{target}&Expires=1"),
            host,
            headers,
            b"",
        )
        .expect(400);
    }
}
