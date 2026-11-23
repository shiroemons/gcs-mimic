use std::error::Error;

#[cfg_attr(not(test), expect(dead_code, reason = "the seed watcher reuses this"))]
pub(crate) struct Report<'a>(pub(crate) &'a (dyn Error + 'static));
impl std::fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut previous = None;
        for cause in causes(self.0) {
            let message = cause.to_string();
            if previous.as_ref() == Some(&message) {
                continue;
            }
            if previous.is_some() {
                f.write_str(": ")?;
            }
            f.write_str(&message)?;
            previous = Some(message);
        }
        Ok(())
    }
}

/// `error` and every error below it through `source()`, outermost first. An `io::Error` hands
/// over its payload's own source, not the payload: the payload's text is already its own, and
/// the decisions that need a failure's type read typed chains instead.
#[cfg_attr(not(test), expect(dead_code, reason = "the seed watcher reuses this"))]
pub(crate) fn causes<'a>(
    error: &'a (dyn Error + 'static),
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(Some(error), |&cause| cause.source())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn reports_do_not_repeat_wrappers_with_the_same_display() {
        let error = io::Error::other(io::Error::other(Wrapped(io::Error::from_raw_os_error(
            libc::EMFILE,
        ))));
        assert_eq!(
            Report(&error).to_string(),
            io::Error::from_raw_os_error(libc::EMFILE).to_string()
        );
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
