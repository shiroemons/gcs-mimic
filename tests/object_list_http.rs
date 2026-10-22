mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
}

impl Server {
    fn start(dir: &TestDir) -> Self {
        let mut child = common::server_command()
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&dir.0)
            .args(["--api-base-url", "https://advertised.invalid/api"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            address: ready["listen"].as_str().unwrap().to_owned(),
        }
    }

    fn request(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: untrusted.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(body).unwrap()
            },
        )
    }

    fn bucket(&self) {
        assert_eq!(
            self.request(
                "POST",
                "/storage/v1/b?project=local",
                r#"{"name":"test-bucket"}"#
            )
            .0,
            200
        );
    }

    fn put(&self, name: &str) -> Value {
        let (status, value) = self.request(
            "POST",
            &format!(
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name={}",
                encode(name)
            ),
            "body",
        );
        assert_eq!(status, 200, "{value}");
        value
    }

    fn list(&self, query: &str) -> Value {
        let (status, value) =
            self.request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "");
        assert_eq!(status, 200, "{query}: {value}");
        value
    }

    fn pages(&self, query: &str, size: usize) -> (Vec<String>, Vec<String>) {
        let mut token = String::new();
        let mut items = Vec::new();
        let mut prefixes = Vec::new();
        for _ in 0..100 {
            let page = self.list(&format!("{query}&maxResults={size}&pageToken={token}"));
            assert_eq!(page["kind"], "storage#objects");
            let names = names(&page);
            let groups = prefixes_of(&page);
            assert!(names.len() + groups.len() <= size, "{page}");
            items.extend(names);
            prefixes.extend(groups);
            match page["nextPageToken"].as_str() {
                Some(next) => {
                    assert_ne!(token, next);
                    token = next.to_owned();
                }
                None => return (items, prefixes),
            }
        }
        panic!("listing failed to finish");
    }

    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

fn encode(value: &str) -> String {
    value.bytes().map(|byte| format!("%{byte:02X}")).collect()
}

fn names(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| item["name"].as_str().unwrap().to_owned())
        .collect()
}

fn prefixes_of(page: &Value) -> Vec<String> {
    page["prefixes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|prefix| prefix.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn object_list_resources_defaults_empty_and_restart() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    server.bucket();
    assert_eq!(server.list(""), json!({"kind": "storage#objects"}));
    for name in ["a", "dir/file", "雪 +%2F"] {
        server.put(name);
    }
    let (status, patched) = server.request(
        "PATCH",
        "/storage/v1/b/test-bucket/o/a?ifMetagenerationMatch=1",
        r#"{"contentType":"text/plain","cacheControl":"private, max-age=60","metadata":{"team":"dev"}}"#,
    );
    assert_eq!(status, 200, "{patched}");
    assert_eq!(patched["metageneration"], "2");
    assert_eq!(patched["contentType"], "text/plain");
    assert_eq!(patched["cacheControl"], "private, max-age=60");
    assert_eq!(patched["metadata"], json!({"team":"dev"}));
    let page = server.list("versions=false&softDeleted=false&includeFoldersAsPrefixes=false&projection=noAcl&prettyPrint=false&alt=json&matchGlob=&filter=");
    assert_eq!(page["items"][0], patched);
    assert_eq!(names(&page), ["a", "dir/file", "雪 +%2F"]);
    assert!(page.get("prefixes").is_none());
    assert!(page.get("nextPageToken").is_none());
    for item in page["items"].as_array().unwrap() {
        assert_eq!(
            &server
                .request(
                    "GET",
                    &format!(
                        "/storage/v1/b/test-bucket/o/{}",
                        encode(item["name"].as_str().unwrap())
                    ),
                    ""
                )
                .1,
            item
        );
    }
    assert_eq!(server.list("projection=full"), page);
    assert_eq!(server.list("includeTrailingDelimiter=true"), page);
    server.stop();
    let server = Server::start(&dir);
    assert_eq!(server.list(""), page);
}

#[test]
fn object_list_prefix_delimiter_offsets_unicode_and_single_entry_pages() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.bucket();
    for name in [
        "a", "a/", "a//", "a/b", "a/c", "b", "c/deep/x", "z", "é/x", "雪", "雪/子", "雪/次",
    ] {
        server.put(name);
    }
    let (items, prefixes) = server.pages("delimiter=%2F", 1);
    assert_eq!(items, ["a", "b", "z", "雪"]);
    assert_eq!(prefixes, ["a/", "c/", "é/", "雪/"]);
    let (items, prefixes) = server.pages("delimiter=%2F&includeTrailingDelimiter=true", 1);
    assert_eq!(items, ["a", "a/", "b", "z", "雪"]);
    assert_eq!(prefixes, ["a/", "c/", "é/", "雪/"]);
    assert_eq!(
        server.pages("prefix=a%2F&delimiter=%2F&includeTrailingDelimiter=true", 1),
        (
            vec!["a/".to_owned(), "a/b".into(), "a/c".into()],
            vec!["a//".into()]
        )
    );
    assert_eq!(
        names(&server.list("prefix=a")),
        ["a", "a/", "a//", "a/b", "a/c"]
    );
    assert_eq!(
        server.pages("startOffset=a%2Fb&endOffset=z&delimiter=%2F", 1),
        (vec!["b".to_owned()], vec!["a/".to_owned(), "c/".to_owned()])
    );
    assert_eq!(
        names(&server.list(&format!(
            "prefix={}&startOffset={}&endOffset={}",
            encode("雪"),
            encode("雪"),
            encode("雪/次")
        ))),
        ["雪", "雪/子"]
    );
    for query in [
        "startOffset=z&endOffset=a",
        "startOffset=b&endOffset=b",
        "prefix=absent",
        "prefix=a&endOffset=a",
        "prefix=a&startOffset=z",
    ] {
        assert_eq!(server.list(query), json!({"kind":"storage#objects"}));
    }
    server.put("x::one");
    server.put("x::two");
    server.put("x::");
    server.put("x::::");
    assert_eq!(
        server.pages("prefix=x&delimiter=%3A%3A&includeTrailingDelimiter=true", 1),
        (vec!["x::".to_owned()], vec!["x::".to_owned()])
    );
    server.put("𐀀/child");
    server.put("𐀁");
    assert_eq!(
        server
            .pages(&format!("prefix={}&delimiter=%2F", encode("𐀀")), 1)
            .1,
        ["𐀀/"]
    );
    server.put("utf\u{7ff}a");
    server.put("utf\u{800}");
    assert_eq!(
        server.pages(&format!("prefix=utf&delimiter={}", encode("\u{7ff}")), 1),
        (vec!["utf\u{800}".to_owned()], vec!["utf\u{7ff}".to_owned()])
    );
}

#[test]
fn object_list_invalid_unsupported_and_missing_bucket_errors() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.bucket();
    for query in [
        "maxResults=-1",
        "maxResults=",
        "maxResults=4294967296",
        "maxResults=1.5",
        "maxResults=%2B1",
        "pageToken=bad",
        "projection=bad",
        "versions=1",
        "softDeleted=bad",
        "includeTrailingDelimiter=bad",
        "includeFoldersAsPrefixes=bad",
        "unknown=1",
        "prefix=%FF",
        "prefix=%",
        "prefix=a&pre%66ix=b",
        "generation=1",
        "ifGenerationMatch=1",
    ] {
        let (status, value) =
            server.request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "");
        assert_eq!(status, 400, "{query}: {value}");
        assert_eq!(
            value["error"]["errors"][0]["reason"], "invalidParameter",
            "{query}"
        );
    }
    for query in [
        "matchGlob=a*",
        "filter=context-filter",
        "fields=items/name",
        "userProject=local",
        "softDeleted=true",
        "includeFoldersAsPrefixes=true",
        "prettyPrint=true",
        "alt=media",
        "key=x",
        "oauth_token=x",
        "callback=x",
        "quotaUser=x",
        "userIp=x",
    ] {
        let (status, value) =
            server.request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "");
        assert_eq!(status, 501, "{query}: {value}");
        assert_eq!(value["error"]["errors"][0]["reason"], "notImplemented");
    }
    let (status, value) = server.request("GET", "/storage/v1/b/missing/o", "");
    assert_eq!(status, 404);
    assert_eq!(value["error"]["errors"][0]["reason"], "notFound");
    let (status, value) = server.request("GET", "/storage/v1/b/test-bucket/o", "{}");
    assert_eq!(status, 400);
    assert_eq!(value["error"]["errors"][0]["reason"], "invalidArgument");
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o", "")
            .0,
        405
    );
    assert_eq!(
        server
            .request("POST", "/storage/v1/b/test-bucket/o", "{}")
            .0,
        404
    );
    for method in ["PUT", "PATCH"] {
        assert_eq!(
            server.request(method, "/storage/v1/b/test-bucket/o", "").0,
            405
        );
    }
    assert_eq!(
        server
            .request("GET", "/download/storage/v1/b/test-bucket/o", "")
            .0,
        404
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o?alt=bad", "")
            .1["error"]["errors"][0]["reason"],
        "invalidAltValue"
    );
}

#[test]
fn object_list_tokens_bind_query_and_reject_corruption() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.bucket();
    server.put("a/");
    server.put("a/child");
    server.put("b");
    let first = server.list("delimiter=%2F&maxResults=1&includeTrailingDelimiter=true");
    let token = first["nextPageToken"].as_str().unwrap();
    assert_eq!(
        server.list(&format!(
            "delimiter=%2F&includeTrailingDelimiter=true&maxResults=100&pageToken={token}"
        ))["prefixes"],
        json!(["a/"])
    );
    for extra in [
        "prefix=a",
        "delimiter=x",
        "startOffset=a",
        "endOffset=z",
        "includeTrailingDelimiter=false",
        "projection=full",
    ] {
        let query =
            if extra.starts_with("delimiter=") || extra.starts_with("includeTrailingDelimiter=") {
                format!("{extra}&pageToken={token}")
            } else {
                format!("delimiter=%2F&includeTrailingDelimiter=true&{extra}&pageToken={token}")
            };
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "")
                .0,
            400,
            "{extra}"
        );
    }
    assert_eq!(server.request("GET", &format!("/storage/v1/b/other-bucket/o?delimiter=%2F&includeTrailingDelimiter=true&pageToken={token}"), "").0, 400);
    let mut corrupted = token.as_bytes().to_vec();
    corrupted[10] = if corrupted[10] == b'0' { b'1' } else { b'0' };
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "/storage/v1/b/test-bucket/o?pageToken={}",
                    String::from_utf8(corrupted).unwrap()
                ),
                ""
            )
            .0,
        400
    );
    assert_eq!(server.list("pageToken="), server.list(""));
}

#[test]
fn object_list_mutations_between_pages_do_not_duplicate_or_skip_survivors() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    server.bucket();
    for name in ["a", "b", "c", "d", "f"] {
        server.put(name);
    }
    let first = server.list("maxResults=2");
    assert_eq!(names(&first), ["a", "b"]);
    let token = first["nextPageToken"].as_str().unwrap().to_owned();
    for name in ["a", "b", "c"] {
        assert_eq!(
            server
                .request("DELETE", &format!("/storage/v1/b/test-bucket/o/{name}"), "")
                .0,
            204
        );
    }
    server.put("aa");
    server.put("e");
    server.put("d");
    server.stop();
    let server = Server::start(&dir);
    assert_eq!(
        names(&server.list(&format!("maxResults=2&pageToken={token}"))),
        ["d", "e"]
    );
    let page = server.list(&format!("maxResults=2&pageToken={token}"));
    assert_eq!(
        names(&server.list(&format!(
            "pageToken={}",
            page["nextPageToken"].as_str().unwrap()
        ))),
        ["f"]
    );
    server.put("g/");
    server.put("g/old");
    server.put("z");
    let page = server.list("prefix=g&delimiter=%2F&includeTrailingDelimiter=true&maxResults=1");
    let token = page["nextPageToken"].as_str().unwrap();
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/g%2F", "")
            .0,
        204
    );
    server.put("g/new");
    assert_eq!(
        server.list(&format!(
            "prefix=g&delimiter=%2F&includeTrailingDelimiter=true&pageToken={token}"
        ))["prefixes"],
        json!(["g/"])
    );
    let page = server.list("startOffset=g&delimiter=%2F&maxResults=1");
    assert_eq!(prefixes_of(&page), ["g/"]);
    assert!(page.get("items").is_none());
    server.put("g/again");
    server.put("h");
    let page = server.list(&format!(
        "startOffset=g&delimiter=%2F&pageToken={}",
        page["nextPageToken"].as_str().unwrap()
    ));
    assert_eq!(names(&page), ["h", "z"]);
    assert!(page.get("prefixes").is_none());
}

#[test]
fn object_list_default_and_upper_page_limit_are_1000_for_both_projections() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.bucket();
    for index in 0..1001 {
        server.put(&format!("object-{index:04}"));
    }
    for query in [
        "",
        "projection=full",
        "maxResults=0",
        "maxResults=4294967295",
    ] {
        let page = server.list(query);
        assert_eq!(names(&page).len(), 1000);
        let token = page["nextPageToken"].as_str().unwrap();
        let page = server.list(&format!("{query}&pageToken={token}"));
        assert_eq!(names(&page), ["object-1000"]);
        assert!(page.get("nextPageToken").is_none());
    }
}
