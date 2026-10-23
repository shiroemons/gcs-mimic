use gcs_mimic::json::{self, compose};
use hyper::StatusCode;
use serde_json::{Value, json};

fn parse(value: Value) -> Result<compose::ComposeRequest, json::JsonError> {
    compose::parse_compose(
        value.to_string().as_bytes(),
        "compose-bucket",
        "destination",
    )
}

#[test]
fn compose_body_preserves_source_order_duplicates_generations_and_metadata() {
    let parsed = parse(json!({
        "kind": "storage#composeRequest",
        "sourceObjects": [
            {"name": "a/雪", "generation": "42", "objectPreconditions": {"ifGenerationMatch": 42}},
            {"name": "a/雪"}
        ],
        "deleteSourceObjects": false,
        "destination": {
            "bucket": "compose-bucket", "name": "destination", "kind": "storage#object",
            "contentType": "text/plain", "metadata": {"key": "value"}, "eventBasedHold": true,
            "md5Hash": "ignored", "crc32c": "ignored"
        }
    }))
    .unwrap();
    assert_eq!(parsed.sources.len(), 2);
    assert_eq!(parsed.sources[0].name, "a/雪");
    assert_eq!(parsed.sources[0].generation, Some(42));
    assert_eq!(parsed.sources[0].if_generation_match, Some(42));
    assert_eq!(parsed.sources[1].generation, None);
    assert_eq!(
        parsed.destination.content_type,
        Some(Some("text/plain".into()))
    );
    assert_eq!(parsed.destination.event_based_hold, Some(true));
    assert!(parsed.destination.replace);
    assert!(!parsed.delete_sources);
    assert_eq!(
        parsed.destination.metadata.unwrap().unwrap()["key"],
        Some("value".into())
    );
}

#[test]
fn compose_body_accepts_one_and_thirty_two_sources_and_empty_destination() {
    for count in [1, 32] {
        let parsed =
            parse(json!({"sourceObjects": vec![json!({"name": "source"}); count]})).unwrap();
        assert_eq!(parsed.sources.len(), count);
        assert_eq!(parsed.destination.content_type, None);
        assert!(!parsed.delete_sources);
    }
    assert!(
        parse(json!({"sourceObjects": [{"name": "source"}], "deleteSourceObjects": true}))
            .unwrap()
            .delete_sources
    );
}

#[test]
fn compose_body_rejects_malformed_source_and_destination_shapes() {
    for body in [
        json!(null),
        json!([]),
        json!({}),
        json!({"sourceObjects": []}),
        json!({"sourceObjects": vec![json!({"name": "source"}); 33]}),
        json!({"sourceObjects": [null]}),
        json!({"sourceObjects": [{}]}),
        json!({"sourceObjects": [{"name": ""}]}),
        json!({"sourceObjects": [{"name": "source", "objectPreconditions": null}]}),
        json!({"sourceObjects": [{"name": "source"}], "destination": null}),
        json!({"sourceObjects": [{"name": "source"}], "destination": {"bucket": "other"}}),
        json!({"sourceObjects": [{"name": "source"}], "deleteSourceObjects": "false"}),
        json!({"sourceObjects": [{"name": "source"}], "kind": "storage#object"}),
    ] {
        let error = parse(body.clone()).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(
        compose::parse_compose(b"{", "bucket", "name")
            .unwrap_err()
            .reason,
        "parseError"
    );
}

#[test]
fn compose_body_rejects_invalid_generation_values() {
    for invalid in [
        json!(-1),
        json!(null),
        json!(false),
        json!(1.5),
        json!("+1"),
        json!("-1"),
        json!("9223372036854775808"),
    ] {
        for key in ["generation", "ifGenerationMatch"] {
            let mut source = json!({"name": "source"});
            if key == "generation" {
                source[key] = invalid.clone();
            } else {
                source["objectPreconditions"] = json!({key: invalid});
            }
            assert_eq!(
                parse(json!({"sourceObjects": [source]}))
                    .unwrap_err()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
    }
}

#[test]
fn compose_body_rejects_recognized_unsupported_and_unknown_semantics() {
    for body in [
        json!({"sourceObjects": [{"name": "source", "bucket": "other"}]}),
        json!({"sourceObjects": [{"name": "source", "objectPreconditions": {"ifMetagenerationMatch": "1"}}]}),
        json!({"sourceObjects": [{"name": "source"}], "destination": {"acl": []}}),
        json!({"sourceObjects": [{"name": "source"}], "destination": {"componentCount": 1}}),
        json!({"sourceObjects": [{"name": "source"}], "unknown": true}),
    ] {
        assert_eq!(
            parse(body.clone()).unwrap_err().status,
            StatusCode::NOT_IMPLEMENTED,
            "{body}"
        );
    }
}
