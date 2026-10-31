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
    let env = BTreeMap::from([("GCS_MIMIC_LISTEN".into(), "127.0.0.1:0".into())]);
    assert!(Config::load(&args, &env).is_err());
    assert!(
        Config::load(
            &["--unix-socket".into(), "private/s".into()],
            &BTreeMap::new()
        )
        .is_err()
    );
}
