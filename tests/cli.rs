#![cfg(unix)]
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn run(flags: &[&str], script: &str) -> Output {
    let store = std::env::temp_dir().join(format!(
        "grent-legacy-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_agent-response"))
        .env("GRENT_STORE", &store)
        .args(flags)
        .args(["--", "sh", "-c", script])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(store);
    output
}
#[test]
fn success_and_failure_contract() {
    let out = run(
        &[],
        "printf 'noise\\nwarning: stdout\\n'; printf 'stderr detail' >&2",
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ok\n");
    assert!(out.stderr.is_empty());
    let failed = run(
        &[],
        "printf 'normal\\nERROR useful\\n'; printf 'bad\\n' >&2; exit 7",
    );
    assert_eq!(failed.status.code(), Some(7));
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("ERROR useful"));
    let silent = run(&[], "exit 3");
    assert_eq!(silent.status.code(), Some(3));
    assert!(!silent.stderr.is_empty());
}
#[test]
fn options_counts_and_signals() {
    let out = run(&["--count"], "printf 'error error\\nnormal\\nwarning\\n'");
    assert_eq!(out.stdout, b"2\n");
    assert_eq!(run(&["--count"], "printf normal").stdout, b"0\n");
    assert!(run(&["--failure-only"], "printf warning; printf error >&2")
        .stderr
        .is_empty());
    assert!(run(&["--no-stdout"], "printf error").stderr.is_empty());
    assert!(
        run(&["--literal", "--pattern", "a|b"], "printf 'a\\na|b\\n'")
            .stdout
            .ends_with(b"a|b\n")
    );
    assert_eq!(run(&[], "kill -TERM $$").status.code(), Some(143));
}
#[test]
fn invalid_regex_does_not_execute_child() {
    let out = run(&["--pattern", "["], "printf 'CHILD RAN' >&2");
    assert_eq!(out.status.code(), Some(125));
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("CHILD RAN"));
}
#[test]
fn concurrent_large_pipes_do_not_deadlock() {
    let out = run(
        &["--max-bytes", "8192"],
        "(head -c 1200000 /dev/zero >&2) & head -c 1200000 /dev/zero; wait; exit 9",
    );
    assert_eq!(out.status.code(), Some(9));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("diagnostics truncated"));
    let count = run(&["--count"], "head -c 1200000 /dev/zero");
    assert!(count.status.success());
    assert_eq!(count.stdout, b"0\n");
}

#[test]
fn explicit_filters_select_both_streams_without_ok() {
    let out = run(
        &["--filter", "warn"],
        "printf 'noise\nWARNING chosen\n'; printf 'noise\nwarning stderr' >&2",
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"WARNING chosen\n");
    assert_eq!(out.stderr, b"warning stderr");
    let empty = run(&["--filter", "absent"], "printf noise; printf noise >&2");
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    assert!(empty.stderr.is_empty());
    let quiet = run(
        &["--filter", "*", "--failure-only"],
        "printf warning; printf error >&2",
    );
    assert_eq!(quiet.stdout, b"ok\n");
    assert!(quiet.stderr.is_empty());
}
#[test]
fn wildcard_is_real_passthrough_including_large_output_and_failure() {
    let out = run(
        &["--filter", "*"],
        "printf 'noise\nwarning'; printf 'stderr' >&2; exit 7",
    );
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, b"noise\nwarning");
    assert_eq!(out.stderr, b"stderr");
    let large = run(&["--filter", "*"], "head -c 1200000 /dev/zero");
    assert!(large.status.success());
    assert_eq!(large.stdout.len(), 1200000);
    assert!(large.stderr.is_empty());
    let literal = run(&["--literal", "--filter", "*"], "printf 'normal\na*b\n'");
    assert_eq!(literal.stdout, b"a*b\n");
    let count = run(&["--filter", "*", "--count"], "printf 'normal\nwarning\n'");
    assert_eq!(count.stdout, b"2\n");
    assert!(count.stderr.is_empty());
}
