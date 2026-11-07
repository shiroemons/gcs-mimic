//! Fits the process's open-file limit to the configured connection and gRPC stream limits.
//!
//! The soft limit starts at 256 on macOS and often 1024 on Linux, while accepted connections
//! fail with EMFILE once it runs out. The hard limit usually allows far more, so the server
//! raises the soft limit at startup, as the Go runtime does for its programs.

use crate::diagnostics::Diagnostic;
use std::num::NonZeroU32;

/// Each connection holds its socket, and a transfer also holds a staged or source file and
/// sometimes its sidecar, so a busy connection can use about this many descriptors.
#[cfg(unix)]
const DESCRIPTORS_PER_CONNECTION: u64 = 3;
/// A gRPC connection runs many transfers at once, each with a source file and a staged file.
#[cfg(unix)]
const DESCRIPTORS_PER_GRPC_STREAM: u64 = 2;
/// Catalog files, listeners, standard streams, watchers and other process-wide descriptors.
#[cfg(unix)]
const BASE_DESCRIPTORS: u64 = 64;

/// What the configured limits can use, split by how a shortfall is treated.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct DescriptorBudget {
    /// Below this, ordinary connections can fail, so falling short warns.
    required: u64,
    /// What to raise toward, also covering every stream of every gRPC connection at its limit.
    /// Falling short stays silent: macOS refuses more than about 10240, and that worst case is
    /// out of its reach at the default limits, so a warning would fire whenever gRPC is on.
    target: u64,
}

#[cfg(unix)]
impl DescriptorBudget {
    fn new(max_connections: usize, grpc_streams: Option<NonZeroU32>) -> Self {
        let connections = u64::try_from(max_connections).unwrap_or(u64::MAX);
        let required = connections
            .saturating_mul(DESCRIPTORS_PER_CONNECTION)
            .saturating_add(BASE_DESCRIPTORS);
        let target = grpc_streams.map_or(required, |streams| {
            connections
                .saturating_mul(u64::from(streams.get()))
                .saturating_mul(DESCRIPTORS_PER_GRPC_STREAM)
                .saturating_add(required)
        });
        Self { required, target }
    }

    /// How to fix the limit that `raised` reports, when it falls short of what is required.
    fn shortfall_fix(
        &self,
        max_connections: usize,
        raised: std::io::Result<u64>,
    ) -> Option<String> {
        let wanted = self.required;
        match raised {
            Ok(reached) if reached >= wanted => None,
            Ok(reached) => Some(format!(
                "max_connections={max_connections} can use about {wanted} file descriptors, but the open-file limit stays at {reached}; raise it (ulimit -n {wanted}, or ulimits in Compose) or lower max_connections, or connections fail under load."
            )),
            Err(error) => Some(format!(
                "Cannot read the open-file limit ({}); make sure it allows about {wanted} descriptors, or lower max_connections.",
                error.kind()
            )),
        }
    }
}

/// Raises the soft limit toward what `max_connections` and, on a server with a gRPC listener,
/// `grpc_streams` can use, and returns a warning when the result falls short of what ordinary
/// connections need. The server still starts then: it only refuses connections past the limit.
#[cfg(unix)]
pub fn raise_for_connections(
    max_connections: usize,
    grpc_streams: Option<NonZeroU32>,
) -> Option<Diagnostic> {
    use crate::diagnostics::DiagnosticSource;
    let budget = DescriptorBudget::new(max_connections, grpc_streams);
    let raised = open_file_limit().map(|limit| {
        raise(budget.target, limit.rlim_cur, limit.rlim_max, |soft| {
            set_soft_limit(soft, limit.rlim_max)
        })
    });
    budget
        .shortfall_fix(max_connections, raised)
        .map(|fix| Diagnostic::new(DiagnosticSource::Runtime, "max_connections", fix))
}

/// Windows has no such limit on sockets, and object files stay well below its handle limits.
#[cfg(not(unix))]
pub fn raise_for_connections(
    _max_connections: usize,
    _grpc_streams: Option<NonZeroU32>,
) -> Option<Diagnostic> {
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

/// How many files a cache may keep open: a quarter of what the process may hold, which leaves the
/// rest to connections and transfers, however low the limit stayed after startup raised it.
#[cfg(unix)]
pub(crate) fn cacheable_files() -> usize {
    open_file_limit().map_or(0, |limit| {
        usize::try_from(limit.rlim_cur / 4).unwrap_or(usize::MAX)
    })
}

#[cfg(not(unix))]
pub(crate) fn cacheable_files() -> usize {
    usize::MAX
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
        let warning = raise_for_connections(usize::MAX, None).unwrap();
        assert_eq!(warning.key, "max_connections");
        assert!(warning.fix.contains("ulimit -n"), "{}", warning.fix);
    }

    #[test]
    fn a_grpc_listener_raises_the_target_and_keeps_the_required_budget() {
        let without_grpc = DescriptorBudget::new(1024, None);
        assert_eq!(
            without_grpc,
            DescriptorBudget {
                required: 3136,
                target: 3136,
            }
        );
        let with_grpc = DescriptorBudget::new(1024, NonZeroU32::new(100));
        assert_eq!(
            with_grpc,
            DescriptorBudget {
                required: 3136,
                // 1024 connections * 100 streams * 2 files, on top of the required budget.
                target: 3136 + 204_800,
            }
        );
    }

    #[test]
    fn the_largest_settings_saturate_instead_of_overflowing() {
        let budget = DescriptorBudget::new(usize::MAX, NonZeroU32::new(u32::MAX));
        assert_eq!(budget.required, u64::MAX);
        assert_eq!(budget.target, u64::MAX);
    }

    /// macOS: the raise toward every stream of every connection stops at a platform ceiling, far
    /// short of the target but well above what ordinary connections need.
    #[test]
    fn a_target_out_of_reach_stays_silent_when_the_required_budget_fits() {
        let budget = DescriptorBudget::new(1024, NonZeroU32::new(100));
        let ceiling = 10_240;
        let reached = raise(budget.target, 256, u64::MAX, |soft| {
            if soft <= ceiling {
                Ok(())
            } else {
                Err(refused())
            }
        });
        assert!(reached < budget.target, "{reached}");
        assert!(reached >= budget.required, "{reached}");
        assert_eq!(budget.shortfall_fix(1024, Ok(reached)), None);
    }

    #[test]
    fn falling_short_of_the_required_budget_warns_with_or_without_grpc() {
        for grpc_streams in [None, NonZeroU32::new(100)] {
            let budget = DescriptorBudget::new(1024, grpc_streams);
            let fix = budget.shortfall_fix(1024, Ok(3135)).unwrap();
            assert!(fix.contains("about 3136 file descriptors"), "{fix}");
            assert!(fix.contains("ulimit -n 3136"), "{fix}");
            assert_eq!(budget.shortfall_fix(1024, Ok(3136)), None);
            let fix = budget.shortfall_fix(1024, Err(refused())).unwrap();
            assert!(fix.contains("about 3136 descriptors"), "{fix}");
        }
    }

    #[test]
    fn a_grpc_listener_never_adds_a_warning_when_the_real_limit_fits() {
        // The real limit cannot go below the 67 descriptors one connection requires in any
        // environment that runs these tests, whatever it does with the far larger target.
        assert!(raise_for_connections(1, NonZeroU32::new(65_536)).is_none());
    }
}
