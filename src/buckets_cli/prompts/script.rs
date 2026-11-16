//! A terminal that answers from a script, so that tests drive the questions as a person would,
//! by what each entry says rather than by its position.

use super::terminal::{Check, EnterMeans, Entry, Need, Question, Terminal};
use inquire::{
    InquireError,
    validator::{ErrorMessage, Validation},
};
use std::collections::VecDeque;

/// One answer, in the order the questions come.
#[derive(Clone, Copy, Debug)]
pub(super) enum Reply {
    /// The first entry whose answer line reads this, or whose shown text starts with it, as
    /// for the entries that ask by hand, whose answer lines are alike.
    Pick(&'static str),
    /// These entries, found as [`Reply::Pick`] finds one; none chooses nothing.
    Picks(&'static [&'static str]),
    /// Enter: the marked entries, the filled-in text or the default.
    Keep,
    Type(&'static str),
    Yes,
    No,
    Esc,
    CtrlC,
}

pub(super) struct Script {
    replies: VecDeque<Reply>,
    /// Each question with its entries' answer lines, each refusal and each text shown, in order.
    pub(super) seen: Vec<String>,
}

impl Script {
    pub(super) fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            seen: Vec::new(),
        }
    }

    /// Whether every reply was used, so that a test cannot pass with questions left unasked.
    pub(super) fn is_done(&self) -> bool {
        self.replies.is_empty()
    }

    fn next(&mut self, question: &str) -> Reply {
        self.replies
            .pop_front()
            .unwrap_or_else(|| panic!("no reply left for {question:?}; seen {:#?}", self.seen))
    }

    fn ask(&mut self, question: Question<'_>, entries: &[Entry]) {
        let answers: Vec<_> = entries.iter().map(|entry| entry.answer.as_str()).collect();
        self.seen
            .push(format!("? {} [{}]", question.text, answers.join(", ")));
    }

    fn refuse(&mut self, message: &ErrorMessage) {
        let message = match message {
            ErrorMessage::Default => "",
            ErrorMessage::Custom(message) => message,
        };
        self.seen.push(format!("# {message}"));
    }
}

fn index(entries: &[Entry], answer: &str, question: &str) -> usize {
    entries
        .iter()
        .position(|entry| entry.answer == answer)
        .or_else(|| {
            entries
                .iter()
                .position(|entry| entry.shown.starts_with(answer))
        })
        .unwrap_or_else(|| panic!("{question:?} has no entry {answer:?}: {entries:?}"))
}

fn ended(reply: Reply) -> InquireError {
    match reply {
        Reply::Esc => InquireError::OperationCanceled,
        Reply::CtrlC => InquireError::OperationInterrupted,
        reply @ (Reply::Pick(_)
        | Reply::Picks(_)
        | Reply::Keep
        | Reply::Type(_)
        | Reply::Yes
        | Reply::No) => panic!("{reply:?} does not answer this question"),
    }
}

impl Terminal for Script {
    fn select(&mut self, question: Question<'_>, entries: &[Entry]) -> Result<usize, InquireError> {
        self.ask(question, entries);
        match self.next(question.text) {
            Reply::Pick(answer) => Ok(index(entries, answer, question.text)),
            reply => Err(ended(reply)),
        }
    }

    fn select_many(
        &mut self,
        question: Question<'_>,
        entries: &[Entry],
        marked: &[usize],
        need: Need,
    ) -> Result<Vec<usize>, InquireError> {
        self.ask(question, entries);
        loop {
            let chosen: Vec<usize> = match self.next(question.text) {
                Reply::Picks(answers) => answers
                    .iter()
                    .map(|answer| index(entries, answer, question.text))
                    .collect(),
                Reply::Keep => marked.to_vec(),
                reply => return Err(ended(reply)),
            };
            if need == Need::AtLeastOne && chosen.is_empty() {
                self.seen.push("# Choose at least one".to_owned());
                continue;
            }
            return Ok(chosen);
        }
    }

    fn text(
        &mut self,
        question: Question<'_>,
        initial: Option<&str>,
        check: Check,
    ) -> Result<String, InquireError> {
        self.ask(question, &[]);
        loop {
            let answer = match self.next(question.text) {
                Reply::Type(answer) => answer.to_owned(),
                Reply::Keep => initial.unwrap_or_default().to_owned(),
                reply => return Err(ended(reply)),
            };
            match check(&answer) {
                Validation::Valid => return Ok(answer),
                Validation::Invalid(message) => self.refuse(&message),
            }
        }
    }

    fn confirm(&mut self, question: &str, enter: EnterMeans) -> Result<bool, InquireError> {
        self.seen.push(format!("? {question}"));
        match self.next(question) {
            Reply::Yes => Ok(true),
            Reply::No => Ok(false),
            Reply::Keep => Ok(enter == EnterMeans::Yes),
            reply => Err(ended(reply)),
        }
    }

    fn show(&mut self, text: &str) {
        self.seen.push(text.to_owned());
    }
}
