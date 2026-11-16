//! The questions `gcs-mimic buckets new` asks on a terminal. Each answer becomes the same
//! [`Setting`] a flag gives and goes through [`Bucket::apply`], so this layer only asks: the
//! library decides what is valid and writes the document. Questions render on stderr, so the
//! document alone reaches stdout.

use gcs_mimic::bucket_init::generate::{
    self, Bucket, Format, GenerateError, Label, PublicAccessPrevention, Setting,
};
use inquire::{
    Confirm, CustomType, InquireError, MultiSelect, Select, Text, error::CustomUserError,
    validator::Validation,
};
use std::fmt;

/// A member the user can choose to set, in the order its questions come. Public access
/// prevention comes before public read, so a bucket that grants public read is never offered
/// the prevention that startup would refuse with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Versioning,
    Cors,
    PublicAccessPrevention,
    PublicRead,
    Labels,
    StorageClass,
    Location,
    SoftDelete,
    DefaultEventBasedHold,
    IamBindings,
    Project,
}

impl Choice {
    const ALL: [Self; 11] = [
        Self::Versioning,
        Self::Cors,
        Self::PublicAccessPrevention,
        Self::PublicRead,
        Self::Labels,
        Self::StorageClass,
        Self::Location,
        Self::SoftDelete,
        Self::DefaultEventBasedHold,
        Self::IamBindings,
        Self::Project,
    ];
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Versioning => "Versioning",
            Self::Cors => "CORS",
            Self::PublicAccessPrevention => "Public access prevention",
            Self::PublicRead => "Public read (allUsers can read objects)",
            Self::Labels => "Labels",
            Self::StorageClass => "Storage class",
            Self::Location => "Location",
            Self::SoftDelete => "Soft delete retention",
            Self::DefaultEventBasedHold => "Default event-based hold",
            Self::IamBindings => "IAM bindings",
            Self::Project => "Owning project",
        })
    }
}

/// An output format with where it goes, for the format question.
#[derive(Clone, Copy)]
struct FormatChoice(Format);

impl fmt::Display for FormatChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let place = match self.0 {
            Format::Json => "a file for --buckets-file",
            Format::Env => "a GCS_MIMIC_BUCKETS line for .env or a shell",
            Format::Compose => "an environment: block for a Compose service",
        };
        write!(f, "{} ({place})", self.0.as_str())
    }
}

/// The location question offers the multi-regions, then any other supported location.
const OTHER_LOCATION: &str = "Another location";
const CORS_METHODS: [&str; 6] = ["GET", "HEAD", "PUT", "POST", "DELETE", "OPTIONS"];

/// Asks for one bucket after another, then for the format unless a flag chose it.
pub(super) fn ask(format: Option<Format>) -> Result<(Vec<Bucket>, Format), InquireError> {
    let mut buckets: Vec<Bucket> = Vec::new();
    loop {
        let taken = buckets
            .iter()
            .map(|bucket| bucket.name().to_owned())
            .collect();
        buckets.push(bucket(taken)?);
        if !Confirm::new("Add another bucket?")
            .with_default(false)
            .prompt()?
        {
            break;
        }
    }
    let format = match format {
        Some(format) => format,
        None => {
            let choices = Format::ALL.iter().copied().map(FormatChoice).collect();
            Select::new("Output format:", choices).prompt()?.0
        }
    };
    Ok((buckets, format))
}

fn bucket(taken: Vec<String>) -> Result<Bucket, InquireError> {
    let name = Text::new("Bucket name:")
        .with_validator(move |name: &str| {
            Ok(if taken.iter().any(|taken| taken == name) {
                Validation::Invalid("This document already has a bucket of that name.".into())
            } else {
                valid(Bucket::new(name).map(drop))
            })
        })
        .prompt()?;
    let mut bucket = Bucket::new(&name).map_err(custom)?;
    let chosen = MultiSelect::new("Settings to configure:", Choice::ALL.to_vec())
        .with_help_message("Space selects, Enter continues; select none for default settings")
        .prompt()?;
    for choice in Choice::ALL {
        if chosen.contains(&choice) {
            configure(&mut bucket, choice, &chosen)?;
        }
    }
    Ok(bucket)
}

/// Asks for what `choice` needs and records it.
fn configure(bucket: &mut Bucket, choice: Choice, chosen: &[Choice]) -> Result<(), InquireError> {
    let setting = match choice {
        Choice::Versioning => Setting::Versioning,
        Choice::PublicRead => Setting::PublicRead,
        Choice::DefaultEventBasedHold => Setting::DefaultEventBasedHold,
        Choice::Cors => return cors(bucket),
        Choice::Labels => return labels(bucket),
        Choice::IamBindings => return bindings(bucket),
        Choice::PublicAccessPrevention => Setting::PublicAccessPrevention(prevention(chosen)?),
        Choice::StorageClass => Setting::StorageClass(
            Select::new("Storage class:", generate::storage_classes().to_vec())
                .prompt()?
                .to_owned(),
        ),
        Choice::Location => Setting::Location(location()?),
        Choice::SoftDelete => Setting::SoftDeleteRetentionSeconds(
            CustomType::<u64>::new("Soft delete retention in seconds:")
                .with_help_message("0 keeps no deleted data; up to 7776000 (90 days)")
                .with_error_message("Enter whole seconds, for example 604800.")
                .prompt()?,
        ),
        Choice::Project => Setting::Project(
            Text::new("Owning project ID:")
                .with_validator(required)
                .prompt()?,
        ),
    };
    bucket.apply(setting).map_err(custom)
}

/// Public read and enforced prevention cannot go together, so a bucket that chose public read
/// is offered only `inherited`.
fn prevention(chosen: &[Choice]) -> Result<PublicAccessPrevention, InquireError> {
    let public_read = chosen.contains(&Choice::PublicRead);
    let choices: Vec<_> = PublicAccessPrevention::ALL
        .iter()
        .copied()
        .filter(|prevention| !public_read || *prevention == PublicAccessPrevention::Inherited)
        .map(PublicAccessPrevention::as_str)
        .collect();
    let mut question = Select::new("Public access prevention:", choices);
    if public_read {
        question =
            question.with_help_message("enforced would refuse public read, so it is not offered");
    }
    question.prompt()?.parse().map_err(custom)
}

fn location() -> Result<String, InquireError> {
    let mut choices = generate::multi_regions().to_vec();
    choices.push(OTHER_LOCATION);
    let chosen = Select::new("Location:", choices).prompt()?;
    if chosen != OTHER_LOCATION {
        return Ok(chosen.to_owned());
    }
    let named = Text::new("Location, such as US-CENTRAL1 or EUROPE-WEST1:")
        .with_validator(|value: &str| {
            Ok(match generate::location(value) {
                Some(_) => Validation::Valid,
                None => Validation::Invalid("That location is not supported.".into()),
            })
        })
        .prompt()?;
    generate::location(&named)
        .map(str::to_owned)
        .ok_or_else(|| InquireError::Custom("location changed after validation".into()))
}

fn cors(bucket: &mut Bucket) -> Result<(), InquireError> {
    for method in MultiSelect::new("CORS methods:", CORS_METHODS.to_vec()).prompt()? {
        bucket
            .apply(Setting::CorsMethod(method.to_owned()))
            .map_err(custom)?;
    }
    for origin in repeated("CORS origin, such as http://localhost:3000")? {
        bucket.apply(Setting::CorsOrigin(origin)).map_err(custom)?;
    }
    for header in repeated("CORS response header, such as Content-Type")? {
        bucket
            .apply(Setting::CorsResponseHeader(header))
            .map_err(custom)?;
    }
    let max_age = Text::new("CORS max age in seconds (empty for none):")
        .with_validator(|value: &str| {
            Ok(if value.is_empty() || value.parse::<u64>().is_ok() {
                Validation::Valid
            } else {
                Validation::Invalid("Enter whole seconds, for example 3600.".into())
            })
        })
        .prompt()?;
    if let Ok(seconds) = max_age.parse() {
        bucket
            .apply(Setting::CorsMaxAgeSeconds(seconds))
            .map_err(custom)?;
    }
    Ok(())
}

fn labels(bucket: &mut Bucket) -> Result<(), InquireError> {
    loop {
        let answer = Text::new("Label as key=value (empty to finish):")
            .with_validator(|answer: &str| {
                Ok(if answer.is_empty() {
                    Validation::Valid
                } else {
                    valid(answer.parse::<Label>().map(drop))
                })
            })
            .prompt()?;
        if answer.is_empty() {
            return Ok(());
        }
        let label = answer.parse().map_err(custom)?;
        // A repeated key asks again instead of ending the questions.
        if let Err(error) = bucket.apply(Setting::Label(label)) {
            eprintln!("{error}");
        }
    }
}

fn bindings(bucket: &mut Bucket) -> Result<(), InquireError> {
    loop {
        let role = Select::new("IAM role:", generate::bucket_roles()).prompt()?;
        let members = Text::new("Members, comma-separated, such as user:dev@example.com:")
            .with_validator(required)
            .prompt()?;
        bucket
            .apply(Setting::IamBinding {
                role: role.to_owned(),
                members: members
                    .split(',')
                    .map(|member| member.trim().to_owned())
                    .collect(),
            })
            .map_err(custom)?;
        if !Confirm::new("Add another IAM binding?")
            .with_default(false)
            .prompt()?
        {
            return Ok(());
        }
    }
}

/// Asks for values one at a time until an empty answer.
fn repeated(what: &str) -> Result<Vec<String>, InquireError> {
    let message = format!("{what} (empty to finish):");
    let mut values = Vec::new();
    loop {
        let value = Text::new(&message).prompt()?;
        if value.is_empty() {
            return Ok(values);
        }
        values.push(value);
    }
}

fn required(value: &str) -> Result<Validation, CustomUserError> {
    Ok(if value.trim().is_empty() {
        Validation::Invalid("An answer is required.".into())
    } else {
        Validation::Valid
    })
}

/// A library refusal shown under the question that asks again.
fn valid(result: Result<(), GenerateError>) -> Validation {
    match result {
        Ok(()) => Validation::Valid,
        Err(error) => Validation::Invalid(error.to_string().into()),
    }
}

/// An answer the library refused after its question was validated, which ends the questions.
fn custom(error: impl std::error::Error + Send + Sync + 'static) -> InquireError {
    InquireError::Custom(Box::new(error))
}
