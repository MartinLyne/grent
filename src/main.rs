use grent::{capture, Matcher, CAPTURE_LIMIT, DEFAULT_PATTERN};
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
            eprintln!("grent: {e}");
            125
        }
    };
    std::process::exit(code);
}
fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let mut pattern = DEFAULT_PATTERN.to_string();
    let mut explicit_filter = false;
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
            Some("--pattern") | Some("--filter") => {
                explicit_filter = true;
                pattern = args
                    .next()
                    .ok_or("--pattern needs a value")?
                    .into_string()
                    .map_err(|_| "pattern must be UTF-8")?
            }
            Some("--help") | Some("-h") => {
                println!("Usage: grent (or agent-response) [--pattern PATTERN] [--regex|--literal] [--no-stdout] [--count] [--failure-only] -- COMMAND [ARGS...]\n\nDefault: success prints only ok; failure shows stderr plus stdout matching err|arning. --filter/--pattern selects logs from both streams on success, without ok. --filter '*' passes through all output (quote the star). --literal treats * literally. --count prints selected stdout line count on success. --failure-only overrides filtered success output. Wrapper errors exit 125; child exit codes are preserved.");
                return Ok(0);
            }
            _ => return Err(format!("unknown option {:?}; use -- before the command", arg).into()),
        }
    }
    if command.is_empty() {
        return Err("missing command after --".into());
    }
    let all = explicit_filter && pattern == "*" && !literal;
    if all && !count && !failure_only && !off {
        let status = Command::new(&command[0]).args(&command[1..]).status()?;
        return Ok(exit_code(status));
    }
    let effective_pattern = if all { "" } else { &pattern };
    let matcher = Matcher::new(effective_pattern, literal, off)?;
    let success_matcher = Matcher::new(effective_pattern, literal, false)?;
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
    if !status.success() {
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
            writeln!(dest, "grent: stderr truncated at {CAPTURE_LIMIT} bytes")?;
        }
        if out.truncated {
            writeln!(
                dest,
                "grent: stdout truncated; diagnostics/count may be incomplete"
            )?;
        }
        if !status.success() {
            writeln!(dest, "grent: command {status}")?;
        }
    }
    if status.success() {
        if count {
            if out.truncated {
                return Err("cannot report an exact count after stdout truncation".into());
            }
            writeln!(io::stdout().lock(), "{}", out.matched_lines)?;
        } else if explicit_filter && !failure_only {
            let selected_err = capture(&err.bytes[..], Some(&success_matcher), CAPTURE_LIMIT)?;
            io::stdout().lock().write_all(&out.bytes)?;
            io::stderr().lock().write_all(&selected_err.bytes)?;
            if out.truncated || err.truncated || selected_err.truncated {
                eprintln!("grent: filtered output truncated");
            }
        } else {
            writeln!(io::stdout().lock(), "ok")?;
        }
    }
    Ok(exit_code(status))
}
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(125)
    }
}
