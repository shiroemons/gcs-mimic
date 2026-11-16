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
        Setting::Label("team=storage".parse().unwrap()),
        Setting::Label("env=dev".parse().unwrap()),
        Setting::Versioning,
        Setting::DefaultEventBasedHold,
        Setting::PublicAccessPrevention(PublicAccessPrevention::Inherited),
        Setting::SoftDeleteRetentionSeconds(604_800),
        Setting::CorsOrigin("http://localhost:3000".parse().unwrap()),
        Setting::CorsMethod("GET".parse().unwrap()),
        Setting::CorsMethod("PUT".parse().unwrap()),
        Setting::CorsResponseHeader("Content-Type".parse().unwrap()),
        Setting::CorsMaxAgeSeconds(3600),
        Setting::PublicRead,
        Setting::IamBinding {
            role: "roles/storage.objectAdmin".into(),
            members: "user:dev@example.com".parse().unwrap(),
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
        (Setting::Label("team=a".parse().unwrap()), ""),
        (Setting::Label("team=b".parse().unwrap()), "labels"),
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
        entry
            .apply(Setting::CorsOrigin(origin.parse().unwrap()))
            .unwrap();
    }
    assert_eq!(entry.cors.as_ref().unwrap().origin.len(), 2);
}

/// A label is refused when it is read exactly when the startup parser would refuse it, so a prompt
/// asks again instead of losing every answer at the end.
#[test]
fn a_label_is_refused_when_read_exactly_when_startup_refuses_it() {
    let long = "a".repeat(64);
    let longest = "a".repeat(63);
    for (key, value) in [
        ("team", "storage"),
        ("team", ""),
        ("équipe", "données"),
        ("team-1_a", "v-2_b"),
        (longest.as_str(), longest.as_str()),
        ("Team", "storage"),
        ("team", "Storage"),
        ("1team", "storage"),
        ("", "storage"),
        ("team", "a b"),
        ("team", "a.b"),
        (long.as_str(), "v"),
        ("team", long.as_str()),
    ] {
        let label = format!("{key}={value}").parse::<Label>();
        let startup = startup_accepts(serde_json::json!({"labels": {key: value}}));
        assert_eq!(label.is_ok(), startup, "{key}={value}");
        if let Err(error) = label {
            assert!(
                matches!(error, GenerateError::InvalidLabel),
                "{key}={value}"
            );
        }
    }
    assert!(matches!(
        "team".parse::<Label>(),
        Err(GenerateError::UnpairedLabel)
    ));
}

/// Whether the startup parser accepts one entry with `members` besides its name.
fn startup_accepts(members: serde_json::Value) -> bool {
    let mut entry = serde_json::json!({"name": "app-uploads"});
    if let (Some(entry), Some(members)) = (entry.as_object_mut(), members.as_object()) {
        entry.extend(members.clone());
    }
    let document = serde_json::json!({"version": 1, "buckets": [entry]});
    parse(
        document.to_string().as_bytes(),
        Form::File,
        "local-project",
        &SystemClock,
        "US",
        None,
    )
    .is_ok()
}

/// CORS values and IAM members are refused when they are read exactly when startup refuses them,
/// as labels are, so a question asks again instead of losing every answer at the end.
#[test]
fn cors_values_and_members_are_refused_when_read_exactly_when_startup_refuses_them() {
    for origin in [
        "http://localhost:3000",
        "*",
        "https://a.example:8443",
        "http://a b",
        "",
        "http://ä.example",
        "http://a\tb",
    ] {
        let read = origin.parse::<CorsOrigin>();
        let startup =
            startup_accepts(serde_json::json!({"cors": [{"origin": [origin], "method": ["GET"]}]}));
        assert_eq!(read.is_ok(), startup, "{origin:?}");
        assert!(read.is_ok() || matches!(read, Err(GenerateError::InvalidCorsOrigin)));
    }
    for method in ["GET", "*", "PATCH", "GE T", ""] {
        let read = method.parse::<CorsMethod>();
        let startup = startup_accepts(
            serde_json::json!({"cors": [{"origin": ["http://localhost:3000"], "method": [method]}]}),
        );
        assert_eq!(read.is_ok(), startup, "{method:?}");
        assert!(read.is_ok() || matches!(read, Err(GenerateError::InvalidCorsMethod)));
    }
    for header in [
        "Content-Type",
        "*",
        "x-goog-meta-a",
        "Content Type",
        "",
        "a:b",
    ] {
        let read = header.parse::<CorsResponseHeader>();
        let startup = startup_accepts(serde_json::json!({"cors": [{
            "origin": ["http://localhost:3000"], "method": ["GET"], "responseHeader": [header],
        }]}));
        assert_eq!(read.is_ok(), startup, "{header:?}");
        assert!(read.is_ok() || matches!(read, Err(GenerateError::InvalidCorsResponseHeader)));
    }
    for member in [
        "allUsers",
        "allAuthenticatedUsers",
        "user:dev@example.com",
        "serviceAccount:ci@local-project.iam.gserviceaccount.com",
        "projectViewer:local-project",
        "dev@example.com",
        "user:",
        "",
        "group:dev@example.com",
        "domain:example.com",
    ] {
        let read = member.parse::<Members>();
        let startup = startup_accepts(serde_json::json!({"iamPolicy": {"bindings": [
            {"role": "roles/storage.objectViewer", "members": [member]},
        ]}}));
        assert_eq!(read.is_ok(), startup, "{member:?}");
    }
    assert!(matches!(
        "group:dev@example.com".parse::<Members>(),
        Err(GenerateError::UnsupportedMember)
    ));
    assert!(matches!(
        "dev@example.com".parse::<Members>(),
        Err(GenerateError::InvalidMember)
    ));
    assert_eq!(
        "allUsers, user:dev@example.com".parse::<Members>().unwrap(),
        "allUsers,user:dev@example.com".parse().unwrap()
    );
}

/// A bucket takes 64 labels; a new key past them is refused while a repeated key stays a repeat.
#[test]
fn a_label_past_the_limit_is_refused_when_given() {
    let mut entry = bucket("app-uploads", []);
    for index in 0..64 {
        entry
            .apply(Setting::Label(format!("k{index}=v").parse().unwrap()))
            .unwrap();
    }
    assert!(
        Document::new(vec![entry.clone()])
            .render(Format::Json)
            .is_ok()
    );
    assert!(matches!(
        entry.apply(Setting::Label("k64=v".parse().unwrap())),
        Err(GenerateError::TooManyLabels)
    ));
    assert!(matches!(
        entry.apply(Setting::Label("k0=w".parse().unwrap())),
        Err(GenerateError::Repeated("labels"))
    ));
    assert_eq!(entry.labels.len(), 64);
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
            members: "allAuthenticatedUsers".parse().unwrap(),
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
        [Setting::CorsOrigin(
            "http://localhost:${PORT}".parse().unwrap(),
        )],
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
        [Setting::CorsOrigin("http://it's.example".parse().unwrap())],
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
        [Setting::CorsOrigin(
            "http://localhost:${PORT}".parse().unwrap(),
        )],
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

/// Every value a question offers is one startup accepts, so no listed choice can fail later.
#[test]
fn every_offered_choice_passes_the_startup_parser() {
    let mut settings: Vec<Setting> = storage_classes()
        .iter()
        .map(|class| Setting::StorageClass((*class).into()))
        .collect();
    settings.extend(
        multi_regions()
            .iter()
            .map(|location| Setting::Location((*location).into())),
    );
    settings.extend(bucket_roles().into_iter().map(|role| Setting::IamBinding {
        role: role.into(),
        members: "user:dev@example.com".parse().unwrap(),
    }));
    let buckets: Vec<_> = settings
        .into_iter()
        .enumerate()
        .map(|(index, setting)| bucket(&format!("choice-{index}"), [setting]))
        .collect();
    assert_eq!(
        parsed(&Document::new(buckets).render(Format::Json).unwrap()).len(),
        4 + 3 + 18
    );
    assert_eq!(multi_regions(), ["US", "EU", "ASIA"]);
    assert!(!bucket_roles().contains(&"roles/storage.hmacKeyAdmin"));
}

#[test]
fn another_location_is_matched_without_regard_to_case() {
    assert_eq!(location("europe-west1"), Some("EUROPE-WEST1"));
    assert_eq!(location("nam4"), Some("NAM4"));
    assert_eq!(location("moon-central1"), None);
}
