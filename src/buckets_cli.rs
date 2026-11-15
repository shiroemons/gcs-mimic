//! `gcs-mimic buckets new`: the argument and output layer over the library's document generator.
//! Flags become the same [`Setting`] answers that the questions give, so both write one document.

use crate::CliError;
use gcs_mimic::{
    bucket_init::generate::{Bucket, Document, Format, GenerateError, Setting},
    diagnostics::{DiagnosticSource, Diagnostics},
};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(crate) const HELP: &str =
    "Usage: gcs-mimic buckets new [--format json|env|compose] [--output PATH]
       gcs-mimic buckets new --name NAME [SETTING]... [--name NAME [SETTING]...]...
                             [--format json|env|compose] [--output PATH]

Writes a bucket initialization document for --buckets-file, GCS_MIMIC_BUCKETS or a Compose
service. With --name it asks nothing; each setting after a --name belongs to that bucket:
  --project ID                  --location LOCATION        --storage-class CLASS
  --public-access-prevention inherited|enforced            --soft-delete-retention-seconds N
  --versioning                  --default-event-based-hold --public-read
  --label KEY=VALUE             --iam-binding ROLE=MEMBER[,MEMBER]...
  --cors-origin ORIGIN          --cors-method METHOD       --cors-response-header NAME
  --cors-max-age-seconds N
--label, the --cors list settings and --iam-binding may repeat; other settings appear once per
bucket. --public-read grants allUsers roles/storage.objectViewer. A bucket without settings
gets default settings.
--format json (the default) writes the document, env one GCS_MIMIC_BUCKETS line in single
quotes, and compose an environment: block with each $ written $$. The document goes to stdout,
or to a new file with --output PATH, which refuses an existing file.
The document passes startup validation before it is written; checks against stored state,
such as a project's public access prevention, still run when the server starts.
No configuration, environment, data directory or network is read.";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
struct Request {
    buckets: Vec<Bucket>,
    format: Format,
    output: Option<PathBuf>,
}

pub(crate) fn run(args: &[String]) -> Result<(), CliError> {
    let request = parse(args)?;
    if request.buckets.is_empty() {
        return Err(failure(
            "--name",
            "No bucket given; pass --name NAME for each bucket, see gcs-mimic buckets new --help.",
        )
        .into());
    }
    let text = Document::new(request.buckets)
        .render(request.format)
        .map_err(rejected)?;
    write(&text, request.output.as_deref())
}

fn failure(key: &str, fix: impl Into<String>) -> Diagnostics {
    Diagnostics::error(DiagnosticSource::Cli, key, fix)
}

/// A refused document keeps the startup parser's own diagnostics, whose keys name entries.
fn rejected(error: GenerateError) -> Diagnostics {
    match error {
        GenerateError::Rejected(diagnostics) => diagnostics,
        error @ (GenerateError::InvalidName
        | GenerateError::Repeated(_)
        | GenerateError::PublicUnderPrevention { .. }
        | GenerateError::Unquotable
        | GenerateError::Encode(_)) => failure("buckets", error.to_string()),
    }
}

fn parse(args: &[String]) -> Result<Request, Diagnostics> {
    let mut buckets: Vec<Bucket> = Vec::new();
    let mut format = None;
    let mut output = None;
    let mut args = args.iter().map(String::as_str);
    while let Some(flag) = args.next() {
        match flag {
            "--name" => {
                let name = value(&mut args, flag)?;
                buckets.push(Bucket::new(name).map_err(|error| failure(flag, error.to_string()))?);
            }
            "--format" => {
                let chosen = value(&mut args, flag)?
                    .parse::<Format>()
                    .map_err(|error| failure(flag, error.to_string()))?;
                once(&mut format, chosen, flag)?;
            }
            "--output" => once(&mut output, PathBuf::from(value(&mut args, flag)?), flag)?,
            flag => {
                let setting = setting(flag, &mut args)?;
                let bucket = buckets.last_mut().ok_or_else(|| {
                    failure(
                        flag,
                        format!("{flag} belongs to a bucket; put --name NAME before it."),
                    )
                })?;
                bucket
                    .apply(setting)
                    .map_err(|error| failure(flag, error.to_string()))?;
            }
        }
    }
    Ok(Request {
        buckets,
        format: format.unwrap_or(Format::Json),
        output,
    })
}

fn value<'a>(args: &mut impl Iterator<Item = &'a str>, flag: &str) -> Result<&'a str, Diagnostics> {
    args.next().ok_or_else(|| {
        failure(
            flag,
            format!("{flag} needs a value; see gcs-mimic buckets new --help."),
        )
    })
}

fn once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), Diagnostics> {
    if slot.replace(value).is_some() {
        return Err(failure(
            flag,
            format!("{flag} was given twice; give it once."),
        ));
    }
    Ok(())
}

/// The answer one bucket setting flag gives.
fn setting<'a>(
    flag: &str,
    args: &mut impl Iterator<Item = &'a str>,
) -> Result<Setting, Diagnostics> {
    let mut text = || value(args, flag).map(str::to_owned);
    Ok(match flag {
        "--versioning" => Setting::Versioning,
        "--default-event-based-hold" => Setting::DefaultEventBasedHold,
        "--public-read" => Setting::PublicRead,
        "--project" => Setting::Project(text()?),
        "--location" => Setting::Location(text()?),
        "--storage-class" => Setting::StorageClass(text()?),
        "--public-access-prevention" => {
            Setting::PublicAccessPrevention(text()?.parse().map_err(
                |error: gcs_mimic::config::InvalidChoice| failure(flag, error.to_string()),
            )?)
        }
        "--soft-delete-retention-seconds" => {
            Setting::SoftDeleteRetentionSeconds(seconds(flag, &text()?)?)
        }
        "--cors-origin" => Setting::CorsOrigin(text()?),
        "--cors-method" => Setting::CorsMethod(text()?),
        "--cors-response-header" => Setting::CorsResponseHeader(text()?),
        "--cors-max-age-seconds" => Setting::CorsMaxAgeSeconds(seconds(flag, &text()?)?),
        "--label" => {
            let label = text()?;
            let (key, value) = label.split_once('=').ok_or_else(|| {
                failure(flag, "--label takes KEY=VALUE, for example team=storage.")
            })?;
            Setting::Label {
                key: key.to_owned(),
                value: value.to_owned(),
            }
        }
        "--iam-binding" => {
            let binding = text()?;
            let (role, members) = binding.split_once('=').ok_or_else(|| {
                failure(
                    flag,
                    "--iam-binding takes ROLE=MEMBER[,MEMBER]..., for example roles/storage.objectViewer=allUsers.",
                )
            })?;
            Setting::IamBinding {
                role: role.to_owned(),
                members: members.split(',').map(str::to_owned).collect(),
            }
        }
        _ => {
            return Err(failure(
                flag,
                "Unknown option; see gcs-mimic buckets new --help.",
            ));
        }
    })
}

fn seconds(flag: &str, value: &str) -> Result<u64, Diagnostics> {
    value.parse().map_err(|_| {
        failure(
            flag,
            format!("{flag} takes whole seconds, for example 3600."),
        )
    })
}

/// Writes the document to a new `output` file, refusing an existing one, or else to stdout.
fn write(text: &str, output: Option<&Path>) -> Result<(), CliError> {
    let Some(path) = output else {
        io::stdout().lock().write_all(text.as_bytes())?;
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            failure(
                "--output",
                if error.kind() == io::ErrorKind::AlreadyExists {
                    "--output names an existing file, which is kept; choose a new path."
                } else {
                    "Cannot create --output; choose a new path in a writable directory."
                },
            )
        })?;
    if file.write_all(text.as_bytes()).is_err() {
        // A partial document would fail startup later and far from its cause.
        let _ = std::fs::remove_file(path);
        return Err(failure(
            "--output",
            "Cannot write --output; free disk space or choose another path, then retry.",
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
