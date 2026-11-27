//! Address policy shared by the command line and the server's outbound clients.

use std::net::IpAddr;

/// Whether the caller treats a final DNS root dot as part of the host it names.
#[derive(Clone, Copy)]
pub(crate) enum TrailingDot {
    Preserve,
    Remove,
}

pub(crate) fn normalized_host(host: &str, trailing_dot: TrailingDot) -> String {
    let host = match trailing_dot {
        TrailingDot::Preserve => host,
        TrailingDot::Remove => host.trim_suffix('.'),
    };
    host.to_ascii_lowercase()
}

/// Whether `ip` is a loopback or private-network address: IPv4 loopback or private ranges, IPv6
/// loopback or unique local, or an IPv4 one carried in IPv6. Local controls and emulator
/// endpoints accept only these, so a token or an event never leaves the host's network.
pub(crate) fn is_local_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || ip.is_loopback() || ip.is_unique_local(),
            |ip| is_local_address(IpAddr::V4(ip)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_normalization_only_removes_a_final_dot_when_requested() {
        for (host, preserved, removed) in [
            ("LOCALHOST", "localhost", "localhost"),
            ("Store.Example.", "store.example.", "store.example"),
            ("Store.Example..", "store.example..", "store.example."),
            ("[2001:DB8::1]", "[2001:db8::1]", "[2001:db8::1]"),
            (" Store.Example. ", " store.example. ", " store.example. "),
        ] {
            assert_eq!(normalized_host(host, TrailingDot::Preserve), preserved);
            assert_eq!(normalized_host(host, TrailingDot::Remove), removed);
        }
    }

    #[test]
    fn only_loopback_and_private_network_addresses_are_local() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_local_address(value.parse().unwrap()), "{value}");
        }
        for value in [
            "0.0.0.0",
            "169.254.169.254",
            "8.8.8.8",
            "::",
            "fe80::1",
            "ff02::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(!is_local_address(value.parse().unwrap()), "{value}");
        }
    }
}
