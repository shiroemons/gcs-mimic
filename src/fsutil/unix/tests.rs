//! The Unix rename fallback and the errno the xattr read maps, which only this module reaches.

use super::*;

#[test]
fn exclusive_rename_and_link_fallback_preserve_existing_destinations() {
    for rename in [rename_noreplace, link_then_unlink] {
        let directory = crate::test_support::TestDir::new();
        let source = directory.0.join("source");
        let destination = directory.0.join("destination");
        std::fs::write(&source, b"first").unwrap();
        rename(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"first");
        std::fs::write(&source, b"second").unwrap();
        assert_eq!(
            rename(&source, &destination).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"second");
        assert_eq!(std::fs::read(&destination).unwrap(), b"first");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn metadata_errno_retains_platform_distinctions() {
    use super::super::SourceMetadataError;
    #[cfg(target_os = "linux")]
    assert_eq!(
        classify(io::Error::from_raw_os_error(libc::ENODATA)).unwrap(),
        None
    );
    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            classify(io::Error::from_raw_os_error(libc::ENOATTR)).unwrap(),
            None
        );
        std::assert_matches!(
            classify(io::Error::from_raw_os_error(libc::ENODATA)),
            Err(SourceMetadataError::Io(_))
        );
    }
    for errno in [libc::ENOTSUP, libc::EOPNOTSUPP] {
        std::assert_matches!(
            classify(io::Error::from_raw_os_error(errno)),
            Err(SourceMetadataError::Unsupported)
        );
    }
    for errno in [libc::EACCES, libc::EPERM, libc::EBADF] {
        let Err(SourceMetadataError::Io(error)) = classify(io::Error::from_raw_os_error(errno))
        else {
            panic!("must preserve I/O error")
        };
        assert_eq!(error.raw_os_error(), Some(errno));
    }
    for errno in [libc::ERANGE, libc::E2BIG] {
        std::assert_matches!(
            classify(io::Error::from_raw_os_error(errno)),
            Err(SourceMetadataError::TooLarge)
        );
    }
}
