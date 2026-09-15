use grent::{
    query::{select_reader, Mode},
    Matcher, DEFAULT_PATTERN,
};
use std::io::{self, BufReader, Read};

#[test]
fn matching_is_literal_or_regex_with_binary_support() {
    let regex = Matcher::new("^error$", false, false).unwrap();
    let selected = select_reader(
        &b"ERROR\r\nnot error\n"[..],
        &regex,
        Mode::Preview,
        0,
        20,
        100,
    )
    .unwrap();
    assert_eq!(selected.output, b"ERROR\r\n");
    let literal = Matcher::new("err|arning", true, false).unwrap();
    assert!(literal.matches(b"ERR|ARNING"));
    assert!(!literal.matches(b"error"));
    assert!(Matcher::new("[", false, false).is_err());
    assert!(Matcher::new("", true, false).unwrap().matches(b""));
    assert!(!Matcher::new("[", false, true).unwrap().matches(b"error"));
    assert!(Matcher::new("error", false, false)
        .unwrap()
        .matches(b"\xfferror\0"));
}
struct Broken;
impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("read failed"))
    }
}
#[test]
fn read_errors_propagate() {
    assert!(select_reader(
        BufReader::new(Broken),
        &Matcher::new("", false, false).unwrap(),
        Mode::Count,
        0,
        20,
        100
    )
    .is_err());
}

// Seeded property-style fuzzing of the actual retained-result selection engine.
// The independent oracle uses literal byte searches, not the regex implementation.
#[test]
fn ten_thousand_random_byte_streams_match_independent_oracle() {
    struct Chunks<'a> {
        bytes: &'a [u8],
        chunk: usize,
    }
    impl Read for Chunks<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.bytes.len().min(buf.len()).min(self.chunk);
            buf[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes = &self.bytes[n..];
            Ok(n)
        }
    }
    let matcher = Matcher::new(DEFAULT_PATTERN, false, false).unwrap();
    let mut seed = 0x123456789abcdefu64;
    fn next(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }
    for case in 0..10000 {
        let len = (next(&mut seed) % 512) as usize;
        let mut input: Vec<u8> = (0..len).map(|_| next(&mut seed) as u8).collect();
        if case % 2 == 0 {
            input.extend_from_slice(b"\nWaRnInG\xff\nnoise\nERR");
        }
        let matched: Vec<&[u8]> = input
            .split_inclusive(|b| *b == b'\n')
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.windows(3).any(|v| v == b"err") || lower.windows(6).any(|v| v == b"arning")
            })
            .collect();
        let offset = (next(&mut seed) % 4) as usize;
        let lines = (next(&mut seed) % 6) as usize;
        let cap = (next(&mut seed) % 64) as usize;
        let chunk = (next(&mut seed) % 31 + 1) as usize;
        let available: Vec<u8> = matched
            .iter()
            .skip(offset)
            .take(lines)
            .flat_map(|x| x.iter().copied())
            .collect();
        let expected = &available[..available.len().min(cap)];
        let result = select_reader(
            BufReader::new(Chunks {
                bytes: &input,
                chunk,
            }),
            &matcher,
            Mode::Preview,
            offset as u64,
            lines as u64,
            cap,
        )
        .unwrap();
        assert_eq!(result.count, matched.len() as u64, "case {case}");
        assert_eq!(result.output, expected, "case {case}");
        assert_eq!(
            result.truncated,
            matched.len() > offset + lines || available.len() > cap,
            "case {case}"
        );
        let count = select_reader(&input[..], &matcher, Mode::Count, 0, 0, 0).unwrap();
        assert_eq!(count.count, matched.len() as u64);
        let exists = select_reader(&input[..], &matcher, Mode::Exists, 0, 0, 0).unwrap();
        assert_eq!(exists.count > 0, !matched.is_empty());
    }
}
