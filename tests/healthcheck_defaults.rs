mod common;

use common::{CleanEnv, TestDir};
use gcs_mimic::config::{Config, Scheme};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

#[test]
fn healthcheck_default_url_maps_wildcards_and_ignores_advertised_origin() {
    for (listen, expected) in [
        ("0.0.0.0:4443", "https://127.0.0.1:4443/_internal/ready"),
        ("[::]:8443", "https://[::1]:8443/_internal/ready"),
    ] {
        let mut config = Config {
            listen: listen.parse().unwrap(),
            api_base_url: Some("https://advertised.invalid".into()),
            ..Config::default()
        };
        assert_eq!(config.healthcheck_url(), expected);
        config.scheme = Scheme::Both;
        config.listen_http = Some("[::]:8080".parse().unwrap());
        assert_eq!(
            config.healthcheck_url(),
            "http://[::1]:8080/_internal/ready"
        );
        config.listen_http = None;
        assert_eq!(
            config.healthcheck_url(),
            if config.listen.is_ipv4() {
                "http://127.0.0.1:8000/_internal/ready"
            } else {
                "http://[::1]:8000/_internal/ready"
            }
        );
    }
}

#[test]
fn healthcheck_defaults_follow_http_https_both_and_changed_ports() {
    for (scheme, wildcard) in [
        ("http", false),
        ("https", false),
        ("both", true),
        ("http", true),
    ] {
        let dir = TestDir::new();
        let path = dir.0.join("server.json");
        let config = json!({"scheme": scheme, "listen": if wildcard {"0.0.0.0:0"} else {"127.0.0.1:0"}, "data_dir": dir.0.join("data")});
        let mut config = config.as_object().unwrap().clone();
        if scheme == "both" {
            config.insert("listen_http".into(), "0.0.0.0:0".into());
        }
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let mut child = common::server_command()
            .arg("serve")
            .env("GCS_MIMIC_CONFIG", &path)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready", "{ready}");
        let listen = ready["listen"].as_str().unwrap();
        config.insert("listen".into(), listen.into());
        if scheme == "both" {
            config.insert(
                "listen_http".into(),
                ready["listeners"][1]["listen"].clone(),
            );
        }
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let probe = || {
            let mut command = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"));
            command
                .clean_env()
                .arg("healthcheck")
                .env("GCS_MIMIC_CONFIG", &path);
            command
        };
        let output = probe().output().unwrap();
        assert!(output.status.success(), "{scheme}: {output:?}");
        let address: std::net::SocketAddr = if scheme == "both" {
            ready["listeners"][1]["listen"].as_str().unwrap()
        } else {
            listen
        }
        .parse()
        .unwrap();
        let address =
            std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), address.port());
        let url = format!(
            "{}://{address}/_internal/ready",
            if scheme == "https" { "https" } else { "http" }
        );
        let output = probe()
            .env("GCS_MIMIC_CONFIG", dir.0.join("missing.json"))
            .env("GCS_MIMIC_LISTEN", "127.0.0.1:1")
            .args(["--url", &url])
            .output()
            .unwrap();
        assert!(output.status.success(), "explicit URL: {output:?}");
        config.insert("scheme".into(), "https".into());
        config.remove("listen_http");
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let output = probe()
            .env(
                "GCS_MIMIC_SCHEME",
                if scheme == "https" { "https" } else { "http" },
            )
            .env("GCS_MIMIC_LISTEN", address.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "environment precedence: {output:?}"
        );
        common::stop_server(&mut child, &mut stdout);
        let output = probe()
            .env(
                "GCS_MIMIC_SCHEME",
                if scheme == "https" { "https" } else { "http" },
            )
            .env("GCS_MIMIC_LISTEN", address.to_string())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(&url), "{error}");
        assert!(error.contains(&format!("--url {url}")), "{error}");
        assert_eq!(
            error.contains("--ca-cert ./tls/ca.pem"),
            scheme == "https",
            "{error}"
        );
    }
}

#[test]
fn healthcheck_config_failure_explains_candidate_url_and_explicit_override() {
    let dir = TestDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .clean_env()
        .arg("healthcheck")
        .env("GCS_MIMIC_CONFIG", dir.0.join("missing.json"))
        .env("GCS_MIMIC_SCHEME", "http")
        .env("GCS_MIMIC_LISTEN", "0.0.0.0:8080")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("http://127.0.0.1:8080/_internal/ready"),
        "{error}"
    );
    assert!(
        error.contains("--url http://127.0.0.1:8080/_internal/ready"),
        "{error}"
    );
}
