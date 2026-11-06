//! Fits the process's open-file limit to the configured connection limit.
//!
//! The soft limit starts at 256 on macOS and often 1024 on Linux, while accepted connections
//! fail with EMFILE once it runs out. The hard limit usually allows far more, so the server
//! raises the soft limit at startup, as the Go runtime does for its programs.

use crate::diagnostics::Diagnostic;

/// Each connection holds its socket, and a transfer also holds a staged or source file and
/// sometimes its sidecar, so a busy connection can use about this many descriptors.
#[cfg(unix)]
const DESCRIPTORS_PER_CONNECTION: u64 = 3;
/// Catalog files, listeners, standard streams, watchers and other process-wide descriptors.
#[cfg(unix)]
const BASE_DESCRIPTORS: u64 = 64;

/// Raises the soft limit toward what `max_connections` can use and returns a warning when the
/// result falls short. The server still starts then: it only refuses connections past the limit.
#[cfg(unix)]
pub fn raise_for_connections(max_connections: usize) -> Option<Diagnostic> {
    use crate::diagnostics::DiagnosticSource;
    let wanted = u64::try_from(max_connections)
        .unwrap_or(u64::MAX)
        .saturating_mul(DESCRIPTORS_PER_CONNECTION)
        .saturating_add(BASE_DESCRIPTORS);
    let raised = open_file_limit().map(|limit| {
        raise(wanted, limit.rlim_cur, limit.rlim_max, |soft| {
            set_soft_limit(soft, limit.rlim_max)
        })
    });
    let fix = match raised {
        Ok(reached) if reached >= wanted => return None,
        Ok(reached) => format!(
            "max_connections={max_connections} can use about {wanted} file descriptors, but the open-file limit stays at {reached}; raise it (ulimit -n {wanted}, or ulimits in Compose) or lower max_connections, or connections fail under load."
        ),
        Err(error) => format!(
            "Cannot read the open-file limit ({}); make sure it allows about {wanted} descriptors, or lower max_connections.",
            error.kind()
        ),
    };
    Some(Diagnostic::new(
        DiagnosticSource::Runtime,
        "max_connections",
        fix,
    ))
}

/// Windows has no such limit on sockets, and object files stay well below its handle limits.
#[cfg(not(unix))]
pub fn raise_for_connections(_max_connections: usize) -> Option<Diagnostic> {
    None
}

/// Returns the soft limit in force afterwards. A platform can refuse values below the hard
/// limit: macOS reports an unlimited hard limit, yet refuses more than `kern.maxfilesperproc`.
/// Each refusal therefore settles for half the remaining raise.
#[cfg(unix)]
fn raise(
    wanted: u64,
    soft: u64,
    hard: u64,
    mut set_soft: impl FnMut(u64) -> std::io::Result<()>,
) -> u64 {
    std::iter::successors(Some(wanted.min(hard)), |attempt| {
        Some(soft + attempt.saturating_sub(soft) / 2)
    })
    .take_while(|attempt| *attempt > soft)
    .find(|attempt| set_soft(*attempt).is_ok())
    .unwrap_or(soft)
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "std has no wrapper for getrlimit; the call only writes the local struct"
)]
fn open_file_limit() -> std::io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid rlimit that outlives the call, which writes nothing else.
    match unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } {
        0 => Ok(limit),
        _ => Err(std::io::Error::last_os_error()),
    }
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "std has no wrapper for setrlimit; the call only reads the local struct"
)]
fn set_soft_limit(soft: u64, hard: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: `limit` is a valid rlimit that outlives the call, which only reads it.
    match unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn refused() -> std::io::Error {
        std::io::Error::from_raw_os_error(libc::EINVAL)
    }

    #[test]
    fn a_sufficient_limit_stays_untouched() {
        let raised = raise(100, 100, 200, |_| panic!("no raise is needed"));
        assert_eq!(raised, 100);
        let raised = raise(100, 500, 500, |_| panic!("no raise is needed"));
        assert_eq!(raised, 500);
    }

    #[test]
    fn the_hard_limit_caps_the_raise() {
        let mut attempts = Vec::new();
        let raised = raise(1000, 256, 512, |soft| {
            attempts.push(soft);
            Ok(())
        });
        assert_eq!(raised, 512);
        assert_eq!(attempts, [512]);
    }

    #[test]
    fn a_refused_raise_settles_for_the_largest_accepted_step() {
        // macOS: an unlimited hard limit, but a lower ceiling that only setrlimit reveals.
        let ceiling = 10_240;
        let mut attempts = Vec::new();
        let raised = raise(40_000, 256, u64::MAX, |soft| {
            attempts.push(soft);
            if soft <= ceiling {
                Ok(())
            } else {
                Err(refused())
            }
        });
        assert!(raised <= ceiling, "{raised}");
        assert!(raised > 256, "{raised}");
        assert_eq!(attempts.first(), Some(&40_000));
        assert_eq!(attempts.last(), Some(&raised));
    }

    #[test]
    fn a_platform_refusing_every_raise_keeps_the_soft_limit_without_looping_forever() {
        let mut attempts = 0;
        let raised = raise(u64::MAX, 256, u64::MAX, |_| {
            attempts += 1;
            Err(refused())
        });
        assert_eq!(raised, 256);
        assert!(attempts <= 64, "{attempts}");
    }

    #[test]
    fn the_real_limit_is_readable() {
        let limit = open_file_limit().unwrap();
        assert!(limit.rlim_cur > 0 && limit.rlim_cur <= limit.rlim_max);
    }

    #[test]
    fn an_unreachable_target_warns_with_the_fix() {
        let warning = raise_for_connections(usize::MAX).unwrap();
        assert_eq!(warning.key, "max_connections");
        assert!(warning.fix.contains("ulimit -n"), "{}", warning.fix);
    }
}
