use super::*;
use std::cell::RefCell;

/// Every command that reaches the server reads a URL the same way: an explicit port, no user
/// information, query or fragment, and the host TLS verifies without IPv6 brackets.
#[test]
fn a_server_url_names_an_origin_with_a_port_and_nothing_else() {
    let (origin, path) = Origin::split("https://[::1]:4443/_internal/ready").unwrap();
    assert_eq!(
        origin,
        Origin {
            scheme: Scheme::Https,
            authority: "[::1]:4443".to_owned(),
            host: "::1".to_owned(),
        }
    );
    assert_eq!(path, "/_internal/ready");
    assert_eq!(origin.to_string(), "https://[::1]:4443");
    assert_eq!(
        "http://localhost:9000/".parse::<Origin>().unwrap().host,
        "localhost"
    );
    for url in [
        "http://localhost",
        "ftp://localhost:9000",
        "http://user@localhost:9000",
        "http://localhost:9000/?a=1",
        "http://localhost:9000/#a",
        "localhost:9000",
    ] {
        assert_eq!(Origin::split(url).unwrap_err(), InvalidOrigin, "{url}");
    }
    assert_eq!(
        "http://localhost:9000/_internal/seed".parse::<Origin>(),
        Err(InvalidOrigin)
    );
}

/// HTTPS trusts the given CA or else the development CA, and plain HTTP takes no CA at all.
#[test]
fn a_tls_client_verifies_https_and_plain_http_takes_no_ca() {
    let http = "http://127.0.0.1:1".parse::<Origin>().unwrap();
    let https = "https://127.0.0.1:1".parse::<Origin>().unwrap();
    assert!(http.tls_client(None).unwrap().is_none());
    assert!(matches!(
        http.tls_client(Some(Path::new("ca.pem"))),
        Err(Untrusted::CaCertOverHttp)
    ));
    assert!(https.tls_client(None).unwrap().is_some());
    let missing = std::env::temp_dir().join("gcs-mimic-no-such-ca.pem");
    assert!(matches!(
        https.tls_client(Some(&missing)),
        Err(Untrusted::Load(tls::TlsError::Io(error))) if error.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn only_a_gcs_mimic_server_with_options_gives_a_fallback() {
    let options = |args: &[&str]| Some(args.iter().map(|arg| (*arg).to_owned()).collect());
    for (cmdline, expected) in [
        (
            b"/usr/local/bin/gcs-mimic\0--scheme\0http\0".as_slice(),
            options(&["--scheme", "http"]),
        ),
        (
            b"gcs-mimic\0serve\0--listen\x000.0.0.0:9000\0",
            options(&["--listen", "0.0.0.0:9000"]),
        ),
        // The image's own command carries no option, so the first attempt already covered it.
        (b"/usr/local/bin/gcs-mimic\0serve\0", None),
        (b"/usr/local/bin/gcs-mimic\0healthcheck\0", None),
        (b"/bin/sh\0-c\0gcs-mimic --scheme http\0", None),
        (b"/usr/local/bin/gcs-mimic\0--scheme\0\xff\0", None),
        (b"", None),
    ] {
        assert_eq!(
            container_server_args(cmdline),
            expected,
            "{}",
            String::from_utf8_lossy(cmdline)
        );
    }
}

/// The configured origin of a container started with `--scheme https`, and the one its PID 1's
/// options give.
const CONFIGURED: &str = "http://127.0.0.1:4443";
const CONTAINER: &str = "https://127.0.0.1:4443";
const PID1: &[u8] = b"/usr/local/bin/gcs-mimic\0--scheme\0https\0";

/// The origin `server_args` give in an empty environment, as the attempt's target.
fn locate(server_args: &[String]) -> Result<String, String> {
    origin(server_args, []).map_err(|unreliable| unreliable.origin)
}

/// A failure reaches the server PID 1's options give, and the answer comes from there; an
/// unchanged target or no gcs-mimic server as PID 1 returns the first failure, and the second
/// attempt's failure is returned as its own.
#[test]
fn a_failure_tries_the_server_pid_1_gives_once() {
    let tried = RefCell::new(Vec::new());
    let attempt = |answers: &'static [Result<&'static str, &'static str>]| {
        let tried = &tried;
        let mut answers = answers.iter();
        move |target: &String| {
            tried.borrow_mut().push(target.clone());
            answers.next().copied().unwrap().map_err(str::to_owned)
        }
    };
    let pid1 = |cmdline: &'static [u8]| move || Some(cmdline.to_vec());

    let answered = fallback_from(
        &CONFIGURED.to_owned(),
        attempt(&[Err("plain HTTP to TLS"), Ok("ready")]),
        locate,
        pid1(PID1),
    );
    assert_eq!(answered, Ok("ready"));
    assert_eq!(tried.take(), [CONFIGURED, CONTAINER]);

    let refused = fallback_from(
        &CONFIGURED.to_owned(),
        attempt(&[Err("refused"), Err("refused again")]),
        locate,
        pid1(PID1),
    );
    assert_eq!(refused, Err("refused again".to_owned()));
    assert_eq!(tried.take(), [CONFIGURED, CONTAINER]);

    for cmdline in [
        b"/usr/local/bin/gcs-mimic\0--scheme\0http\0".as_slice(),
        b"/bin/sh\0",
    ] {
        let failed = fallback_from(
            &CONFIGURED.to_owned(),
            attempt(&[Err("first")]),
            locate,
            pid1(cmdline),
        );
        assert_eq!(failed, Err("first".to_owned()));
        assert_eq!(tried.take(), [CONFIGURED]);
    }
}
