pub mod query;
pub mod runner;
pub mod store;
use regex::bytes::{Regex, RegexBuilder};

pub const DEFAULT_PATTERN: &str = "err|arning";

pub enum Matcher {
    Regex(Regex),
    Literal(Vec<u8>),
    Off,
}
impl Matcher {
    pub fn new(pattern: &str, literal: bool, off: bool) -> Result<Self, regex::Error> {
        if off {
            return Ok(Self::Off);
        }
        if literal {
            return Ok(Self::Literal(
                pattern
                    .as_bytes()
                    .iter()
                    .map(u8::to_ascii_lowercase)
                    .collect(),
            ));
        }
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .unicode(false)
            .build()
            .map(Self::Regex)
    }
    pub fn matches(&self, line: &[u8]) -> bool {
        match self {
            Self::Off => false,
            Self::Regex(re) => re.is_match(line),
            Self::Literal(needle) => {
                needle.is_empty()
                    || line.windows(needle.len()).any(|part| {
                        part.iter()
                            .zip(needle)
                            .all(|(a, b)| a.to_ascii_lowercase() == *b)
                    })
            }
        }
    }
}
