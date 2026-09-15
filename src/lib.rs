use regex::bytes::{Regex, RegexBuilder};
use std::io::{self, Read};

pub const DEFAULT_PATTERN: &str = "err|arning";
pub const CAPTURE_LIMIT: usize = 1024 * 1024;

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

#[derive(Debug)]
pub struct Capture {
    pub bytes: Vec<u8>,
    pub matched_lines: u64,
    pub truncated: bool,
}

// Drain both pipes concurrently in the caller. Bound retained bytes AND individual
// lines; a very long line is emitted only if its retained prefix matches.
// Truncation is always reported so omitted diagnostics cannot appear complete.
pub fn capture(
    mut input: impl Read,
    matcher: Option<&Matcher>,
    limit: usize,
) -> io::Result<Capture> {
    let mut result = Capture {
        bytes: Vec::new(),
        matched_lines: 0,
        truncated: false,
    };
    let mut line = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match input.read(&mut buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if n == 0 {
            break;
        }
        if matcher.is_none() {
            let keep = n.min(limit.saturating_sub(result.bytes.len()));
            result.bytes.extend_from_slice(&buf[..keep]);
            result.truncated |= keep != n;
            continue;
        }
        for &byte in &buf[..n] {
            if line.len() < limit {
                line.push(byte);
            } else {
                result.truncated = true;
            }
            if byte == b'\n' {
                retain_line(&mut result, &line, matcher.unwrap(), limit);
                line.clear();
            }
        }
    }
    if !line.is_empty() {
        retain_line(&mut result, &line, matcher.unwrap(), limit);
    }
    Ok(result)
}
fn retain_line(out: &mut Capture, line: &[u8], matcher: &Matcher, limit: usize) {
    if !matcher.matches(line) {
        return;
    }
    out.matched_lines += 1;
    let keep = line.len().min(limit.saturating_sub(out.bytes.len()));
    out.bytes.extend_from_slice(&line[..keep]);
    out.truncated |= keep != line.len();
}
