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
    self, AuthMode, Bucket, CorsOrigin, Document, Format, Label, Members, PublicAccessPrevention,
    Setting,
};
use inquire::{InquireError, validator::Validation};
use lists::{
    Noted, Offer, Pick, SecondsPick, choose_many, choose_one, choose_one_at, custom, pick_many,
    pick_seconds, positions, required, select_one, valid, write_noted,
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
    /// Chosen like the others, so that Enter on an entry that Space never marked is refused at
    /// once instead of quietly leaving the bucket with default settings.
    Defaults,
    BrowserUploads,
    BrowserDownloads,
    KeepVersions,
    KeepDeleted,
    More,
}

impl Goal {
    /// Public read is no goal: whether a bucket is public or private is asked before its goals.
    const ALL: [Self; 6] = [
        Self::Defaults,
        Self::BrowserUploads,
        Self::BrowserDownloads,
        Self::KeepVersions,
        Self::KeepDeleted,
        Self::More,
    ];
}

impl Noted for Goal {
    fn name(self) -> &'static str {
        match self {
            Self::Defaults => "Default settings",
            Self::BrowserUploads => "Browser uploads",
            Self::BrowserDownloads => "Browser downloads",
            Self::KeepVersions => "Keep old versions",
            Self::KeepDeleted => "Keep deleted objects",
            Self::More => "More settings…",
        }
    }

    // Each note says when a bucket needs the goal, since the members' names do not: a page
    // shows objects through <img> and links without CORS.
    fn note(self) -> &'static str {
        match self {
            Self::Defaults => "nothing more to set up",
            Self::BrowserUploads => "your web or mobile app uploads files: CORS for PUT and POST",
            Self::BrowserDownloads => {
                "your page reads objects with fetch(); <img> and links need none"
            }
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

/// Who reads a bucket, asked right after its names: people think of a bucket as public or
/// private before what it is for. Only strict mode keeps a bucket private, and the mode is the
/// whole server's, so one private bucket puts every bucket in strict mode, where a public one
/// then needs public read; without one, the default permissive mode lets anyone read them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readers {
    Public,
    Private,
}

impl Readers {
    /// Public first, where the cursor starts: private moves the application's servers to another
    /// port, so Enter alone changes nothing.
    const ALL: [Self; 2] = [Self::Public, Self::Private];

    /// The bucket's line in the summary. It says what was answered in either mode, since public
    /// read is written only when strict mode needs it.
    fn summary(self) -> &'static str {
        match self {
            Self::Public => "public: anyone reads its objects",
            Self::Private => "private: only your servers and the URLs they sign read it",
        }
    }
}

impl Noted for Readers {
    fn name(self) -> &'static str {
        match self {
            Self::Public => "Public",
            Self::Private => "Private",
        }
    }

    // The notes say what each choice changes rather than what the word means: "public" alone
    // would push someone who does not care toward private, which changes how the application
    // connects.
    fn note(self) -> &'static str {
        match self {
            Self::Public => "anyone reads its objects; the default, nothing else changes",
            Self::Private => {
                "only your servers read it; the server turns strict and they move to port 4444"
            }
        }
    }
}

impl fmt::Display for Readers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// The mode as `--auth-mode` or the other buckets settle it before a bucket is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settled {
    /// `--auth-mode` chose it, so no answer changes it.
    Chosen(AuthMode),
    /// The other buckets need it, until a private answer makes it strict.
    Needed(AuthMode),
}

impl Settled {
    /// What `--auth-mode` chose, or else what `others` need: strict once one is private.
    fn of<'a>(chosen: Option<AuthMode>, mut others: impl Iterator<Item = &'a Asked>) -> Self {
        match chosen {
            Some(mode) => Self::Chosen(mode),
            None if others.any(|entry| entry.readers == Readers::Private) => {
                Self::Needed(AuthMode::Strict)
            }
            None => Self::Needed(AuthMode::Permissive),
        }
    }

    fn mode(self) -> AuthMode {
        match self {
            Self::Chosen(mode) | Self::Needed(mode) => mode,
        }
    }

    /// The summary's first line in strict mode, which every bucket shares, so that a private
    /// answer's effect on the others stays in sight.
    fn summary(self) -> &'static str {
        match self {
            Self::Chosen(AuthMode::Strict) => {
                "Strict mode, as --auth-mode chose: your servers use port 4444\n"
            }
            Self::Needed(AuthMode::Strict) => {
                "Strict mode for the whole server, since a bucket is private: your servers use port 4444\n"
            }
            Self::Chosen(AuthMode::Permissive) | Self::Needed(AuthMode::Permissive) => "",
        }
    }
}

/// Said when a private answer turns strict mode on: it changes every bucket and how the
/// application connects, which the answer's note alone could leave unread.
const STRICT_ON: &str =
    "Strict mode is now on for the whole server: your servers then use port 4444,
and browsers reach private objects through signed URLs.
";

/// Said when no private bucket is left, so that strict mode does not end unnoticed.
const STRICT_OFF: &str =
    "No bucket is private now, so the server keeps the default permissive mode.\n";

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
/// for the buckets, who reads them unless a flag chose the mode, and for a file to save a JSON
/// document in when `file` asks for one.
pub(super) fn ask(
    format: Option<Format>,
    auth_mode: Option<AuthMode>,
    file: FileQuestion,
) -> Result<Answers, InquireError> {
    ask_on(&mut Inquire, format, auth_mode, file)
}

/// Esc goes back one step everywhere, so it ends the questions only on the first one: from the
/// first bucket names back to how gcs-mimic runs, or out when a flag chose the format.
fn ask_on(
    term: &mut impl Terminal,
    format: Option<Format>,
    auth_mode: Option<AuthMode>,
    file: FileQuestion,
) -> Result<Answers, InquireError> {
    loop {
        let chosen = match format {
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
        let file = match chosen {
            Format::Json => file,
            Format::Env | Format::Compose => FileQuestion::Skip,
        };
        match document(term, chosen, auth_mode, file)? {
            Some(answers) => return Ok(answers),
            None if format.is_none() => {}
            None => return Err(InquireError::OperationCanceled),
        }
    }
}

/// Asks for the first buckets, then shows the buckets and asks what next until they are written
/// or cancelled; `None` is Esc on the first bucket names, which goes back before them. Esc in a
/// later bucket's names comes back here, and a document that cannot be written or saved does
/// too, so no answer is lost. The document is in strict mode when `chosen` says so or a bucket
/// is private.
fn document(
    term: &mut impl Terminal,
    format: Format,
    chosen: Option<AuthMode>,
    file: FileQuestion,
) -> Result<Option<Answers>, InquireError> {
    let first = Settled::of(chosen, iter::empty());
    let Some(mut asked) = bucket(term, None, &[], &[], first)? else {
        return Ok(None);
    };
    loop {
        let settled = Settled::of(chosen, asked.iter());
        let shown: String = asked.iter().map(Asked::to_string).collect();
        term.show(&format!("\n{}{shown}", settled.summary()));
        let next = match select_one(term, Question::new("Next:"), &Next::offered(asked.len())) {
            Err(InquireError::OperationCanceled) => Next::Cancel,
            next => next?,
        };
        match next {
            Next::Write => {
                let mode = settled.mode();
                let buckets = asked
                    .iter()
                    .map(|entry| entry.written(mode))
                    .collect::<Result<_, _>>()?;
                let document = Document::new(buckets).with_auth_mode(mode);
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
                // What follows, the document or the next step, would run on from the last
                // answer without a blank line.
                term.show("\n");
                return Ok(Some(Answers {
                    document,
                    format,
                    text,
                    saved,
                }));
            }
            Next::Add => {
                let taken = names(&asked, None);
                let latest = latest_origins(&asked).to_vec();
                if let Some(added) = bucket(term, None, &taken, &latest, settled)? {
                    asked.extend(added);
                }
            }
            Next::Change => {
                let Some(index) = back(which(term, &asked, "Change which bucket?"))? else {
                    continue;
                };
                let taken = names(&asked, Some(index));
                let others = asked
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, entry)| entry);
                let others = Settled::of(chosen, others);
                let Some(previous) = asked.get(index) else {
                    continue;
                };
                let latest = latest_origins(&asked);
                if let Some(changed) = bucket(term, Some(previous), &taken, latest, others)? {
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
        let now = Settled::of(chosen, asked.iter());
        if settled == Settled::Needed(AuthMode::Strict)
            && now == Settled::Needed(AuthMode::Permissive)
        {
            term.show(STRICT_OFF);
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
    readers: Readers,
    goals: Vec<Goal>,
    details: Vec<Detail>,
    origins: Vec<CorsOrigin>,
}

impl Asked {
    /// The entry to write in `mode`. A public bucket needs public read in strict mode only:
    /// permissive mode lets anyone read it anyway, and a bucket without settings then keeps the
    /// short name-only form.
    fn written(&self, mode: AuthMode) -> Result<Bucket, InquireError> {
        let mut bucket = self.bucket.clone();
        match (mode, self.readers) {
            (AuthMode::Strict, Readers::Public) => {
                bucket.apply(Setting::PublicRead).map_err(custom)?;
            }
            (AuthMode::Strict, Readers::Private)
            | (AuthMode::Permissive, Readers::Public | Readers::Private) => {}
        }
        Ok(bucket)
    }
}

/// The bucket as the summary shows it: its name, who reads it, then one line per setting.
impl fmt::Display for Asked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.bucket.name())?;
        writeln!(f, "  {}", self.readers.summary())?;
        for line in self.bucket.summary_lines() {
            writeln!(f, "  {line}")?;
        }
        Ok(())
    }
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
    term.select(Question::new(question), &names, 0)
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

/// Asks for buckets: their names, who reads them, then what they are for and only what those
/// goals need. Several names share one set of answers. Asked again, a bucket starts from
/// `previous`: its name filled in, its readers chosen and its goals and settings marked.
/// `latest` are the origins a new bucket is offered first, and `settled` the mode before it.
/// Esc after the names goes back to them with what was typed filled in, and Esc on the names
/// gives `None`, going back before them.
fn bucket(
    term: &mut impl Terminal,
    previous: Option<&Asked>,
    taken: &[String],
    latest: &[CorsOrigin],
    settled: Settled,
) -> Result<Option<Vec<Asked>>, InquireError> {
    let mut typed = previous.map(|entry| entry.bucket.name().to_owned());
    loop {
        let Some(names) = back(bucket_names(term, typed.as_deref(), taken))? else {
            return Ok(None);
        };
        typed = Some(names.comma_separated());
        if let Some(asked) = readers_and_goals(term, &names, previous, latest, settled)? {
            return Ok(Some(asked));
        }
    }
}

/// Asks who reads the named buckets, then their goals; Esc on the goals asks who reads again,
/// starting from the answer given, and Esc there gives `None`, back to the names. With
/// `--auth-mode permissive` anyone reads every bucket, so nothing is asked about it and Esc on
/// the goals goes back to the names.
fn readers_and_goals(
    term: &mut impl Terminal,
    names: &Names,
    previous: Option<&Asked>,
    latest: &[CorsOrigin],
    settled: Settled,
) -> Result<Option<Vec<Asked>>, InquireError> {
    if settled == Settled::Chosen(AuthMode::Permissive) {
        return back(goals_and_settings(
            term,
            names,
            Readers::Public,
            previous,
            latest,
        ));
    }
    let mut answered = previous.map(|entry| entry.readers);
    loop {
        let question = format!("Who can read {names}?");
        let start = positions(&Readers::ALL, answered.as_slice());
        let readers = choose_one_at(
            term,
            Question::new(&question),
            &Readers::ALL,
            start.first().copied().unwrap_or_default(),
        );
        let Some(readers) = back(readers)? else {
            return Ok(None);
        };
        answered = Some(readers);
        if readers == Readers::Private && settled == Settled::Needed(AuthMode::Permissive) {
            term.show(STRICT_ON);
        }
        if let Some(asked) = back(goals_and_settings(term, names, readers, previous, latest))? {
            return Ok(Some(asked));
        }
    }
}

/// Asks what the named buckets are for and what those goals need, and records it for each
/// with who reads it.
fn goals_and_settings(
    term: &mut impl Terminal,
    names: &Names,
    readers: Readers,
    previous: Option<&Asked>,
    latest: &[CorsOrigin],
) -> Result<Vec<Asked>, InquireError> {
    let mut bucket = Bucket::new(names.first()).map_err(custom)?;
    // Default settings and any goal contradict each other on the answer line.
    let need = Goal::ALL
        .iter()
        .position(|goal| *goal == Goal::Defaults)
        .map_or(Need::AtLeastOne, Need::AtLeastOneWithAlone);
    let goals = choose_many(
        term,
        Question::new(&format!("What should {names} do?"))
            .with_help("Space selects, Enter continues"),
        &Goal::ALL,
        &positions(&Goal::ALL, previous.map_or(&[], |entry| &entry.goals)),
        need,
    )?;
    let details = if goals.contains(&Goal::More) {
        choose_many(
            term,
            Question::new(&format!("More settings for {names}:"))
                .with_help("Space selects, Enter continues"),
            &Detail::ALL,
            &positions(&Detail::ALL, previous.map_or(&[], |entry| &entry.details)),
            Need::Optional,
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
        configure(term, &mut bucket, *detail, readers)?;
    }
    let others = names
        .rest()
        .iter()
        .map(|name| {
            Ok(Asked {
                bucket: bucket.with_name(name).map_err(custom)?,
                readers,
                goals: goals.clone(),
                details: details.clone(),
                origins: origins.clone(),
            })
        })
        .collect::<Result<Vec<_>, InquireError>>()?;
    let first = Asked {
        bucket,
        readers,
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
        Goal::Defaults | Goal::BrowserUploads | Goal::BrowserDownloads | Goal::More => {
            return Ok(());
        }
    };
    bucket.apply(setting).map_err(custom)
}

/// Asks for what `detail` needs and records it.
fn configure(
    term: &mut impl Terminal,
    bucket: &mut Bucket,
    detail: Detail,
    readers: Readers,
) -> Result<(), InquireError> {
    let setting = match detail {
        // Asked with the browser goals, before the other details.
        Detail::Cors => return Ok(()),
        Detail::Labels => return labels(term, bucket),
        Detail::IamBindings => return bindings(term, bucket),
        Detail::PublicAccessPrevention => {
            Setting::PublicAccessPrevention(prevention(term, readers)?)
        }
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

/// Public read and enforced prevention cannot go together, and a public bucket gets public read
/// as soon as another bucket is private, so a public bucket is offered only `inherited`.
fn prevention(
    term: &mut impl Terminal,
    readers: Readers,
) -> Result<PublicAccessPrevention, InquireError> {
    let public = readers == Readers::Public;
    let choices: Vec<_> = PublicAccessPrevention::ALL
        .iter()
        .copied()
        .filter(|prevention| !public || *prevention == PublicAccessPrevention::Inherited)
        .map(Prevention)
        .collect();
    let question = Question::new("Public access prevention:");
    let question = if public {
        question
            .with_help("enforced would refuse a public bucket's public read, so it is not offered")
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
