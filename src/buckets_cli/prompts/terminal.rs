//! The terminal the questions are asked on. The questions decide what to ask and what each
//! answer means; a terminal only shows a question and returns the answer, so that tests can
//! drive the whole flow with a script instead of keystrokes.

use inquire::{
    Confirm, InquireError, MultiSelect, Select, Text, list_option::ListOption,
    validator::Validation,
};
use std::{fmt, rc::Rc};

/// A question and the help shown under it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Question<'a> {
    pub(super) text: &'a str,
    pub(super) help: Option<&'a str>,
}

impl<'a> Question<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self { text, help: None }
    }

    pub(super) fn with_help(self, help: &'a str) -> Self {
        Self {
            help: Some(help),
            ..self
        }
    }
}

/// A list entry as the list shows it, and as the answer line keeps it once chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) shown: String,
    pub(super) answer: String,
}

impl Entry {
    /// An entry whose answer line keeps what the list shows.
    pub(super) fn plain(shown: impl fmt::Display) -> Self {
        let shown = shown.to_string();
        Self {
            answer: shown.clone(),
            shown,
        }
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.shown)
    }
}

/// What a list question accepts as an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Need {
    Optional,
    AtLeastOne,
    /// At least one, and the entry at this index only on its own, as default settings, which
    /// any other choice would contradict.
    AtLeastOneWithAlone(usize),
}

impl Need {
    /// Why the entries at `chosen` are refused, shown under the question that asks again. The
    /// terminal and the tests' script both judge through this, so they cannot drift apart.
    pub(super) fn refusal(self, entries: &[Entry], chosen: &[usize]) -> Option<String> {
        match self {
            Self::Optional => None,
            Self::AtLeastOne | Self::AtLeastOneWithAlone(_) if chosen.is_empty() => {
                Some("Choose at least one; Space selects.".to_owned())
            }
            Self::AtLeastOne => None,
            Self::AtLeastOneWithAlone(alone) => {
                let mixed = chosen.len() > 1 && chosen.contains(&alone);
                let name = entries.get(alone).map(|entry| entry.answer.as_str());
                mixed.then(|| {
                    format!(
                        "{} goes alone; Space unmarks.",
                        name.unwrap_or("That entry")
                    )
                })
            }
        }
    }
}

/// Checks a typed answer. A refusal is shown under the question, which asks again.
pub(super) type Check = Rc<dyn Fn(&str) -> Validation>;

/// What Enter alone answers to a yes-or-no question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EnterMeans {
    Yes,
    No,
}

pub(super) trait Terminal {
    /// The index of the chosen entry, with the cursor on the entry at `start` at first, so that
    /// Enter keeps an earlier answer.
    fn select(
        &mut self,
        question: Question<'_>,
        entries: &[Entry],
        start: usize,
    ) -> Result<usize, InquireError>;

    /// The indices of the chosen entries, with those at `marked` chosen at the start.
    fn select_many(
        &mut self,
        question: Question<'_>,
        entries: &[Entry],
        marked: &[usize],
        need: Need,
    ) -> Result<Vec<usize>, InquireError>;

    /// A typed answer that `check` accepts, starting from `initial`.
    fn text(
        &mut self,
        question: Question<'_>,
        initial: Option<&str>,
        check: Check,
    ) -> Result<String, InquireError>;

    fn confirm(&mut self, question: &str, enter: EnterMeans) -> Result<bool, InquireError>;

    /// Shows `text` between questions, such as the buckets before they are written.
    fn show(&mut self, text: &str);
}

/// The terminal itself, through inquire on stderr. Every list shows all its entries at once,
/// because an entry below the fold goes unseen.
pub(super) struct Inquire;

impl Terminal for Inquire {
    fn select(
        &mut self,
        question: Question<'_>,
        entries: &[Entry],
        start: usize,
    ) -> Result<usize, InquireError> {
        let mut list = Select::new(question.text, entries.to_vec())
            .with_page_size(entries.len())
            .with_starting_cursor(start)
            .with_formatter(&answer);
        if let Some(help) = question.help {
            list = list.with_help_message(help);
        }
        list.raw_prompt().map(|chosen| chosen.index)
    }

    fn select_many(
        &mut self,
        question: Question<'_>,
        entries: &[Entry],
        marked: &[usize],
        need: Need,
    ) -> Result<Vec<usize>, InquireError> {
        let mut list = MultiSelect::new(question.text, entries.to_vec())
            .with_page_size(entries.len())
            .with_default(marked)
            .with_formatter(&answers);
        if let Some(help) = question.help {
            list = list.with_help_message(help);
        }
        if need != Need::Optional {
            let shown = entries.to_vec();
            list = list.with_validator(move |chosen: &[ListOption<&Entry>]| {
                let chosen: Vec<_> = chosen.iter().map(|option| option.index).collect();
                Ok(match need.refusal(&shown, &chosen) {
                    Some(refusal) => Validation::Invalid(refusal.into()),
                    None => Validation::Valid,
                })
            });
        }
        list.raw_prompt()
            .map(|chosen| chosen.into_iter().map(|option| option.index).collect())
    }

    fn text(
        &mut self,
        question: Question<'_>,
        initial: Option<&str>,
        check: Check,
    ) -> Result<String, InquireError> {
        let mut text =
            Text::new(question.text).with_validator(move |answer: &str| Ok(check(answer)));
        if let Some(help) = question.help {
            text = text.with_help_message(help);
        }
        if let Some(initial) = initial {
            text = text.with_initial_value(initial);
        }
        text.prompt()
    }

    fn confirm(&mut self, question: &str, enter: EnterMeans) -> Result<bool, InquireError> {
        Confirm::new(question)
            .with_default(enter == EnterMeans::Yes)
            .prompt()
    }

    fn show(&mut self, text: &str) {
        eprint!("{text}");
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "inquire's OptionFormatter passes the chosen option by value"
)]
fn answer(chosen: ListOption<&Entry>) -> String {
    chosen.value.answer.clone()
}

fn answers(chosen: &[ListOption<&Entry>]) -> String {
    chosen
        .iter()
        .map(|option| option.value.answer.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
