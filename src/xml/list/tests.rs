//! The position a version 2 listing starts from, which the catalog receives as its cursor.

use super::*;

fn cursor(query: &str) -> Option<ObjectListCursor> {
    parse("bucket".into(), query, &HeaderMap::new())
        .unwrap()
        .query
        .cursor
}

fn after(name: &str) -> Option<ObjectListCursor> {
    Some(ObjectListCursor {
        name: name.into(),
        is_prefix: false,
        generation: None,
    })
}

/// Cloud Storage listed from the start for an empty token, and from `start-after` when that was
/// sent too (recorded).
#[test]
fn an_empty_continuation_token_leaves_start_after_in_effect() {
    assert_eq!(cursor("list-type=2&continuation-token="), None);
    assert_eq!(cursor("list-type=2&continuation-token"), None);
    assert_eq!(
        cursor("list-type=2&continuation-token=&start-after=a"),
        after("a")
    );
    assert_eq!(cursor("list-type=2&start-after="), None);
}

#[test]
fn an_issued_continuation_token_takes_precedence_over_start_after() {
    let token = json::encode_object_page_token(&ObjectListCursor {
        name: "c".into(),
        is_prefix: false,
        generation: None,
    });
    assert_eq!(
        cursor(&format!(
            "list-type=2&start-after=a&continuation-token={CONTINUATION_PREFIX}{token}"
        )),
        after("c")
    );
}

#[test]
fn version_1_refuses_a_continuation_token_even_when_empty() {
    for query in ["continuation-token=", "list-type=1&continuation-token="] {
        let refused = parse("bucket".into(), query, &HeaderMap::new()).is_err();
        assert!(refused, "{query}");
    }
}
