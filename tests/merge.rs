use grent::{capture, Matcher, DEFAULT_PATTERN};
use std::io::{self, Read};
fn default_matcher() -> Matcher {
    Matcher::new(DEFAULT_PATTERN, false, false).unwrap()
}

#[test]
fn explicit_diagnostics() {
    let input = b"normal\nERROR failed\r\nwarning: careful\nERR\nWarning\nlast error";
    let out = capture(&input[..], Some(&default_matcher()), 1024).unwrap();
    assert_eq!(
        out.bytes,
        b"ERROR failed\r\nwarning: careful\nERR\nWarning\nlast error"
    );
    assert_eq!(out.matched_lines, 5);
    assert!(!out.truncated);
}
#[test]
fn binary_empty_and_modes() {
    let regex = Matcher::new("^error", false, false).unwrap();
    assert!(regex.matches(b"ERROR\xff"));
    assert!(!regex.matches(b"an error"));
    let literal = Matcher::new("err|arning", true, false).unwrap();
    assert!(literal.matches(b"ERR|ARNING"));
    assert!(!literal.matches(b"error"));
    assert!(!Matcher::new("[", false, true).unwrap().matches(b"error"));
    assert!(Matcher::new("[", false, false).is_err());
    assert!(Matcher::new("", true, false).unwrap().matches(b""));
    assert_eq!(
        capture(&b""[..], Some(&default_matcher()), 10)
            .unwrap()
            .matched_lines,
        0
    );
    let raw = b"\xff\0stderr without newline";
    assert_eq!(capture(&raw[..], None, 100).unwrap().bytes, raw);
}
#[test]
fn bounded_capture_reports_loss() {
    let out = capture(
        &b"error0123456789\nwarning\n"[..],
        Some(&default_matcher()),
        8,
    )
    .unwrap();
    assert!(out.truncated);
    assert!(out.bytes.len() <= 8);
    assert_eq!(out.matched_lines, 2);
    let raw = capture(&b"123456789"[..], None, 8).unwrap();
    assert_eq!(raw.bytes, b"12345678");
    assert!(raw.truncated);
    assert!(
        capture(&b"error"[..], Some(&default_matcher()), 0)
            .unwrap()
            .truncated
    );
}
struct Broken;
impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Other, "read failed"))
    }
}
#[test]
fn read_errors_propagate() {
    assert!(capture(Broken, None, 10).is_err());
}

// Deterministic fuzz/property test: arbitrary bytes, injected matches, random
// stream chunk boundaries, and a separate literal oracle for the default regex.
#[test]
fn fuzz_bytes_and_chunk_boundaries() {
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
    let matcher = default_matcher();
    let mut seed = 0x123456789abcdefu64;
    fn next(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }
    for iteration in 0..10000 {
        let len = (next(&mut seed) % 512) as usize;
        let mut input: Vec<u8> = (0..len).map(|_| next(&mut seed) as u8).collect();
        if iteration % 2 == 0 {
            input.extend_from_slice(b"\nWaRnInG\xff\nnoise\nERR");
        }
        let mut expected = Vec::new();
        let mut count = 0;
        for line in input.split_inclusive(|b| *b == b'\n') {
            let lower = line.to_ascii_lowercase();
            if lower.windows(3).any(|w| w == b"err") || lower.windows(6).any(|w| w == b"arning") {
                expected.extend_from_slice(line);
                count += 1;
            }
        }
        let out = capture(
            Chunks {
                bytes: &input,
                chunk: (next(&mut seed) % 31 + 1) as usize,
            },
            Some(&matcher),
            4096,
        )
        .unwrap();
        assert_eq!(out.bytes, expected, "iteration {iteration}");
        assert_eq!(out.matched_lines, count);
        assert!(!out.truncated);
        let cap = (next(&mut seed) % 64) as usize;
        let bounded = capture(&input[..], Some(&matcher), cap).unwrap();
        assert!(bounded.bytes.len() <= cap);
    }
}
