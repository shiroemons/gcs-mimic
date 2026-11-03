use crate::common::server::declare_server;
mod common;

use common::TestDir;
use common::server::http_command;
use serde_json::{Value, json};
use std::time::Duration;

impl Server {
    fn start(dir: &TestDir) -> Self {
        Self::launch(&mut http_command(&dir.0.join("data")))
    }

    fn request(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let reply = common::http::Client {
            address: &self.address,
            host: "local.invalid",
            headers: "Content-Type: application/json\r\n",
            timeout: Duration::from_secs(5),
        }
        .request(method, path, "", body.as_bytes());
        (
            reply.status,
            serde_json::from_slice(&reply.body).unwrap_or(Value::Null),
        )
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

/// A time with a sub-millisecond fraction, far enough ahead to stay valid and active.
const FINE: &str = "2099-01-02T03:04:05.123456789Z";

fn object_path(name: &str) -> String {
    format!("/storage/v1/b/rt-bucket/o/{name}")
}

fn setup(server: &Server, names: &[&str]) {
    let (status, body) = server.request(
        "POST",
        "/storage/v1/b?project=local&enableObjectRetention=true",
        r#"{"name":"rt-bucket"}"#,
    );
    assert_eq!(status, 200, "{body}");
    for name in names {
        let (status, body) = server.request(
            "POST",
            &format!("/upload/storage/v1/b/rt-bucket/o?uploadType=media&name={name}"),
            "payload",
        );
        assert_eq!(status, 200, "{body}");
    }
}

fn set_retention(server: &Server, name: &str, mode: &str) -> Value {
    let (status, body) = server.request(
        "PATCH",
        &object_path(name),
        &json!({"retention":{"mode":mode,"retainUntilTime":FINE}}).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    body
}

/// Observed on Cloud Storage (2026-10-31): a Locked retention refuses shortening, removal and a
/// mode change with 403 `forbidden` and its own message, with or without the override. The
/// displayed millisecond value is a shortening, the same time again is accepted (and counts as a
/// metadata write), and a time a nanosecond later extends it.
#[test]
fn locked_retention_refuses_every_reduction_with_403_and_accepts_equal_or_later() {
    const LOCKED_MESSAGE: &str = "The locked object retention cannot be removed. Its retention mode cannot be changed and its retention period cannot be shortened.";
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["locked"]);
    let stored = set_retention(&server, "locked", "Locked");
    assert_eq!(
        stored["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    let patch = |server: &Server, body: Value, query: &str| {
        server.request(
            "PATCH",
            &format!("{}{query}", object_path("locked")),
            &body.to_string(),
        )
    };
    let locked = |time: &str| json!({"retention":{"mode":"Locked","retainUntilTime":time}});
    for query in ["", "?overrideUnlockedRetention=true"] {
        for body in [
            locked("2099-01-02T03:04:05.123Z"),
            locked("2099-01-02T03:04:05.123456788Z"),
            locked("2099-01-02T03:03:35.123456789Z"),
            json!({"retention":null}),
            json!({"retention":{"mode":"Unlocked","retainUntilTime":FINE}}),
        ] {
            let (status, reply) = patch(&server, body.clone(), query);
            assert_eq!(status, 403, "{body} {query} {reply}");
            assert_eq!(reply["error"]["errors"][0]["reason"], "forbidden");
            assert_eq!(reply["error"]["message"], LOCKED_MESSAGE);
        }
    }
    let (status, same) = patch(&server, locked(FINE), "");
    assert_eq!(status, 200, "{same}");
    let (status, later) = patch(&server, locked("2099-01-02T03:04:05.123456790Z"), "");
    assert_eq!(status, 200, "{later}");
    assert_eq!(
        later["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    // A restart keeps the nanoseconds, so the earlier time is now a shortening.
    server.stop();
    let mut server = Server::start(&dir);
    let (status, reply) = patch(&server, locked(FINE), "");
    assert_eq!(status, 403, "{reply}");
    server.stop();
}

/// Locking an Unlocked retention needs the override, with the Unlocked wording (observed).
#[test]
fn locking_an_unlocked_retention_needs_the_override() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["lock"]);
    set_retention(&server, "lock", "Unlocked");
    let body = json!({"retention":{"mode":"Locked","retainUntilTime":FINE}}).to_string();
    let (status, reply) = server.request("PATCH", &object_path("lock"), &body);
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["error"]["errors"][0]["reason"], "forbidden");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("The unlocked object retention cannot be removed")
    );
    let (status, reply) = server.request(
        "PATCH",
        &format!("{}?overrideUnlockedRetention=true", object_path("lock")),
        &body,
    );
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["retention"]["mode"], "Locked");
    server.stop();
}

/// Cloud Storage kept the nanosecond value of an Unlocked time for comparisons while showing
/// milliseconds (observed 2026-10-31): a time one nanosecond earlier and the displayed
/// millisecond time were refused with 403, the same time and a later one were accepted.
/// gcs-mimic compares at the same precision, also after a restart.
#[test]
fn unlocked_retention_compares_below_the_displayed_millisecond() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["unlocked"]);
    let stored = set_retention(&server, "unlocked", "Unlocked");
    assert_eq!(
        stored["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    let send = |server: &Server, time: &str, query: &str| {
        server.request(
            "PATCH",
            &format!("{}{query}", object_path("unlocked")),
            &json!({"retention":{"mode":"Unlocked","retainUntilTime":time}}).to_string(),
        )
    };
    let refused = |server: &Server, time: &str| {
        let (status, body) = send(server, time, "");
        assert_eq!(status, 403, "{time} {body}");
        assert_eq!(body["error"]["errors"][0]["reason"], "forbidden");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("The unlocked object retention cannot be removed")
        );
    };
    for time in [
        "2099-01-02T03:04:05.123456788Z",
        "2099-01-02T03:04:05.123456Z",
        "2099-01-02T03:04:05.123Z",
    ] {
        refused(&server, time);
    }
    // A restart keeps the nanosecond remainder.
    server.stop();
    let mut server = Server::start(&dir);
    refused(&server, "2099-01-02T03:04:05.123456788Z");
    let (status, body) = send(&server, FINE, "");
    assert_eq!(status, 200, "{body}");
    // One nanosecond later is a real extension; the display does not change.
    let (status, body) = send(&server, "2099-01-02T03:04:05.123456790Z", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    // The new time is now the floor: the original FINE time is a shortening.
    refused(&server, FINE);
    let (status, body) = send(
        &server,
        "2099-01-02T03:04:05.123Z",
        "?overrideUnlockedRetention=true",
    );
    assert_eq!(status, 200, "{body}");
    server.stop();
}

#[test]
fn custom_time_read_back_is_accepted_and_a_decrease_is_not() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["custom"]);
    let (status, body) = server.request(
        "PATCH",
        &object_path("custom"),
        &json!({"customTime":FINE}).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    for method in ["PATCH", "PUT"] {
        let (status, body) = server.request(
            method,
            &object_path("custom"),
            &json!({"customTime":body["customTime"]}).to_string(),
        );
        assert_eq!(status, 200, "{method} {body}");
    }
    let (status, body) = server.request(
        "PATCH",
        &object_path("custom"),
        r#"{"customTime":"2099-01-02T03:04:05.123456788Z"}"#,
    );
    assert_eq!(status, 400, "{body}");
    server.stop();
}

/// Observed on Cloud Storage (2026-10-31): deleting a retained object is 403
/// `retentionPolicyNotMet`; the message names the object and the retain-until time in US
/// Pacific time to the nanosecond, and `overrideUnlockedRetention` is honoured on PATCH only.
#[test]
fn deleting_a_retained_object_names_it_and_ignores_the_override() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["held", "locked"]);
    set_retention(&server, "held", "Unlocked");
    set_retention(&server, "locked", "Locked");
    for name in ["held", "locked"] {
        for query in ["", "?overrideUnlockedRetention=true"] {
            let (status, body) =
                server.request("DELETE", &format!("{}{query}", object_path(name)), "");
            assert_eq!(status, 403, "{name} {query} {body}");
            assert_eq!(
                body["error"]["errors"][0]["reason"],
                "retentionPolicyNotMet"
            );
            assert_eq!(
                body["error"]["message"],
                format!(
                    "Object 'rt-bucket/{name}' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until 2099-01-01T19:04:05.123456789-08:00"
                )
            );
        }
    }
    server.stop();
}

/// `retentionExpireTime` is a gRPC field name, not a JSON one (observed 2026-10-31).
#[test]
fn fields_selection_rejects_the_grpc_retention_expire_time_name() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["fields"]);
    let (status, body) = server.request(
        "GET",
        &format!("{}?fields=name,retentionExpireTime", object_path("fields")),
        "",
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["errors"][0]["reason"], "invalidParameter");
    assert_eq!(
        body["error"]["message"],
        "Invalid field selection retentionExpireTime"
    );
    assert_eq!(body["error"]["errors"][0]["locationType"], "parameter");
    assert_eq!(body["error"]["errors"][0]["location"], "fields");
    let (status, body) = server.request(
        "GET",
        &format!(
            "{}?fields=name,retentionExpirationTime",
            object_path("fields")
        ),
        "",
    );
    assert_eq!(status, 200, "{body}");
    server.stop();
}

/// Observed on Cloud Storage (2026-10-31, us-central1): a bucket retention policy denies a
/// delete and an overwrite with the same 403 `retentionPolicyNotMet` and the same message as
/// object retention, naming the creation time plus the period in US Pacific time to the
/// microsecond (trailing zeros trimmed). `retentionPolicy: null` lifts the policy and the
/// delete then succeeds.
#[test]
fn bucket_retention_policy_denies_delete_and_overwrite_with_the_object_retention_wording() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let (status, body) = server.request(
        "POST",
        "/storage/v1/b?project=local",
        r#"{"name":"policy-bucket","retentionPolicy":{"retentionPeriod":"3600"}}"#,
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = server.request(
        "POST",
        "/upload/storage/v1/b/policy-bucket/o?uploadType=media&name=r",
        "payload",
    );
    assert_eq!(status, 200, "{body}");
    let expires = body["retentionExpirationTime"].as_str().unwrap().to_owned();
    let delete = server.request("DELETE", "/storage/v1/b/policy-bucket/o/r", "");
    let overwrite = server.request(
        "POST",
        "/upload/storage/v1/b/policy-bucket/o?uploadType=media&name=r",
        "again",
    );
    for (status, body) in [&delete, &overwrite] {
        assert_eq!(*status, 403, "{body}");
        assert_eq!(
            body["error"]["errors"][0]["reason"],
            "retentionPolicyNotMet"
        );
    }
    assert_eq!(
        delete.1["error"]["message"],
        overwrite.1["error"]["message"]
    );
    let message = delete.1["error"]["message"].as_str().unwrap();
    let (head, until) = message.split_once(" until ").unwrap();
    assert_eq!(
        head,
        "Object 'policy-bucket/r' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten"
    );
    // Back in UTC, the message time is the creation time plus the period at microsecond
    // precision, so it equals the JSON expiration once cut to the millisecond.
    let (local, offset) = until.split_at(until.len() - 6);
    assert!(offset == "-07:00" || offset == "-08:00", "{until}");
    let (whole, fraction) = local.split_once('.').unwrap_or((local, ""));
    assert!(fraction.len() <= 6 && !fraction.ends_with('0'), "{until}");
    let offset_hours: i64 = offset[1..3].parse().unwrap();
    assert_eq!(
        epoch_seconds(whole) + offset_hours * 3600,
        epoch_seconds(&expires[..19]),
        "{until} {expires}"
    );
    assert_eq!(
        format!("{fraction:0<6}")[..3],
        expires[20..23],
        "{until} {expires}"
    );
    let (status, body) = server.request(
        "PATCH",
        "/storage/v1/b/policy-bucket",
        r#"{"retentionPolicy":null}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.get("retentionPolicy").is_none(), "{body}");
    let (status, body) = server.request("DELETE", "/storage/v1/b/policy-bucket/o/r", "");
    assert_eq!(status, 204, "{body}");
    server.stop();
}

/// Observed on Cloud Storage (2026-10-31): removing or shortening an Unlocked retention
/// without the override is 403 `forbidden` with one shared text; with the override both work.
#[test]
fn unlocked_removal_and_shortening_share_one_refusal_without_the_override() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["unlocked"]);
    set_retention(&server, "unlocked", "Unlocked");
    let shorter = json!({"retention":{"mode":"Unlocked","retainUntilTime":"2099-01-02T03:04:04Z"}});
    for body in [json!({"retention":null}), shorter] {
        let (status, reply) = server.request("PATCH", &object_path("unlocked"), &body.to_string());
        assert_eq!(status, 403, "{reply}");
        assert_eq!(reply["error"]["errors"][0]["reason"], "forbidden");
        assert_eq!(
            reply["error"]["message"],
            "The unlocked object retention cannot be removed and its retention period cannot be shortened without overriding unlocked retention intent and permission."
        );
    }
    let (status, reply) = server.request(
        "PATCH",
        &format!("{}?overrideUnlockedRetention=true", object_path("unlocked")),
        r#"{"retention":null}"#,
    );
    assert_eq!(status, 200, "{reply}");
    assert!(reply.get("retention").is_none(), "{reply}");
    server.stop();
}

/// Seconds since the epoch of `YYYY-MM-DDTHH:MM:SS`, read as UTC (civil-date arithmetic).
fn epoch_seconds(stamp: &str) -> i64 {
    let field = |range: std::ops::Range<usize>| stamp[range].parse::<i64>().unwrap();
    let (year, month, day) = (field(0..4), field(5..7), field(8..10));
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days * 86_400 + field(11..13) * 3600 + field(14..16) * 60 + field(17..19)
}

declare_server!();
