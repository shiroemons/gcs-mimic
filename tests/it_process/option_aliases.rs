#[cfg(unix)]
use crate::common::CleanEnv;
use crate::common::{self, TestDir, process::Server};
use serde_json::Value;
use std::{
    fs,
    io::Read,
    net::TcpListener,
    process::{Output, Stdio},
    time::Duration,
};
#[cfg(unix)]
use std::{io::BufReader, net::TcpStream, path::Path, process::Command};

const EVENT_FLAGS: [(&str, &str); 5] = [
    ("event.pubsub-project-id", "local-project"),
    ("event.pubsub-topic", "uploads"),
    ("event.bucket", "notification-bucket"),
    ("event.object-prefix", "incoming/"),
    ("event.list", "finalize,delete,metadataUpdate,archive"),
];

fn records(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn rejected(output: Output) -> Vec<Value> {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    records(&output.stderr)
        .into_iter()
        .filter(|event| event["level"] == "error")
        .flat_map(|event| event["diagnostics"].as_array().unwrap().clone())
        .collect()
}

fn stop(server: &mut Server) -> Vec<Value> {
    records(stop_stderr(server).as_bytes())
}

fn stop_stderr(server: &mut Server) -> String {
    server.stop();
    let mut text = String::new();
    server
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[test]
fn refused_flags_report_every_original_key_before_startup() {
    for (prefix, equals) in [("-", false), ("--", false), ("-", true), ("--", true)] {
        for auth in ["permissive", "strict"] {
            for profile in ["native", "fake-gcs-server"] {
                let root = TestDir::new();
                let data = root.0.join("never-created");
                let mut command = common::server_command();
                command
                    .args([
                        "serve",
                        "--auth-mode",
                        auth,
                        "--compat-profile",
                        profile,
                        "--data-dir",
                    ])
                    .arg(&data);
                if auth == "strict" {
                    command.args(["--test-signer", "enabled"]);
                }
                let names = [
                    "filesystem-root",
                    "external-url",
                    "cors-headers",
                    "location",
                ];
                for name in names {
                    let key = format!("{prefix}{name}");
                    if equals {
                        command.arg(format!("{key}=secret-do-not-print\n"));
                    } else {
                        command.args([key.as_str(), "secret-do-not-print\n"]);
                    }
                }
                let errors = rejected(command.output().unwrap());
                assert!(!data.exists());
                assert_eq!(errors.len(), names.len(), "{errors:?}");
                for (name, replacement) in names.into_iter().zip([
                    "gcs-mimic import fake-gcs-server",
                    "--api-base-url",
                    "--cors-fallback allow-all",
                    "--default-location",
                ]) {
                    let error = errors
                        .iter()
                        .find(|error| error["key"] == format!("{prefix}{name}"))
                        .unwrap();
                    assert_eq!(error["source"], "cli");
                    assert!(
                        error["fix"].as_str().unwrap().contains(replacement),
                        "{error}"
                    );
                    assert!(
                        !error.to_string().contains("secret-do-not-print"),
                        "{error}"
                    );
                    if name == "external-url" {
                        let fix = error["fix"].as_str().unwrap();
                        assert!(fix.contains("selfLink/mediaLink"), "{fix}");
                        assert!(
                            fix.contains("resumable upload Location follows the request address"),
                            "{fix}"
                        );
                        assert!(
                            fix.contains("choose an address every client can reach"),
                            "{fix}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn native_listener_flags_start_both_listeners_and_reject_unused_http_port() {
    for (prefix, equals) in [("-", false), ("--", false), ("-", true), ("--", true)] {
        let mut command = common::server_command();
        command.args(["--backend", "memory"]).stderr(Stdio::piped());
        for (name, value) in [
            ("scheme", "both"),
            ("host", "localhost"),
            ("port", "0"),
            ("port-http", "0"),
            ("log-level", "info"),
        ] {
            if equals {
                command.arg(format!("{prefix}{name}={value}"));
            } else {
                command.args([format!("{prefix}{name}"), value.to_owned()]);
            }
        }
        let mut server = Server::launch(&mut command);
        assert_eq!(server.ready["scheme"], "both");
        let listeners = server.ready["listeners"].as_array().unwrap();
        assert_eq!(listeners.len(), 2);
        for listener in listeners {
            let listen = listener["listen"].as_str().unwrap();
            assert!(listen.starts_with("127.0.0.1:"), "{listener}");
            let url = format!(
                "{}://{listen}/_internal/healthcheck",
                listener["scheme"].as_str().unwrap()
            );
            let result = common::server_command()
                .args(["healthcheck", "--url", &url])
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
        }
        assert!(
            !stop(&mut server)
                .iter()
                .any(|event| event["event"] == "diagnostics")
        );
    }
    for scheme in [None, Some("http"), Some("https")] {
        let mut command = common::server_command();
        command.args(["serve", "--backend", "memory", "--port-http", "0"]);
        if let Some(scheme) = scheme {
            command.args(["--scheme", scheme]);
        }
        let errors = rejected(command.output().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0]["key"], "--port-http");
        assert!(errors[0]["fix"].as_str().unwrap().contains("--scheme both"));
        assert!(
            errors[0]["fix"]
                .as_str()
                .unwrap()
                .contains("remove --port-http")
        );
    }
}

#[test]
fn empty_listen_environment_keeps_defaults_when_a_cli_part_overrides_it() {
    for (flag, value) in [("--port", "0"), ("--host", "127.0.0.1")] {
        let root = TestDir::new();
        let config = root.0.join("listen.json");
        fs::write(&config, r#"{"listen":"127.0.0.1:0"}"#).unwrap();
        let mut command = common::server_command();
        command
            .args(["serve", "--backend", "memory", flag, value])
            .arg("--config")
            .arg(config)
            .env("GCS_MIMIC_LISTEN", "");
        let mut server = Server::launch(&mut command);
        assert!(server.address.starts_with("127.0.0.1:"));
        server.stop();
    }
}

#[test]
fn public_host_accepts_dns_and_ip_origins_and_preserves_explicit_api_conflict_errors() {
    let root = TestDir::new();
    fs::create_dir_all(root.0.join("fixture-bucket")).unwrap();
    fs::write(root.0.join("fixture-bucket/hello.txt"), b"fixture bytes").unwrap();
    for profile in ["native", "fake-gcs-server"] {
        for host in ["storage.invalid:9000", "127.0.0.1:9000", "[::1]:9000"] {
            for flag in ["-public-host", "--public-host"] {
                let mut command = common::server_command();
                command
                    .args([
                        "serve",
                        "--scheme",
                        "http",
                        "--port",
                        "0",
                        "--backend",
                        "memory",
                        "--compat-profile",
                        profile,
                        flag,
                        host,
                    ])
                    .arg("--seed-dir")
                    .arg(&root.0)
                    .stderr(Stdio::piped());
                let mut server = Server::launch(&mut command);
                assert_eq!(server.ready["public_base_url"], format!("http://{host}"));
                let mut client = common::http::Client {
                    address: &server.address,
                    host,
                    headers: "",
                    timeout: Duration::from_secs(10),
                };
                let reply = client.request("GET", "/fixture-bucket/hello.txt", "", b"");
                assert_eq!(reply.status, 200, "{:?}", reply.body);
                assert_eq!(reply.body, b"fixture bytes");
                if host == "storage.invalid:9000" {
                    client.host = "fixture-bucket.storage.invalid:9000";
                    let reply = client.request("GET", "/hello.txt", "", b"");
                    assert_eq!(reply.status, 200, "{:?}", reply.body);
                    assert_eq!(reply.body, b"fixture bytes");
                }
                let events = stop(&mut server);
                let info = events
                    .iter()
                    .find(|event| event["level"] == "info" && event["event"] == "diagnostics")
                    .unwrap();
                let fix = info["diagnostics"][0]["fix"].as_str().unwrap();
                assert!(fix.contains("call the JSON API through a host outside those subdomains or set --api-base-url explicitly"), "{fix}");
                assert!(
                    fix.contains(&format!("--public-base-url 'http://{host}'")),
                    "{fix}"
                );
            }
        }
    }
    let output = common::server_command()
        .args([
            "serve",
            "--backend",
            "memory",
            "--public-host",
            "storage.invalid",
            "--api-base-url",
            "https://api.storage.invalid",
        ])
        .output()
        .unwrap();
    let errors = rejected(output);
    assert!(
        errors.iter().any(|error| error["key"] == "--api-base-url"
            && error["fix"]
                .as_str()
                .unwrap()
                .contains("host selects an XML virtual bucket")),
        "{errors:?}"
    );
}

#[test]
fn invalid_native_and_translated_flag_values_keep_the_original_key() {
    for (flag, value) in [
        ("port", "invalid"),
        ("port-http", "65536"),
        ("host", "invalid"),
        ("public-host", "user:secret@host"),
        ("cert-location", ""),
        ("private-key-location", ""),
    ] {
        for (prefix, equals) in [("-", false), ("--", false), ("-", true), ("--", true)] {
            let root = TestDir::new();
            let data = root.0.join("never-created");
            let key = format!("{prefix}{flag}");
            let mut command = common::server_command();
            command
                .args(["serve", "--scheme", "both", "--data-dir"])
                .arg(&data);
            if equals {
                command.arg(format!("{key}={value}"));
            } else {
                command.args([key.as_str(), value]);
            }
            let output = command.output().unwrap();
            assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
            let errors = rejected(output);
            assert!(
                errors
                    .iter()
                    .any(|error| error["source"] == "cli" && error["key"] == key),
                "{errors:?}"
            );
            assert!(!data.exists());
        }
    }
}

#[test]
fn event_aliases_require_an_effective_local_emulator_host() {
    let flags: Vec<_> = EVENT_FLAGS
        .iter()
        .flat_map(|(flag, value)| [format!("--{flag}"), (*value).into()])
        .collect();
    let root = TestDir::new();
    let config = root.0.join("config.json");
    let local = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = local.local_addr().unwrap().to_string();
    fs::write(
        &config,
        serde_json::json!({"pubsub_emulator_host": endpoint}).to_string(),
    )
    .unwrap();
    for source in ["standard", "native", "json", "cli"] {
        let mut command = common::server_command();
        command
            .args(["serve", "--backend", "memory", "--port", "0"])
            .args(&flags)
            .stderr(Stdio::piped());
        match source {
            "standard" => {
                command.env("PUBSUB_EMULATOR_HOST", &endpoint);
            }
            "native" => {
                command.env("GCS_MIMIC_PUBSUB_EMULATOR_HOST", &endpoint);
            }
            "json" => {
                command.arg("--config").arg(&config);
            }
            "cli" => {
                command.args(["--pubsub-emulator-host", &endpoint]);
            }
            _ => unreachable!(),
        }
        let mut server = Server::launch(&mut command);
        assert_eq!(server.ready["scheme"], "auto");
        let events = stop(&mut server);
        let diagnostics: Vec<_> = events
            .iter()
            .filter(|event| event["event"] == "diagnostics")
            .collect();
        assert_eq!(diagnostics.len(), 1, "{events:?}");
        assert_eq!(diagnostics[0]["level"], "info");
        for (flag, _) in EVENT_FLAGS {
            assert!(
                diagnostics[0]["diagnostics"][0]["fix"]
                    .as_str()
                    .unwrap()
                    .contains(&flag.replace('.', "-"))
            );
        }
    }
    for clearing in ["absent", "standard-empty", "native-empty", "cli-empty"] {
        let mut command = common::server_command();
        command.args(["serve", "--backend", "memory"]).args(&flags);
        match clearing {
            "absent" => {}
            "standard-empty" => {
                command.env("PUBSUB_EMULATOR_HOST", "");
            }
            "native-empty" => {
                command
                    .env("PUBSUB_EMULATOR_HOST", &endpoint)
                    .env("GCS_MIMIC_PUBSUB_EMULATOR_HOST", "")
                    .arg("--config")
                    .arg(&config);
            }
            "cli-empty" => {
                command
                    .env("PUBSUB_EMULATOR_HOST", &endpoint)
                    .args(["--pubsub-emulator-host", ""]);
            }
            _ => unreachable!(),
        }
        let errors = rejected(command.output().unwrap());
        assert_eq!(errors.len(), EVENT_FLAGS.len(), "{clearing}: {errors:?}");
        for (flag, _) in EVENT_FLAGS {
            let error = errors
                .iter()
                .find(|error| error["key"] == format!("--{flag}"))
                .unwrap();
            assert_eq!(error["source"], "cli");
            assert!(
                error["fix"]
                    .as_str()
                    .unwrap()
                    .contains("gcs-mimic never publishes to Google")
            );
        }
    }
}

#[test]
fn alias_values_that_have_no_source_meaning_or_invalid_tls_stop_startup() {
    let output = common::server_command()
        .args([
            "serve",
            "--pubsub-emulator-host",
            "127.0.0.1:8085",
            "--event.list",
            "none",
        ])
        .output()
        .unwrap();
    let errors = rejected(output);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["key"], "--event.list");
    assert!(
        errors[0]["fix"]
            .as_str()
            .unwrap()
            .contains("--event-list none")
    );

    let root = TestDir::new();
    let cert = root.0.join("secret-cert.pem");
    let key = root.0.join("secret-key.pem");
    fs::write(&cert, "invalid-secret-certificate").unwrap();
    fs::write(&key, "invalid-secret-key").unwrap();
    let output = common::server_command()
        .args(["serve", "-cert-location"])
        .arg(cert)
        .arg("-private-key-location")
        .arg(key)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(!text.contains("invalid-secret-certificate"));
    assert!(!text.contains("invalid-secret-key"));
    let errors = rejected(output);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["source"], "tls");
    assert_eq!(errors[0]["key"], "tls_key");
}

#[cfg(unix)]
#[test]
fn accepted_native_args_round_trip_through_tls_and_notifications() {
    let root = TestDir::new();
    let seed = root.0.join("seed with spaces and ' $HOME");
    fs::create_dir_all(seed.join("fixture-bucket")).unwrap();
    fs::create_dir_all(seed.join("notification-bucket")).unwrap();
    fs::create_dir_all(seed.join("ignored-bucket")).unwrap();
    fs::write(seed.join("fixture-bucket/hello.txt"), "fixture bytes").unwrap();
    let tls = root.0.join("tls with spaces and ' $HOME");
    assert!(
        Command::new("bash")
            .arg("scripts/tls-fixtures.sh")
            .arg(&tls)
            .status()
            .unwrap()
            .success()
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let base = [
        "serve",
        "--scheme",
        "both",
        "--port",
        "0",
        "--port-http",
        "0",
        "--pubsub-emulator-host",
        &endpoint,
        "--api-base-url",
        "https://api.invalid:4443",
        "--data-dir",
    ];
    let mut command = common::server_command();
    command
        .args(base)
        .arg(root.0.join("data"))
        .args([
            "-backend",
            "filesystem",
            "-public-host",
            "storage.invalid:4443",
            "-cert-location",
        ])
        .arg(tls.join("server.pem"))
        .arg("-private-key-location")
        .arg(tls.join("server.key"))
        .arg("-data")
        .arg(&seed)
        .stderr(Stdio::piped());
    for (flag, value) in EVENT_FLAGS {
        let value = match flag {
            "event.list" => "finalize",
            "event.object-prefix" => "incoming/'$HOME/",
            _ => value,
        };
        command.args([format!("-{flag}"), value.into()]);
    }
    let mut server = Server::launch(&mut command);
    let ready = server.ready.clone();
    let delivered = translated_runtime(&server, &listener, &tls.join("ca.pem"));
    let events = stop(&mut server);
    let diagnostics: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "diagnostics")
        .collect();
    assert_eq!(diagnostics.len(), 1, "{events:?}");
    assert_eq!(diagnostics[0]["level"], "info");
    assert_eq!(diagnostics[0]["diagnostics"].as_array().unwrap().len(), 1);
    let text = diagnostics[0]["diagnostics"][0]["fix"].as_str().unwrap();
    assert!(!text.contains("<REDACTED>"), "{text}");
    assert!(!text.contains("Use native options:"), "{text}");
    assert!(text.contains("native_args"), "{text}");
    let native_args: Vec<_> = diagnostics[0]["diagnostics"][0]["native_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(native_args.len(), 20);
    for (key, value) in [
        ("--seed-dir", seed.to_str().unwrap()),
        ("--event-object-prefix", "incoming/'$HOME/"),
        ("--tls-cert", tls.join("server.pem").to_str().unwrap()),
        ("--tls-key", tls.join("server.key").to_str().unwrap()),
    ] {
        assert!(native_args.as_chunks::<2>().0.contains(&[key, value]));
    }
    let mut pasted = common::server_command();
    pasted
        .args(base)
        .arg(root.0.join("fresh-data"))
        .args(native_args)
        .stderr(Stdio::piped());
    let mut server = Server::launch(&mut pasted);
    for field in [
        "scheme",
        "backend",
        "api_base_url",
        "public_base_url",
        "cors_fallback",
    ] {
        assert_eq!(server.ready[field], ready[field], "{field}");
    }
    assert_eq!(
        translated_runtime(&server, &listener, &tls.join("ca.pem")),
        delivered
    );
    assert!(
        !stop(&mut server)
            .iter()
            .any(|event| event["event"] == "diagnostics")
    );
}

#[cfg(unix)]
#[test]
fn safe_native_line_round_trips_from_raw_stderr() {
    let root = TestDir::new();
    let seed = "seed with spaces $HOME ; &";
    fs::create_dir_all(root.0.join(seed).join("fixture-bucket")).unwrap();
    fs::write(
        root.0.join(seed).join("fixture-bucket/hello.txt"),
        b"fixture bytes",
    )
    .unwrap();
    let base = [
        "serve",
        "--scheme",
        "http",
        "--port",
        "0",
        "--backend",
        "memory",
    ];
    let mut command = common::server_command();
    command
        .current_dir(&root.0)
        .args(base)
        .args(["-data", seed, "-public-host", "storage.invalid"])
        .stderr(Stdio::piped());
    let mut server = Server::launch(&mut command);
    let ready = server.ready.clone();
    let raw = stop_stderr(&mut server);
    // Slice the emitted JSON bytes without decoding its string escapes.
    let (_, remainder) = raw.split_once("Use native options: ").unwrap();
    let (block, _) = remainder.split_once('"').unwrap();
    assert!(!block.contains(['\\', '\n', '\t']));
    let mut pasted = Command::new("sh");
    pasted
        .clean_env()
        .current_dir(&root.0)
        .args([
            "-c",
            &format!("exec \"$@\" {block}"),
            "native-equivalent",
            env!("CARGO_BIN_EXE_gcs-mimic"),
        ])
        .args(base)
        .stderr(Stdio::piped());
    let mut server = Server::launch(&mut pasted);
    assert_eq!(server.ready["public_base_url"], ready["public_base_url"]);
    let reply =
        common::effects::request(&server.address, "GET", "/fixture-bucket/hello.txt", "", b"");
    reply.expect(200);
    assert_eq!(reply.body, b"fixture bytes");
    assert!(
        !stop(&mut server)
            .iter()
            .any(|event| event["event"] == "diagnostics")
    );
}

#[cfg(unix)]
#[test]
fn json_escaped_values_use_exact_native_args_without_a_pasteable_line() {
    let root = TestDir::new();
    for value in [
        "it's",
        "say \"hi\"",
        "back\\slash",
        "line\nbreak",
        "tab\tvalue",
    ] {
        fs::create_dir_all(root.0.join(value).join("fixture-bucket")).unwrap();
        let mut command = common::server_command();
        command
            .current_dir(&root.0)
            .args([
                "serve",
                "--backend",
                "memory",
                "--port",
                "0",
                "-data",
                value,
            ])
            .stderr(Stdio::piped());
        let mut server = Server::launch(&mut command);
        let raw = stop_stderr(&mut server);
        assert!(!raw.contains("Use native options:"), "{raw}");
        let events = records(raw.as_bytes());
        let info = events
            .iter()
            .find(|event| event["event"] == "diagnostics")
            .unwrap();
        assert_eq!(
            info["diagnostics"][0]["native_args"],
            serde_json::json!(["--seed-dir", value])
        );
        assert!(
            info["diagnostics"][0]["fix"]
                .as_str()
                .unwrap()
                .contains("decode it with jq")
        );
    }
}

#[cfg(unix)]
fn translated_runtime(server: &Server, publisher: &TcpListener, ca: &Path) -> (String, Value) {
    use tokio_rustls::rustls::{ClientConnection, StreamOwned, pki_types::ServerName};

    let socket = TcpStream::connect(&server.address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let connection = ClientConnection::new(
        gcs_mimic::tls::client_config(ca).unwrap(),
        ServerName::try_from("localhost").unwrap(),
    )
    .unwrap();
    let mut tls = StreamOwned::new(connection, socket);
    common::process::write_request(
        &mut tls,
        "GET",
        "/fixture-bucket/hello.txt",
        "localhost",
        "Connection: close\r\n",
        b"",
    );
    let mut reader = BufReader::new(tls);
    let mut reply = common::http::Reply::read_head(&mut reader);
    reply.read_body(&mut reader);
    reply.expect(200);
    assert_eq!(reply.body, b"fixture bytes");

    let address = server.ready["listeners"][1]["listen"].as_str().unwrap();
    for (bucket, name) in [
        ("ignored-bucket", "incoming%2F%27%24HOME%2Fitem"),
        ("notification-bucket", "unmatched"),
    ] {
        common::effects::request(
            address,
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
            "",
            b"ignored",
        )
        .json_ok();
    }
    let object = common::effects::request(address, "POST", "/upload/storage/v1/b/notification-bucket/o?uploadType=media&name=incoming%2F%27%24HOME%2Fitem", "", b"published").json_ok();
    let (path, message) = common::notifications::accept_local_publish(publisher);
    assert_eq!(
        path,
        "POST /v1/projects/local-project/topics/uploads:publish HTTP/1.1\r\n"
    );
    assert_eq!(message["attributes"]["eventType"], "OBJECT_FINALIZE");
    assert_eq!(message["attributes"]["bucketId"], "notification-bucket");
    assert_eq!(message["attributes"]["objectId"], "incoming/'$HOME/item");
    assert_eq!(message["attributes"]["payloadFormat"], "JSON_API_V1");
    assert_eq!(
        message["attributes"]["objectGeneration"],
        object["generation"]
    );
    assert!(
        message["data"]
            .as_str()
            .is_some_and(|data| !data.is_empty())
    );
    (
        path,
        serde_json::json!({
            "eventType": message["attributes"]["eventType"],
            "bucketId": message["attributes"]["bucketId"],
            "objectId": message["attributes"]["objectId"],
            "payloadFormat": message["attributes"]["payloadFormat"],
        }),
    )
}
