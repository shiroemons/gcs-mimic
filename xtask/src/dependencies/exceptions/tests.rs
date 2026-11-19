use super::*;
use crate::dependencies::{LEDGER, RUST_TOOLCHAIN, WORKSPACE, with_causes};
use serde_json::json;

const JAVA: &str = "sdk-tests/java/pom.xml";
const SCORECARD: &str = "sdk-scorecard/java/pom.xml";
const PIN: &str = "27.0.1.0.2+1";
const NEWER: &str = "27.0.1.1+4";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

/// A complete record that defers the Temurin toolchain of the Java SDK suite until 2026-11-26.
fn record() -> Value {
    json!({
        "input": "Temurin toolchain",
        "manifests": [JAVA],
        "current": PIN,
        "latest": NEWER,
        "reference": "https://github.com/adoptium/temurin27-binaries/releases/tag/jdk-27.0.1.1%2B4",
        "evidence": "docs/en/evidence/dependency-refresh.md#refresh-2026-11-19",
        "reason": "No macOS archive yet.",
        "impact": "Development-only.",
        "owner": "project owner",
        "next_action": "Adopt it once the macOS archive is published.",
        "review_by": "2026-11-26"
    })
}

fn parse(records: &Value) -> Result<Exceptions, ExceptionError> {
    Exceptions::parse(root(), records)
}

fn full_check_on(today: &str, records: &Value) -> Report {
    let today = today.parse().unwrap();
    Report::new(parse(records).unwrap(), Deferrals::Honored { today })
}

fn java() -> InputKey {
    InputKey::new("Temurin toolchain", JAVA)
}

const fn update<'a>(current: &'a str, latest: &'a str) -> Found<'a> {
    Found::Update {
        current,
        latest,
        on: None,
    }
}

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().copied().map(str::to_owned).collect()
}

const DEFERRED: &str = "Temurin toolchain: DEFERRED current=27.0.1.0.2+1 latest=27.0.1.1+4 \
                        review_by=2026-11-26 manifest=sdk-tests/java/pom.xml";
const UPDATE: &str = "Temurin toolchain: UPDATE_AVAILABLE current=27.0.1.0.2+1 latest=27.0.1.1+4";

#[test]
fn an_in_date_exception_defers_its_update_and_the_full_check_passes() {
    let mut both = record();
    both["manifests"] = json!([JAVA, SCORECARD]);
    let mut report = full_check_on("2026-11-19", &json!([both]));
    assert_eq!(
        report.record(java(), update(PIN, NEWER)),
        Outcome {
            lines: lines(&[DEFERRED]),
            fails: false
        }
    );
    let scorecard = report.record(
        InputKey::new("Temurin toolchain", SCORECARD),
        update(PIN, NEWER),
    );
    assert_eq!(
        scorecard.lines,
        lines(&[
            "Temurin toolchain: DEFERRED current=27.0.1.0.2+1 latest=27.0.1.1+4 \
             review_by=2026-11-26 manifest=sdk-scorecard/java/pom.xml"
        ])
    );
    assert!(!scorecard.fails);
    // Another input in the same manifest is reported as before.
    assert_eq!(
        report.record(
            InputKey::new("Maven toolchain", JAVA),
            update("3.10.1", "3.10.2")
        ),
        Outcome {
            lines: lines(&["Maven toolchain: UPDATE_AVAILABLE current=3.10.1 latest=3.10.2"]),
            fails: true
        }
    );
    assert_eq!(
        report.finish(),
        Outcome {
            lines: lines(&[
                "DEFERRED: 2 updates deferred by recorded exceptions, earliest review_by \
                 2026-11-26; dependencies are not all current"
            ]),
            fails: false
        }
    );
}

#[test]
fn the_exception_holds_through_its_review_date_and_fails_the_day_after() {
    for today in ["2026-11-25", "2026-11-26"] {
        let mut report = full_check_on(today, &json!([record()]));
        let outcome = report.record(java(), update(PIN, NEWER));
        assert_eq!(outcome.lines, lines(&[DEFERRED]), "{today}");
        assert!(!outcome.fails, "{today}");
        assert!(!report.finish().fails, "{today}");
    }
    let mut report = full_check_on("2026-11-27", &json!([record()]));
    assert_eq!(
        report.record(java(), update(PIN, NEWER)),
        Outcome {
            lines: lines(&[
                UPDATE,
                "EXCEPTION_EXPIRED: Temurin toolchain in sdk-tests/java/pom.xml: exceptions[0] \
                 had review_by=2026-11-26; adopt 27.0.1.1+4, or re-test it and record a new \
                 review_by",
            ]),
            fails: true
        }
    );
    assert_eq!(report.finish(), Outcome::default());
}

#[test]
fn an_upstream_release_past_the_recorded_latest_fails() {
    let mut report = full_check_on("2026-11-19", &json!([record()]));
    assert_eq!(
        report.record(java(), update(PIN, "27.0.1.2+1")),
        Outcome {
            lines: lines(&[
                "Temurin toolchain: UPDATE_AVAILABLE current=27.0.1.0.2+1 latest=27.0.1.2+1",
                "EXCEPTION_OUTDATED: Temurin toolchain in sdk-tests/java/pom.xml: the latest \
                 release is 27.0.1.2+1, but exceptions[0] records latest=27.0.1.1+4; re-test \
                 27.0.1.2+1, then adopt it or record it in the exception",
            ]),
            fails: true
        }
    );
}

#[test]
fn a_pin_other_than_the_recorded_current_fails() {
    let mut report = full_check_on("2026-11-19", &json!([record()]));
    assert_eq!(
        report.record(java(), update("27.0.1.0.3+1", NEWER)),
        Outcome {
            lines: lines(&[
                "Temurin toolchain: UPDATE_AVAILABLE current=27.0.1.0.3+1 latest=27.0.1.1+4",
                "EXCEPTION_OUTDATED: Temurin toolchain in sdk-tests/java/pom.xml: the pin is \
                 27.0.1.0.3+1, but exceptions[0] records current=27.0.1.0.2+1; record the new pin \
                 in the exception or delete it",
            ]),
            fails: true
        }
    );
}

#[test]
fn every_reason_a_lapsed_exception_no_longer_holds_is_reported() {
    let mut report = full_check_on("2026-12-01", &json!([record()]));
    let outcome = report.record(java(), update("27.0.1.0.3+1", "27.0.1.2+1"));
    assert!(outcome.fails);
    let statuses: Vec<_> = outcome
        .lines
        .iter()
        .map(|line| line.split_once(':').unwrap().0)
        .collect();
    assert_eq!(
        statuses,
        [
            "Temurin toolchain",
            "EXCEPTION_OUTDATED",
            "EXCEPTION_OUTDATED",
            "EXCEPTION_EXPIRED"
        ]
    );
}

#[test]
fn an_exception_for_a_current_input_is_stale() {
    let mut report = full_check_on("2026-11-19", &json!([record()]));
    assert_eq!(
        report.record(java(), Found::Latest(NEWER)),
        Outcome {
            lines: lines(&[
                "Temurin toolchain: CURRENT current=27.0.1.1+4 latest=27.0.1.1+4",
                "EXCEPTION_STALE: Temurin toolchain in sdk-tests/java/pom.xml reports CURRENT; \
                 remove it from exceptions[0]",
            ]),
            fails: true
        }
    );
    // A moving image tag that matches on its declared platforms is current as well.
    let mut image = record();
    image["input"] = json!("docker node:24-trixie-slim");
    image["manifests"] = json!([LEDGER]);
    let mut report = full_check_on("2026-11-19", &json!([image]));
    let outcome = report.record(
        InputKey::new("docker node:24-trixie-slim", LEDGER),
        Found::LatestOn {
            declared: "linux/amd64, linux/arm64",
        },
    );
    assert!(outcome.fails);
    assert_eq!(
        outcome.lines[1],
        "EXCEPTION_STALE: docker node:24-trixie-slim in compatibility/dependency-status.json \
         reports CURRENT; remove it from exceptions[0]"
    );
}

#[test]
fn an_exception_that_matches_no_input_fails() {
    // The key is exact: a pin file in place of the declaring manifest names no input.
    let mut misnamed = record();
    misnamed["manifests"] = json!(["sdk-tests/java/.java-version"]);
    let mut report = full_check_on("2026-11-19", &json!([misnamed]));
    assert_eq!(
        report.record(java(), update(PIN, NEWER)),
        Outcome {
            lines: lines(&[UPDATE]),
            fails: true
        }
    );
    assert_eq!(
        report.finish(),
        Outcome {
            lines: lines(&[
                "EXCEPTION_STALE: Temurin toolchain in sdk-tests/java/.java-version matches no \
                 checked input; correct the input and manifests of exceptions[0], or remove it"
            ]),
            fails: true
        }
    );
}

#[test]
fn two_exceptions_for_one_input_fail() {
    let mut second = record();
    second["manifests"] = json!([SCORECARD, JAVA]);
    assert_eq!(
        parse(&json!([record(), second])).unwrap_err().to_string(),
        "exceptions[0] and exceptions[1] both record Temurin toolchain in sdk-tests/java/pom.xml; \
         keep one exception per input"
    );
    let mut repeated = record();
    repeated["manifests"] = json!([JAVA, SCORECARD, JAVA]);
    assert_eq!(
        parse(&json!([repeated])).unwrap_err().to_string(),
        "exceptions[0] (Temurin toolchain): `manifests` lists sdk-tests/java/pom.xml twice"
    );
    // The same label in another manifest is another input.
    let mut scorecard = record();
    scorecard["manifests"] = json!([SCORECARD]);
    assert!(parse(&json!([record(), scorecard])).is_ok());
}

/// The CHECK_FAILED text for `record` with `change` applied.
fn rejection(change: impl FnOnce(&mut Map<String, Value>)) -> String {
    let mut record = record();
    change(record.as_object_mut().unwrap());
    with_causes(&parse(&json!([record])).unwrap_err())
}

#[test]
fn each_missing_unknown_or_empty_field_is_named() {
    for field in Field::ALL {
        let name = field.as_str();
        let entry = if *field == Field::Input {
            "exceptions[0]"
        } else {
            "exceptions[0] (Temurin toolchain)"
        };
        assert_eq!(
            rejection(|record| {
                record.remove(name);
            }),
            format!("{entry}: missing field `{name}`")
        );
        for empty in [json!(""), json!(" \t"), json!([])] {
            let message = rejection(|record| {
                record.insert(name.to_owned(), empty.clone());
            });
            let expected = match (field, empty.is_array()) {
                (Field::Manifests, false) => {
                    format!("{entry}: `manifests` must be a list of strings")
                }
                (_, false) | (Field::Manifests, true) => format!("{entry}: `{name}` is empty"),
                (_, true) => format!("{entry}: `{name}` must be a string"),
            };
            assert_eq!(message, expected, "{name} = {empty}");
        }
    }
    assert_eq!(
        rejection(|record| {
            record.insert("manifests".to_owned(), json!([JAVA, ""]));
        }),
        "exceptions[0] (Temurin toolchain): `manifests` is empty"
    );
    assert_eq!(
        rejection(|record| {
            record.insert("current".to_owned(), json!(27));
        }),
        "exceptions[0] (Temurin toolchain): `current` must be a string"
    );
    let unknown = rejection(|record| {
        record.insert(
            "package".to_owned(),
            json!("Temurin toolchain (two manifests)"),
        );
    });
    assert!(
        unknown.starts_with("exceptions[0] (Temurin toolchain): unknown field `package`; "),
        "{unknown}"
    );
    assert_eq!(
        with_causes(&parse(&json!([1])).unwrap_err()),
        "exceptions[0] must be an object"
    );
    for missing in [json!(null), json!({})] {
        assert_eq!(
            with_causes(&parse(&missing).unwrap_err()),
            "the snapshot's `exceptions` must be a list"
        );
    }
}

#[test]
fn a_review_date_must_be_a_calendar_date() {
    for date in [
        "2026-11-31",
        "2026/11/26",
        "26-11-26",
        "next week",
        "2026-11-26T00:00:00Z",
    ] {
        assert_eq!(
            rejection(|record| {
                record.insert("review_by".to_owned(), json!(date));
            }),
            format!(
                "exceptions[0] (Temurin toolchain): `review_by` must be a YYYY-MM-DD date: \
                 {date:?} is not a calendar date in YYYY-MM-DD form"
            )
        );
    }
}

#[test]
fn evidence_must_be_a_file_in_the_repository() {
    for (evidence, message) in [
        (
            "docs/en/evidence/no-such-record.md#refresh",
            "`evidence` file docs/en/evidence/no-such-record.md does not exist",
        ),
        (
            "docs/en/evidence",
            "`evidence` file docs/en/evidence does not exist",
        ),
        (
            "/etc/hosts",
            "`evidence` \"/etc/hosts\" is not a repository-relative path",
        ),
        (
            "docs/../../outside.md",
            "`evidence` \"docs/../../outside.md\" is not a repository-relative path",
        ),
        (
            "#refresh-2026-11-19",
            "`evidence` \"\" is not a repository-relative path",
        ),
    ] {
        assert_eq!(
            rejection(|record| {
                record.insert("evidence".to_owned(), json!(evidence));
            }),
            format!("exceptions[0] (Temurin toolchain): {message}"),
            "{evidence}"
        );
    }
    let mut without_fragment = record();
    without_fragment["evidence"] = json!("docs/en/evidence/dependency-refresh.md");
    assert!(parse(&json!([without_fragment])).is_ok());
}

#[test]
fn release_scope_never_defers_a_release_input() {
    let mut crate_record = record();
    crate_record["input"] = json!("rusqlite");
    crate_record["manifests"] = json!([WORKSPACE]);
    crate_record["current"] = json!("0.43.0");
    crate_record["latest"] = json!("0.44.0");
    let records = json!([crate_record, record()]);
    let rusqlite = || InputKey::new("rusqlite", WORKSPACE);
    // The full check would defer it ...
    let mut full = full_check_on("2026-11-19", &records);
    assert!(!full.record(rusqlite(), update("0.43.0", "0.44.0")).fails);
    // ... but the release gate fails it and says why.
    let mut release = Report::new(parse(&records).unwrap(), Deferrals::Refused);
    assert_eq!(
        release.record(rusqlite(), update("0.43.0", "0.44.0")),
        Outcome {
            lines: lines(&[
                "rusqlite: UPDATE_AVAILABLE current=0.43.0 latest=0.44.0",
                "EXCEPTION_REFUSED: rusqlite in Cargo.toml: an exception cannot defer a release \
                 input under --scope release; adopt 0.44.0 before releasing",
            ]),
            fails: true
        }
    );
    // A current release input stays current, and the release gate skips the development-only
    // Temurin input, so its exception neither defers nor goes stale there.
    assert_eq!(
        release.record(rusqlite(), Found::Latest("0.44.0")),
        Outcome {
            lines: lines(&["rusqlite: CURRENT current=0.44.0 latest=0.44.0"]),
            fails: false
        }
    );
    assert_eq!(release.finish(), Outcome::default());
}

#[test]
fn without_exceptions_every_input_is_reported_as_before() {
    for deferrals in [
        Deferrals::Honored {
            today: "2026-11-19".parse().unwrap(),
        },
        Deferrals::Refused,
    ] {
        let mut report = Report::new(Exceptions::default(), deferrals);
        let key = || InputKey::new("docker node:24-trixie-slim", LEDGER);
        for (found, line, fails) in [
            (
                Found::Latest("1.0.0"),
                "docker node:24-trixie-slim: CURRENT current=1.0.0 latest=1.0.0",
                false,
            ),
            (
                update("1.0.0", "2.0.0"),
                "docker node:24-trixie-slim: UPDATE_AVAILABLE current=1.0.0 latest=2.0.0",
                true,
            ),
            (
                Found::LatestOn {
                    declared: "linux/amd64, linux/arm64",
                },
                "docker node:24-trixie-slim: CURRENT on declared platforms linux/amd64, linux/arm64",
                false,
            ),
            (
                Found::Update {
                    current: "sha256:a",
                    latest: "sha256:b",
                    on: Some("linux/amd64"),
                },
                "docker node:24-trixie-slim: UPDATE_AVAILABLE on linux/amd64 current=sha256:a \
                 latest=sha256:b",
                true,
            ),
        ] {
            assert_eq!(
                report.record(key(), found),
                Outcome {
                    lines: lines(&[line]),
                    fails
                },
                "{deferrals:?}"
            );
        }
        assert_eq!(report.finish(), Outcome::default(), "{deferrals:?}");
    }
}

#[test]
fn a_deferred_moving_image_tag_names_its_platforms() {
    let mut image = record();
    image["input"] = json!("docker node:24-trixie-slim");
    image["manifests"] = json!([LEDGER]);
    image["current"] = json!("sha256:a");
    image["latest"] = json!("sha256:b");
    let mut report = full_check_on("2026-11-19", &json!([image]));
    let outcome = report.record(
        InputKey::new("docker node:24-trixie-slim", LEDGER),
        Found::Update {
            current: "sha256:a",
            latest: "sha256:b",
            on: Some("linux/amd64"),
        },
    );
    assert_eq!(
        outcome.lines,
        lines(&[
            "docker node:24-trixie-slim: DEFERRED on linux/amd64 current=sha256:a latest=sha256:b \
             review_by=2026-11-26 manifest=compatibility/dependency-status.json"
        ])
    );
    assert!(!outcome.fails);
}

#[test]
fn committed_exceptions_parse_and_name_manifests_the_check_reports() {
    let status = crate::read_json(&root().join(LEDGER)).unwrap();
    let exceptions = parse(&status["exceptions"]).unwrap();
    let mut manifests: BTreeSet<&str> = status["sdk_ecosystems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|scope| scope["manifest"].as_str().unwrap())
        .collect();
    manifests.extend([LEDGER, RUST_TOOLCHAIN, WORKSPACE]);
    for key in exceptions.0.keys() {
        assert!(manifests.contains(key.manifest.as_str()), "{key}");
    }
}
