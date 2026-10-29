#![deny(clippy::unwrap_used, clippy::expect_used)]

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    pattern: String,
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
}

enum Token {
    Character(Character),
    Star(bool),
    Directories,
    Alternatives(Vec<Vec<Token>>),
}

impl Glob {
    pub fn parse(pattern: &str) -> Result<Self, &'static str> {
        if pattern.len() > 1024 {
            return Err(
                "matchGlob must be at most 1024 UTF-8 bytes; use a shorter pattern such as **/*.txt",
            );
        }
        let chars: Vec<_> = pattern.chars().collect();
        let tokens = sequence(&chars, &mut 0, Scope::Pattern)?;
        let mut glob = Self {
            pattern: pattern.into(),
            states: vec![State::Accept],
            start: 0,
        };
        glob.start = glob.compile(tokens, 0);
        Ok(glob)
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
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
                    let split = self.push(State::Accept);
                    let consume = self.push(State::Consume(Character::Any(slash), split));
                    self.states[split] = State::Split(next, consume);
                    split
                }
                Token::Directories => {
                    let split = self.push(State::Accept);
                    let slash = self.push(State::Consume(Character::Literal('/'), next));
                    let consume = self.push(State::Consume(Character::Any(true), split));
                    self.states[split] = State::Split(slash, consume);
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

    fn activate(&self, active: &mut [bool], start: usize) {
        let mut pending = vec![start];
        while let Some(index) = pending.pop() {
            if active[index] {
                continue;
            }
            active[index] = true;
            if let State::Split(a, b) = self.states[index] {
                pending.push(a);
                pending.push(b);
            }
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        // NFA state sets avoid exponential backtracking and brace expansion.
        let mut active = vec![false; self.states.len()];
        let mut next = active.clone();
        self.activate(&mut active, self.start);
        for character in name.chars() {
            next.fill(false);
            for (index, enabled) in active.iter().enumerate() {
                if !enabled {
                    continue;
                }
                if let State::Consume(test, target) = &self.states[index] {
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
                    };
                    if matches {
                        self.activate(&mut next, *target);
                    }
                }
            }
            std::mem::swap(&mut active, &mut next);
        }
        active[0]
    }
}

const INVALID: &str = "Invalid matchGlob syntax; close brackets/braces, use ascending ranges and keep / and ** outside braces (example: **/*.{txt,csv})";

/// Whether a token sequence is the whole pattern or one alternative inside `{...}`, where
/// `,` and `}` end the sequence and `/` is not allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Pattern,
    Brace,
}

fn sequence(
    chars: &[char],
    position: &mut usize,
    scope: Scope,
) -> Result<Vec<Token>, &'static str> {
    let brace = scope == Scope::Brace;
    let mut tokens = Vec::new();
    while let Some(&character) = chars.get(*position) {
        if brace && matches!(character, ',' | '}') {
            break;
        }
        let boundary = *position == 0 || chars[*position - 1] == '/';
        *position += 1;
        let token = match character {
            '\\' => {
                let value = *chars.get(*position).ok_or(INVALID)?;
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
                if ranges.is_empty() || chars.get(*position) != Some(&']') {
                    return Err(INVALID);
                }
                *position += 1;
                Token::Character(Character::Class(negated, ranges))
            }
            '{' => {
                let mut branches = Vec::new();
                loop {
                    branches.push(sequence(chars, position, Scope::Brace)?);
                    match chars.get(*position) {
                        Some(',') => *position += 1,
                        Some('}') => {
                            *position += 1;
                            break;
                        }
                        _ => return Err(INVALID),
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

fn class_character(chars: &[char], position: &mut usize) -> Result<char, &'static str> {
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
    #![allow(clippy::unwrap_used)]
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
        for pattern in [
            "[", "[]", "[!]", "[z-a]", "{a,b", "a\\", "a]", "{a/b,c}", "{**,b}",
        ] {
            assert!(Glob::parse(pattern).is_err(), "{pattern}");
        }
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
