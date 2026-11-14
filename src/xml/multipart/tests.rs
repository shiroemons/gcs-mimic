use super::*;

fn parse(method: Method, name: &str, query: &[(&str, &str)]) -> Result<Operation, Error> {
    let mut query = query
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    Operation::parse(&method, name, &mut query)
}

fn message(result: Result<Operation, Error>) -> String {
    match result {
        Ok(_) => "accepted".to_owned(),
        Err(error) => error.message,
    }
}

/// A request can fail several checks; the first in the order the parser has always used names
/// the problem.
#[test]
fn multipart_operation_errors_keep_their_order() {
    let long_id = "x".repeat(1025);
    for (method, name, query, expected) in [
        // The part number is checked before the upload ID.
        (
            Method::PUT,
            "object",
            &[("partNumber", "0"), ("uploadId", "")][..],
            "Multipart number is out of range",
        ),
        (
            Method::PUT,
            "object",
            &[("partNumber", "1"), ("uploadId", "")][..],
            "Invalid uploadId",
        ),
        (
            Method::PUT,
            "object",
            &[("partNumber", "10001"), ("uploadId", "id")][..],
            "Multipart number is out of range",
        ),
        (
            Method::PUT,
            "object",
            &[("partNumber", "+1"), ("uploadId", "id")][..],
            "Multipart number is out of range",
        ),
        // The uploads and uploadId combination is checked before the method.
        (
            Method::GET,
            "",
            &[("uploads", "x")][..],
            "Use uploads or uploadId, with an empty uploads value",
        ),
        (
            Method::PATCH,
            "object",
            &[("uploads", ""), ("uploadId", "id")][..],
            "Use uploads or uploadId, with an empty uploads value",
        ),
        (
            Method::PATCH,
            "object",
            &[("uploadId", "")][..],
            "Invalid multipart method or query combination",
        ),
        (
            Method::GET,
            "object",
            &[("uploads", "")][..],
            "Invalid multipart method or query combination",
        ),
        (
            Method::DELETE,
            "object",
            &[("uploadId", long_id.as_str())][..],
            "Invalid uploadId",
        ),
        (
            Method::PUT,
            "object",
            &[("partNumber", "10000"), ("uploadId", "id")][..],
            "accepted",
        ),
    ] {
        assert_eq!(
            message(parse(method.clone(), name, query)),
            expected,
            "{method} {name:?} {query:?}"
        );
    }
}
