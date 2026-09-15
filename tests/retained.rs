#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "grent-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        self.binary(env!("CARGO_BIN_EXE_grent"), args)
    }
    fn binary(&self, binary: &str, args: &[&str]) -> Output {
        Command::new(binary)
            .args(args)
            .env("GRENT_STORE", self.0.join("store"))
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
    fn shell(&self, options: &[&str], script: &str) -> Output {
        let mut args = options.to_vec();
        args.extend(["--", "sh", "-c", script]);
        self.run(&args)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; output={out:?}"))
}
fn successful(out: Output) -> Value {
    assert!(out.status.success(), "{out:?}");
    json(&out)
}
fn id(value: &Value) -> &str {
    value["run_id"].as_str().unwrap()
}

#[test]
fn check_and_alias_have_same_contract() {
    let s = Sandbox::new();
    assert_eq!(
        s.shell(&["check"], "printf noise; printf warning >&2")
            .stdout,
        b"ok\n"
    );
    let args = ["check", "--json", "--", "sh", "-c", "printf noise"];
    let mut a = successful(s.run(&args));
    let mut b = successful(s.binary(env!("CARGO_BIN_EXE_agent-response"), &args));
    assert_eq!(a["exit_code"], 0);
    assert_eq!(a["mode"], "check");
    a.as_object_mut().unwrap().remove("run_id");
    b.as_object_mut().unwrap().remove("run_id");
    assert_eq!(a, b);
}

#[test]
fn retained_data_supports_queries_without_reexecution() {
    let s = Sandbox::new();
    let first = successful(s.shell(
        &["check", "--json"],
        "printf x >> counter; printf 'alpha\nbeta\nalpha two\n'; printf 'problem\n' >&2",
    ));
    let run = id(&first);
    let count = successful(s.run(&[
        "read", run, "--mode", "count", "--filter", "alpha", "--json",
    ]));
    assert_eq!(count["count"], 2);
    let preview = successful(s.run(&[
        "read", run, "--mode", "preview", "--filter", "alpha", "--offset", "1", "--lines", "1",
        "--json",
    ]));
    assert_eq!(preview["output"], "alpha two\n");
    let stderr = successful(s.run(&[
        "read", run, "--stream", "stderr", "--mode", "full", "--json",
    ]));
    assert_eq!(stderr["output"], "problem\n");
    assert_eq!(fs::read(s.0.join("counter")).unwrap(), b"x");
    assert!(s.run(&["forget", run]).status.success());
    assert_eq!(s.run(&["read", run, "--json"]).status.code(), Some(125));
}

#[test]
fn counts_existence_and_literal_matching_are_explicit() {
    let s = Sandbox::new();
    let script = "printf 'a\nb\na|b\nlast'";
    assert_eq!(
        successful(s.shell(&["count", "--json"], script))["count"],
        4
    );
    assert_eq!(
        successful(s.shell(&["count", "--json", "--filter", "a|b"], script))["count"],
        4
    );
    assert_eq!(
        successful(s.shell(&["count", "--json", "--filter", "a|b", "--literal"], script))["count"],
        1
    );
    assert_eq!(
        successful(s.shell(&["exists", "--json", "--filter", "absent"], script))["exists"],
        false
    );
    assert_eq!(
        successful(s.shell(&["exists", "--json", "--filter", "last"], script))["exists"],
        true
    );
    assert_eq!(successful(s.shell(&["count", "--json"], ":"))["count"], 0);
}

#[test]
fn preview_limits_and_matching_offsets_are_exact() {
    let s = Sandbox::new();
    let script = "printf 'no\nyes1\nno\nyes2\nyes3\n'";
    let value = successful(s.shell(
        &[
            "preview", "--json", "--filter", "yes", "--offset", "1", "--lines", "1",
        ],
        script,
    ));
    assert_eq!(value["output"], "yes2\n");
    assert_eq!(value["truncated"], true);
    let value = successful(s.shell(&["preview", "--json", "--max-bytes", "3"], "printf abcdef"));
    assert_eq!(value["output"], "abc");
    assert_eq!(value["truncated"], true);
    let value = successful(s.shell(&["preview", "--json", "--offset", "100"], script));
    assert_eq!(value["output"], "");
}

#[test]
fn large_lines_are_not_silently_omitted_from_counts() {
    let s = Sandbox::new();
    let value = successful(s.shell(
        &["count", "--json"],
        "head -c 1200000 /dev/zero; printf '\nend\n'",
    ));
    assert_eq!(value["count"], 2);
    assert_eq!(value["incomplete"], false);
    let value = successful(s.run(&[
        "read",
        id(&value),
        "--mode",
        "exists",
        "--filter",
        "end",
        "--json",
    ]));
    assert_eq!(value["exists"], true);
}

#[test]
fn captures_parallel_streams_and_preserves_invalid_utf8() {
    let s = Sandbox::new();
    let value = successful(s.shell(
        &["check", "--json"],
        "(head -c 1200000 /dev/zero >&2) & head -c 1200000 /dev/zero; wait",
    ));
    for stream in ["stdout", "stderr"] {
        let result = successful(s.run(&[
            "read",
            id(&value),
            "--stream",
            stream,
            "--mode",
            "count",
            "--json",
        ]));
        assert_eq!(result["count"], 1);
    }
    let value = successful(s.shell(&["preview", "--json"], r"printf '\377\n'"));
    assert!(value["output"].as_str().unwrap().contains('\u{fffd}'));
    let logs = fs::read_dir(s.0.join("store").join(id(&value))).unwrap();
    assert!(logs
        .filter_map(Result::ok)
        .any(|e| fs::read(e.path()).ok().as_deref() == Some(&[255, 10])));
}

#[test]
fn failure_replay_preserves_source_status_without_failing_query() {
    let s = Sandbox::new();
    let failed = s.shell(
        &["check", "--json"],
        "printf 'error useful\n'; printf 'detail\n' >&2; exit 7",
    );
    assert_eq!(failed.status.code(), Some(7));
    let value = json(&failed);
    assert_eq!(value["exit_code"], 7);
    assert!(value["diagnostics"].as_str().unwrap().contains("detail"));
    let replay = successful(s.run(&["read", id(&value), "--mode", "count", "--json"]));
    assert_eq!(replay["source_exit_code"], 7);
    assert_eq!(replay["count"], 1);
}

#[test]
fn validation_and_spawn_errors_are_json_and_do_not_execute_child() {
    let s = Sandbox::new();
    let out = s.shell(&["count", "--json", "--filter", "["], "touch executed");
    assert_eq!(out.status.code(), Some(125));
    json(&out);
    assert!(!s.0.join("executed").exists());
    let out = s.run(&["check", "--json", "--", "/nonexistent/grent-command"]);
    assert_eq!(out.status.code(), Some(125));
    json(&out);
    let out = s.run(&["read", "../../outside", "--json"]);
    assert_eq!(out.status.code(), Some(125));
    json(&out);
}

#[test]
fn timeout_covers_child_and_inherited_pipe_grandchild() {
    let s = Sandbox::new();
    for script in ["sleep 10", "sleep 10 & exit 0"] {
        let started = Instant::now();
        let out = s.shell(&["check", "--json", "--timeout-ms", "100"], script);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timeout did not close inherited pipes"
        );
        assert_eq!(out.status.code(), Some(124));
        assert_eq!(json(&out)["incomplete"], true);
    }
}

#[test]
fn log_limit_returns_failure_and_marks_incomplete() {
    let s = Sandbox::new();
    let out = s.shell(
        &["count", "--json", "--max-log-bytes", "1024"],
        "head -c 100000 /dev/zero",
    );
    assert_eq!(out.status.code(), Some(125));
    let value = json(&out);
    assert_eq!(value["incomplete"], true);
    let replay = successful(s.run(&["read", id(&value), "--mode", "preview", "--json"]));
    assert_eq!(replay["incomplete"], true);
}

#[test]
fn retained_files_are_private_and_symlinks_are_refused() {
    let s = Sandbox::new();
    let value = successful(s.shell(&["check", "--json"], "printf secret"));
    let store = s.0.join("store");
    let run = store.join(id(&value));
    for path in [store.clone(), run.clone()] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    let mut log = None;
    for entry in fs::read_dir(&run).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        if fs::read(&path).ok().as_deref() == Some(b"secret") {
            log = Some(path);
        }
    }
    let log = log.expect("stdout was retained verbatim");
    fs::write(s.0.join("outside"), b"outside secret").unwrap();
    fs::remove_file(&log).unwrap();
    symlink(s.0.join("outside"), &log).unwrap();
    let out = s.run(&["read", id(&value), "--mode", "full", "--json"]);
    assert_eq!(out.status.code(), Some(125));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("outside secret"));
    fs::rename(&store, s.0.join("real-store")).unwrap();
    symlink(s.0.join("real-store"), &store).unwrap();
    let out = s.shell(&["check", "--json"], "touch executed");
    assert_eq!(out.status.code(), Some(125));
    assert!(!s.0.join("executed").exists());
}

#[test]
fn seeded_query_cases_match_independent_line_selection() {
    let s = Sandbox::new();
    let mut state = 0x7139_582a_u64;
    let mut lines = Vec::new();
    for index in 0..300 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        lines.push(format!(
            "{} {index} {}\n",
            if state & 1 == 0 { "selected" } else { "other" },
            "x".repeat((state % 97) as usize)
        ));
    }
    fs::write(s.0.join("fixture"), lines.concat()).unwrap();
    let captured = successful(s.shell(&["check", "--json"], "cat fixture"));
    let selected: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("selected"))
        .collect();
    let count = successful(s.run(&[
        "read",
        id(&captured),
        "--mode",
        "count",
        "--filter",
        "selected",
        "--literal",
        "--json",
    ]));
    assert_eq!(count["count"], selected.len());
    for case in 0..24 {
        let offset = case * 9;
        let limit = case % 7 + 1;
        let bytes = case * 11 + 1;
        let expected: String = selected
            .iter()
            .skip(offset)
            .take(limit)
            .map(|line| line.as_str())
            .collect();
        let expected = &expected[..expected.len().min(bytes)];
        let result = successful(s.run(&[
            "read",
            id(&captured),
            "--mode",
            "preview",
            "--filter",
            "selected",
            "--literal",
            "--offset",
            &offset.to_string(),
            "--lines",
            &limit.to_string(),
            "--max-bytes",
            &bytes.to_string(),
            "--json",
        ]));
        assert_eq!(result["output"], expected, "case {case}");
    }
}

#[test]
fn incomplete_logs_never_claim_an_exact_count_or_absence() {
    let s = Sandbox::new();
    let result = s.shell(
        &["check", "--json", "--max-log-bytes", "8"],
        "printf abcdefghijklmnop",
    );
    let value = json(&result);
    for mode in ["count", "exists"] {
        let out = s.run(&["read", id(&value), "--mode", mode, "--json"]);
        assert_eq!(out.status.code(), Some(125));
        let response = json(&out);
        assert_eq!(response["incomplete"], true);
        assert!(response.get("count").is_none());
        assert!(response.get("exists").is_none());
    }
}

#[test]
fn exact_log_limit_is_complete_and_zero_preview_is_marked() {
    let s = Sandbox::new();
    let value = successful(s.shell(
        &["count", "--json", "--max-log-bytes", "8"],
        "printf abcdefgh; printf 12345678 >&2",
    ));
    assert_eq!(value["count"], 1);
    assert_eq!(value["incomplete"], false);
    for limit in ["--lines", "--max-bytes"] {
        let result = successful(s.run(&[
            "read",
            id(&value),
            "--mode",
            "preview",
            limit,
            "0",
            "--json",
        ]));
        assert_eq!(result["output"], "");
        assert_eq!(result["truncated"], true);
    }
}

#[test]
fn termination_cancels_child_and_retains_parseable_result() {
    let s = Sandbox::new();
    let child = Command::new(env!("CARGO_BIN_EXE_grent"))
        .args([
            "check",
            "--json",
            "--",
            "sh",
            "-c",
            "echo $$ > child.pid; exec sleep 30",
        ])
        .env("GRENT_STORE", s.0.join("store"))
        .current_dir(&s.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !s.0.join("child.pid").exists() && started.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.0.join("child.pid").exists(), "child failed to start");
    // SAFETY: the process was launched by this test and has not been reaped.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let out = child.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(out.status.code(), Some(143));
    assert_eq!(json(&out)["incomplete"], true);
    let pid: i32 = fs::read_to_string(s.0.join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero only checks whether this recorded child PID exists.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "child survived cancellation"
    );
}

#[test]
fn read_refuses_hardlinks_and_nonregular_files() {
    let s = Sandbox::new();
    let value = successful(s.shell(&["check", "--json"], "printf original"));
    let path = s.0.join("store").join(id(&value)).join("stdout.bin");
    fs::hard_link(&path, s.0.join("second-link")).unwrap();
    assert_eq!(
        s.run(&["read", id(&value), "--json"]).status.code(),
        Some(125)
    );
    fs::remove_file(s.0.join("second-link")).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        s.run(&["read", id(&value), "--json"]).status.code(),
        Some(125)
    );
}

#[test]
fn prune_removes_only_expired_completed_runs() {
    let s = Sandbox::new();
    let old = successful(s.shell(&["check", "--json"], ":"));
    let recent = successful(s.shell(&["check", "--json"], ":"));
    let old_path = s.0.join("store").join(id(&old));
    let metadata_path = old_path.join("metadata.json");
    let mut metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["finished_at"] = serde_json::json!(1);
    fs::write(metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let pending = s.0.join("store").join("r0000000000000000");
    fs::create_dir(&pending).unwrap();
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o700)).unwrap();
    let value = successful(s.run(&["prune", "--older-than-hours", "24", "--json"]));
    assert_eq!(value["removed"], 1);
    assert!(!old_path.exists());
    assert!(s.0.join("store").join(id(&recent)).exists());
    assert!(pending.exists());
}

#[test]
fn corrupt_metadata_and_public_store_fail_before_claiming_success() {
    let s = Sandbox::new();
    let value = successful(s.shell(&["check", "--json"], ":"));
    let path = s.0.join("store").join(id(&value)).join("metadata.json");
    fs::write(path, b"{}").unwrap();
    let out = s.run(&["read", id(&value), "--json"]);
    assert_eq!(out.status.code(), Some(125));
    json(&out);
    fs::set_permissions(s.0.join("store"), fs::Permissions::from_mode(0o755)).unwrap();
    let out = s.shell(&["check", "--json"], "touch executed");
    assert_eq!(out.status.code(), Some(125));
    assert!(!s.0.join("executed").exists());
}

#[test]
fn failure_json_marks_bounded_diagnostics_and_retains_the_rest() {
    let s = Sandbox::new();
    let out = s.shell(
        &["check", "--json", "--max-bytes", "4"],
        "printf 'error useful\\n'; printf 'stderr detail\\n' >&2; exit 7",
    );
    assert_eq!(out.status.code(), Some(7));
    let value = json(&out);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["incomplete"], false);
    let replay = successful(s.run(&["read", id(&value), "--mode", "full", "--json"]));
    assert_eq!(replay["output"], "error useful\n");
    assert_eq!(replay["truncated"], false);
}
