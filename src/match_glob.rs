#![deny(clippy::unwrap_used, clippy::expect_used)]

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Glob {
    states: Vec<State>,
    start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Accept,
    Consume(Character, usize),
    Split(usize, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Character {
    Literal(char),
    Any(bool),
    Class(bool, Vec<(char, char)>),
    /// A trailing `\` escapes nothing. Cloud Storage accepted such a pattern and listed no
    /// objects (recorded 2026-11), so it matches no character.
    Nothing,
}

enum Token {
    Character(Character),
    Star(bool),
    Directories,
    Alternatives(Vec<Vec<Token>>),
}

impl Glob {
    pub(crate) fn parse(pattern: &str) -> Result<Self, GlobError> {
        if pattern.len() > 1024 {
            return Err(GlobError::TooLong);
        }
        let chars: Vec<_> = pattern.chars().collect();
        let tokens = sequence(&chars, &mut 0, Scope::Pattern)?;
        let mut glob = Self {
            states: vec![State::Accept],
            start: 0,
        };
        glob.start = glob.compile(tokens, 0);
        Ok(glob)
    }

    fn push(&mut self, state: State) -> usize {
        self.states.push(state);
        self.states.len() - 1
    }

    fn compile(&mut self, tokens: Vec<Token>, mut next: usize) -> usize {
        for token in tokens.into_iter().rev() {
            next = match token {
                Token::Character(character) => self.push(State::Consume(character, next)),
                Token::Star(slash) => {
                    let split = self.states.len();
                    self.push(State::Split(next, split + 1));
                    self.push(State::Consume(Character::Any(slash), split));
                    split
                }
                Token::Directories => {
                    let split = self.states.len();
                    self.push(State::Split(split + 1, split + 2));
                    self.push(State::Consume(Character::Literal('/'), next));
                    self.push(State::Consume(Character::Any(true), split));
                    self.push(State::Split(next, split))
                }
                Token::Alternatives(branches) => {
                    let mut branches = branches.into_iter();
                    let Some(first) = branches.next() else {
                        continue;
                    };
                    let mut start = self.compile(first, next);
                    for tokens in branches {
                        let branch = self.compile(tokens, next);
                        start = self.push(State::Split(start, branch));
                    }
                    start
                }
            };
        }
        next
    }

    fn activate(&self, active: &mut [bool], start: usize, pending: &mut Vec<usize>) {
        pending.push(start);
        while let Some(index) = pending.pop() {
            let Some(enabled) = active.get_mut(index) else {
                continue;
            };
            if *enabled {
                continue;
            }
            *enabled = true;
            if let Some(State::Split(a, b)) = self.states.get(index) {
                pending.push(*a);
                pending.push(*b);
            }
        }
    }

    pub(crate) fn matches(&self, name: &str) -> bool {
        // NFA state sets avoid exponential backtracking and brace expansion.
        let mut active = vec![false; self.states.len()];
        let mut next = active.clone();
        let mut pending = Vec::with_capacity(self.states.len());
        self.activate(&mut active, self.start, &mut pending);
        for character in name.chars() {
            next.fill(false);
            for (state, enabled) in self.states.iter().zip(&active) {
                if !enabled {
                    continue;
                }
                if let State::Consume(test, target) = state {
                    let matches = match test {
                        Character::Literal(value) => character == *value,
                        Character::Any(slash) => *slash || character != '/',
                        Character::Class(negated, ranges) => {
                            character != '/'
                                && (ranges
                                    .iter()
                                    .any(|(a, b)| *a <= character && character <= *b)
                                    != *negated)
                        }
                        Character::Nothing => false,
                    };
                    if matches {
                        self.activate(&mut next, *target, &mut pending);
                    }
                }
            }
            std::mem::swap(&mut active, &mut next);
        }
        active.first().copied().unwrap_or(false)
    }
}

/// Why a `matchGlob` pattern was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlobError {
    TooLong,
    InvalidSyntax,
    /// A `[` at this character index that no `]` closes; Cloud Storage names the index.
    UnclosedClass(usize),
    /// A `{` at this character index that no `}` closes.
    UnclosedBrace(usize),
}

impl GlobError {
    pub(crate) fn message(self, field: &str) -> String {
        match self {
            Self::TooLong => format!(
                "{field} must be at most 1024 UTF-8 bytes; use a shorter pattern such as **/*.txt"
            ),
            Self::InvalidSyntax => format!(
                "Invalid {field} syntax; close brackets/braces, use ascending ranges and keep / and ** outside braces (example: **/*.{{txt,csv}})"
            ),
            Self::UnclosedClass(index) => format!(
                "Glob pattern character class has no closing ']' for opening '[' at index {index}. Either add a closing ']' or use '\\[' to match a literal '['."
            ),
            Self::UnclosedBrace(index) => format!(
                "Glob pattern brace expansion opening '{{' at index {index} has no closing '}}'. Use '\\{{' to match a literal '{{'."
            ),
        }
    }
}

impl std::fmt::Display for GlobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message("matchGlob"))
    }
}

const INVALID: GlobError = GlobError::InvalidSyntax;

/// Whether a token sequence is the whole pattern or one alternative inside `{...}`, where
/// `,` and `}` end the sequence and `/` is not allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Pattern,
    Brace,
}

fn sequence(chars: &[char], position: &mut usize, scope: Scope) -> Result<Vec<Token>, GlobError> {
    let brace = scope == Scope::Brace;
    let mut tokens = Vec::new();
    while let Some(&character) = chars.get(*position) {
        if brace && matches!(character, ',' | '}') {
            break;
        }
        let boundary = *position == 0
            || position.checked_sub(1).and_then(|index| chars.get(index)) == Some(&'/');
        *position += 1;
        let token = match character {
            '\\' => {
                let Some(&value) = chars.get(*position) else {
                    tokens.push(Token::Character(Character::Nothing));
                    break;
                };
                if brace && value == '/' {
                    return Err(INVALID);
                }
                *position += 1;
                Token::Character(Character::Literal(value))
            }
            '?' => Token::Character(Character::Any(false)),
            '*' => {
                let double = chars.get(*position) == Some(&'*');
                if double {
                    if brace {
                        return Err(INVALID);
                    }
                    *position += 1;
                }
                if double && boundary && chars.get(*position) == Some(&'/') {
                    *position += 1;
                    Token::Directories
                } else {
                    Token::Star(double)
                }
            }
            '[' => {
                let opening = *position - 1;
                let negated = matches!(chars.get(*position), Some('!' | '^'));
                if negated {
                    *position += 1;
                }
                let mut ranges = Vec::new();
                while let Some(&character) = chars.get(*position) {
                    if character == ']' {
                        break;
                    }
                    let start = class_character(chars, position)?;
                    if brace && start == '/' {
                        return Err(INVALID);
                    }
                    let end = if chars.get(*position) == Some(&'-')
                        && chars.get(*position + 1).is_some_and(|c| *c != ']')
                    {
                        *position += 1;
                        class_character(chars, position)?
                    } else {
                        start
                    };
                    if end < start || (brace && end == '/') {
                        return Err(INVALID);
                    }
                    ranges.push((start, end));
                }
                if chars.get(*position).is_none() {
                    return Err(GlobError::UnclosedClass(opening));
                }
                if ranges.is_empty() || chars.get(*position) != Some(&']') {
                    return Err(INVALID);
                }
                *position += 1;
                Token::Character(Character::Class(negated, ranges))
            }
            '{' => {
                let opening = *position - 1;
                let mut branches = Vec::new();
                loop {
                    branches.push(sequence(chars, position, Scope::Brace)?);
                    match chars.get(*position) {
                        Some(',') => *position += 1,
                        Some('}') => {
                            *position += 1;
                            break;
                        }
                        Some(_) => return Err(INVALID),
                        None => return Err(GlobError::UnclosedBrace(opening)),
                    }
                }
                Token::Alternatives(branches)
            }
            ']' | '}' => return Err(INVALID),
            '/' if brace => return Err(INVALID),
            value => Token::Character(Character::Literal(value)),
        };
        tokens.push(token);
    }
    Ok(tokens)
}

fn class_character(chars: &[char], position: &mut usize) -> Result<char, GlobError> {
    let mut value = *chars.get(*position).ok_or(INVALID)?;
    *position += 1;
    if value == '\\' {
        value = *chars.get(*position).ok_or(INVALID)?;
        *position += 1;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_glob_syntax() {
        for (pattern, yes, no) in [
            ("a*", "abc", "a/b"),
            ("**", "a/b", ""),
            ("foo/**/bar", "foo/bar", "foo/xbar"),
            ("foo/**/bar", "foo/a/b/bar", "foo/bar/x"),
            ("**/bar", "bar", "xbar"),
            ("a?", "a雪", "a/"),
            ("[a-cXY]", "b", "d"),
            ("[!abc]", "雪", "a"),
            ("[^a-c]", "z", "/"),
            ("{foo,{a,b}{x,y},bar}", "by", "bz"),
            ("{foo*,*bar}", "foobar", "foo/bar"),
            (r"a\?\*", "a?*", "abc"),
            (r"[\]]", "]", "x"),
        ] {
            let glob = Glob::parse(pattern).unwrap();
            assert!(glob.matches(yes), "{pattern} {yes}");
            if !no.is_empty() {
                assert!(!glob.matches(no), "{pattern} {no}");
            }
        }
        assert!(Glob::parse("**").unwrap().matches(""));
        assert!(Glob::parse(&"a".repeat(1024)).is_ok());
        assert!(Glob::parse(&"雪".repeat(342)).is_err());
        for pattern in ["[", "[]", "[!]", "[z-a]", "{a,b", "a]", "{a/b,c}", "{**,b}"] {
            assert!(Glob::parse(pattern).is_err(), "{pattern}");
        }
        let trailing = Glob::parse("a\\").unwrap();
        for name in ["a", "a\\", "ab"] {
            assert!(!trailing.matches(name), "{name}");
        }
        assert_eq!(
            Glob::parse("d/{a,b").unwrap_err().to_string(),
            "Glob pattern brace expansion opening '{' at index 2 has no closing '}'. Use '\\{' to match a literal '{'."
        );
        let nested = format!("{}x{}", "{".repeat(500), "}".repeat(500));
        assert!(Glob::parse(&nested).unwrap().matches("x"));
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_patterns_do_not_panic(pattern in ".{0,1024}", name in ".{0,128}") {
            if let Ok(glob) = Glob::parse(&pattern) { let _ = glob.matches(&name); }
        }
    }
}
