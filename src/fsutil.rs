#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Safe source metadata reads; unsafe is confined to the Linux/macOS libc ABI.

use std::{fs::File, io};

pub const SOURCE_METADATA_CAP: usize = 65_536;

pub struct SourceMetadataBuffer(Box<[u8; SOURCE_METADATA_CAP + 1]>);

impl Default for SourceMetadataBuffer {
    fn default() -> Self {
        Self(Box::new([0; SOURCE_METADATA_CAP + 1]))
    }
}

#[derive(Debug)]
pub enum SourceMetadataError {
    Unsupported,
    TooLarge,
    Io(io::Error),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify(error: io::Error) -> Result<Option<usize>, SourceMetadataError> {
    let errno = error.raw_os_error();
    #[cfg(target_os = "linux")]
    let missing = libc::ENODATA;
    #[cfg(target_os = "macos")]
    let missing = libc::ENOATTR;
    if errno == Some(missing) {
        Ok(None)
    } else if errno == Some(libc::ENOTSUP) || errno == Some(libc::EOPNOTSUPP) {
        Err(SourceMetadataError::Unsupported)
    } else if errno == Some(libc::ERANGE) || errno == Some(libc::E2BIG) {
        Err(SourceMetadataError::TooLarge)
    } else {
        Err(SourceMetadataError::Io(error))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn checked_length(length: usize) -> Result<Option<usize>, SourceMetadataError> {
    if length > SOURCE_METADATA_CAP {
        Err(SourceMetadataError::TooLarge)
    } else {
        Ok(Some(length))
    }
}

/// The dedicated buffer prevents callers from supplying a smaller allocation.
///
/// ```compile_fail
/// use gcs_mimic::fsutil::read_source_metadata;
/// let file = std::fs::File::open("body").unwrap();
/// let mut bytes = [0u8; 1];
/// read_source_metadata(&file, &mut bytes);
/// ```
pub fn read_source_metadata<'a>(
    file: &File,
    buffer: &'a mut SourceMetadataBuffer,
) -> Result<Option<&'a [u8]>, SourceMetadataError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the live file descriptor and literal NUL-terminated key are valid;
        // the exclusive buffer pointer covers exactly the supplied allocation length.
        let result = unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    c"user.metadata".as_ptr(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                )
            }
            #[cfg(target_os = "macos")]
            {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    c"user.metadata".as_ptr(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                    0,
                    0,
                )
            }
        };
        let length = if result < 0 {
            classify(io::Error::last_os_error())?
        } else {
            checked_length(result as usize)?
        };
        Ok(length.map(|length| &buffer.0[..length]))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (file, &mut buffer.0);
        Err(SourceMetadataError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn metadata_lengths_include_exact_cap_and_empty() {
        assert_eq!(checked_length(0).unwrap(), Some(0));
        assert_eq!(
            checked_length(SOURCE_METADATA_CAP).unwrap(),
            Some(SOURCE_METADATA_CAP)
        );
        assert!(matches!(
            checked_length(SOURCE_METADATA_CAP + 1),
            Err(SourceMetadataError::TooLarge)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn metadata_errno_retains_platform_distinctions() {
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
            assert!(matches!(
                classify(io::Error::from_raw_os_error(libc::ENODATA)),
                Err(SourceMetadataError::Io(_))
            ));
        }
        for errno in [libc::ENOTSUP, libc::EOPNOTSUPP] {
            assert!(matches!(
                classify(io::Error::from_raw_os_error(errno)),
                Err(SourceMetadataError::Unsupported)
            ));
        }
        for errno in [libc::EACCES, libc::EPERM, libc::EBADF] {
            let Err(SourceMetadataError::Io(error)) = classify(io::Error::from_raw_os_error(errno))
            else {
                panic!("must preserve I/O error")
            };
            assert_eq!(error.raw_os_error(), Some(errno));
        }
        for errno in [libc::ERANGE, libc::E2BIG] {
            assert!(matches!(
                classify(io::Error::from_raw_os_error(errno)),
                Err(SourceMetadataError::TooLarge)
            ));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn descriptor_xattr_reads_are_bounded_and_read_only() {
        let directory = super::test_support::SourceDir::new();
        let path = directory.0.join("body");
        std::fs::write(&path, b"hello").unwrap();
        let file = File::open(path).unwrap();
        let mut buffer = SourceMetadataBuffer::default();
        match read_source_metadata(&file, &mut buffer) {
            Ok(None) => (),
            Err(SourceMetadataError::Unsupported) => {
                eprintln!(
                    "xattr execution unavailable: source filesystem does not support user.metadata"
                );
                return;
            }
            other => panic!("unexpected empty fixture attribute: {other:?}"),
        }
        for size in [0, 37, SOURCE_METADATA_CAP, SOURCE_METADATA_CAP + 1] {
            let value = vec![b'x'; size];
            match super::test_support::set_metadata(&file, &value) {
                Ok(true) => (),
                Ok(false) => panic!("filesystem xattr capability changed"),
                Err(error) => {
                    assert!(size >= SOURCE_METADATA_CAP);
                    assert!(matches!(
                        error.raw_os_error(),
                        Some(libc::E2BIG | libc::ENOSPC | libc::ERANGE)
                    ));
                    eprintln!("xattr write ceiling for {size} bytes: {error}");
                    continue;
                }
            }
            let before = file.metadata().unwrap();
            if size <= SOURCE_METADATA_CAP {
                assert_eq!(
                    read_source_metadata(&file, &mut buffer).unwrap(),
                    Some(value.as_slice())
                );
            } else {
                assert!(matches!(
                    read_source_metadata(&file, &mut buffer),
                    Err(SourceMetadataError::TooLarge)
                ));
            }
            assert_eq!(
                before.modified().unwrap(),
                file.metadata().unwrap().modified().unwrap()
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod test_support {
    //! Test-only snapshot creation; production source readers never call this module.
    use std::{fs, io, path::PathBuf};

    pub struct SourceDir(pub PathBuf);
    impl SourceDir {
        pub fn new() -> Self {
            let mut random = [0; 8];
            getrandom::fill(&mut random).expect("test directory randomness");
            let path = std::env::temp_dir().join(format!(
                "gcs-mimic-source-{}-{}",
                std::process::id(),
                u64::from_ne_bytes(random)
            ));
            fs::create_dir(&path).expect("test source directory");
            Self(path)
        }
    }
    impl Drop for SourceDir {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                if std::thread::panicking() {
                    eprintln!("source fixture cleanup failed: {error}");
                } else {
                    panic!("source fixture cleanup failed: {error}");
                }
            }
        }
    }

    pub fn set_metadata(file: &fs::File, bytes: &[u8]) -> io::Result<bool> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the test owns the live descriptor; the literal key and input
            // allocation remain valid for the synchronous Linux/macOS ABI call.
            let result = unsafe {
                #[cfg(target_os = "linux")]
                {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        c"user.metadata".as_ptr(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        0,
                    )
                }
                #[cfg(target_os = "macos")]
                {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        c"user.metadata".as_ptr(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        0,
                        0,
                    )
                }
            };
            if result == 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTSUP)
                || error.raw_os_error() == Some(libc::EOPNOTSUPP)
            {
                Ok(false)
            } else {
                Err(error)
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (file, bytes);
            Ok(false)
        }
    }
}
