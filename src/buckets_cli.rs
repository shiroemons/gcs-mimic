//! `gcs-mimic buckets new`: the argument and output layer over the library's document generator.
//! Flags become the same [`Setting`] answers that the questions give, so both write one document.

use crate::CliError;
use gcs_mimic::{
    bucket_init::generate::{
        AuthMode, Bucket, CorsGap, Document, Format, GenerateError, STRICT_SERVE_OPTIONS, Setting,
    },
    diagnostics::{DiagnosticSource, Diagnostics},
};
use inquire::InquireError;
use std::{
    borrow::Cow,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

mod prompts;

pub(crate) const HELP: &str =
    "Usage: gcs-mimic buckets new [--format json|env|compose] [--auth-mode MODE] [--output PATH]
       gcs-mimic buckets new --name NAME [SETTING]... [--name NAME [SETTING]...]...
                             [--format json|env|compose] [--auth-mode MODE] [--output PATH]

Writes a bucket initialization document for --buckets-file, GCS_MIMIC_BUCKETS or a Compose
service. Without --name it asks on a terminal: how you run gcs-mimic unless --format is given,
then bucket names (several, comma-separated, share their answers), whether they are public or
private and what they are for, and shows the buckets to write, change or add to. A private
bucket puts every bucket in strict mode, where public ones get public read; --auth-mode fixes
the mode instead, and permissive asks nothing about public or private. A JSON document bound
for the terminal is saved to a file it asks for, or printed if none can be created. Questions
use stderr; Esc goes back, and Ctrl-C writes nothing.
With --name it asks nothing; each setting after a --name belongs to that bucket:
  --project ID                  --location LOCATION        --storage-class CLASS
  --public-access-prevention inherited|enforced            --soft-delete-retention-seconds N
  --versioning                  --default-event-based-hold --public-read
  --label KEY=VALUE             --iam-binding ROLE=MEMBER[,MEMBER]...
  --browser-uploads             --browser-downloads        --cors-origin ORIGIN
  --cors-method METHOD          --cors-response-header NAME --cors-max-age-seconds N
--label, the --cors list settings and --iam-binding may repeat; other settings appear once per
bucket. A bucket's CORS settings form one rule, which needs at least one --cors-origin and one
method to match a request. --browser-uploads adds PUT and POST with the Content-Type and
x-goog-resumable request headers, and --browser-downloads GET and HEAD, for the rule's origins.
--public-read grants allUsers roles/storage.objectViewer. A bucket without settings gets
default settings.
--format json (the default) writes the document, env one GCS_MIMIC_BUCKETS line in single
quotes, and compose an environment: block with each $ written $$; for buckets without settings,
env and compose write their names alone. --auth-mode permissive (the default) lets anyone read
every bucket; strict keeps buckets without --public-read private, so env and compose then also
write the settings that start the server so, with a trusted listener on port 4444 for your
servers and the built-in test signer for URLs they sign for browsers, and json is followed by
the matching serve options. The document goes to stdout, or to a new file with --output PATH,
which refuses an existing file. On a terminal, stderr then says how to use it.
The document passes startup validation before it is written; checks against stored state,
such as a project's public access prevention, still run when the server starts.
No configuration, environment, data directory or network is read.";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
struct Request {
    buckets: Vec<Bucket>,
    /// `None` until a flag chooses one: the questions then ask, and flags default to JSON.
    format: Option<Format>,
    /// `None` until a flag chooses one: the questions then ask, and flags default to permissive.
    auth_mode: Option<AuthMode>,
    output: Option<PathBuf>,
}

/// What the questions settled on: the document, its text, and the file they saved it in.
struct Answers {
    document: Document,
    format: Format,
    text: String,
    saved: Option<PathBuf>,
}

/// The document as written, and the file it went to rather than stdout.
struct Written {
    document: Document,
    format: Format,
    file: Option<PathBuf>,
}

pub(crate) fn run(args: &[String]) -> Result<(), CliError> {
    let Request {
        buckets,
        format,
        auth_mode,
        output,
    } = parse(args)?;
    let written = if buckets.is_empty() {
        ask(format, auth_mode, output)?
    } else {
        from_flags(buckets, format, auth_mode, output)?
    };
    // Advice is for a person at a terminal; a script reads the document and the exit code.
    if io::stderr().is_terminal() {
        let destination = match written.file.as_deref() {
            Some(path) => Destination::File(path),
            None if io::stdout().is_terminal() => Destination::Terminal,
            None => Destination::Redirected,
        };
        eprint!("{}", advice(&written.document, written.format, destination));
    }
    Ok(())
}

fn from_flags(
    buckets: Vec<Bucket>,
    format: Option<Format>,
    auth_mode: Option<AuthMode>,
    output: Option<PathBuf>,
) -> Result<Written, CliError> {
    let format = format.unwrap_or(Format::Json);
    let document = Document::new(buckets).with_auth_mode(auth_mode.unwrap_or(AuthMode::Permissive));
    let text = document.render(format).map_err(rejected)?;
    write(&text, output.as_deref())?;
    Ok(Written {
        document,
        format,
        file: output,
    })
}

/// Where the document went, which decides what to do with it next.
#[derive(Clone, Copy, Debug)]
enum Destination<'a> {
    File(&'a Path),
    Terminal,
    /// Stdout went to a file or a pipe whose name the command cannot know.
    Redirected,
}

/// The next step for the document, then in strict mode where servers and browsers connect, then
/// a note for each bucket whose CORS rule would refuse a browser upload, which startup accepts
/// and a browser reports far from its cause.
fn advice(document: &Document, format: Format, destination: Destination<'_>) -> String {
    let notes = document.uploads_refusing_content_type().map(|bucket| {
        Cow::Owned(format!(
            "Note: browser uploads to {bucket} send Content-Type, which its CORS rule refuses; add --cors-response-header Content-Type\n"
        ))
    });
    let next = match document.auth_mode() {
        AuthMode::Permissive => next_step(format, destination),
        AuthMode::Strict => strict_steps(format, destination).into(),
    };
    // A document printed just above would run on into the advice without a blank line.
    let apart = match destination {
        Destination::Terminal => "\n",
        Destination::File(_) | Destination::Redirected => "",
    };
    [Cow::Borrowed(apart), next]
        .into_iter()
        .chain(notes)
        .collect()
}

/// Strict mode changes how the application connects, so each change gets a line of its own
/// after the next step, short enough to fit a terminal: the servers' port, since they send no
/// credentials, as browsers do not, and the browsers' way to private objects with the command
/// that prints the signing key where the server runs. Public and private buckets share the main
/// port; only the servers move.
fn strict_steps(format: Format, destination: Destination<'_>) -> String {
    let (next, servers, credentials) = match format {
        Format::Json => (
            strict_serve(destination),
            "Your servers on this machine then use http://127.0.0.1:4444",
            "gcs-mimic test-signer credentials",
        ),
        Format::Env => (
            "Next: export these lines before gcs-mimic serve, on the machine that runs your servers"
                .to_owned(),
            // A container's loopback is its own, and the trusted port must stay unpublished, so
            // servers outside a docker run container cannot reach it; Compose puts them beside it.
            "Your servers then use http://127.0.0.1:4444. With docker run, choose Docker Compose: 4444 stays unpublished",
            "gcs-mimic test-signer credentials",
        ),
        Format::Compose => (
            "Next: add these settings to the gcs-mimic service's environment: in compose.yaml, then docker compose up -d"
                .to_owned(),
            "Your servers on the Compose network then use port 4444, such as http://gcs:4444; never publish it",
            "docker compose run --rm gcs test-signer credentials",
        ),
    };
    format!(
        "{next}\n{servers}\nBrowsers keep the main port: public buckets are open there, private ones need signed URLs\nSign them with the key that {credentials} prints\n"
    )
}

/// The serve command for a buckets file in strict mode, continued on a second line so that both
/// fit a terminal and paste as one command.
fn strict_serve(destination: Destination<'_>) -> String {
    match destination {
        Destination::File(path) => format!(
            "Next: gcs-mimic serve --buckets-file {} \\\n  {STRICT_SERVE_OPTIONS}",
            shell_word(path)
        ),
        Destination::Terminal => format!(
            "Next: save this as buckets.json, then run gcs-mimic serve --buckets-file buckets.json \\\n  {STRICT_SERVE_OPTIONS}"
        ),
        Destination::Redirected => format!(
            "Next: run gcs-mimic serve --buckets-file with the file this went to, adding\n  {STRICT_SERVE_OPTIONS}"
        ),
    }
}

/// What to do with the document where it went, in one line that fits a terminal, since a
/// wrapped paragraph after a document goes unread.
fn next_step(format: Format, destination: Destination<'_>) -> Cow<'static, str> {
    match (format, destination) {
        (Format::Json, Destination::File(path)) => {
            format!("Next: gcs-mimic serve --buckets-file {}\n", shell_word(path)).into()
        }
        (Format::Json, Destination::Terminal) => {
            "Next: save this as buckets.json, then run gcs-mimic serve --buckets-file buckets.json\n"
                .into()
        }
        (Format::Json, Destination::Redirected) => {
            "Next: run gcs-mimic serve --buckets-file with the file this went to\n".into()
        }
        (Format::Env, _) => {
            "Next: export this line before gcs-mimic serve, or put it in a Compose env_file\n".into()
        }
        (Format::Compose, _) => "Next: add GCS_MIMIC_BUCKETS to the gcs-mimic service's environment: in compose.yaml, then docker compose up -d\n".into(),
    }
}

/// `path` as one shell word, quoted only when it has to be, so the next command can be pasted.
fn shell_word(path: &Path) -> String {
    let text = path.to_string_lossy();
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./+,:@%=".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Asks on a terminal. Elsewhere nothing could answer, so it fails before asking anything, as it
/// does for an existing `--output`, which refused after the last question would lose every answer.
fn ask(
    format: Option<Format>,
    auth_mode: Option<AuthMode>,
    output: Option<PathBuf>,
) -> Result<Written, CliError> {
    if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
        return Err(failure(
            "--name",
            "No bucket given and no terminal to ask on; pass --name NAME for each bucket, see gcs-mimic buckets new --help.",
        )
        .into());
    }
    if output.as_deref().is_some_and(Path::exists) {
        return Err(failure("--output", EXISTING_OUTPUT).into());
    }
    // A JSON document on a terminal only scrolls past, so the questions then ask for a file.
    let file = if output.is_none() && io::stdout().is_terminal() {
        prompts::FileQuestion::Ask
    } else {
        prompts::FileQuestion::Skip
    };
    let Answers {
        document,
        format,
        text,
        saved,
    } = prompts::ask(format, auth_mode, file).map_err(unanswered)?;
    let file = match saved {
        Some(path) => Some(path),
        None => {
            write(&text, output.as_deref())?;
            output
        }
    };
    Ok(Written {
        document,
        format,
        file,
    })
}

fn unanswered(error: InquireError) -> Diagnostics {
    match error {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            failure("buckets", "Cancelled; nothing was written.")
        }
        InquireError::Custom(error) => failure("buckets", error.to_string()),
        InquireError::NotTTY | InquireError::IO(_) | InquireError::InvalidConfiguration(_) => {
            failure(
                "--name",
                "Cannot ask questions on this terminal; pass --name NAME for each bucket instead, see gcs-mimic buckets new --help.",
            )
        }
    }
}

fn failure(key: &str, fix: impl Into<String>) -> Diagnostics {
    Diagnostics::error(DiagnosticSource::Cli, key, fix)
}

/// A refused document keeps the startup parser's own diagnostics, whose keys name entries.
fn rejected(error: GenerateError) -> Diagnostics {
    match error {
        GenerateError::Rejected(diagnostics) => diagnostics,
        GenerateError::IncompleteCors { bucket, gap } => {
            let (key, lacks, add) = match gap {
                CorsGap::Origin => (
                    "--cors-origin",
                    "--cors-origin",
                    "--cors-origin http://localhost:3000",
                ),
                CorsGap::Method => ("--cors-method", "--cors-method", "--cors-method GET"),
                CorsGap::OriginAndMethod => (
                    "--cors-origin",
                    "--cors-origin or --cors-method",
                    "--cors-origin http://localhost:3000 --cors-method GET",
                ),
            };
            failure(
                key,
                format!(
                    "{bucket} has CORS settings but no {lacks}, so its rule matches no request; add one after --name {bucket}, for example {add}."
                ),
            )
        }
        error @ (GenerateError::InvalidName
        | GenerateError::UnpairedLabel
        | GenerateError::InvalidLabel
        | GenerateError::TooManyLabels
        | GenerateError::InvalidCorsOrigin
        | GenerateError::InvalidCorsMethod
        | GenerateError::InvalidCorsResponseHeader
        | GenerateError::InvalidMember
        | GenerateError::UnsupportedMember
        | GenerateError::Repeated(_)
        | GenerateError::PublicUnderPrevention { .. }
        | GenerateError::Unquotable
        | GenerateError::Encode(_)) => failure("buckets", error.to_string()),
    }
}

fn parse(args: &[String]) -> Result<Request, Diagnostics> {
    let mut buckets: Vec<Bucket> = Vec::new();
    let mut format = None;
    let mut auth_mode = None;
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
            "--auth-mode" => {
                let chosen = value(&mut args, flag)?
                    .parse::<AuthMode>()
                    .map_err(|error| failure(flag, error.to_string()))?;
                once(&mut auth_mode, chosen, flag)?;
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
        format,
        auth_mode,
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
        "--browser-uploads" => Setting::BrowserUploads,
        "--browser-downloads" => Setting::BrowserDownloads,
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
        "--cors-origin" => Setting::CorsOrigin(parsed(flag, &text()?)?),
        "--cors-method" => Setting::CorsMethod(parsed(flag, &text()?)?),
        "--cors-response-header" => Setting::CorsResponseHeader(parsed(flag, &text()?)?),
        "--cors-max-age-seconds" => Setting::CorsMaxAgeSeconds(seconds(flag, &text()?)?),
        "--label" => Setting::Label(parsed(flag, &text()?)?),
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
                members: parsed(flag, members)?,
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

/// A flag value read through the generator's own check, refused under the flag's name.
fn parsed<T: FromStr<Err = GenerateError>>(flag: &str, value: &str) -> Result<T, Diagnostics> {
    value
        .parse()
        .map_err(|error: GenerateError| failure(flag, error.to_string()))
}

fn seconds(flag: &str, value: &str) -> Result<u64, Diagnostics> {
    value.parse().map_err(|_| {
        failure(
            flag,
            format!("{flag} takes whole seconds, for example 3600."),
        )
    })
}

const EXISTING_OUTPUT: &str = "--output names an existing file, which is kept; choose a new path.";

/// Writes the document to a new `output` file, refusing an existing one, or else to stdout.
fn write(text: &str, output: Option<&Path>) -> Result<(), CliError> {
    let Some(path) = output else {
        io::stdout().lock().write_all(text.as_bytes())?;
        return Ok(());
    };
    save_new(path, text).map_err(|error| {
        let fix = match error {
            SaveError::Exists => EXISTING_OUTPUT,
            SaveError::Create(_) => {
                "Cannot create --output; choose a new path in a writable directory."
            }
            SaveError::Write(_) => {
                "Cannot write --output; free disk space or choose another path, then retry."
            }
        };
        failure("--output", fix).into()
    })
}

/// Why a document could not be saved as a new file.
#[derive(Debug)]
enum SaveError {
    /// The file exists, and is kept.
    Exists,
    Create(io::Error),
    /// The partial file was removed.
    Write(io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exists => f.write_str("That file exists and is kept."),
            Self::Create(error) => write!(f, "Cannot create that file: {error}."),
            Self::Write(error) => write!(f, "Cannot write that file, so it was removed: {error}."),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Exists => None,
            Self::Create(error) | Self::Write(error) => Some(error),
        }
    }
}

/// Saves `text` as a new file at `path`, never replacing one. A partial file is removed, since
/// a partial document would fail startup later and far from its cause.
fn save_new(path: &Path, text: &str) -> Result<(), SaveError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                SaveError::Exists
            } else {
                SaveError::Create(error)
            }
        })?;
    if let Err(error) = file.write_all(text.as_bytes()) {
        let _ = std::fs::remove_file(path);
        return Err(SaveError::Write(error));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
