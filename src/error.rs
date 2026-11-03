use std::{error::Error, io};

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

pub(crate) fn causes<'a>(
    error: &'a (dyn Error + 'static),
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(Some(error), |&cause| {
        // `io::Error::source()` skips the wrapped error itself, hiding its type from downcasts.
        cause
            .downcast_ref::<io::Error>()
            .and_then(|error| error.get_ref().map(|inner| inner as &(dyn Error + 'static)))
            .or_else(|| cause.source())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn causes_include_each_nested_io_error_and_the_typed_inner_error() {
        let error = io::Error::other(io::Error::other(Wrapped(io::ErrorKind::TimedOut.into())));
        let chain: Vec<_> = causes(&error).collect();
        assert_eq!(chain.len(), 4);
        assert!(std::ptr::eq(
            chain[0].downcast_ref::<io::Error>().unwrap(),
            &error
        ));
        assert!(chain[1].is::<io::Error>());
        assert!(chain[2].is::<Wrapped>());
        assert_eq!(
            chain[3].downcast_ref::<io::Error>().unwrap().kind(),
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
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1].to_string(), "message");
    }
}
