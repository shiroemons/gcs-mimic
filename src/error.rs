use std::error::Error;

/// An error and its causes on one line, skipping a cause that displays the same text as the
/// one above it.
pub(crate) struct Report<'a, E: ?Sized = dyn Error + 'static>(pub(crate) &'a E);

impl<'a> Report<'a> {
    pub(crate) fn chain(error: &'a (dyn Error + 'static)) -> Self {
        Self(error)
    }
}
impl std::fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[cfg(test)]
        assert_distinct_causes(self.0);
        join(f, causes(self.0).map(ToString::to_string))
    }
}

pub(crate) fn join(
    output: &mut impl std::fmt::Write,
    messages: impl Iterator<Item = String>,
) -> std::fmt::Result {
    let mut previous = None;
    for message in messages {
        if previous.as_ref() == Some(&message) {
            continue;
        }
        if previous.is_some() {
            output.write_str(": ")?;
        }
        output.write_str(&message)?;
        previous = Some(message);
    }
    Ok(())
}

/// `error` and every error below it through `source()`, outermost first. An `io::Error` hands
/// over its payload's own source, not the payload: the payload's text is already its own, and
/// the decisions that need a failure's type read typed chains instead.
pub(crate) fn causes<'a>(
    error: &'a (dyn Error + 'static),
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(Some(error), |&cause| cause.source())
}

#[cfg(test)]
pub(crate) fn assert_distinct_causes(error: &(dyn Error + 'static)) {
    let mut outer = error;
    while let Some(inner) = outer.source() {
        assert!(
            !outer.to_string().ends_with(&inner.to_string())
                || upstream_repeat(outer, inner)
                || crate::fsutil::clone_failure_pair(outer, inner)
                || retained_diagnostic_pair(outer, inner)
                || crate::storage::failure::public_source_tests::compatible_pair(outer, inner),
            "error repeats its source: {outer:?} -> {inner:?}"
        );
        outer = inner;
    }
}

#[cfg(test)]
fn retained_diagnostic_pair(outer: &(dyn Error + 'static), inner: &(dyn Error + 'static)) -> bool {
    use crate::bucket_init::generate::GenerateError;
    use crate::config::EventSettingsError;
    use crate::storage::admin::PurgeError;
    use crate::tls::ServerIdentityError;

    if let Some(PurgeError::Admin(cause)) = outer.downcast_ref() {
        return inner.downcast_ref().is_some_and(|e| std::ptr::eq(cause, e))
            && outer.to_string() == format!("bucket purge: {cause}");
    }
    let retained: Option<&dyn Error> =
        if let Some(GenerateError::Rejected(e)) = outer.downcast_ref() {
            Some(e)
        } else {
            match (outer.downcast_ref(), outer.downcast_ref()) {
                (Some(EventSettingsError::Endpoint(e)), _) => Some(e),
                (Some(EventSettingsError::Topic(e) | EventSettingsError::ObjectPrefix(e)), _) => {
                    Some(e)
                }
                (
                    _,
                    Some(
                        ServerIdentityError::Certificate(e)
                        | ServerIdentityError::PrivateKey(e)
                        | ServerIdentityError::Identity(e),
                    ),
                ) => Some(e),
                (_, Some(ServerIdentityError::Rejected(e))) => Some(e),
                _ => crate::storage::test_fixtures::claimed_catalog_source(outer),
            }
        };
    retained.is_some_and(|e| std::ptr::addr_eq(e, inner)) && outer.to_string() == inner.to_string()
}

#[cfg(test)]
fn upstream_repeat(outer: &(dyn Error + 'static), inner: &(dyn Error + 'static)) -> bool {
    match outer.downcast_ref::<rusqlite::Error>() {
        // Without a database message rusqlite displays the FFI error itself.
        Some(rusqlite::Error::SqliteFailure(ffi, None)) => inner
            .downcast_ref()
            .is_some_and(|source| std::ptr::eq(ffi, source)),
        // Column decoding appends the underlying conversion failure after the column and index.
        Some(rusqlite::Error::FromSqlConversionFailure(_, _, source)) => {
            (inner.is::<std::io::Error>() || inner.is::<serde_json::Error>())
                && std::ptr::addr_eq(source.as_ref() as &dyn Error, inner)
        }
        // Encoding a JSON parameter displays the serde error verbatim.
        Some(rusqlite::Error::ToSqlConversionFailure(source)) => {
            inner.is::<serde_json::Error>()
                && std::ptr::addr_eq(source.as_ref() as &dyn Error, inner)
        }
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn sqlite_failure_without_a_message_displays_its_ffi_source() {
        let error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        );
        let source = error.source().unwrap();
        assert!(source.is::<rusqlite::ffi::Error>());
        assert_eq!(error.to_string(), source.to_string());
        assert_eq!(Report::chain(&error).to_string(), source.to_string());
        assert_distinct_causes(&error);
    }

    #[test]
    fn sqlite_column_conversion_displays_its_source_at_the_end() {
        let error = rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(io::Error::other("invalid column")),
        );
        assert_eq!(
            error.to_string(),
            "Conversion error from type Text at index: 2, invalid column"
        );
        assert_eq!(error.source().unwrap().to_string(), "invalid column");
        assert_distinct_causes(&error);
    }

    #[test]
    fn sqlite_parameter_conversion_displays_its_json_source() {
        let error = rusqlite::Error::ToSqlConversionFailure(Box::new(
            serde_json::from_str::<bool>("missing").unwrap_err(),
        ));
        assert_eq!(error.to_string(), error.source().unwrap().to_string());
        assert_distinct_causes(&error);
    }

    #[test]
    #[should_panic(expected = "error repeats its source")]
    fn reports_reject_wrappers_with_the_same_display() {
        let error = io::Error::other(io::Error::other(Wrapped(io::Error::from_raw_os_error(
            libc::EMFILE,
        ))));
        let _ = Report::chain(&error).to_string();
    }

    #[test]
    fn joining_adjacent_equal_messages_writes_the_text_once() {
        let error = Wrapped(io::Error::other("disk detail"));
        let mut message = String::new();
        join(&mut message, causes(&error).map(ToString::to_string)).unwrap();
        assert_eq!(message, "disk detail");
    }

    #[derive(Debug)]
    struct Wrapped(io::Error);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }
    impl Error for Wrapped {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    #[should_panic(expected = "error repeats its source")]
    fn reports_reject_a_wrapper_ending_with_its_source_text() {
        #[derive(Debug)]
        struct Prefixed(io::Error);
        impl std::fmt::Display for Prefixed {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "operation: {}", self.0)
            }
        }
        impl Error for Prefixed {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }
        let _ = Report::chain(&Prefixed(io::ErrorKind::TimedOut.into())).to_string();
    }

    /// Each `io::Error` shows its payload's text and hands over the payload's own source, so
    /// the wrapped payloads are not separate links.
    #[test]
    fn causes_follow_source_past_the_payloads_of_io_errors() {
        let error = io::Error::other(io::Error::other(Wrapped(io::ErrorKind::TimedOut.into())));
        let chain: Vec<_> = causes(&error).collect();
        assert_eq!(chain.len(), 2);
        assert!(std::ptr::eq(
            chain[0].downcast_ref::<io::Error>().unwrap(),
            &error
        ));
        assert_eq!(
            chain[1].downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn causes_end_at_bare_io_errors_and_wrapped_messages() {
        assert_eq!(
            causes(&io::Error::from(io::ErrorKind::PermissionDenied)).count(),
            1
        );
        let error = io::Error::other("message");
        let chain: Vec<_> = causes(&error).collect();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].to_string(), "message");
    }
}
