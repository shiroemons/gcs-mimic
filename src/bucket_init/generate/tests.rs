use super::*;
use crate::clock::SystemClock;

fn bucket(name: &str, settings: impl IntoIterator<Item = Setting>) -> Bucket {
    let mut bucket = Bucket::new(name).unwrap();
    for setting in settings {
        bucket.apply(setting).unwrap();
    }
    bucket
}

/// Every answer at once, so that one document exercises every member the generator writes.
fn every_setting() -> Vec<Setting> {
    vec![
        Setting::Project("other-project".into()),
        Setting::Location("EU".into()),
        Setting::StorageClass("NEARLINE".into()),
        Setting::Label {
            key: "team".into(),
            value: "storage".into(),
        },
        Setting::Label {
            key: "env".into(),
            value: "dev".into(),
        },
        Setting::Versioning,
        Setting::DefaultEventBasedHold,
        Setting::PublicAccessPrevention(PublicAccessPrevention::Inherited),
        Setting::SoftDeleteRetentionSeconds(604_800),
        Setting::CorsOrigin("http://localhost:3000".into()),
        Setting::CorsMethod("GET".into()),
        Setting::CorsMethod("PUT".into()),
        Setting::CorsResponseHeader("Content-Type".into()),
        Setting::CorsMaxAgeSeconds(3600),
        Setting::PublicRead,
        Setting::IamBinding {
            role: "roles/storage.objectAdmin".into(),
            members: vec!["user:dev@example.com".into()],
        },
    ]
}

fn parsed(text: &str) -> Vec<super::super::InitEntry> {
    parse(
        text.as_bytes(),
        Form::File,
        "local-project",
        &SystemClock,
        "US",
        None,
    )
    .unwrap()
}

#[test]
fn a_bucket_without_answers_is_a_name_only_entry() {
    let rendered = Document::new(vec![bucket("app-uploads", [])])
        .render(Format::Json)
        .unwrap();
    assert_eq!(
        rendered,
        "{\n  \"version\": 1,\n  \"buckets\": [\n    {\n      \"name\": \"app-uploads\"\n    }\n  ]\n}\n"
    );
}

#[test]
fn every_member_passes_the_startup_parser_in_every_format() {
    let document = Document::new(vec![
        bucket("app-uploads", every_setting()),
        bucket("app-exports", [Setting::Versioning]),
    ]);
    for format in Format::ALL.iter().copied() {
        assert!(document.render(format).is_ok(), "{}", format.as_str());
    }
    let entries = parsed(&document.render(Format::Json).unwrap());
    assert_eq!(entries.len(), 2);
    let record = &entries[0].record;
    assert_eq!(record.project, "other-project");
    assert_eq!(record.location.as_str(), "EU");
    assert_eq!(record.storage_class.as_str(), "NEARLINE");
    assert_eq!(record.labels.len(), 2);
    assert!(record.default_event_based_hold);
    assert_eq!(record.cors.len(), 1);
    let bindings = &entries[0].policy.as_ref().unwrap().bindings;
    assert_eq!(bindings.len(), 2);
}

/// Answers to different members give the same entry in any order, so a prompt, which asks in
/// its own order, and flags in the order a user typed them agree. List members keep their
/// order, which both entrances preserve.
#[test]
fn answers_to_different_members_give_the_same_entry_in_any_order() {
    let singles: Vec<_> = every_setting()
        .into_iter()
        .filter(|setting| {
            !matches!(
                setting,
                Setting::CorsOrigin(_)
                    | Setting::CorsMethod(_)
                    | Setting::CorsResponseHeader(_)
                    | Setting::PublicRead
                    | Setting::IamBinding { .. }
            )
        })
        .collect();
    let mut reversed = singles.clone();
    reversed.reverse();
    assert_eq!(
        bucket("app-uploads", singles),
        bucket("app-uploads", reversed)
    );
}

#[test]
fn single_valued_members_refuse_a_second_value_and_lists_accumulate() {
    let mut entry = bucket("app-uploads", [Setting::Location("EU".into())]);
    for (setting, member) in [
        (Setting::Location("US".into()), "location"),
        (
            Setting::Label {
                key: "team".into(),
                value: "a".into(),
            },
            "",
        ),
        (
            Setting::Label {
                key: "team".into(),
                value: "b".into(),
            },
            "labels",
        ),
        (Setting::Versioning, ""),
        (Setting::Versioning, "versioning"),
        (Setting::PublicRead, ""),
        (Setting::PublicRead, "public read"),
        (Setting::CorsMaxAgeSeconds(1), ""),
        (Setting::CorsMaxAgeSeconds(2), "cors.maxAgeSeconds"),
    ] {
        let result = entry.apply(setting);
        if member.is_empty() {
            assert!(result.is_ok());
        } else {
            assert!(
                matches!(result, Err(GenerateError::Repeated(repeated)) if repeated == member),
                "{member}"
            );
        }
    }
    for origin in ["http://a.example", "http://b.example"] {
        entry.apply(Setting::CorsOrigin(origin.into())).unwrap();
    }
    assert_eq!(entry.cors.as_ref().unwrap().origin.len(), 2);
}

#[test]
fn an_invalid_name_is_refused_before_any_answer() {
    for name in ["Uploads", "ab", "-uploads", "goog-bucket"] {
        assert!(
            matches!(Bucket::new(name), Err(GenerateError::InvalidName)),
            "{name}"
        );
    }
}

/// Startup refuses a public grant under the entry's own prevention when it plans the entry, so the
/// generator refuses it before writing, and a prompt can leave public read out beforehand.
#[test]
fn a_public_grant_under_enforced_prevention_is_refused() {
    let public_members = [
        Setting::PublicRead,
        Setting::IamBinding {
            role: "roles/storage.objectViewer".into(),
            members: vec!["allAuthenticatedUsers".into()],
        },
    ];
    for public in public_members {
        let entry = bucket(
            "app-uploads",
            [
                Setting::PublicAccessPrevention(PublicAccessPrevention::Enforced),
                public,
            ],
        );
        assert!(entry.prevents_public_access());
        let document = Document::new(vec![bucket("app-exports", []), entry]);
        assert!(matches!(
            document.render(Format::Json),
            Err(GenerateError::PublicUnderPrevention { index: 1 })
        ));
    }
    let inherited = bucket(
        "app-uploads",
        [
            Setting::PublicAccessPrevention(PublicAccessPrevention::Inherited),
            Setting::PublicRead,
        ],
    );
    assert!(!inherited.prevents_public_access());
    assert!(Document::new(vec![inherited]).render(Format::Json).is_ok());
}

#[test]
fn a_repeated_bucket_is_refused_by_the_startup_parser() {
    let document = Document::new(vec![bucket("app-uploads", []), bucket("app-uploads", [])]);
    assert!(matches!(
        document.render(Format::Env),
        Err(GenerateError::Rejected(_))
    ));
}

#[test]
fn env_is_one_single_quoted_line_of_the_compact_document() {
    let document = Document::new(vec![bucket(
        "app-uploads",
        [Setting::CorsOrigin("http://localhost:${PORT}".into())],
    )]);
    let line = document.render(Format::Env).unwrap();
    assert_eq!(line.lines().count(), 1);
    let value = line
        .strip_prefix("GCS_MIMIC_BUCKETS='")
        .and_then(|rest| rest.strip_suffix("'\n"))
        .unwrap();
    assert_eq!(value, serde_json::to_string(&document).unwrap());
    parse(
        value.as_bytes(),
        Form::Inline,
        "local-project",
        &SystemClock,
        "US",
        None,
    )
    .unwrap();
    let quoted = Document::new(vec![bucket(
        "app-uploads",
        [Setting::CorsOrigin("http://it's.example".into())],
    )]);
    assert!(matches!(
        quoted.render(Format::Env),
        Err(GenerateError::Unquotable)
    ));
}

/// Compose reads `$$` as `$`, so undoing the escape and the indentation gives back the document.
#[test]
fn compose_indents_the_document_and_doubles_each_dollar() {
    let document = Document::new(vec![bucket(
        "app-uploads",
        [Setting::CorsOrigin("http://localhost:${PORT}".into())],
    )]);
    let snippet = document.render(Format::Compose).unwrap();
    let mut lines = snippet.lines();
    assert_eq!(lines.next(), Some("environment:"));
    assert_eq!(lines.next(), Some("  GCS_MIMIC_BUCKETS: |"));
    let body: Vec<_> = lines
        .map(|line| line.strip_prefix("    ").unwrap().replace("$$", "$"))
        .collect();
    assert_eq!(
        body.join("\n"),
        serde_json::to_string_pretty(&document).unwrap()
    );
    assert!(snippet.contains("$${PORT}"));
}

#[test]
fn formats_parse_from_their_names() {
    for format in Format::ALL.iter().copied() {
        assert_eq!(format.as_str().parse::<Format>().unwrap(), format);
    }
    assert!("yaml".parse::<Format>().is_err());
}
