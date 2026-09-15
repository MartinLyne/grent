use crate::{store, Matcher};
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Check,
    Count,
    Exists,
    Preview,
    Full,
}
impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "check" => Some(Self::Check),
            "count" => Some(Self::Count),
            "exists" => Some(Self::Exists),
            "preview" => Some(Self::Preview),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Count => "count",
            Self::Exists => "exists",
            Self::Preview => "preview",
            Self::Full => "full",
        }
    }
}
pub struct Selection {
    pub count: u64,
    pub output: Vec<u8>,
    pub truncated: bool,
}
pub fn select(
    path: &Path,
    matcher: &Matcher,
    mode: Mode,
    offset: u64,
    lines: u64,
    max_bytes: usize,
) -> io::Result<Selection> {
    let file = store::read_file(path)?;
    if file.metadata()?.len() > store::HARD_LOG_LIMIT {
        return Err(io::Error::other("retained file exceeds supported size"));
    }
    select_reader(
        BufReader::new(file),
        matcher,
        mode,
        offset,
        lines,
        max_bytes,
    )
}

pub fn select_reader(
    mut reader: impl BufRead,
    matcher: &Matcher,
    mode: Mode,
    offset: u64,
    lines: u64,
    max_bytes: usize,
) -> io::Result<Selection> {
    let mut line = Vec::new();
    let mut selection = Selection {
        count: 0,
        output: Vec::new(),
        truncated: false,
    };
    while reader.read_until(b'\n', &mut line)? != 0 {
        let content = line.strip_suffix(b"\n").unwrap_or(&line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if matcher.matches(content) {
            let index = selection.count;
            selection.count += 1;
            if mode == Mode::Exists {
                break;
            }
            if matches!(mode, Mode::Preview | Mode::Full) && index >= offset {
                if index - offset >= lines {
                    selection.truncated = true;
                } else {
                    let keep = line
                        .len()
                        .min(max_bytes.saturating_sub(selection.output.len()));
                    selection.output.extend_from_slice(&line[..keep]);
                    selection.truncated |= keep < line.len();
                }
            }
        }
        line.clear();
    }
    Ok(selection)
}
