//! List questions: values are picked from offers wherever one can be offered, so typing is left
//! to values nobody could list, since a typed value is where mistakes come from.

use super::terminal::{Check, Entry, Need, Question, Terminal};
use inquire::{InquireError, validator::Validation};
use std::{fmt, num::NonZeroU16, rc::Rc, str::FromStr};

/// A value a list offers, with what it is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Offer {
    pub(super) value: &'static str,
    pub(super) note: &'static str,
}

impl Noted for Offer {
    fn name(self) -> &'static str {
        self.value
    }

    fn note(self) -> &'static str {
        self.note
    }
}

impl fmt::Display for Offer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_noted(*self, f)
    }
}

/// An entry of a list question: an offered value, or one of the entries after the offers that
/// ask for values by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pick {
    Offered(Offer),
    /// Asks for ports on this origin, each answer giving `{origin}:{port}`.
    Port(&'static str),
    /// Asks for whole values, introduced by this label.
    Other(&'static str),
}

impl Pick {
    /// What the answer line keeps: the value alone, since its note only helped choose, or a
    /// pointer to the typed values that follow on their own lines.
    pub(super) fn answer(self) -> &'static str {
        match self {
            Self::Offered(offer) => offer.value,
            Self::Port(_) | Self::Other(_) => "typed below",
        }
    }
}

impl fmt::Display for Pick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offered(offer) => offer.fmt(f),
            Self::Port(origin) => write!(f, "{origin}:PORT  (type the port, such as 3001)"),
            Self::Other(label) => f.write_str(label),
        }
    }
}

/// A port typed for [`Pick::Port`], from 1 to 65535.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Port(NonZeroU16);

impl Port {
    /// The origin `origin` on this port, such as `http://localhost:3001`.
    pub(super) fn on(self, origin: &str) -> String {
        format!("{origin}:{}", self.0)
    }
}

impl FromStr for Port {
    type Err = InvalidPort;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self).map_err(|_| InvalidPort)
    }
}

/// A typed port that is not a number from 1 to 65535.
#[derive(Debug)]
pub(super) struct InvalidPort;

impl fmt::Display for InvalidPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Enter a port from 1 to 65535, for example 3001.")
    }
}

impl std::error::Error for InvalidPort {}

/// An entry of a question for a number of seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SecondsPick {
    Offered {
        seconds: u64,
        means: &'static str,
    },
    /// The member stays unset.
    Unset(&'static str),
    Other,
}

impl fmt::Display for SecondsPick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offered { seconds, means } => write!(f, "{means} ({seconds} seconds)"),
            Self::Unset(label) => f.write_str(label),
            Self::Other => f.write_str("Another number of seconds…"),
        }
    }
}

/// A list entry shown with a note that only helps choose it, so the answer line keeps the name.
pub(super) trait Noted: Copy {
    fn name(self) -> &'static str;
    fn note(self) -> &'static str;
}

/// How a [`Noted`] entry shows in its list: its name, then its note.
pub(super) fn write_noted(entry: impl Noted, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}  ({})", entry.name(), entry.note())
}

fn noted(entry: impl Noted + fmt::Display) -> Entry {
    Entry {
        shown: entry.to_string(),
        answer: entry.name().to_owned(),
    }
}

/// The indices of `all` that `chosen` holds, which mark earlier answers when a list asks again.
pub(super) fn positions<T: PartialEq>(all: &[T], chosen: &[T]) -> Vec<usize> {
    all.iter()
        .enumerate()
        .filter(|(_, entry)| chosen.contains(entry))
        .map(|(index, _)| index)
        .collect()
}

/// The entry at `index`, which a terminal returns for an entry it showed.
fn entry<T: Clone>(entries: &[T], index: usize) -> Result<T, InquireError> {
    entries
        .get(index)
        .cloned()
        .ok_or_else(|| custom(UnknownEntry(index)))
}

/// A terminal answered with an entry it was not shown.
#[derive(Debug)]
struct UnknownEntry(usize);

impl fmt::Display for UnknownEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "The terminal chose entry {}, which it was not shown.",
            self.0
        )
    }
}

impl std::error::Error for UnknownEntry {}

/// Asks to choose one of `entries`, with the answer line keeping what the list shows.
pub(super) fn select_one<T>(
    term: &mut impl Terminal,
    question: Question<'_>,
    entries: &[T],
) -> Result<T, InquireError>
where
    T: Clone + fmt::Display,
{
    let shown: Vec<_> = entries.iter().map(Entry::plain).collect();
    entry(entries, term.select(question, &shown)?)
}

/// Asks to choose one of `entries`, with the answer line naming it without its note.
pub(super) fn choose_one<T>(
    term: &mut impl Terminal,
    question: Question<'_>,
    entries: &[T],
) -> Result<T, InquireError>
where
    T: Noted + fmt::Display,
{
    let shown: Vec<_> = entries.iter().copied().map(noted).collect();
    entry(entries, term.select(question, &shown)?)
}

/// Asks to choose any of `entries`, with those at `marked` chosen at the start and the answer
/// line naming them without their notes.
pub(super) fn choose_many<T>(
    term: &mut impl Terminal,
    question: Question<'_>,
    entries: &[T],
    marked: &[usize],
    need: Need,
) -> Result<Vec<T>, InquireError>
where
    T: Noted + fmt::Display,
{
    let shown: Vec<_> = entries.iter().copied().map(noted).collect();
    term.select_many(question, &shown, marked, need)?
        .into_iter()
        .map(|index| entry(entries, index))
        .collect()
}

/// Asks for any number of values: the offers are picked from a list that ends in `by_hand`, the
/// entries that ask for more by hand. `marked` lists the offers chosen at the start.
pub(super) fn pick_many<T>(
    term: &mut impl Terminal,
    question: &str,
    offers: &[Offer],
    by_hand: &[Pick],
    need: Need,
    marked: &[usize],
) -> Result<Vec<T>, InquireError>
where
    T: FromStr + 'static,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let picks: Vec<Pick> = offers
        .iter()
        .copied()
        .map(Pick::Offered)
        .chain(by_hand.iter().copied())
        .collect();
    let shown: Vec<_> = picks
        .iter()
        .map(|pick| Entry {
            shown: pick.to_string(),
            answer: pick.answer().to_owned(),
        })
        .collect();
    let chosen = term.select_many(Question::new(question), &shown, marked, need)?;
    let mut values = Vec::new();
    for index in chosen {
        match entry(&picks, index)? {
            Pick::Offered(offer) => values.push(offer.value.parse().map_err(custom)?),
            Pick::Port(origin) => {
                for port in typed::<Port>(term, &format!("Port for {origin}"))? {
                    values.push(port.on(origin).parse().map_err(custom)?);
                }
            }
            Pick::Other(label) => values.extend(typed::<T>(term, label)?),
        }
    }
    Ok(values)
}

/// Asks for values one at a time after a list entry that asks by hand was chosen: the first
/// answer is required, since choosing that entry asked for one, and an empty answer ends the
/// rest. Each is checked as it is given, so a value startup would refuse asks again.
pub(super) fn typed<T>(term: &mut impl Terminal, what: &str) -> Result<Vec<T>, InquireError>
where
    T: FromStr + 'static,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let first = format!("{what}:");
    let more = format!("{what} (empty to finish):");
    let mut values = Vec::new();
    loop {
        let optional = !values.is_empty();
        let message = if optional { &more } else { &first };
        let check: Check = Rc::new(move |answer: &str| match answer {
            "" if optional => Validation::Valid,
            "" => Validation::Invalid("An answer is required.".into()),
            answer => valid(answer.parse::<T>().map(drop)),
        });
        let answer = term.text(Question::new(message), None, check)?;
        if answer.is_empty() {
            return Ok(values);
        }
        values.push(answer.parse().map_err(custom)?);
    }
}

/// Asks `question` with a list of seconds whose last entry asks `typed` for any up to `max`.
/// `None` is the list's unset entry.
pub(super) fn pick_seconds(
    term: &mut impl Terminal,
    question: &str,
    typed: &str,
    offers: &[SecondsPick],
    max: u64,
) -> Result<Option<u64>, InquireError> {
    match select_one(term, Question::new(question), offers)? {
        SecondsPick::Offered { seconds, .. } => Ok(Some(seconds)),
        SecondsPick::Unset(_) => Ok(None),
        SecondsPick::Other => {
            let check: Check = Rc::new(move |answer: &str| match answer.parse::<u64>() {
                Ok(seconds) if seconds <= max => Validation::Valid,
                Ok(_) => Validation::Invalid(format!("Enter at most {max} seconds.").into()),
                Err(_) => Validation::Invalid("Enter whole seconds, for example 3600.".into()),
            });
            let answer = term.text(Question::new(typed), None, check)?;
            answer
                .parse()
                .map(Some)
                .map_err(|error| custom(InvalidSeconds(error)))
        }
    }
}

/// Typed seconds the check accepted but a second reading refused, which ends the questions.
#[derive(Debug)]
struct InvalidSeconds(std::num::ParseIntError);

impl fmt::Display for InvalidSeconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cannot read the seconds: {}", self.0)
    }
}

impl std::error::Error for InvalidSeconds {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Refuses an empty answer.
pub(super) fn required() -> Check {
    Rc::new(|value: &str| {
        if value.trim().is_empty() {
            Validation::Invalid("An answer is required.".into())
        } else {
            Validation::Valid
        }
    })
}

/// A refusal shown under the question that asks again.
pub(super) fn valid(result: Result<(), impl fmt::Display>) -> Validation {
    match result {
        Ok(()) => Validation::Valid,
        Err(error) => Validation::Invalid(error.to_string().into()),
    }
}

/// An answer the library refused after its question was validated, which ends the questions.
pub(super) fn custom(error: impl std::error::Error + Send + Sync + 'static) -> InquireError {
    InquireError::Custom(Box::new(error))
}
