use super::super::{PublicAccessPrevention, Setting};
use super::*;

fn bucket(name: &str, settings: impl IntoIterator<Item = Setting>) -> Bucket {
    let mut bucket = Bucket::new(name).unwrap();
    for setting in settings {
        bucket.apply(setting).unwrap();
    }
    bucket
}

#[test]
fn a_bucket_without_answers_keeps_default_settings() {
    assert_eq!(
        bucket("app-exports", []).to_string(),
        "app-exports\n  default settings\n"
    );
}

/// Each answer gets one line in words, and public read is not shown again as its IAM binding.
#[test]
fn each_answer_is_one_line_in_words() {
    let entry = bucket(
        "app-uploads",
        [
            Setting::CorsOrigin("http://localhost:3000".parse().unwrap()),
            Setting::CorsOrigin("capacitor://localhost".parse().unwrap()),
            Setting::CorsMethod("PUT".parse().unwrap()),
            Setting::CorsMethod("POST".parse().unwrap()),
            Setting::CorsResponseHeader("Content-Type".parse().unwrap()),
            Setting::CorsMaxAgeSeconds(3600),
            Setting::PublicRead,
            Setting::Versioning,
            Setting::SoftDeleteRetentionSeconds(604_800),
            Setting::Label("team=storage".parse().unwrap()),
            Setting::Label("env=dev".parse().unwrap()),
            Setting::PublicAccessPrevention(PublicAccessPrevention::Inherited),
            Setting::DefaultEventBasedHold,
            Setting::Location("EU".into()),
            Setting::StorageClass("NEARLINE".into()),
            Setting::Project("other-project".into()),
            Setting::IamBinding {
                role: "roles/storage.objectAdmin".into(),
                members: "user:a@example.com,user:b@example.com".parse().unwrap(),
            },
        ],
    );
    assert_eq!(
        entry.to_string(),
        "app-uploads
  CORS: PUT, POST from http://localhost:3000, capacitor://localhost; headers Content-Type; max age 1 hour
  public read: anyone can read objects
  versioning: overwrites and deletes keep the old object
  soft delete: keeps deleted objects for 7 days
  labels: env=dev, team=storage
  public access prevention: inherited
  event-based hold: new objects start held
  location: EU
  storage class: NEARLINE
  project: other-project
  IAM: roles/storage.objectAdmin for user:a@example.com, user:b@example.com
"
    );
}

/// Public read is one line, not repeated as the binding it writes.
#[test]
fn public_read_is_shown_once() {
    assert_eq!(
        bucket("app-assets", [Setting::PublicRead]).to_string(),
        "app-assets\n  public read: anyone can read objects\n"
    );
}

/// A binding that grants allUsers reads without public read is shown, since nothing else says so.
#[test]
fn a_public_binding_given_by_hand_is_shown() {
    let entry = bucket(
        "app-assets",
        [
            Setting::IamBinding {
                role: "roles/storage.objectViewer".into(),
                members: "allUsers".parse().unwrap(),
            },
            Setting::SoftDeleteRetentionSeconds(0),
        ],
    );
    assert_eq!(
        entry.to_string(),
        "app-assets\n  soft delete: off\n  IAM: roles/storage.objectViewer for allUsers\n"
    );
}

#[test]
fn spans_use_the_largest_whole_unit() {
    for (seconds, words) in [
        (1, "1 second"),
        (90, "90 seconds"),
        (600, "10 minutes"),
        (3600, "1 hour"),
        (5400, "90 minutes"),
        (86_400, "1 day"),
        (7_776_000, "90 days"),
        (0, "0 seconds"),
    ] {
        assert_eq!(Span(seconds).to_string(), words);
    }
}
