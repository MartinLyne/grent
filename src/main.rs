use agent_response::{capture, Matcher, CAPTURE_LIMIT, DEFAULT_PATTERN};
use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    process::{Command, Stdio},
    thread,
};

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("agent-response: {e}");
            125
        }
    };
    std::process::exit(code);
}
fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let mut pattern = DEFAULT_PATTERN.to_string();
    let (mut literal, mut off, mut count, mut failure_only) = (false, false, false, false);
    let mut command: Vec<OsString> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") => {
                command.extend(args);
                break;
            }
            Some("--regex") => literal = false,
            Some("--literal") => literal = true,
            Some("--no-stdout") => off = true,
            Some("--count") => count = true,
            Some("--failure-only") => failure_only = true,
            Some("--pattern") => {
                pattern = args
                    .next()
                    .ok_or("--pattern needs a value")?
                    .into_string()
                    .map_err(|_| "pattern must be UTF-8")?
            }
            Some("--help") | Some("-h") => {
                println!("Usage: agent-response [--pattern PATTERN] [--regex|--literal] [--no-stdout] [--count] [--failure-only] -- COMMAND [ARGS...]\n\nDefault: case-insensitive regex err|arning. Always emit stderr and selected stdout to stderr; print ok on success. --count prints selected stdout line count on success. --failure-only suppresses diagnostics on success. Wrapper errors exit 125; child exit codes are preserved.");
                return Ok(0);
            }
            _ => return Err(format!("unknown option {:?}; use -- before the command", arg).into()),
        }
    }
    if command.is_empty() {
        return Err("missing command after --".into());
    }
    let matcher = Matcher::new(&pattern, literal, off)?;
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("stdout pipe missing")?;
    let stderr = child.stderr.take().ok_or("stderr pipe missing")?;
    let out_worker = thread::spawn(move || capture(stdout, Some(&matcher), CAPTURE_LIMIT));
    let err_worker = thread::spawn(move || capture(stderr, None, CAPTURE_LIMIT));
    let status = child.wait()?;
    let out_result = out_worker.join().map_err(|_| "stdout reader panicked")?;
    let err_result = err_worker.join().map_err(|_| "stderr reader panicked")?;
    let out = out_result?;
    let err = err_result?;
    if !status.success() || !failure_only {
        let mut dest = io::stderr().lock();
        dest.write_all(&err.bytes)?;
        if !err.bytes.is_empty() && !err.bytes.ends_with(b"\n") {
            dest.write_all(b"\n")?;
        }
        if !out.bytes.is_empty() {
            dest.write_all(b"[stdout diagnostics]\n")?;
            dest.write_all(&out.bytes)?;
            if !out.bytes.ends_with(b"\n") {
                dest.write_all(b"\n")?;
            }
        }
        if err.truncated {
            writeln!(
                dest,
                "agent-response: stderr truncated at {CAPTURE_LIMIT} bytes"
            )?;
        }
        if out.truncated {
            writeln!(
                dest,
                "agent-response: stdout truncated; diagnostics/count may be incomplete"
            )?;
        }
        if !status.success() {
            writeln!(dest, "agent-response: command {status}")?;
        }
    }
    if status.success() {
        if count {
            if out.truncated {
                return Err("cannot report an exact count after stdout truncation".into());
            }
            writeln!(io::stdout().lock(), "{}", out.matched_lines)?;
        } else {
            writeln!(io::stdout().lock(), "ok")?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code().unwrap_or(125))
    }
}
