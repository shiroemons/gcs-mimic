//! Flat-namespace object-name rules shared by every transport and import path.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InvalidObjectName {
    Empty,
    TooLong,
    LineBreak,
    DotSegment,
    AcmeChallenge,
}

pub(crate) const MAX_OBJECT_NAME_BYTES: usize = 1024;

pub(crate) fn validate(name: &str) -> Result<(), InvalidObjectName> {
    if name.is_empty() {
        return Err(InvalidObjectName::Empty);
    }
    if name.len() > MAX_OBJECT_NAME_BYTES {
        return Err(InvalidObjectName::TooLong);
    }
    if name.contains(['\r', '\n']) {
        return Err(InvalidObjectName::LineBreak);
    }
    if matches!(name, "." | "..") {
        return Err(InvalidObjectName::DotSegment);
    }
    if name.starts_with(".well-known/acme-challenge/") {
        return Err(InvalidObjectName::AcmeChallenge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_namespace_boundaries_and_reserved_names() {
        for name in [
            "...",
            ".well-known/acme-challenge",
            "a\tb",
            &"x".repeat(1024),
            &"é".repeat(512),
        ] {
            assert!(validate(name).is_ok(), "{name:?}");
        }
        for (name, error) in [
            ("", InvalidObjectName::Empty),
            (".", InvalidObjectName::DotSegment),
            ("..", InvalidObjectName::DotSegment),
            ("a\rb", InvalidObjectName::LineBreak),
            ("a\nb", InvalidObjectName::LineBreak),
            (
                ".well-known/acme-challenge/x",
                InvalidObjectName::AcmeChallenge,
            ),
            (&"x".repeat(1025), InvalidObjectName::TooLong),
        ] {
            assert_eq!(validate(name), Err(error));
        }
    }
}
