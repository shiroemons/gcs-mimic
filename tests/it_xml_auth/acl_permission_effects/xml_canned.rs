//! `x-goog-acl` on XML writes. The values and refusals are Cloud Storage's (recorded 2026-11-09).

use super::predefined_upload::{ADMIN_OWNER, absent, pairs, stored};
use super::*;
use crate::common::errors::xml_error;

const UNIFORM_ACCESS: &str = "Cannot insert legacy ACL for an object when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access";

#[track_caller]
fn assert_details(reply: &Reply, details: &str) {
    reply.expect(400);
    let body = String::from_utf8_lossy(&reply.body);
    let expected = format!(
        "<Code>InvalidArgument</Code><Message>Invalid argument.</Message><Details>{details}</Details>"
    );
    assert!(body.contains(&expected), "{body}");
}

fn upload_id(reply: &Reply) -> String {
    let body = String::from_utf8_lossy(&reply.body);
    let (_, rest) = body.split_once("<UploadId>").unwrap();
    rest.split_once("</UploadId>").unwrap().0.to_owned()
}

/// The requests that can carry `x-goog-acl`.
#[derive(Clone, Copy, Debug)]
enum Write {
    Put,
    Resumable,
    Multipart,
    Copy,
}

const WRITES: [Write; 4] = [Write::Put, Write::Resumable, Write::Multipart, Write::Copy];

impl Write {
    /// A PUT or copy publishes here; the other two open a session for [`Self::finish`].
    fn start(self, server: &Server, name: &str, acl: &str) -> Reply {
        let path = format!("/test-bucket/{name}");
        let (method, path, extra, body): (_, _, _, &[u8]) = match self {
            Self::Put => ("PUT", path, "", b"bytes"),
            Self::Resumable => ("POST", path, "x-goog-resumable: start\r\n", b""),
            Self::Multipart => ("POST", format!("{path}?uploads"), "", b""),
            Self::Copy => (
                "PUT",
                path,
                "x-goog-copy-source: /test-bucket/source\r\n",
                b"",
            ),
        };
        let headers = format!("{ADMIN}x-goog-acl: {acl}\r\n{extra}");
        server.request(method, &path, &headers, body)
    }

    fn finish(self, server: &Server, name: &str, started: &Reply) -> Option<Reply> {
        match self {
            Self::Put | Self::Copy => None,
            Self::Resumable => {
                let uri: hyper::Uri = started.headers["location"].parse().unwrap();
                let session = uri.path_and_query().unwrap().as_str();
                Some(server.request("PUT", session, "Content-Range: bytes 0-4/5\r\n", b"bytes"))
            }
            Self::Multipart => {
                let path = format!("/test-bucket/{name}?uploadId={}", upload_id(started));
                let part = server.request("PUT", &format!("{path}&partNumber=1"), ADMIN, b"bytes");
                part.expect(200);
                let manifest = format!(
                    "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part></CompleteMultipartUpload>",
                    part.headers["etag"]
                );
                Some(server.request("POST", &path, ADMIN, manifest.as_bytes()))
            }
        }
    }

    fn started_status(self) -> u16 {
        match self {
            Self::Put | Self::Multipart | Self::Copy => 200,
            Self::Resumable => 201,
        }
    }
}

fn start_server(backend: &str) -> (TestDir, TestDir, Server) {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, backend);
    server.bucket();
    server
        .request("PUT", "/test-bucket/source", ADMIN, b"bytes")
        .expect(200);
    (data, inputs, server)
}

#[test]
fn xml_canned_acl_values_produce_the_recorded_entries_on_every_write() {
    for backend in ["disk", "memory"] {
        let (_data, _inputs, server) = start_server(backend);
        // A stored default object ACL shows that the canned value replaces it.
        server
            .admin(
                "POST",
                &format!("{BUCKET}/defaultObjectAcl"),
                json!({"entity":"user-reader@example.test","role":"READER"}),
            )
            .json_ok();
        let cases = [
            ("private", vec![ADMIN_OWNER]),
            (
                "project-private",
                vec![
                    ADMIN_OWNER,
                    ("project-owners-0", "OWNER"),
                    ("project-editors-0", "OWNER"),
                    ("project-viewers-0", "READER"),
                ],
            ),
            (
                "bucket-owner-read",
                vec![ADMIN_OWNER, ("project-owners-0", "READER")],
            ),
            (
                "bucket-owner-full-control",
                vec![ADMIN_OWNER, ("project-owners-0", "OWNER")],
            ),
            ("public-read", vec![ADMIN_OWNER, ("allUsers", "READER")]),
            (
                "authenticated-read",
                vec![ADMIN_OWNER, ("allAuthenticatedUsers", "READER")],
            ),
        ];
        for (value, entries) in cases {
            for write in WRITES {
                let name = format!("{write:?}-{value}");
                let started = write.start(&server, &name, value);
                started.expect_with(write.started_status(), format_args!("{write:?} {value}"));
                if let Some(finished) = write.finish(&server, &name, &started) {
                    finished.expect_with(200, format_args!("{write:?} {value}"));
                }
                assert_eq!(stored(&server, &name), pairs(&entries), "{write:?} {value}");
            }
        }
    }
}

#[test]
fn xml_canned_acl_applies_to_a_copy_that_replaces_metadata() {
    let (_data, _inputs, server) = start_server("disk");
    server
        .request(
            "PUT",
            "/test-bucket/replaced",
            &format!(
                "{ADMIN}x-goog-copy-source: /test-bucket/source\r\nx-goog-metadata-directive: REPLACE\r\nx-goog-meta-new: value\r\nx-goog-acl: bucket-owner-read\r\n"
            ),
            b"",
        )
        .expect(200);
    assert_eq!(
        stored(&server, "replaced"),
        pairs(&[ADMIN_OWNER, ("project-owners-0", "READER")])
    );
}

#[test]
fn xml_canned_acl_refuses_json_spellings_and_unknown_values_without_writing() {
    let (_data, _inputs, server) = start_server("disk");
    for value in [
        "projectPrivate",
        "publicRead",
        "bucketOwnerRead",
        "authenticatedRead",
        "public-read-write",
        "PRIVATE",
        "bogus",
    ] {
        for write in WRITES {
            let name = format!("{write:?}-{value}");
            assert_details(
                &write.start(&server, &name, value),
                &format!("Invalid canned acl: {value}"),
            );
            absent(&server, &name);
        }
    }
    xml_error(
        &server.request(
            "PUT",
            "/test-bucket/repeated",
            &format!("{ADMIN}x-goog-acl: private\r\nx-goog-acl: public-read\r\n"),
            b"bytes",
        ),
        400,
        "ExcessHeaderValues",
    );
    absent(&server, "repeated");
}

#[test]
fn xml_canned_acl_under_uniform_access_answers_the_recorded_error() {
    let (_data, _inputs, server) = start_server("disk");
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json_ok();
    for write in WRITES {
        let name = format!("{write:?}-uniform");
        assert_details(&write.start(&server, &name, "private"), UNIFORM_ACCESS);
        absent(&server, &name);
    }
    server
        .request("PUT", "/test-bucket/without-acl", ADMIN, b"bytes")
        .expect(200);
}

#[test]
fn xml_canned_acl_is_rechecked_at_publication_with_the_recorded_error() {
    let (_data, _inputs, server) = start_server("disk");
    let sessions: Vec<_> = [Write::Resumable, Write::Multipart]
        .into_iter()
        .map(|write| {
            let name = format!("{write:?}-late");
            let started = write.start(&server, &name, "private");
            started.expect(write.started_status());
            (write, name, started)
        })
        .collect();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json_ok();
    for (write, name, started) in sessions {
        let finished = write.finish(&server, &name, &started).unwrap();
        assert_details(&finished, UNIFORM_ACCESS);
        absent(&server, &name);
    }
}

#[test]
fn xml_canned_acl_needs_a_caller_and_respects_public_access_prevention() {
    let (_data, _inputs, server) = start_server("disk");
    // Documented for uploads: an anonymous caller cannot choose its object's ACL.
    super::bucket::grant(&server, "allUsers", "WRITER");
    xml_error(
        &server.request(
            "PUT",
            "/test-bucket/anonymous",
            "x-goog-acl: private\r\n",
            b"bytes",
        ),
        400,
        "InvalidArgument",
    );
    absent(&server, "anonymous");
    server
        .request("PUT", "/test-bucket/anonymous", "", b"bytes")
        .expect(200);

    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json_ok();
    for value in ["public-read", "authenticated-read"] {
        xml_error(
            &Write::Put.start(&server, "public", value),
            412,
            "PreconditionFailed",
        );
        absent(&server, "public");
    }
    Write::Put.start(&server, "private", "private").expect(200);
    assert_eq!(stored(&server, "private"), pairs(&[ADMIN_OWNER]));
}

#[test]
fn xml_canned_acl_is_refused_after_multipart_initiation() {
    let (_data, _inputs, server) = start_server("disk");
    let started = server.request("POST", "/test-bucket/late?uploads", ADMIN, b"");
    started.expect(200);
    let path = format!("/test-bucket/late?uploadId={}", upload_id(&started));
    let with_acl = format!("{ADMIN}x-goog-acl: private\r\n");
    xml_error(
        &server.request("PUT", &format!("{path}&partNumber=1"), &with_acl, b"bytes"),
        400,
        "InvalidArgument",
    );
    let part = server.request("PUT", &format!("{path}&partNumber=1"), ADMIN, b"bytes");
    part.expect(200);
    let manifest = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part></CompleteMultipartUpload>",
        part.headers["etag"]
    );
    xml_error(
        &server.request("POST", &path, &with_acl, manifest.as_bytes()),
        400,
        "InvalidArgument",
    );
    absent(&server, "late");
}

/// Copy and multipart publish through the derived path, which now writes the requested ACL.
#[test]
fn xml_canned_acl_write_failure_publishes_nothing_on_derived_writes() {
    let (data, _inputs, server) = start_server("disk");
    let db = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_acl BEFORE INSERT ON acl_object_entries WHEN NEW.name LIKE 'atomic-%' BEGIN SELECT RAISE(ABORT,'injected ACL failure'); END;").unwrap();
    let name = "atomic-copy";
    Write::Copy
        .start(&server, name, "project-private")
        .expect(503);
    absent(&server, name);
    let name = "atomic-multipart";
    let started = Write::Multipart.start(&server, name, "project-private");
    started.expect(200);
    Write::Multipart
        .finish(&server, name, &started)
        .unwrap()
        .expect(503);
    absent(&server, name);
}
