use super::Config;
use crate::{
    diagnostics::{DiagnosticSource, Diagnostics},
    test_support::TestDir,
};
use std::{collections::BTreeMap, fmt::Debug};

type Loader = fn(&[String], &BTreeMap<String, String>) -> Result<Config, Diagnostics>;

#[track_caller]
pub(super) fn assert_precedence<T: Debug + PartialEq>(
    file_args: &[String],
    environment: &BTreeMap<String, String>,
    options: &[&str],
    expected: [T; 4],
    load: Loader,
    value: impl Fn(Config) -> T,
) -> Vec<String> {
    let mut cli = file_args.to_vec();
    cli.extend(options.iter().map(|option| (*option).to_owned()));
    let empty = BTreeMap::new();
    for (index, ((args, env), expected)) in [
        (&[][..], &empty),
        (file_args, &empty),
        (file_args, environment),
        (cli.as_slice(), environment),
    ]
    .into_iter()
    .zip(expected)
    .enumerate()
    {
        assert_eq!(
            value(load(args, env).unwrap()),
            expected,
            "stage {index}: args={args:?} env={env:?}"
        );
    }
    cli
}

#[track_caller]
pub(super) fn assert_rejected(setting: &str, values: &[impl AsRef<str>], fix: &str) {
    let directory = TestDir::new();
    let file = directory.0.join("config.json");
    let flag = format!("--{}", setting.replace('_', "-"));
    let key = format!("GCS_MIMIC_{}", setting.to_ascii_uppercase());
    for value in values {
        let value = value.as_ref();
        std::fs::write(
            &file,
            serde_json::to_vec(&BTreeMap::from([(setting, value)])).unwrap(),
        )
        .unwrap();
        for (source, key, args, env) in [
            (
                DiagnosticSource::Cli,
                flag.as_str(),
                vec![flag.clone(), value.into()],
                BTreeMap::new(),
            ),
            (
                DiagnosticSource::Environment,
                key.as_str(),
                vec![],
                BTreeMap::from([(key.clone(), value.into())]),
            ),
            (
                DiagnosticSource::Config,
                setting,
                vec!["--config".into(), file.to_str().unwrap().into()],
                BTreeMap::new(),
            ),
        ] {
            let errors = Config::load(&args, &env).unwrap_err();
            assert_eq!(
                errors.errors[0].source, source,
                "{setting}={value:?}: {errors}"
            );
            assert_eq!(errors.errors[0].key, key, "{setting}={value:?}: {errors}");
            assert!(
                errors.errors[0].fix.contains(fix),
                "{setting}={value:?}: {errors}"
            );
        }
    }
}
