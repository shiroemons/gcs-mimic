//! How a command on the server's host finds the server when it is given no address, as the
//! healthcheck does.

use gcs_mimic::{config::Config, diagnostics::Diagnostics};
use std::{ffi::OsString, path::Path};

/// A configuration whose errors keep the origin it gives from being relied on.
#[derive(Debug)]
pub(super) struct Unreliable {
    /// The origin the configuration would give, which the diagnostics around it name.
    pub(super) origin: String,
    pub(super) diagnostics: Diagnostics,
}

/// The origin through which a client on this host reaches the server that `server_args` and
/// `environment` configure, with the config file the environment selects: plain HTTP wherever
/// a listener accepts it, and a wildcard listen address reached on loopback.
pub(super) fn origin(
    server_args: &[String],
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<String, Unreliable> {
    let report = Config::preflight_os(server_args, environment);
    let origin = report.config.local_origin();
    if report.diagnostics.is_err() {
        Err(Unreliable {
            origin,
            diagnostics: report.diagnostics,
        })
    } else {
        Ok(origin)
    }
}

/// Runs `attempt` at `configured`, the server the configuration gives. When that fails, it runs
/// again at the server the options of the container's PID 1 give, which `locate` turns into the
/// attempt's target, unless they give the same one. A Compose `command` such as
/// `--scheme https` reaches only the server process, not the environment of a command run beside
/// it. Returns the answer of the attempt that answered; when the fallback is not tried, the
/// first failure.
pub(super) fn with_container_fallback<L: PartialEq, T, E>(
    configured: &L,
    attempt: impl FnMut(&L) -> Result<T, E>,
    locate: impl FnOnce(&[String]) -> Result<L, E>,
) -> Result<T, E> {
    // Only a failed attempt consults PID 1: Rosetta emulates /proc/<pid>/cmdline with a
    // temporary file, which crashes the command on a read-only root, so ordinary setups never
    // read it.
    let pid1 = || std::fs::read("/proc/1/cmdline").ok();
    fallback_from(configured, attempt, locate, pid1)
}

/// [`with_container_fallback`] with PID 1's command line read by `pid1`.
fn fallback_from<L: PartialEq, T, E>(
    configured: &L,
    mut attempt: impl FnMut(&L) -> Result<T, E>,
    locate: impl FnOnce(&[String]) -> Result<L, E>,
    pid1: impl FnOnce() -> Option<Vec<u8>>,
) -> Result<T, E> {
    let error = match attempt(configured) {
        Ok(answer) => return Ok(answer),
        Err(error) => error,
    };
    let Some(server_args) = pid1().and_then(|cmdline| container_server_args(&cmdline)) else {
        return Err(error);
    };
    let fallback = locate(&server_args)?;
    if fallback == *configured {
        return Err(error);
    }
    attempt(&fallback)
}

/// The server options in `cmdline`, the NUL-separated `/proc/1/cmdline` of a container's PID 1,
/// when that process is gcs-mimic running the server with at least one option, as in the
/// container image. The image's own command carries none, so the configuration already covered
/// it.
fn container_server_args(cmdline: &[u8]) -> Option<Vec<String>> {
    let cmdline = cmdline.strip_suffix(b"\0").unwrap_or(cmdline);
    let args = cmdline
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let (program, args) = args.split_first()?;
    if Path::new(program).file_name() != Some("gcs-mimic".as_ref()) {
        return None;
    }
    gcs_mimic::config_input::server_arguments(args)
        .filter(|options| !options.is_empty())
        .map(<[String]>::to_vec)
}

#[cfg(test)]
mod tests;
