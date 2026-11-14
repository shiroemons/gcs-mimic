use crate::sync::SyncError;

pub(super) fn paused(error: &SyncError) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Error,
        "sync_paused",
        diagnostic(
            error,
            serde_json::json!({"reason":crate::error::Report(error).to_string(),"remedy":"Preserve pending data, correct the reported condition and request an explicit rescan"}),
        ),
    );
}
pub(super) fn retrying(error: &SyncError, delay: std::time::Duration) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "sync_retrying",
        diagnostic(
            error,
            serde_json::json!({"reason":crate::error::Report(error).to_string(),"delay_ms":delay.as_millis()}),
        ),
    );
}

pub(super) fn diagnostic(error: &SyncError, details: serde_json::Value) -> serde_json::Value {
    #[cfg(unix)]
    let mut details = details;
    #[cfg(unix)]
    if error.causes().any(|cause| {
        matches!(
            cause,
            crate::sync::Cause::Io(error)
                if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
        )
    }) && let Some(fields) = details.as_object_mut()
    {
        fields.insert("descriptor_limit".into(), limits());
    }
    #[cfg(not(unix))]
    let _ = error;
    details
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "getrlimit is the platform interface for the current descriptor budget"
)]
fn limits() -> serde_json::Value {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only this initialized rlimit and does not retain its pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } == 0 {
        serde_json::json!({"soft":limits.rlim_cur,"hard":limits.rlim_max,"remedy":"Close unused descriptors or raise the process descriptor budget with ulimit -n, then retry synchronization"})
    } else {
        serde_json::json!({"query_error":std::io::Error::last_os_error().to_string(),"remedy":"Inspect the process descriptor budget with ulimit -n and close unused descriptors"})
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn descriptor_pressure_diagnostics_include_limits_only_for_descriptor_exhaustion() {
        for errno in [libc::EMFILE, libc::ENFILE] {
            let error = SyncError::from(std::io::Error::other(std::io::Error::from_raw_os_error(
                errno,
            )));
            let value = diagnostic(&error, serde_json::json!({"reason":"failed"}));
            assert!(value["descriptor_limit"]["soft"].as_u64().is_some());
            assert!(value["descriptor_limit"]["hard"].as_u64().is_some());
            assert!(
                value["descriptor_limit"]["remedy"]
                    .as_str()
                    .is_some_and(|value| value.contains("ulimit -n"))
            );
        }
        let ordinary = diagnostic(
            &SyncError::from(std::io::Error::from_raw_os_error(libc::EIO)),
            serde_json::json!({}),
        );
        assert!(ordinary.get("descriptor_limit").is_none());
    }
}
