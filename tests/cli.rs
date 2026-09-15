#![cfg(unix)]
use std::process::{Command, Output};
fn run(flags: &[&str], script: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-response"))
        .args(flags)
        .args(["--", "sh", "-c", script])
        .output()
        .unwrap()
}
#[test]
fn success_and_failure_contract() {
    let out = run(
        &[],
        "printf 'noise\\nwarning: stdout\\n'; printf 'stderr detail' >&2",
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ok\n");
    assert_eq!(
        out.stderr,
        b"stderr detail\n[stdout diagnostics]\nwarning: stdout\n"
    );
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
            .stderr
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
    let out = Command::new(env!("CARGO_BIN_EXE_agent-response"))
        .args(["--", "/nonexistent/agent-response-command"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125));
    assert!(!out.stderr.is_empty());
}
#[test]
fn concurrent_large_pipes_do_not_deadlock() {
    let out = run(
        &[],
        "(head -c 1200000 /dev/zero >&2) & head -c 1200000 /dev/zero; wait; exit 9",
    );
    assert_eq!(out.status.code(), Some(9));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("stderr truncated"));
    assert!(err.contains("stdout truncated"));
    let count = run(&["--count"], "head -c 1200000 /dev/zero");
    assert_eq!(count.status.code(), Some(125));
    assert!(count.stdout.is_empty());
}
