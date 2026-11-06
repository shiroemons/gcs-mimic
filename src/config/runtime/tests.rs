use super::*;
use std::collections::BTreeMap;

fn config() -> Config {
    Config::load(
        &[
            "--scheme".into(),
            "http".into(),
            "--public-base-url".into(),
            "http://old.invalid".into(),
            "--xml-virtual-host-domains".into(),
            "explicit.invalid".into(),
        ],
        &BTreeMap::new(),
    )
    .unwrap()
}

#[test]
fn endpoint_updates_keep_in_flight_snapshots_and_replace_only_the_implicit_public_domain() {
    let config = config();
    let runtime = &config.runtime_endpoints;
    let old = runtime.snapshot(&config, "http://initial.invalid").unwrap();
    runtime
        .update(
            &config,
            "http://initial.invalid",
            Some("http://mapped.invalid:9876/proxy/"),
            Some("new.invalid:9876"),
        )
        .unwrap();
    let new = runtime.snapshot(&config, "http://initial.invalid").unwrap();
    assert_eq!(old.fixed_api_base.url, "http://initial.invalid");
    assert_eq!(
        old.public_base_url.as_ref().unwrap().authority().as_str(),
        "old.invalid"
    );
    assert_eq!(new.fixed_api_base.url, "http://mapped.invalid:9876/proxy");
    assert_eq!(
        new.public_base_url.as_ref().unwrap().authority().as_str(),
        "new.invalid:9876"
    );
    assert_eq!(
        new.xml_virtual_host_domains,
        ["explicit.invalid", "new.invalid"]
    );
    assert!(Arc::ptr_eq(
        &new,
        &runtime.snapshot(&config, "ignored").unwrap()
    ));
}

#[test]
fn invalid_endpoint_pairs_leave_both_values_unchanged() {
    let config = config();
    let runtime = &config.runtime_endpoints;
    let before = runtime.snapshot(&config, "http://initial.invalid").unwrap();
    for (external, public) in [
        ("https://valid.invalid/proxy", "user@public.invalid"),
        ("https://valid.invalid/proxy", "public.invalid/path"),
        ("https://user:password@api.invalid", "public.invalid"),
        ("https://api.invalid:0", "public.invalid"),
        ("https://api.invalid?token=secret", "public.invalid"),
        ("https://bucket.public.invalid", "public.invalid"),
    ] {
        assert!(
            runtime
                .update(
                    &config,
                    "http://initial.invalid",
                    Some(external),
                    Some(public)
                )
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &before,
            &runtime.snapshot(&config, "ignored").unwrap()
        ));
    }
}

#[test]
fn concurrent_partial_endpoint_updates_do_not_lose_the_other_field() {
    let config = config();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            config
                .runtime_endpoints
                .update(
                    &config,
                    "http://initial.invalid",
                    Some("http://mapped.invalid:9876"),
                    None,
                )
                .unwrap()
        });
        scope.spawn(|| {
            config
                .runtime_endpoints
                .update(
                    &config,
                    "http://initial.invalid",
                    None,
                    Some("public.invalid:9876"),
                )
                .unwrap()
        });
    });
    let snapshot = config
        .runtime_endpoints
        .snapshot(&config, "ignored")
        .unwrap();
    assert_eq!(snapshot.fixed_api_base.url, "http://mapped.invalid:9876");
    assert_eq!(
        snapshot
            .public_base_url
            .as_ref()
            .unwrap()
            .authority()
            .as_str(),
        "public.invalid:9876"
    );
}

#[test]
fn endpoint_domain_limit_rejects_without_publishing_and_preserves_explicit_public_domain() {
    let domains = (0..32)
        .map(|index| format!("domain{index}.invalid"))
        .collect::<Vec<_>>()
        .join(",");
    let config = Config::load(
        &[
            "--scheme".into(),
            "http".into(),
            "--public-base-url".into(),
            "http://domain0.invalid".into(),
            "--xml-virtual-host-domains".into(),
            domains,
        ],
        &BTreeMap::new(),
    )
    .unwrap();
    let initial = config
        .runtime_endpoints
        .snapshot(&config, "http://initial.invalid")
        .unwrap();
    assert!(matches!(
        config
            .runtime_endpoints
            .update(&config, "ignored", None, Some("extra.invalid")),
        Err(EndpointError::DomainLimit)
    ));
    assert!(Arc::ptr_eq(
        &initial,
        &config
            .runtime_endpoints
            .snapshot(&config, "ignored")
            .unwrap()
    ));
    config
        .runtime_endpoints
        .update(&config, "ignored", None, Some("domain1.invalid"))
        .unwrap();
    let snapshot = config
        .runtime_endpoints
        .snapshot(&config, "ignored")
        .unwrap();
    assert!(
        snapshot
            .xml_virtual_host_domains
            .iter()
            .any(|domain| domain == "domain0.invalid")
    );
}

const FIXED: &str = "http://initial.invalid";

fn host_headers(hosts: &[&[u8]]) -> hyper::HeaderMap {
    let mut headers = hyper::HeaderMap::new();
    for host in hosts {
        headers.append(
            "host",
            hyper::header::HeaderValue::from_bytes(host).unwrap(),
        );
    }
    headers
}

fn advertised(config: &Config, scheme: ConnectionScheme, uri: &str, hosts: &[&[u8]]) -> String {
    config
        .runtime_endpoints
        .snapshot(config, FIXED)
        .unwrap()
        .for_request(scheme, &uri.parse().unwrap(), &host_headers(hosts))
        .api_base_url()
        .to_owned()
}

#[test]
fn request_host_sets_the_advertised_base_and_never_the_fixed_one() {
    let config = config();
    for (host, expected) in [
        ("gcs:4443", "http://gcs:4443"),
        ("localhost:54321", "http://localhost:54321"),
        ("gcs", "http://gcs"),
        ("my_gcs:4443", "http://my_gcs:4443"),
        ("GCS.Example:4443", "http://GCS.Example:4443"),
        ("127.0.0.1:4443", "http://127.0.0.1:4443"),
        ("[::1]:4443", "http://[::1]:4443"),
        ("[::1]", "http://[::1]"),
        // A configured base domain addresses the API; only its subdomains name buckets.
        ("explicit.invalid:4443", "http://explicit.invalid:4443"),
        ("EXPLICIT.Invalid:4443", "http://EXPLICIT.Invalid:4443"),
        // A trailing dot routes path-style, so this host selects no bucket.
        (
            "bucket.explicit.invalid.:4443",
            "http://bucket.explicit.invalid.:4443",
        ),
    ] {
        assert_eq!(
            advertised(
                &config,
                ConnectionScheme::Http,
                "/storage/v1/b",
                &[host.as_bytes()]
            ),
            expected,
            "{host}"
        );
    }
    let view = config
        .runtime_endpoints
        .snapshot(&config, FIXED)
        .unwrap()
        .for_request(
            ConnectionScheme::Http,
            &"/storage/v1/b".parse().unwrap(),
            &host_headers(&[b"evil.example.com"]),
        );
    assert_eq!(view.api_base_url(), "http://evil.example.com");
    assert_eq!(view.api_authority().as_str(), "evil.example.com");
    assert_eq!(
        view.snapshot().fixed_api_authority().as_str(),
        "initial.invalid"
    );
}

#[test]
fn a_virtual_host_advertises_its_configured_domain_and_never_the_bucket() {
    let config = config();
    for (host, expected) in [
        (
            "bucket.explicit.invalid:4443",
            "http://explicit.invalid:4443",
        ),
        ("bucket.explicit.invalid", "http://explicit.invalid"),
        (
            "BUCKET.Explicit.Invalid:4443",
            "http://explicit.invalid:4443",
        ),
        ("a.b.explicit.invalid:80", "http://explicit.invalid:80"),
        // The public base URL's domain selects buckets too, on whatever port reached the server.
        ("bucket.old.invalid:8080", "http://old.invalid:8080"),
    ] {
        assert_eq!(
            advertised(&config, ConnectionScheme::Http, "/o", &[host.as_bytes()]),
            expected,
            "{host}"
        );
    }
    assert_eq!(
        advertised(
            &config,
            ConnectionScheme::Https,
            "/o",
            &[b"bucket.explicit.invalid:4443"]
        ),
        "https://explicit.invalid:4443",
        "the scheme follows the connection"
    );
    // Only this request's view changes, so the management guard's fixed base stays out of reach.
    let view = config
        .runtime_endpoints
        .snapshot(&config, FIXED)
        .unwrap()
        .for_request(
            ConnectionScheme::Http,
            &"/o".parse().unwrap(),
            &host_headers(&[b"bucket.explicit.invalid:4443"]),
        );
    assert_eq!(view.api_authority().as_str(), "explicit.invalid:4443");
    assert_eq!(
        view.snapshot().fixed_api_authority().as_str(),
        "initial.invalid"
    );
}

#[test]
fn the_most_specific_domain_names_the_api_behind_a_virtual_host() {
    let config = Config::load(
        &[
            "--scheme".into(),
            "http".into(),
            "--xml-virtual-host-domains".into(),
            "storage.invalid,eu.storage.invalid".into(),
        ],
        &BTreeMap::new(),
    )
    .unwrap();
    for (host, expected) in [
        ("bucket.eu.storage.invalid:1", "http://eu.storage.invalid:1"),
        ("bucket.storage.invalid:1", "http://storage.invalid:1"),
        // The derived base selects no bucket itself, so a client can follow it.
        ("eu.storage.invalid:1", "http://eu.storage.invalid:1"),
    ] {
        assert_eq!(
            advertised(&config, ConnectionScheme::Http, "/o", &[host.as_bytes()]),
            expected,
            "{host}"
        );
    }
}

#[test]
fn a_runtime_public_host_domain_names_the_api_behind_its_virtual_hosts() {
    let config = config();
    config
        .runtime_endpoints
        .update(&config, FIXED, None, Some("new.invalid:9876"))
        .unwrap();
    assert_eq!(
        advertised(
            &config,
            ConnectionScheme::Http,
            "/o",
            &[b"bucket.new.invalid:4443"]
        ),
        "http://new.invalid:4443"
    );
}

#[test]
fn unusable_request_authorities_keep_the_fixed_base() {
    let config = config();
    for host in [
        b"".as_slice(),
        b" ",
        b"a@b",
        b"user:secret@gcs:4443",
        b"gcs/path",
        b"gcs:4443/path",
        b"gcs?query",
        b"gcs#fragment",
        b"gcs\tinjected",
        b"gc\xffs",
        b"gcs:",
        b"gcs:name",
        b"gcs:+4443",
        b"gcs:04443",
        b"gcs:99999",
        b"gcs:0",
        b":4443",
        b"a&b",
        b"a'b",
        b"[::1",
        b"[zz]",
        b"[::1]extra:4443",
        // A virtual host's port stays subject to the same checks as any other host's.
        b"bucket.explicit.invalid:",
        b"bucket.explicit.invalid:0",
        b"bucket.explicit.invalid:04443",
        b"bucket.explicit.invalid:99999",
        b"user@bucket.explicit.invalid",
    ] {
        assert_eq!(
            advertised(&config, ConnectionScheme::Http, "/storage/v1/b", &[host]),
            FIXED,
            "{}",
            String::from_utf8_lossy(host)
        );
    }
    assert_eq!(
        advertised(&config, ConnectionScheme::Http, "/storage/v1/b", &[]),
        FIXED,
        "HTTP/1.0 requests may omit Host"
    );
    assert_eq!(
        advertised(
            &config,
            ConnectionScheme::Http,
            "/storage/v1/b",
            &[b"gcs:4443", b"gcs:4443"]
        ),
        FIXED,
        "a repeated Host is ambiguous even when the values agree"
    );
}

#[test]
fn absolute_form_target_overrides_host_and_is_validated_the_same_way() {
    let config = config();
    for (uri, hosts, expected) in [
        (
            "http://gcs:4443/storage/v1/b",
            [b"other.invalid".as_slice()].as_slice(),
            "http://gcs:4443",
        ),
        ("http://gcs:4443/storage/v1/b", &[], "http://gcs:4443"),
        (
            "http://gcs:4443/storage/v1/b",
            &[b"a".as_slice(), b"b".as_slice()],
            "http://gcs:4443",
        ),
        // The target decides, so a usable Host does not rescue an unusable target.
        ("http://user@gcs:4443/storage/v1/b", &[b"gcs"], FIXED),
        (
            "http://bucket.explicit.invalid/o",
            &[b"gcs"],
            "http://explicit.invalid",
        ),
        // The client's claim about the scheme does not describe this connection.
        (
            "https://gcs:4443/storage/v1/b",
            &[b"gcs:4443"],
            "http://gcs:4443",
        ),
    ] {
        assert_eq!(
            advertised(&config, ConnectionScheme::Http, uri, hosts),
            expected,
            "{uri}"
        );
    }
}

#[test]
fn advertised_scheme_follows_the_connection_and_forwarded_headers_do_not_apply() {
    let config = config();
    let uri: hyper::Uri = "/storage/v1/b".parse().unwrap();
    let mut headers = host_headers(&[b"gcs:4443"]);
    for (name, value) in [
        ("x-forwarded-host", "evil.invalid"),
        ("x-forwarded-proto", "https"),
        ("forwarded", "host=evil.invalid;proto=https"),
    ] {
        headers.insert(name, value.parse().unwrap());
    }
    for (scheme, expected) in [
        (ConnectionScheme::Http, "http://gcs:4443"),
        (ConnectionScheme::Https, "https://gcs:4443"),
    ] {
        let view = config
            .runtime_endpoints
            .snapshot(&config, FIXED)
            .unwrap()
            .for_request(scheme, &uri, &headers);
        assert_eq!(view.api_base_url(), expected);
    }
}

#[test]
fn explicit_base_ignores_the_request_host() {
    let args = [
        "--scheme".into(),
        "http".into(),
        "--api-base-url".into(),
        "https://advertised.invalid/api/".into(),
    ];
    let config = Config::load(&args, &BTreeMap::new()).unwrap();
    let explicit = config.effective_api_base_url(config.listen);
    let view = config
        .runtime_endpoints
        .snapshot(&config, &explicit)
        .unwrap()
        .for_request(
            ConnectionScheme::Https,
            &"http://gcs:4443/storage/v1/b".parse().unwrap(),
            &host_headers(&[b"gcs:4443"]),
        );
    assert_eq!(view.api_base_url(), "https://advertised.invalid/api");
    assert_eq!(view.api_authority().as_str(), "advertised.invalid");
    assert_eq!(view.api_base_source(), ApiBaseSource::Explicit);
}

#[test]
fn only_an_external_url_update_fixes_a_derived_base() {
    let config = config();
    let runtime = &config.runtime_endpoints;
    let derived = |host: &str| {
        advertised(
            &config,
            ConnectionScheme::Http,
            "/storage/v1/b",
            &[host.as_bytes()],
        )
    };
    runtime
        .update(&config, FIXED, None, Some("public.invalid:9876"))
        .unwrap();
    assert_eq!(derived("gcs:4443"), "http://gcs:4443");
    // The update's new XML domain applies to the very next request.
    assert_eq!(derived("bucket.public.invalid"), "http://public.invalid");
    assert!(
        runtime
            .update(&config, FIXED, Some("https://user@api.invalid"), None)
            .is_err()
    );
    assert_eq!(derived("gcs:4443"), "http://gcs:4443");
    runtime
        .update(
            &config,
            FIXED,
            Some("http://mapped.invalid:9876/proxy"),
            None,
        )
        .unwrap();
    assert_eq!(derived("gcs:4443"), "http://mapped.invalid:9876/proxy");
    let fixed = runtime.snapshot(&config, FIXED).unwrap();
    assert_eq!(fixed.fixed_api_authority().as_str(), "mapped.invalid:9876");
    // A later partial update does not unfix it.
    runtime
        .update(&config, FIXED, None, Some("other.invalid"))
        .unwrap();
    assert_eq!(derived("gcs:4443"), "http://mapped.invalid:9876/proxy");
}

#[test]
fn poisoned_endpoint_state_fails_visibly() {
    let config = config();
    let runtime = config.runtime_endpoints.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = runtime.current.write().unwrap();
            panic!("injected endpoint writer panic");
        })
        .join()
        .is_err()
    );
    assert!(matches!(
        config
            .runtime_endpoints
            .snapshot(&config, "http://initial.invalid"),
        Err(EndpointError::Poisoned)
    ));
    assert!(matches!(
        config
            .runtime_endpoints
            .update(&config, "http://initial.invalid", None, None),
        Err(EndpointError::Poisoned)
    ));
}
