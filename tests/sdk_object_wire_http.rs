mod object_effects_support;

use object_effects_support::{Server, common::TestDir};

#[test]
fn json_object_head_preserves_selection_conditions_and_authorization() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let object = server.put("head", b"payload").json();
    let path = format!(
        "/storage/v1/b/test-bucket/o/head?generation={}",
        object["generation"].as_str().unwrap()
    );
    let metadata = server.request("GET", &path, "Authorization: Bearer admin\r\n", b"");
    let head = server.request("HEAD", &path, "Authorization: Bearer admin\r\n", b"");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.headers["etag"], metadata.headers["etag"]);
    assert_eq!(
        head.headers["content-length"],
        metadata.body.len().to_string()
    );
    for (path, headers, status) in [
        (path.as_str(), "Authorization: Bearer creator\r\n", 403),
        (
            "/storage/v1/b/test-bucket/o/missing",
            "Authorization: Bearer admin\r\n",
            404,
        ),
        (
            "/storage/v1/b/test-bucket/o/head?ifGenerationMatch=0",
            "Authorization: Bearer admin\r\n",
            412,
        ),
    ] {
        let response = server.request("HEAD", path, headers, b"");
        assert_eq!(response.status, status);
        assert!(response.body.is_empty());
    }
    assert_eq!(server.get("head").json(), object);
}
