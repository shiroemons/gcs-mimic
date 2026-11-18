use super::*;

#[test]
fn unix_socket_configuration_has_no_implicit_tcp_or_tls_fallback() {
    let args = ["--unix-socket", "private/s", "--scheme", "http"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let result = Config::load(&args, &BTreeMap::new());
    if !cfg!(unix) {
        assert!(result.is_err());
        return;
    }
    let config = result.unwrap();
    assert_eq!(config.unix_socket.as_deref(), Some(Path::new("private/s")));
    assert!(config.grpc_listen.is_none());
    for (key, value) in [
        ("--listen", "127.0.0.1:0"),
        ("--listen-http", "127.0.0.1:0"),
        ("--grpc-listen", "127.0.0.1:0"),
        ("--tls-cert", "certificate.pem"),
    ] {
        let mut supplied = args.clone();
        supplied.extend([key.into(), value.into()]);
        assert!(Config::load(&supplied, &BTreeMap::new()).is_err(), "{key}");
    }
    // A valid strict pair still names a TCP listener, which the socket excludes.
    let mut trusted = args.clone();
    trusted.extend(
        [
            "--auth-mode",
            "strict",
            "--identity-file",
            "identities.json",
            "--trusted-listen",
            "127.0.0.1:0",
            "--trusted-principal",
            "app@example.invalid",
        ]
        .map(str::to_owned),
    );
    let error = Config::load(&trusted, &BTreeMap::new()).unwrap_err();
    assert!(
        error
            .errors
            .iter()
            .any(|error| error.key == "--unix-socket" && error.fix.contains("trusted_listen")),
        "{error}"
    );
    let env = BTreeMap::from([("GCS_MIMIC_LISTEN".into(), "127.0.0.1:0".into())]);
    assert!(Config::load(&args, &env).is_err());
}

/// A Unix socket excludes the gRPC listener as well, so a `grpc_listen` supplied beside it gives
/// the TLS settings no use: they are refused as unused, which also keeps startup from reading
/// the files, while the socket names the listener conflict.
#[test]
fn a_grpc_listener_beside_a_unix_socket_leaves_tls_settings_unused() {
    if !cfg!(unix) {
        return;
    }
    for scheme in ["auto", "http"] {
        let args = [
            "--unix-socket",
            "private/s",
            "--scheme",
            scheme,
            "--grpc-listen",
            "127.0.0.1:0",
            "--tls-cert",
            "certificate.pem",
            "--tls-key",
            "key.pem",
        ]
        .map(str::to_owned);
        let error = Config::load(&args, &BTreeMap::new()).unwrap_err();
        for key in ["--tls-cert", "--tls-key"] {
            assert!(
                error
                    .errors
                    .iter()
                    .any(|error| error.key == key && error.fix.contains("plain HTTP only")),
                "{scheme} {key}: {error}"
            );
        }
        assert!(
            error
                .errors
                .iter()
                .any(|error| error.key == "--unix-socket" && error.fix.contains("grpc_listen")),
            "{scheme}: {error}"
        );
    }
}

/// The default `auto` needs no scheme beside a Unix socket, which serves plain HTTP; only the
/// TLS-only schemes and TLS settings, which the socket would ignore, are refused.
#[test]
fn a_unix_socket_serves_plain_http_under_the_default_scheme() {
    let socket = ["--unix-socket", "private/s"].map(str::to_owned);
    let result = Config::load(&socket, &BTreeMap::new());
    if !cfg!(unix) {
        assert!(result.is_err());
        return;
    }
    assert_eq!(result.unwrap().scheme, Scheme::Auto);
    for (key, value, fix) in [
        ("--scheme", "https", "scheme=auto or http"),
        ("--scheme", "both", "scheme=auto or http"),
        ("--tls-cert", "certificate.pem", "plain HTTP only"),
        ("--tls-handshake-timeout-ms", "100", "plain HTTP only"),
        // The socket excludes the gRPC listener, which the scheme would configure.
        ("--grpc-scheme", "tls", "requires grpc_listen"),
    ] {
        let mut supplied = socket.to_vec();
        supplied.extend([key.into(), value.into()]);
        let error = Config::load(&supplied, &BTreeMap::new()).unwrap_err();
        assert!(
            error.errors.iter().any(|error| error.fix.contains(fix)),
            "{key} {value}: {error}"
        );
    }
}
