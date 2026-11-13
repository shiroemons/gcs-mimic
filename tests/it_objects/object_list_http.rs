use crate::common;
use crate::common::process::declare_server;

use common::TestDir;
use common::process::http_command;
use serde_json::{Value, json};
use std::time::Duration;

impl Server {
    fn start(dir: &TestDir) -> Self {
        Self::launch(
            http_command(&dir.0).args(["--api-base-url", "https://advertised.invalid/api"]),
        )
    }

    fn request(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let reply = common::http::Client {
            address: &self.address,
            host: "untrusted.invalid",
            headers: "Content-Type: application/json\r\n",
            timeout: Duration::from_secs(10),
        }
        .request_eof(method, path, "", body.as_bytes());
        (
            reply.status,
            if reply.body.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&reply.body).unwrap()
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
        self.0.stop();
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
fn match_glob_filters_items_prefixes_and_page_tokens() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.bucket();
    for name in [
        "a.txt",
        "b.csv",
        "c.txt",
        "dir/",
        "dir/a.txt",
        "dir/sub/c.txt",
        "skip",
        "雪.txt",
    ] {
        server.put(name);
    }
    for (pattern, expected) in [
        ("*.txt", vec!["a.txt", "c.txt", "雪.txt"]),
        (
            "**/{a,c}.txt",
            vec!["a.txt", "c.txt", "dir/a.txt", "dir/sub/c.txt"],
        ),
        ("[!ab]*.txt", vec!["c.txt", "雪.txt"]),
        ("?.csv", vec!["b.csv"]),
    ] {
        let (items, prefixes) = server.pages(&format!("matchGlob={}", encode(pattern)), 1);
        assert_eq!(items, expected, "{pattern}");
        assert!(prefixes.is_empty());
    }
    let (items, prefixes) =
        server.pages(&format!("prefix=dir/&matchGlob={}", encode("**/*.txt")), 1);
    assert_eq!(items, ["dir/a.txt", "dir/sub/c.txt"]);
    assert!(prefixes.is_empty());
    let (items, prefixes) =
        server.pages("delimiter=/&includeTrailingDelimiter=true&matchGlob=**/", 1);
    assert_eq!(items, ["dir/"]);
    assert_eq!(prefixes, ["dir/"]);
    let (items, prefixes) = server.pages("delimiter=/&matchGlob=*.txt", 1);
    assert_eq!(items, ["a.txt", "c.txt", "雪.txt"]);
    assert!(prefixes.is_empty());
    assert_eq!(
        server.list("matchGlob=missing*"),
        json!({"kind":"storage#objects"})
    );
    assert_eq!(server.list("matchGlob="), server.list(""));
    let page = server.list("matchGlob=*.txt&maxResults=1");
    let token = page["nextPageToken"].as_str().unwrap();
    // A token is a position: another glob continues after its name, as Cloud Storage does for
    // another prefix, delimiter or versions flag (recorded 2026-11).
    let first = page["items"][0]["name"].as_str().unwrap().to_owned();
    let changed = format!("/storage/v1/b/test-bucket/o?matchGlob=**&pageToken={token}");
    let (status, continued) = server.request("GET", &changed, "");
    assert_eq!(status, 200, "{continued}");
    assert!(
        names(&continued).iter().all(|name| *name > first),
        "{continued}"
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o?pageToken=bogus", "")
            .0,
        400
    );
    for pattern in [
        "[",
        "[]",
        "[z-a]",
        "{a,b",
        "{a/b,c}",
        "{**,a}",
        &"a".repeat(1025),
    ] {
        let path = format!("/storage/v1/b/test-bucket/o?matchGlob={}", encode(pattern));
        let (status, error) = server.request("GET", &path, "");
        assert_eq!(status, 400, "{pattern}: {error}");
        assert!(
            error["error"]["message"].as_str().unwrap().contains("lob"),
            "{error}"
        );
    }
    // Cloud Storage's answers in case obj-list-edges (recorded 2026-11): a trailing backslash
    // lists nothing, and a glob with another delimiter is refused with its own text.
    assert_eq!(
        server.list(&format!("matchGlob={}", encode("a\\"))),
        json!({"kind":"storage#objects"})
    );
    let (status, error) = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o?matchGlob=*&delimiter=::",
        "",
    );
    assert_eq!(status, 400);
    assert_eq!(error["error"]["errors"][0]["reason"], "invalid");
    assert_eq!(
        error["error"]["message"],
        "When listing with a glob pattern, the only supported delimiter is '/'."
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o?matchGlob=&delimiter=::",
                ""
            )
            .0,
        200
    );
}

/// The recorded obj-list-edges case (2026-11): one token reused with another prefix, a
/// delimiter or the versions flag continues after the name it names.
#[test]
fn a_page_token_continues_after_its_name_under_other_listing_parameters() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    server.bucket();
    for name in ["d/a--x", "d/b--y", "d/céz"] {
        server.put(name);
    }
    let page = server.list("prefix=d/&maxResults=1");
    assert_eq!(names(&page), ["d/a--x"]);
    let token = page["nextPageToken"].as_str().unwrap();
    let other_prefix = server.list(&format!("prefix=d/b&pageToken={token}"));
    assert_eq!(names(&other_prefix), ["d/b--y"]);
    let delimited = server.list(&format!("prefix=d/&delimiter=-&pageToken={token}"));
    assert_eq!(names(&delimited), ["d/céz"]);
    assert_eq!(prefixes_of(&delimited), ["d/b-"]);
    let versions = server.list(&format!("prefix=d/&versions=true&pageToken={token}"));
    assert_eq!(names(&versions), ["d/b--y", "d/céz"]);
    server.stop();
}

#[test]
fn match_glob_versions_preserve_each_generation_and_page_boundary() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.bucket();
    let (status, value) = server.request(
        "PATCH",
        "/storage/v1/b/test-bucket",
        r#"{"versioning":{"enabled":true}}"#,
    );
    assert_eq!(status, 200, "{value}");
    let first = server.put("a.txt");
    let second = server.put("a.txt");
    server.put("b.csv");
    server.put("c.txt");
    let (items, _) = server.pages("versions=true&matchGlob=*.txt", 1);
    assert_eq!(items, ["a.txt", "a.txt", "c.txt"]);
    let page = server.list("versions=true&matchGlob=*.txt&maxResults=1");
    assert_eq!(page["items"][0]["generation"], first["generation"]);
    let token = page["nextPageToken"].as_str().unwrap();
    let next = server.list(&format!(
        "versions=true&matchGlob=*.txt&maxResults=2&pageToken={token}"
    ));
    assert_eq!(next["items"][0]["generation"], second["generation"]);
    assert_eq!(names(&next), ["a.txt", "c.txt"]);
    assert!(next.get("nextPageToken").is_none());
    let before_delete = server.list("versions=false&matchGlob=*.txt&maxResults=1");
    let live_token = before_delete["nextPageToken"].as_str().unwrap();
    server.request("DELETE", "/storage/v1/b/test-bucket/o/c.txt", "");
    let empty = server.list(&format!(
        "versions=false&matchGlob=*.txt&maxResults=1&pageToken={live_token}"
    ));
    assert_eq!(empty, json!({"kind":"storage#objects"}));
    let final_page = server.list("versions=false&matchGlob=*.txt&maxResults=1");
    assert!(final_page.get("nextPageToken").is_none());
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
    let mut full = server.list("projection=full");
    for item in full["items"].as_array_mut().unwrap() {
        assert_eq!(item["acl"].as_array().unwrap().len(), 4);
        *item = common::without_acl(item.take());
    }
    assert_eq!(full, page);
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
    // includeFoldersAsPrefixes adds only folders and managed folders, which no local bucket
    // holds, so the listing and its page tokens match the plain delimiter listing. This is
    // the request gcloud storage ls sends.
    assert_eq!(
        server.list("delimiter=%2F&includeFoldersAsPrefixes=True&projection=noAcl"),
        server.list("delimiter=%2F&projection=noAcl")
    );
    assert_eq!(
        server.pages("delimiter=%2F&includeFoldersAsPrefixes=true", 1),
        server.pages("delimiter=%2F", 1)
    );
    assert_eq!(
        server.pages(
            "prefix=a%2F&delimiter=%2F&includeFoldersAsPrefixes=true&includeTrailingDelimiter=true",
            1
        ),
        server.pages("prefix=a%2F&delimiter=%2F&includeTrailingDelimiter=true", 1)
    );
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
        // The reference requires delimiter=/ with includeFoldersAsPrefixes.
        "includeFoldersAsPrefixes=true",
        "includeFoldersAsPrefixes=true&delimiter=",
        "includeFoldersAsPrefixes=true&delimiter=-",
        "includeFoldersAsPrefixes=True&delimiter=%2F%2F",
        "prefix=%FF",
        "prefix=%",
        "prefix=a&pre%66ix=b",
    ] {
        let (status, value) =
            server.request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "");
        assert_eq!(status, 400, "{query}: {value}");
        // A bad page token is `invalid` with the token quoted (recorded); the rest are
        // invalid parameters.
        let expected = if query.starts_with("pageToken=") {
            "invalid"
        } else {
            "invalidParameter"
        };
        assert_eq!(value["error"]["errors"][0]["reason"], expected, "{query}");
    }
    for query in [
        "filter=context-filter",
        "fields=",
        "userProject=local",
        "delimiter=%2F&userProject=local",
        "alt=media",
        "oauth_token=x",
        "callback=x",
        "quotaUser=x",
        "userIp=x",
    ] {
        let (status, value) =
            server.request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "");
        assert_eq!(status, 400, "{query}: {value}");
        assert_eq!(value["error"]["errors"][0]["reason"], "badRequest");
    }
    // objects.list ignores names it does not declare, as Cloud Storage did with autoPaginate
    // and an arbitrary name (case obj-list-undeclared-query).
    for query in [
        "unknown=1",
        "autoPaginate=false",
        "generation=1",
        "ifGenerationMatch=1",
    ] {
        let (status, value) =
            server.request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "");
        assert_eq!(status, 200, "{query}: {value}");
        assert_eq!(value["kind"], "storage#objects", "{query}");
    }
    // prettyPrint only selects the body layout; a non-boolean value is still rejected.
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o?prettyPrint=true", "")
            .0,
        200
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o?prettyPrint=maybe", "")
            .0,
        400
    );
    assert!(names(&server.list("softDeleted=true")).is_empty());
    let (status, value) = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o?softDeleted=true&versions=true",
        "",
    );
    assert_eq!(status, 400, "{value}");
    assert_eq!(value["error"]["errors"][0]["reason"], "invalidArgument");
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
    let (status, value) = server.request("POST", "/storage/v1/b/test-bucket/o", "{}");
    assert_eq!(status, 400);
    assert_eq!(value["error"]["errors"][0]["reason"], "badRequest");
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
fn object_list_tokens_are_positional_and_reject_corruption() {
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
        // Cloud Storage accepted a token under other listing parameters (recorded 2026-11).
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/test-bucket/o?{query}"), "")
                .0,
            200,
            "{extra}"
        );
    }
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
    for query in ["", "projection=full", "maxResults=4294967295"] {
        let page = server.list(query);
        assert_eq!(names(&page).len(), 1000);
        let token = page["nextPageToken"].as_str().unwrap();
        let page = server.list(&format!("{query}&pageToken={token}"));
        assert_eq!(names(&page), ["object-1000"]);
        assert!(page.get("nextPageToken").is_none());
    }
    // maxResults=0 is an empty page, not the default size (recorded).
    assert_eq!(
        server.list("maxResults=0"),
        json!({"kind":"storage#objects"})
    );
}

declare_server!();
