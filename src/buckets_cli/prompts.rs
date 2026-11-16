//! The questions `gcs-mimic buckets new` asks on a terminal. They start from how the user runs
//! gcs-mimic and what each bucket is for, not from the document's members, and show the buckets
//! before anything is written. Each answer becomes the same [`Setting`] a flag gives and goes
//! through [`Bucket::apply`], so this layer only asks: the library decides what is valid and
//! writes the document. Questions render on stderr, so the document alone reaches stdout.

mod cors;
mod lists;
mod names;
mod terminal;

use super::{Answers, save_new};
use gcs_mimic::bucket_init::generate::{
    self, Bucket, CorsOrigin, Document, Format, Label, Members, PublicAccessPrevention, Setting,
};
use inquire::{InquireError, validator::Validation};
use lists::{
    Noted, Offer, Pick, SecondsPick, choose_many, choose_one, custom, pick_many, pick_seconds,
    positions, required, select_one, valid, write_noted,
};
use names::Names;
use std::{
    fmt, iter,
    path::{Path, PathBuf},
    rc::Rc,
};
use terminal::{Check, EnterMeans, Inquire, Need, Question, Terminal};

/// How the user runs gcs-mimic, which decides the format. It is asked first because people know
/// how they start the server, while the formats' names mean nothing yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Launch(Format);

impl Launch {
    /// Compose first: the README starts there.
    const ALL: [Self; 3] = [Self(Format::Compose), Self(Format::Env), Self(Format::Json)];
}

impl Noted for Launch {
    fn name(self) -> &'static str {
        match self.0 {
            Format::Compose => "Docker Compose",
            Format::Env => "docker run or a shell",
            Format::Json => "gcs-mimic serve --buckets-file",
        }
    }

    fn note(self) -> &'static str {
        match self.0 {
            Format::Compose => "an environment: block to paste into compose.yaml",
            Format::Env => "one GCS_MIMIC_BUCKETS line",
            Format::Json => "a JSON file",
        }
    }
}

impl fmt::Display for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// What a bucket is for. Each goal sets the members it needs, so nobody has to know which
/// members those are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Goal {
    BrowserUploads,
    BrowserDownloads,
    PublicRead,
    KeepVersions,
    KeepDeleted,
    More,
}

impl Goal {
    const ALL: [Self; 6] = [
        Self::BrowserUploads,
        Self::BrowserDownloads,
        Self::PublicRead,
        Self::KeepVersions,
        Self::KeepDeleted,
        Self::More,
    ];
}

impl Noted for Goal {
    fn name(self) -> &'static str {
        match self {
            Self::BrowserUploads => "Browser uploads",
            Self::BrowserDownloads => "Browser downloads",
            Self::PublicRead => "Public read",
            Self::KeepVersions => "Keep old versions",
            Self::KeepDeleted => "Keep deleted objects",
            Self::More => "More settings…",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::BrowserUploads => "your web or mobile app uploads files: CORS for PUT and POST",
            Self::BrowserDownloads => "your app fetches objects: CORS for GET and HEAD",
            Self::PublicRead => "anyone can read objects without signing in",
            Self::KeepVersions => "overwrites and deletes keep the old object: versioning",
            Self::KeepDeleted => "deleted objects can be restored for a while: soft delete",
            Self::More => "labels, IAM, CORS details, location, storage class and more",
        }
    }
}

impl fmt::Display for Goal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// A setting behind "More settings", in the order its questions come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detail {
    Cors,
    Labels,
    IamBindings,
    PublicAccessPrevention,
    EventBasedHold,
    Location,
    StorageClass,
    Project,
}

impl Detail {
    const ALL: [Self; 8] = [
        Self::Cors,
        Self::Labels,
        Self::IamBindings,
        Self::PublicAccessPrevention,
        Self::EventBasedHold,
        Self::Location,
        Self::StorageClass,
        Self::Project,
    ];
}

impl Noted for Detail {
    fn name(self) -> &'static str {
        match self {
            Self::Cors => "CORS details",
            Self::Labels => "Labels",
            Self::IamBindings => "IAM bindings",
            Self::PublicAccessPrevention => "Public access prevention",
            Self::EventBasedHold => "Event-based hold",
            Self::Location => "Location",
            Self::StorageClass => "Storage class",
            Self::Project => "Owning project",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::Cors => "choose the methods, response headers and max age yourself",
            Self::Labels => "key=value pairs on the bucket",
            Self::IamBindings => "grant roles to users and service accounts",
            Self::PublicAccessPrevention => {
                "enforced refuses grants to allUsers and allAuthenticatedUsers"
            }
            Self::EventBasedHold => "new objects start held and cannot be deleted until released",
            Self::Location => "US unless set",
            Self::StorageClass => "STANDARD unless set",
            Self::Project => "seed_project, local-project by default, unless set",
        }
    }
}

impl fmt::Display for Detail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// What to do once the buckets so far are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Write,
    Add,
    Change,
    Remove,
    Cancel,
}

impl Next {
    /// The choices with `count` buckets: writing, changing and removing need one.
    fn offered(count: usize) -> Vec<Self> {
        if count == 0 {
            vec![Self::Add, Self::Cancel]
        } else {
            vec![
                Self::Write,
                Self::Add,
                Self::Change,
                Self::Remove,
                Self::Cancel,
            ]
        }
    }
}

impl fmt::Display for Next {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Write => "Write it",
            Self::Add => "Add a bucket",
            Self::Change => "Change a bucket",
            Self::Remove => "Remove a bucket",
            Self::Cancel => "Cancel and write nothing",
        })
    }
}

/// Whether the questions end by asking where to save a JSON document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileQuestion {
    /// The document would go to a terminal, where it only scrolls past.
    Ask,
    /// `--output` names the file, or stdout goes to one.
    Skip,
}

/// The roles most local setups grant, with what each allows; the rest come after them.
const ROLES: [Offer; 5] = [
    Offer {
        value: "roles/storage.objectViewer",
        note: "read and list objects",
    },
    Offer {
        value: "roles/storage.objectCreator",
        note: "create objects, without reading or overwriting them",
    },
    Offer {
        value: "roles/storage.objectUser",
        note: "read, create, overwrite and delete objects",
    },
    Offer {
        value: "roles/storage.objectAdmin",
        note: "full control of objects",
    },
    Offer {
        value: "roles/storage.admin",
        note: "full control of the bucket and its objects",
    },
];

/// The principals a binding can name without an identity.
const MEMBERS: [Offer; 2] = [
    Offer {
        value: "allUsers",
        note: "anyone, signed in or not",
    },
    Offer {
        value: "allAuthenticatedUsers",
        note: "anyone signed in",
    },
];

const SOFT_DELETE_RETENTIONS: [SecondsPick; 5] = [
    SecondsPick::Offered {
        seconds: 604_800,
        means: "7 days, the Cloud Storage default",
    },
    SecondsPick::Offered {
        seconds: 0,
        means: "Off: keep no deleted objects",
    },
    SecondsPick::Offered {
        seconds: 2_592_000,
        means: "30 days",
    },
    SecondsPick::Offered {
        seconds: generate::MAX_SOFT_DELETE_RETENTION_SECONDS,
        means: "90 days, the longest",
    },
    SecondsPick::Other,
];

/// The location question offers the multi-regions, then any other supported location.
const OTHER_LOCATION: &str = "Another location";

/// Asks on the terminal how the server reads the document unless a flag chose its format, then
/// for the buckets, and for a file to save a JSON document in when `file` asks for one.
pub(super) fn ask(format: Option<Format>, file: FileQuestion) -> Result<Answers, InquireError> {
    ask_on(&mut Inquire, format, file)
}

fn ask_on(
    term: &mut impl Terminal,
    format: Option<Format>,
    file: FileQuestion,
) -> Result<Answers, InquireError> {
    let format = match format {
        Some(format) => format,
        None => {
            choose_one(
                term,
                Question::new("How do you run gcs-mimic?"),
                &Launch::ALL,
            )?
            .0
        }
    };
    let file = match format {
        Format::Json => file,
        Format::Env | Format::Compose => FileQuestion::Skip,
    };
    document(term, format, file)
}

/// Asks for the first buckets, then shows the buckets and asks what next until they are written
/// or cancelled. Esc inside a bucket's questions drops only that bucket's answers and comes back
/// here; the first bucket has nothing to come back to, so Esc there ends the questions. A
/// document that cannot be written or saved also comes back here, so no answer is lost.
fn document(
    term: &mut impl Terminal,
    format: Format,
    file: FileQuestion,
) -> Result<Answers, InquireError> {
    let mut asked = bucket(term, None, &[], &[])?;
    loop {
        let shown: String = asked.iter().map(|entry| entry.bucket.to_string()).collect();
        term.show(&format!("\n{shown}"));
        let next = match select_one(term, Question::new("Next:"), &Next::offered(asked.len())) {
            Err(InquireError::OperationCanceled) => Next::Cancel,
            next => next?,
        };
        match next {
            Next::Write => {
                let document =
                    Document::new(asked.iter().map(|entry| entry.bucket.clone()).collect());
                let text = match document.render(format) {
                    Ok(text) => text,
                    Err(error) => {
                        term.show(&format!("{error}\n"));
                        continue;
                    }
                };
                let saved = match file {
                    FileQuestion::Skip => None,
                    FileQuestion::Ask => match save_document(term, &text)? {
                        Saved::File(path) => Some(path),
                        Saved::Here => None,
                        Saved::Back => continue,
                    },
                };
                return Ok(Answers {
                    document,
                    format,
                    text,
                    saved,
                });
            }
            Next::Add => {
                let taken = names(&asked, None);
                let latest = latest_origins(&asked).to_vec();
                if let Some(added) = back(bucket(term, None, &taken, &latest))? {
                    asked.extend(added);
                }
            }
            Next::Change => {
                let Some(index) = back(which(term, &asked, "Change which bucket?"))? else {
                    continue;
                };
                let taken = names(&asked, Some(index));
                let Some(previous) = asked.get(index) else {
                    continue;
                };
                let changed = back(bucket(term, Some(previous), &taken, latest_origins(&asked)))?;
                if let Some(changed) = changed {
                    asked.splice(index..=index, changed);
                }
            }
            Next::Remove => {
                if let Some(index) = back(which(term, &asked, "Remove which bucket?"))? {
                    asked.remove(index);
                }
            }
            Next::Cancel => {
                let discard =
                    term.confirm("Discard these buckets and write nothing?", EnterMeans::No);
                if back(discard)? == Some(true) {
                    return Err(InquireError::OperationCanceled);
                }
            }
        }
    }
}

/// Esc as "go back": `None` instead of the cancellation, which other errors keep.
fn back<T>(answer: Result<T, InquireError>) -> Result<Option<T>, InquireError> {
    match answer {
        Err(InquireError::OperationCanceled) => Ok(None),
        answer => answer.map(Some),
    }
}

/// A bucket with the answers that made it, so that asking it again starts from them.
struct Asked {
    bucket: Bucket,
    goals: Vec<Goal>,
    details: Vec<Detail>,
    origins: Vec<CorsOrigin>,
}

/// The names of the buckets other than the one at `except`, which a new name must not repeat.
fn names(asked: &[Asked], except: Option<usize>) -> Vec<String> {
    asked
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != except)
        .map(|(_, entry)| entry.bucket.name().to_owned())
        .collect()
}

/// The CORS origins of the latest bucket that has them, which a new bucket usually shares.
fn latest_origins(asked: &[Asked]) -> &[CorsOrigin] {
    asked
        .iter()
        .rev()
        .map(|entry| entry.origins.as_slice())
        .find(|origins| !origins.is_empty())
        .unwrap_or_default()
}

/// The index of the bucket to act on; with one bucket there is nothing to ask.
fn which(term: &mut impl Terminal, asked: &[Asked], question: &str) -> Result<usize, InquireError> {
    if let [_] = asked {
        return Ok(0);
    }
    let names: Vec<_> = asked
        .iter()
        .map(|entry| terminal::Entry::plain(entry.bucket.name()))
        .collect();
    term.select(Question::new(question), &names)
}

/// Where a written document went.
enum Saved {
    File(PathBuf),
    /// It goes to stdout, the terminal.
    Here,
    /// Esc: back to the buckets.
    Back,
}

/// What to do when the file cannot be created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fallback {
    Another,
    Here,
}

impl Fallback {
    const ALL: [Self; 2] = [Self::Another, Self::Here];
}

impl Noted for Fallback {
    fn name(self) -> &'static str {
        match self {
            Self::Another => "Choose another file",
            Self::Here => "Print it here instead",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::Another => "in a directory you can write to",
            Self::Here => "and copy it into a file yourself",
        }
    }
}

impl fmt::Display for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// Asks for a file and saves `text` in it, asking again when the file cannot be created. In
/// the image the working directory is not writable, and a file there would leave with the
/// container anyway, so a failure also offers printing the document instead.
fn save_document(term: &mut impl Terminal, text: &str) -> Result<Saved, InquireError> {
    loop {
        let Some(path) = back(save_as(term))? else {
            return Ok(Saved::Back);
        };
        let Err(error) = save_new(&path, text) else {
            return Ok(Saved::File(path));
        };
        term.show(&format!("{error}\n"));
        match back(choose_one(term, Question::new("Then:"), &Fallback::ALL))? {
            Some(Fallback::Another) => {}
            Some(Fallback::Here) => return Ok(Saved::Here),
            None => return Ok(Saved::Back),
        }
    }
}

/// Asks where to save a JSON document, refusing an existing file as `--output` does.
fn save_as(term: &mut impl Terminal) -> Result<PathBuf, InquireError> {
    let check: Check = Rc::new(|path: &str| new_file(Path::new(path)));
    term.text(
        Question::new("Save the document as:"),
        Some("buckets.json"),
        check,
    )
    .map(PathBuf::from)
}

/// Whether the command can create `path`: an existing file is kept, so it must not exist, and
/// its directory must.
fn new_file(path: &Path) -> Validation {
    if path.as_os_str().is_empty() {
        return Validation::Invalid("Enter a file name, such as buckets.json.".into());
    }
    if path.exists() {
        return Validation::Invalid("That file exists and is kept; choose another name.".into());
    }
    match path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() && !directory.is_dir() => {
            Validation::Invalid("That directory does not exist.".into())
        }
        Some(_) | None => Validation::Valid,
    }
}

/// Asks for buckets: their names, what they are for, and only what those goals need. Several
/// names share one set of answers. Asked again, a bucket starts from `previous`: its name filled
/// in and its goals and settings marked. `latest` are the origins a new bucket is offered first.
fn bucket(
    term: &mut impl Terminal,
    previous: Option<&Asked>,
    taken: &[String],
    latest: &[CorsOrigin],
) -> Result<Vec<Asked>, InquireError> {
    let names = bucket_names(term, previous.map(|entry| entry.bucket.name()), taken)?;
    let mut bucket = Bucket::new(names.first()).map_err(custom)?;
    let goals = choose_many(
        term,
        Question::new(&format!("What should {names} do?"))
            .with_help("Space selects, Enter continues; choose none for default settings"),
        &Goal::ALL,
        &positions(&Goal::ALL, previous.map_or(&[], |entry| &entry.goals)),
    )?;
    let details = if goals.contains(&Goal::More) {
        choose_many(
            term,
            Question::new(&format!("More settings for {names}:"))
                .with_help("Space selects, Enter continues"),
            &Detail::ALL,
            &positions(&Detail::ALL, previous.map_or(&[], |entry| &entry.details)),
        )?
    } else {
        Vec::new()
    };
    let access = cors::Access {
        uploads: goals.contains(&Goal::BrowserUploads),
        downloads: goals.contains(&Goal::BrowserDownloads),
        custom: details.contains(&Detail::Cors),
    };
    let origins = if access.wanted() {
        let before = previous
            .map(|entry| entry.origins.as_slice())
            .filter(|origins| !origins.is_empty())
            .unwrap_or(latest);
        cors::ask(term, &mut bucket, access, before)?
    } else {
        Vec::new()
    };
    for goal in &goals {
        pursue(term, &mut bucket, *goal)?;
    }
    for detail in &details {
        configure(term, &mut bucket, *detail, &goals)?;
    }
    let others = names
        .rest()
        .iter()
        .map(|name| {
            Ok(Asked {
                bucket: bucket.with_name(name).map_err(custom)?,
                goals: goals.clone(),
                details: details.clone(),
                origins: origins.clone(),
            })
        })
        .collect::<Result<Vec<_>, InquireError>>()?;
    let first = Asked {
        bucket,
        goals,
        details,
        origins,
    };
    Ok(iter::once(first).chain(others).collect())
}

fn bucket_names(
    term: &mut impl Terminal,
    name: Option<&str>,
    taken: &[String],
) -> Result<Names, InquireError> {
    let checked = taken.to_vec();
    let check: Check = Rc::new(move |answer: &str| valid(Names::parse(answer, &checked).map(drop)));
    let question = Question::new("Bucket name:")
        .with_help("Several names, comma-separated, share the answers that follow");
    let answer = term.text(question, name, check)?;
    Names::parse(&answer, taken).map_err(custom)
}

/// Records what `goal` needs beyond the CORS rule, which [`cors::ask`] records.
fn pursue(term: &mut impl Terminal, bucket: &mut Bucket, goal: Goal) -> Result<(), InquireError> {
    let setting = match goal {
        Goal::PublicRead => Setting::PublicRead,
        Goal::KeepVersions => Setting::Versioning,
        Goal::KeepDeleted => {
            let Some(seconds) = pick_seconds(
                term,
                "Keep deleted objects for:",
                "Keep deleted objects for how many seconds?",
                &SOFT_DELETE_RETENTIONS,
                generate::MAX_SOFT_DELETE_RETENTION_SECONDS,
            )?
            else {
                return Ok(());
            };
            Setting::SoftDeleteRetentionSeconds(seconds)
        }
        Goal::BrowserUploads | Goal::BrowserDownloads | Goal::More => return Ok(()),
    };
    bucket.apply(setting).map_err(custom)
}

/// Asks for what `detail` needs and records it.
fn configure(
    term: &mut impl Terminal,
    bucket: &mut Bucket,
    detail: Detail,
    goals: &[Goal],
) -> Result<(), InquireError> {
    let setting = match detail {
        // Asked with the browser goals, before the other details.
        Detail::Cors => return Ok(()),
        Detail::Labels => return labels(term, bucket),
        Detail::IamBindings => return bindings(term, bucket),
        Detail::PublicAccessPrevention => Setting::PublicAccessPrevention(prevention(term, goals)?),
        Detail::EventBasedHold => Setting::DefaultEventBasedHold,
        Detail::Location => Setting::Location(location(term)?),
        Detail::StorageClass => Setting::StorageClass(
            select_one(
                term,
                Question::new("Storage class:"),
                &generate::storage_classes(),
            )?
            .to_owned(),
        ),
        Detail::Project => Setting::Project(term.text(
            Question::new("Owning project ID:").with_help(
                "Without this setting the bucket belongs to seed_project, local-project by default",
            ),
            None,
            required(),
        )?),
    };
    bucket.apply(setting).map_err(custom)
}

/// A public access prevention choice with what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Prevention(PublicAccessPrevention);

impl Noted for Prevention {
    fn name(self) -> &'static str {
        self.0.as_str()
    }

    fn note(self) -> &'static str {
        match self.0 {
            PublicAccessPrevention::Inherited => "follows the project's setting",
            PublicAccessPrevention::Enforced => {
                "refuses grants to allUsers and allAuthenticatedUsers"
            }
        }
    }
}

impl fmt::Display for Prevention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// Public read and enforced prevention cannot go together, so a bucket that chose public read
/// is offered only `inherited`.
fn prevention(
    term: &mut impl Terminal,
    goals: &[Goal],
) -> Result<PublicAccessPrevention, InquireError> {
    let public_read = goals.contains(&Goal::PublicRead);
    let choices: Vec<_> = PublicAccessPrevention::ALL
        .iter()
        .copied()
        .filter(|prevention| !public_read || *prevention == PublicAccessPrevention::Inherited)
        .map(Prevention)
        .collect();
    let question = Question::new("Public access prevention:");
    let question = if public_read {
        question.with_help("enforced would refuse public read, so it is not offered")
    } else {
        question
    };
    Ok(choose_one(term, question, &choices)?.0)
}

fn location(term: &mut impl Terminal) -> Result<String, InquireError> {
    let mut choices = generate::multi_regions().to_vec();
    choices.push(OTHER_LOCATION);
    let chosen = select_one(term, Question::new("Location:"), &choices)?;
    if chosen != OTHER_LOCATION {
        return Ok(chosen.to_owned());
    }
    let check: Check = Rc::new(|value: &str| match generate::location(value) {
        Some(_) => Validation::Valid,
        None => Validation::Invalid("That location is not supported.".into()),
    });
    let named = term.text(
        Question::new("Location, such as US-CENTRAL1 or EUROPE-WEST1:"),
        None,
        check,
    )?;
    generate::location(&named)
        .map(str::to_owned)
        .ok_or_else(|| InquireError::Custom("location changed after validation".into()))
}

fn labels(term: &mut impl Terminal, bucket: &mut Bucket) -> Result<(), InquireError> {
    loop {
        let check: Check = Rc::new(|answer: &str| {
            if answer.is_empty() {
                Validation::Valid
            } else {
                valid(answer.parse::<Label>().map(drop))
            }
        });
        let answer = term.text(
            Question::new("Label as key=value (empty to finish):"),
            None,
            check,
        )?;
        if answer.is_empty() {
            return Ok(());
        }
        let label = answer.parse().map_err(custom)?;
        // A repeated key asks again instead of ending the questions.
        if let Err(error) = bucket.apply(Setting::Label(label)) {
            term.show(&format!("{error}\n"));
        }
    }
}

fn bindings(term: &mut impl Terminal, bucket: &mut Bucket) -> Result<(), InquireError> {
    loop {
        let role = role(term)?;
        let members = pick_many::<Members>(
            term,
            "Members:",
            &MEMBERS,
            &[Pick::Other("Another member, such as user:dev@example.com")],
            Need::AtLeastOne,
            &[],
        )?;
        bucket
            .apply(Setting::IamBinding {
                role,
                members: members.into_iter().collect(),
            })
            .map_err(custom)?;
        if !term.confirm("Add another IAM binding?", EnterMeans::No)? {
            return Ok(());
        }
    }
}

/// An entry of the role question: a common role, or the entry that lists the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RolePick {
    Common(Offer),
    Other,
}

impl Noted for RolePick {
    fn name(self) -> &'static str {
        match self {
            Self::Common(offer) => offer.value,
            Self::Other => "Another role…",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::Common(offer) => offer.note,
            Self::Other => "the other roles a bucket policy can grant",
        }
    }
}

impl fmt::Display for RolePick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// The common roles with what they allow, then any other role a bucket policy can grant.
fn role(term: &mut impl Terminal) -> Result<String, InquireError> {
    let mut entries: Vec<RolePick> = ROLES.iter().copied().map(RolePick::Common).collect();
    entries.push(RolePick::Other);
    match choose_one(term, Question::new("IAM role:"), &entries)? {
        RolePick::Common(offer) => Ok(offer.value.to_owned()),
        RolePick::Other => {
            Ok(select_one(term, Question::new("IAM role:"), &other_roles())?.to_owned())
        }
    }
}

/// The roles a bucket policy can grant that [`ROLES`] does not list.
fn other_roles() -> Vec<&'static str> {
    generate::bucket_roles()
        .into_iter()
        .filter(|role| ROLES.iter().all(|offer| offer.value != *role))
        .collect()
}

#[cfg(test)]
mod script;
#[cfg(test)]
mod tests;
