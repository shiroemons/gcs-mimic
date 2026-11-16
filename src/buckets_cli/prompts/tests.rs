use super::cors::{Access, LOCALHOST, MAX_AGES, METHODS, Needs, ORIGINS, RESPONSE_HEADERS};
use super::lists::Port;
use super::script::{Reply, Script};
use super::*;
use gcs_mimic::bucket_init::generate::{CorsMethod, CorsResponseHeader, GenerateError};
use std::str::FromStr;

/// Several names typed at once share the answers that follow; empty names around commas are
/// skipped, and each name is refused for what startup or the document would refuse.
#[test]
fn several_names_share_one_set_of_answers() {
    let names = Names::parse(" app-uploads, app-exports ,, app-assets,", &[]).unwrap();
    assert_eq!(names.first(), "app-uploads");
    assert_eq!(names.rest(), ["app-exports", "app-assets"]);
    assert_eq!(names.to_string(), "app-uploads, app-exports and app-assets");
    assert_eq!(
        Names::parse("app-uploads,app-exports", &[])
            .unwrap()
            .to_string(),
        "app-uploads and app-exports"
    );
    assert_eq!(
        Names::parse("app-uploads", &[]).unwrap().to_string(),
        "app-uploads"
    );
    let taken = ["app-exports".to_owned()];
    for (answer, says) in [
        ("", "Enter a bucket name, or several separated by commas."),
        (
            " , ",
            "Enter a bucket name, or several separated by commas.",
        ),
        (
            "app-uploads, Bad",
            "Bad: Bucket names use lower-case letters",
        ),
        ("app-uploads,app-uploads", "app-uploads is named twice."),
        (
            "app-uploads, app-exports",
            "This document already has a bucket named app-exports.",
        ),
    ] {
        let error = Names::parse(answer, &taken).unwrap_err();
        assert!(error.to_string().starts_with(says), "{answer:?}: {error}");
    }
}

/// A file that cannot be created offers another file or the terminal, so no answer is lost.
#[test]
fn a_file_that_cannot_be_created_offers_another_or_the_terminal() {
    assert_eq!(Fallback::ALL, [Fallback::Another, Fallback::Here]);
    assert_eq!(
        Fallback::Here.to_string(),
        "Print it here instead  (and copy it into a file yourself)"
    );
}

fn offered<T: FromStr<Err = GenerateError>>(offers: &[Offer]) -> Vec<T> {
    offers
        .iter()
        .map(|offer| offer.value.parse().unwrap())
        .collect()
}

fn seconds(picks: &[SecondsPick]) -> Vec<u64> {
    picks
        .iter()
        .filter_map(|pick| match pick {
            SecondsPick::Offered { seconds, .. } => Some(*seconds),
            SecondsPick::Unset(_) | SecondsPick::Other => None,
        })
        .collect()
}

/// Every value a list offers is one startup accepts, so picking from a list can never fail after
/// the last question and lose the answers.
#[test]
fn every_offered_value_passes_the_startup_parser() {
    let mut rule = Bucket::new("every-offer").unwrap();
    for method in METHODS.map(|method| method.parse::<CorsMethod>().unwrap()) {
        rule.apply(Setting::CorsMethod(method)).unwrap();
    }
    for origin in offered::<CorsOrigin>(&ORIGINS) {
        rule.apply(Setting::CorsOrigin(origin)).unwrap();
    }
    // Every port the port entry accepts gives an origin startup accepts.
    for port in ["1", "3001", "65535"] {
        let origin = port.parse::<Port>().unwrap().on(LOCALHOST);
        rule.apply(Setting::CorsOrigin(origin.parse().unwrap()))
            .unwrap();
    }
    for header in offered::<CorsResponseHeader>(&RESPONSE_HEADERS) {
        rule.apply(Setting::CorsResponseHeader(header)).unwrap();
    }
    for role in ROLES.iter().map(|offer| offer.value).chain(other_roles()) {
        rule.apply(Setting::IamBinding {
            role: role.into(),
            members: offered::<Members>(&MEMBERS).into_iter().collect(),
        })
        .unwrap();
    }
    let mut buckets = vec![rule];
    for (index, seconds) in seconds(&MAX_AGES).into_iter().enumerate() {
        let mut bucket = Bucket::new(&format!("max-age-{index}")).unwrap();
        for setting in [
            Setting::CorsOrigin("http://localhost:3000".parse().unwrap()),
            Setting::CorsMethod("GET".parse().unwrap()),
            Setting::CorsMaxAgeSeconds(seconds),
        ] {
            bucket.apply(setting).unwrap();
        }
        buckets.push(bucket);
    }
    for (index, seconds) in seconds(&SOFT_DELETE_RETENTIONS).into_iter().enumerate() {
        let mut bucket = Bucket::new(&format!("soft-delete-{index}")).unwrap();
        bucket
            .apply(Setting::SoftDeleteRetentionSeconds(seconds))
            .unwrap();
        buckets.push(bucket);
    }
    assert_eq!(buckets.len(), 1 + 2 + 4);
    for format in Format::ALL.iter().copied() {
        Document::new(buckets.clone()).render(format).unwrap();
    }
}

/// The common roles come first and the other entry lists exactly the rest, so every role a
/// bucket policy can grant is offered once.
#[test]
fn the_role_question_offers_every_bucket_role_once() {
    let mut offered: Vec<_> = ROLES.iter().map(|offer| offer.value).collect();
    assert!(
        offered
            .iter()
            .all(|role| generate::bucket_roles().contains(role))
    );
    offered.extend(other_roles());
    offered.sort_unstable();
    let mut all = generate::bucket_roles();
    all.sort_unstable();
    assert_eq!(offered, all);
}

/// Each goal gives a rule browsers can use from the origins alone: it matches the requests the
/// goal makes, and an upload's preflight passes.
#[test]
fn browser_goals_give_rules_browsers_can_use() {
    for (access, methods) in [
        (
            Access {
                uploads: true,
                ..Access::default()
            },
            vec!["PUT", "POST"],
        ),
        (
            Access {
                downloads: true,
                ..Access::default()
            },
            vec!["GET", "HEAD"],
        ),
        (
            Access {
                uploads: true,
                downloads: true,
                custom: false,
            },
            vec!["GET", "HEAD", "PUT", "POST"],
        ),
    ] {
        let needs = Needs::from(access);
        assert_eq!(needs.methods, methods, "{access:?}");
        let mut bucket = Bucket::new("app-uploads").unwrap();
        let settings = std::iter::once(Setting::CorsOrigin(
            "http://localhost:3000".parse().unwrap(),
        ))
        .chain(
            needs
                .methods
                .iter()
                .map(|method| Setting::CorsMethod(method.parse().unwrap())),
        )
        .chain(
            needs
                .headers
                .iter()
                .map(|header| Setting::CorsResponseHeader(header.parse().unwrap())),
        );
        for setting in settings {
            bucket.apply(setting).unwrap();
        }
        let document = Document::new(vec![bucket]);
        document.render(Format::Json).unwrap();
        assert_eq!(document.uploads_refusing_content_type().count(), 0);
    }
    assert!(!Access::default().wanted());
}

#[test]
fn the_launch_question_starts_with_compose_and_names_every_format_once() {
    assert_eq!(Launch::ALL[0], Launch(Format::Compose));
    for format in Format::ALL {
        assert_eq!(
            Launch::ALL
                .iter()
                .filter(|launch| launch.0 == *format)
                .count(),
            1
        );
    }
}

/// Writing, changing and removing need a bucket; adding and cancelling are always there.
#[test]
fn the_review_offers_what_the_buckets_allow() {
    assert_eq!(Next::offered(0), [Next::Add, Next::Cancel]);
    assert_eq!(
        Next::offered(2),
        [
            Next::Write,
            Next::Add,
            Next::Change,
            Next::Remove,
            Next::Cancel
        ]
    );
}

/// Esc goes back, but Ctrl-C and other failures still end the questions.
#[test]
fn esc_goes_back_and_other_errors_still_end_the_questions() {
    assert!(matches!(back(Ok(3)), Ok(Some(3))));
    assert!(matches!(
        back::<u8>(Err(InquireError::OperationCanceled)),
        Ok(None)
    ));
    assert!(matches!(
        back::<u8>(Err(InquireError::OperationInterrupted)),
        Err(InquireError::OperationInterrupted)
    ));
}

fn asked(name: &str, origins: &[&str]) -> Asked {
    Asked {
        bucket: Bucket::new(name).unwrap(),
        goals: Vec::new(),
        details: Vec::new(),
        origins: origins
            .iter()
            .map(|origin| origin.parse().unwrap())
            .collect(),
    }
}

/// A bucket asked again may keep its own name but not take another bucket's.
#[test]
fn a_bucket_asked_again_may_keep_its_own_name() {
    let entries = [
        asked("app-uploads", &[]),
        asked("app-assets", &[]),
        asked("app-exports", &[]),
    ];
    assert_eq!(names(&entries, Some(1)), ["app-uploads", "app-exports"]);
    assert_eq!(names(&entries, None).len(), 3);
}

/// A new bucket is offered the origins of the latest bucket that has any.
#[test]
fn a_new_bucket_is_offered_the_latest_origins() {
    let entries = [
        asked("app-uploads", &["http://localhost:3000"]),
        asked(
            "app-assets",
            &["http://localhost:5173", "capacitor://localhost"],
        ),
        asked("app-exports", &[]),
    ];
    let latest: Vec<_> = latest_origins(&entries)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(latest, ["http://localhost:5173", "capacitor://localhost"]);
    assert!(latest_origins(&entries[2..]).is_empty());
}

/// Asked again, a list starts with the earlier answers marked, in the list's own order.
#[test]
fn earlier_answers_are_marked_when_a_bucket_is_asked_again() {
    assert_eq!(
        positions(&Goal::ALL, &[Goal::More, Goal::BrowserUploads]),
        [1, 6]
    );
    assert_eq!(
        positions(&Detail::ALL, &[Detail::Project, Detail::Cors]),
        [0, 7]
    );
    assert!(positions(&Goal::ALL, &[]).is_empty());
}

/// A saved document never replaces a file, as --output never does, and needs a directory.
#[test]
fn a_new_file_must_not_exist_and_its_directory_must() {
    for refused in [
        "",
        "Cargo.toml",
        "no-such-directory-for-buckets/buckets.json",
    ] {
        assert!(
            matches!(new_file(Path::new(refused)), Validation::Invalid(_)),
            "{refused:?}"
        );
    }
    for accepted in [
        "buckets-new-unused-name.json",
        "src/buckets-new-unused-name.json",
    ] {
        assert!(
            matches!(new_file(Path::new(accepted)), Validation::Valid),
            "{accepted:?}"
        );
    }
}

/// Each list ends in the entry that asks by hand, and entries carry the note that helps choose.
#[test]
fn entries_carry_notes_and_lists_end_in_the_entry_that_asks_by_hand() {
    assert_eq!(MAX_AGES.last(), Some(&SecondsPick::Other));
    assert_eq!(SOFT_DELETE_RETENTIONS.last(), Some(&SecondsPick::Other));
    // Default settings comes first, where the cursor starts, so choosing it is Space and Enter.
    assert_eq!(Goal::ALL.first(), Some(&Goal::Defaults));
    assert_eq!(Goal::ALL.last(), Some(&Goal::More));
    assert_eq!(
        Pick::Offered(ORIGINS[0]).to_string(),
        "http://localhost:3000  (Next.js, Rails, Nuxt)"
    );
    assert_eq!(
        Pick::Port(LOCALHOST).to_string(),
        "http://localhost:PORT  (type the port, such as 3001)"
    );
    assert_eq!(
        SOFT_DELETE_RETENTIONS[0].to_string(),
        "7 days, the Cloud Storage default (604800 seconds)"
    );
    // The notes say when a page needs each goal: <img> and links need no CORS, and the
    // default permissive mode already lets anyone read, so a bucket is private only in strict
    // mode.
    assert_eq!(
        Goal::Defaults.to_string(),
        "Default settings  (private in strict mode; anyone reads in the default permissive mode)"
    );
    assert_eq!(
        Goal::BrowserDownloads.to_string(),
        "Browser downloads  (your page reads objects with fetch(); <img> and links need none)"
    );
    assert_eq!(
        Goal::PublicRead.to_string(),
        "Public read  (anyone reads objects, in strict mode too: <img> and links)"
    );
    assert_eq!(
        Prevention(PublicAccessPrevention::Enforced).to_string(),
        "enforced  (refuses grants to allUsers and allAuthenticatedUsers)"
    );
    assert_eq!(
        Prevention(PublicAccessPrevention::Inherited).to_string(),
        "inherited  (follows the project's setting)"
    );
    assert_eq!(
        Launch(Format::Compose).to_string(),
        "Docker Compose  (an environment: block to paste into compose.yaml)"
    );
    assert_eq!(
        RolePick::Other.to_string(),
        "Another role…  (the other roles a bucket policy can grant)"
    );
}

/// The answer line keeps the values and names, and drops the notes that only helped choose.
#[test]
fn the_answer_line_lists_the_values_alone() {
    let origin = |value: &str| {
        let offer = ORIGINS.iter().find(|offer| offer.value == value).unwrap();
        Pick::Offered(*offer)
    };
    let picks = [
        origin("http://localhost:5173"),
        origin("*"),
        Pick::Port(LOCALHOST),
        Pick::Other("Another origin"),
    ];
    assert_eq!(
        picks.map(Pick::answer),
        ["http://localhost:5173", "*", "typed below", "typed below"]
    );
    let (_, script) = run(None, FileQuestion::Skip, [Reply::Esc]);
    assert_eq!(
        script.seen[0],
        "? How do you run gcs-mimic? [Docker Compose, docker run or a shell, gcs-mimic serve --buckets-file]"
    );
}

/// Runs the questions on a script and returns their outcome with what the script saw.
fn run(
    format: Option<Format>,
    file: FileQuestion,
    replies: impl IntoIterator<Item = Reply>,
) -> (Result<Answers, InquireError>, Script) {
    run_in(format, None, file, replies)
}

/// [`run`], with `--auth-mode` given or not.
fn run_in(
    format: Option<Format>,
    auth_mode: Option<AuthMode>,
    file: FileQuestion,
    replies: impl IntoIterator<Item = Reply>,
) -> (Result<Answers, InquireError>, Script) {
    let mut script = Script::new(replies);
    let answers = ask_on(&mut script, format, auth_mode, file);
    (answers, script)
}

/// The written document as JSON, for checks that should not depend on its layout.
fn written(answers: &Answers) -> serde_json::Value {
    serde_json::to_value(&answers.document).unwrap()
}

/// Two names with default settings take four answers and come out as one Compose line.
#[test]
fn named_buckets_without_goals_come_out_as_one_compose_line() {
    let (answers, script) = run(
        None,
        FileQuestion::Ask,
        [
            Reply::Pick("Docker Compose"),
            Reply::Type("app-uploads, app-exports"),
            Reply::Picks(&["Default settings"]),
            Reply::Pick("Write it"),
            Reply::Pick("Anyone"),
        ],
    );
    let answers = answers.unwrap();
    assert!(script.is_done());
    assert_eq!(
        answers.text,
        "environment:\n  GCS_MIMIC_BUCKETS: \"app-uploads,app-exports\"\n"
    );
    assert_eq!(answers.saved, None);
    assert!(
        script
            .seen
            .contains(&"? What should app-uploads and app-exports do? [Default settings, Browser uploads, Browser downloads, Public read, Keep old versions, Keep deleted objects, More settings…]".to_owned()),
        "{:#?}",
        script.seen
    );
}

/// Browser uploads ask only for the origins and give a rule an upload's preflight passes; a
/// later bucket is offered the same origins, and declining them asks again.
#[test]
fn browser_uploads_ask_only_for_the_origins() {
    let (answers, script) = run(
        Some(Format::Json),
        FileQuestion::Skip,
        [
            Reply::Type("app-uploads"),
            Reply::Picks(&["Browser uploads"]),
            Reply::Picks(&["http://localhost:3000", "http://localhost:PORT"]),
            Reply::Type("3001"),
            Reply::Type(""),
            Reply::Pick("Add a bucket"),
            Reply::Type("app-assets"),
            Reply::Picks(&["Browser downloads"]),
            Reply::Yes,
            Reply::Pick("Add a bucket"),
            Reply::Type("app-media"),
            Reply::Picks(&["Browser downloads"]),
            Reply::No,
            Reply::Picks(&["*"]),
            Reply::Pick("Write it"),
            Reply::Pick("Anyone"),
        ],
    );
    let document = written(&answers.unwrap());
    assert!(script.is_done());
    let origins = ["http://localhost:3000", "http://localhost:3001"];
    assert_eq!(
        document["buckets"][0]["cors"][0],
        serde_json::json!({
            "origin": origins,
            "method": ["PUT", "POST"],
            "responseHeader": ["Content-Type", "x-goog-resumable"]
        })
    );
    assert_eq!(
        document["buckets"][2]["cors"][0],
        serde_json::json!({"origin": ["*"], "method": ["GET", "HEAD"]})
    );
    assert_eq!(
        document["buckets"][1]["cors"][0],
        serde_json::json!({"origin": origins, "method": ["GET", "HEAD"]})
    );
}

/// A bucket asked again starts from its answers, so Enter keeps them; Esc while asking it
/// again goes back to its name and then to the buckets, leaving it as it was.
#[test]
fn a_bucket_asked_again_starts_from_its_answers_and_esc_keeps_it() {
    let (answers, script) = run(
        Some(Format::Json),
        FileQuestion::Skip,
        [
            Reply::Type("app-assets"),
            Reply::Picks(&["Browser downloads", "Public read"]),
            Reply::Picks(&["http://localhost:5173"]),
            Reply::Pick("Change a bucket"),
            Reply::Keep,
            Reply::Esc,
            Reply::Esc,
            Reply::Pick("Change a bucket"),
            Reply::Keep,
            Reply::Keep,
            Reply::Keep,
            Reply::Pick("Write it"),
        ],
    );
    let document = written(&answers.unwrap());
    assert!(script.is_done());
    let bucket = &document["buckets"][0];
    assert_eq!(bucket["name"], "app-assets");
    assert_eq!(
        bucket["cors"][0]["origin"],
        serde_json::json!(["http://localhost:5173"])
    );
    assert_eq!(
        bucket["iamPolicy"]["bindings"][0]["members"],
        serde_json::json!(["allUsers"])
    );
    assert_eq!(document["buckets"].as_array().unwrap().len(), 1);
}

/// Esc goes back one step and ends the questions only on the first one: a mistyped name is
/// fixed by going back from the goals, which keeps what was typed; the names go back to how
/// gcs-mimic runs; and cancelling later asks first, with Esc there returning to the buckets.
#[test]
fn esc_goes_back_one_step_and_ends_only_on_the_first_question() {
    let (answers, script) = run(Some(Format::Env), FileQuestion::Skip, [Reply::Esc]);
    assert!(matches!(answers, Err(InquireError::OperationCanceled)));
    assert!(script.is_done());

    let (answers, script) = run(
        None,
        FileQuestion::Skip,
        [
            Reply::Pick("Docker Compose"),
            Reply::Type("app-uplaods"),
            Reply::Esc,
            Reply::Type("app-uploads"),
            Reply::Esc,
            Reply::Esc,
            Reply::Pick("docker run or a shell"),
            Reply::Type("app-uploads"),
            Reply::Picks(&["Default settings"]),
            Reply::Pick("Write it"),
            Reply::Pick("Anyone"),
        ],
    );
    assert_eq!(answers.unwrap().text, "GCS_MIMIC_BUCKETS=app-uploads\n");
    assert!(script.is_done());
    let launches = script
        .seen
        .iter()
        .filter(|seen| seen.starts_with("? How do you run gcs-mimic?"))
        .count();
    assert_eq!(launches, 2);

    // Enter on the names asked again keeps what was typed: an empty answer would be refused.
    let (answers, script) = run(
        Some(Format::Env),
        FileQuestion::Skip,
        [
            Reply::Type("app-uplaods"),
            Reply::Esc,
            Reply::Keep,
            Reply::Esc,
            Reply::Esc,
        ],
    );
    assert!(matches!(answers, Err(InquireError::OperationCanceled)));
    assert!(script.is_done());
    assert!(
        !script.seen.iter().any(|seen| seen.starts_with("# Enter")),
        "{:#?}",
        script.seen
    );

    let (answers, script) = run(
        Some(Format::Env),
        FileQuestion::Skip,
        [
            Reply::Type("app-uploads"),
            Reply::Picks(&["Default settings"]),
            Reply::Esc,
            Reply::Esc,
            Reply::Pick("Cancel and write nothing"),
            Reply::Yes,
        ],
    );
    assert!(matches!(answers, Err(InquireError::OperationCanceled)));
    assert!(script.is_done());
    let confirmations = script
        .seen
        .iter()
        .filter(|seen| *seen == "? Discard these buckets and write nothing?")
        .count();
    assert_eq!(confirmations, 2);
    let (answers, _) = run(
        Some(Format::Env),
        FileQuestion::Skip,
        [Reply::Type("app-uploads"), Reply::CtrlC],
    );
    assert!(matches!(answers, Err(InquireError::OperationInterrupted)));
}

/// An answer startup would refuse is explained and asked again; a list that needs a choice
/// refuses none.
#[test]
fn refused_answers_are_asked_again() {
    let (answers, script) = run(
        Some(Format::Env),
        FileQuestion::Skip,
        [
            Reply::Type("Bad"),
            Reply::Type("app-uploads"),
            Reply::Picks(&["Browser downloads"]),
            Reply::Picks(&[]),
            Reply::Picks(&["*"]),
            Reply::Pick("Write it"),
            Reply::Pick("Anyone"),
        ],
    );
    assert!(answers.is_ok());
    assert!(script.is_done());
    assert!(
        script
            .seen
            .iter()
            .any(|seen| seen.starts_with("# Bad: Bucket names use")),
        "{:#?}",
        script.seen
    );
    assert!(
        script
            .seen
            .contains(&"# Choose at least one; Space selects.".to_owned())
    );
}

/// Enter on a goal that Space never marked is refused at once instead of leaving the bucket
/// with default settings, which are a choice of their own and go alone: with another goal they
/// would put a contradiction on the answer line.
#[test]
fn enter_without_a_choice_is_refused_until_one_is_made() {
    let (answers, script) = run(
        Some(Format::Env),
        FileQuestion::Skip,
        [
            Reply::Type("app-uploads"),
            Reply::Picks(&[]),
            Reply::Picks(&["Default settings", "Browser uploads"]),
            Reply::Picks(&["Browser uploads"]),
            Reply::Picks(&["http://localhost:3000"]),
            Reply::Pick("Write it"),
            Reply::Pick("Anyone"),
        ],
    );
    assert!(
        answers
            .unwrap()
            .text
            .contains("\"method\":[\"PUT\",\"POST\"]")
    );
    assert!(script.is_done());
    let goals = script
        .seen
        .iter()
        .position(|seen| seen.starts_with("? What should app-uploads do?"))
        .unwrap();
    assert_eq!(
        script.seen.get(goals + 1..goals + 3),
        Some(
            &[
                "# Choose at least one; Space selects.".to_owned(),
                "# Default settings goes alone; Space unmarks.".to_owned(),
            ][..]
        )
    );
}

/// Who reads is asked once, before writing, about the buckets that public read leaves closed,
/// with Anyone first so that Enter keeps the default; Esc there goes back to the buckets.
/// Choosing strict mode writes its settings beside the buckets.
#[test]
fn public_and_private_buckets_ask_who_reads_the_private_ones() {
    let (answers, script) = run(
        None,
        FileQuestion::Skip,
        [
            Reply::Pick("Docker Compose"),
            Reply::Type("app-public"),
            Reply::Picks(&["Public read"]),
            Reply::Pick("Add a bucket"),
            Reply::Type("app-private, app-uploads"),
            Reply::Picks(&["Default settings"]),
            Reply::Pick("Write it"),
            Reply::Esc,
            Reply::Pick("Write it"),
            Reply::Pick("Only your servers and the URLs they sign"),
        ],
    );
    let answers = answers.unwrap();
    assert!(script.is_done());
    assert_eq!(answers.document.auth_mode(), AuthMode::Strict);
    assert!(
        answers
            .text
            .starts_with("environment:\n  GCS_MIMIC_AUTH_MODE: strict\n"),
        "{}",
        answers.text
    );
    let question = "? Who can read app-private and app-uploads? [Anyone, Only your servers and the URLs they sign]";
    let asked = script.seen.iter().filter(|seen| *seen == question).count();
    assert_eq!(asked, 2, "{:#?}", script.seen);
    let reviews = script
        .seen
        .iter()
        .filter(|seen| seen.starts_with("? Next:"))
        .count();
    assert_eq!(reviews, 3);
    assert_eq!(Readers::ALL.first(), Some(&Readers(AuthMode::Permissive)));
    assert_eq!(
        Readers(AuthMode::Strict).to_string(),
        "Only your servers and the URLs they sign  (strict mode; your servers then use port 4444)"
    );
}

/// Public read opens a bucket in either mode, so buckets that all have it raise no question,
/// and neither does a mode that --auth-mode chose.
#[test]
fn who_reads_is_not_asked_when_nothing_depends_on_it() {
    let (answers, script) = run(
        Some(Format::Env),
        FileQuestion::Skip,
        [
            Reply::Type("app-public"),
            Reply::Picks(&["Public read"]),
            Reply::Pick("Write it"),
        ],
    );
    assert_eq!(answers.unwrap().document.auth_mode(), AuthMode::Permissive);
    assert!(script.is_done());
    let (answers, script) = run_in(
        Some(Format::Env),
        Some(AuthMode::Strict),
        FileQuestion::Skip,
        [
            Reply::Type("app-private"),
            Reply::Picks(&["Default settings"]),
            Reply::Pick("Write it"),
        ],
    );
    assert!(
        answers
            .unwrap()
            .text
            .starts_with("GCS_MIMIC_AUTH_MODE=strict\n")
    );
    assert!(script.is_done());
}

/// With public read chosen, public access prevention offers only inherited.
#[test]
fn public_read_is_offered_only_inherited_prevention() {
    let (answers, script) = run(
        Some(Format::Json),
        FileQuestion::Skip,
        [
            Reply::Type("app-assets"),
            Reply::Picks(&["Public read", "More settings…"]),
            Reply::Picks(&["Public access prevention"]),
            Reply::Pick("inherited"),
            Reply::Pick("Write it"),
        ],
    );
    assert!(answers.is_ok());
    assert!(
        script
            .seen
            .contains(&"? Public access prevention: [inherited]".to_owned()),
        "{:#?}",
        script.seen
    );
}

/// A file that cannot be created offers another file or printing, and no answer is lost.
#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_created_offers_another_file_or_printing() {
    let dir = crate::test_support::TestDir::new();
    // A dangling link passes the existence check but cannot be created as a new file.
    let blocked = dir.0.join("blocked.json");
    std::os::unix::fs::symlink(dir.0.join("missing/target.json"), &blocked).unwrap();
    let blocked: &'static str = Box::leak(blocked.to_string_lossy().into_owned().into_boxed_str());
    let saved = dir.0.join("buckets.json");
    let saved_text: &'static str = Box::leak(saved.to_string_lossy().into_owned().into_boxed_str());
    let start = [
        Reply::Type("app-data"),
        Reply::Picks(&["Default settings"]),
        Reply::Pick("Write it"),
        Reply::Pick("Anyone"),
        Reply::Type(blocked),
    ];

    let printing = start
        .iter()
        .copied()
        .chain([Reply::Pick("Print it here instead")]);
    let (answers, script) = run(Some(Format::Json), FileQuestion::Ask, printing);
    let answers = answers.unwrap();
    assert!(script.is_done());
    assert_eq!(answers.saved, None);
    assert!(answers.text.starts_with('{'));
    assert!(
        script
            .seen
            .contains(&"That file exists and is kept.\n".to_owned())
    );

    let another = start
        .iter()
        .copied()
        .chain([Reply::Pick("Choose another file"), Reply::Type(saved_text)]);
    let (answers, script) = run(Some(Format::Json), FileQuestion::Ask, another);
    let answers = answers.unwrap();
    assert!(script.is_done());
    assert_eq!(answers.saved.as_deref(), Some(saved.as_path()));
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), answers.text);
}

/// The port entry takes a port alone and writes the whole origin, so a server that moved to the
/// next free port takes a few keystrokes; anything but 1 to 65535 asks again.
#[test]
fn a_typed_port_gives_the_origin_on_that_port() {
    let port = "3001".parse::<Port>().unwrap();
    assert_eq!(port.on(LOCALHOST), "http://localhost:3001");
    for refused in ["0", "65536", "", "abc", "30 01", "-1", "3001/"] {
        let error = refused.parse::<Port>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "Enter a port from 1 to 65535, for example 3001.",
            "{refused:?}"
        );
    }
}
