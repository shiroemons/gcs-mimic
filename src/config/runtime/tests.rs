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
    assert_eq!(old.api_base_url, "http://initial.invalid");
    assert_eq!(
        old.public_base_url.as_ref().unwrap().authority().as_str(),
        "old.invalid"
    );
    assert_eq!(new.api_base_url, "http://mapped.invalid:9876/proxy");
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
    assert_eq!(snapshot.api_base_url, "http://mapped.invalid:9876");
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
