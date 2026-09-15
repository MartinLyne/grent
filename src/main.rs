use grent::{
    query::{self, Mode},
    runner, store, Matcher, DEFAULT_PATTERN,
};
use serde_json::json;
use std::{
    env,
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    time::Duration,
};

const HELP: &str = "grent — explicit command results, retained locally\n\nUsage: grent [check|count|exists|preview] [OPTIONS] -- COMMAND [ARGS...]\n       grent read RUN_ID [--mode count|exists|preview|full] [OPTIONS]\n       grent forget RUN_ID [--store DIR]\n       grent prune [--older-than-hours N] [--store DIR]\n\nagent-response is the same executable under a descriptive name.\n\nOptions:\n  --json                    Structured response including run_id\n  --filter, --pattern TEXT  Case-insensitive regex; use --literal for plain text\n  --regex / --literal       Interpret filter as regex (default) or literal text\n  --stream stdout|stderr   Stream to query (default stdout)\n  --lines N                Maximum matching preview lines (default 20)\n  --offset N               Skip N matching lines (default 0)\n  --max-bytes N            Preview/diagnostic byte limit (default 8192)\n  --timeout-ms N           Execution and pipe deadline (default 30000)\n  --max-log-bytes N         Retained limit PER STREAM (default 67108864)\n  --store DIR              Override GRENT_STORE or local state directory\n\ncheck: only ok on success; count: matching line count; exists: true/false.\npreview: bounded matching lines. Use --json to obtain a retained run ID.\nread queries that run without executing the command again. Count/exists refuse\nincomplete logs. Preview reports truncation; full removes the line limit but\nkeeps --max-bytes. Forget removes one run. Completed runs expire after 7 days\nwhen a command starts, or explicitly via prune.\n\nCompatibility: --count selects err|arning unless a filter is supplied; explicit\ncount selects all lines. A filter without a mode returns matching lines on\nsuccess. --filter '*' passes through retained output; quote it. --literal makes\n'*' literal. --failure-only suppresses successful logs; --no-stdout omits stdout.\n\nChild status is preserved; absence in exists is success. Timeout=124, wrapper\nerror=125, Unix signal=128+signal. Use shell pipefail for pipelines when needed.\n";

struct Options {
    mode: Mode,
    action: String,
    id: Option<String>,
    json: bool,
    filter: Option<String>,
    literal: bool,
    no_stdout: bool,
    failure_only: bool,
    lines: u64,
    offset: u64,
    max_bytes: usize,
    timeout_ms: u64,
    max_log: u64,
    store: PathBuf,
    stream: String,
    command: Vec<OsString>,
    legacy_filter: bool,
    hours: u64,
}
fn default_store() -> Result<PathBuf, String> {
    if let Some(value) = env::var_os("GRENT_STORE") {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(value).join("grent"));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/state/grent"))
        .ok_or("set GRENT_STORE or HOME".into())
}
fn parse() -> Result<Option<Options>, String> {
    let mut args = env::args_os().skip(1).peekable();
    let mut mode = Mode::Check;
    let mut action = "run".to_string();
    let mut explicit_mode = false;
    if let Some(word) = args.peek().and_then(|s| s.to_str()) {
        if let Some(value) = Mode::parse(word) {
            if value == Mode::Full {
                return Err("use read --mode full or --filter '*'".into());
            }
            mode = value;
            explicit_mode = true;
            args.next();
        } else if matches!(word, "read" | "forget" | "prune") {
            action = word.to_string();
            args.next();
        }
    }
    let id = if matches!(action.as_str(), "read" | "forget") {
        Some(
            args.next()
                .ok_or("missing run ID")?
                .into_string()
                .map_err(|_| "invalid run ID")?,
        )
    } else {
        None
    };
    if action == "read" {
        mode = Mode::Preview;
    }
    let mut options = Options {
        mode,
        action,
        id,
        json: false,
        filter: None,
        literal: false,
        no_stdout: false,
        failure_only: false,
        lines: 20,
        offset: 0,
        max_bytes: 8192,
        timeout_ms: 30000,
        max_log: store::MAX_LOG_BYTES,
        store: PathBuf::new(),
        stream: "stdout".into(),
        command: Vec::new(),
        legacy_filter: false,
        hours: store::RETENTION_HOURS,
    };
    let mut legacy_count = false;
    while let Some(arg) = args.next() {
        let text = arg.to_str().ok_or("option is not UTF-8")?;
        if text == "--" {
            options.command.extend(args);
            break;
        }
        match text {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--version" | "-V" => {
                println!("grent {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--json" => options.json = true,
            "--regex" => options.literal = false,
            "--literal" => options.literal = true,
            "--no-stdout" => options.no_stdout = true,
            "--failure-only" => options.failure_only = true,
            "--count" => {
                options.mode = Mode::Count;
                legacy_count = true;
            }
            "--store" => {
                options.store = PathBuf::from(args.next().ok_or("--store needs a directory")?)
            }
            "--filter" | "--pattern" | "--mode" | "--stream" | "--lines" | "--offset"
            | "--max-bytes" | "--timeout-ms" | "--max-log-bytes" | "--older-than-hours" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{text} needs a value"))?
                    .into_string()
                    .map_err(|_| "option value is not UTF-8")?;
                match text {
                    "--filter" | "--pattern" => options.filter = Some(value),
                    "--mode" => {
                        if options.action != "read" {
                            return Err("--mode is only valid for read".into());
                        }
                        options.mode = Mode::parse(&value).ok_or("unknown read mode")?;
                    }
                    "--stream" => {
                        if !matches!(value.as_str(), "stdout" | "stderr") {
                            return Err("stream must be stdout or stderr".into());
                        }
                        options.stream = value;
                    }
                    _ => {
                        let number: u64 = value
                            .parse()
                            .map_err(|_| format!("{text} requires a non-negative integer"))?;
                        match text {
                            "--lines" => options.lines = number,
                            "--offset" => options.offset = number,
                            "--max-bytes" => {
                                options.max_bytes =
                                    usize::try_from(number).map_err(|_| "byte limit too large")?
                            }
                            "--timeout-ms" => options.timeout_ms = number,
                            "--max-log-bytes" => options.max_log = number,
                            "--older-than-hours" => options.hours = number,
                            _ => unreachable!(),
                        }
                    }
                }
            }
            _ => return Err(format!("unknown option {text:?}; put the command after --")),
        }
    }
    if options.store.as_os_str().is_empty() {
        options.store = default_store()?;
    }
    if options.action == "run" && options.command.is_empty() {
        return Err("missing command after --".into());
    }
    if options.action != "run" && (options.failure_only || options.no_stdout || legacy_count) {
        return Err("legacy output flags are only valid when executing a command".into());
    }
    if options.action == "read" && options.mode == Mode::Check {
        return Err("read mode must be count, exists, preview, or full".into());
    }
    if options.action != "run" && !options.command.is_empty() {
        return Err("read/forget/prune cannot execute commands".into());
    }
    if options.timeout_ms == 0 {
        return Err("timeout must be greater than zero".into());
    }
    if options.max_log == 0
        || options.max_log > store::HARD_LOG_LIMIT
        || options.max_bytes as u64 > store::HARD_LOG_LIMIT
    {
        return Err("byte limits must fit within 1 GiB; log limit must be nonzero".into());
    }
    if legacy_count && options.filter.is_none() {
        options.filter = Some(DEFAULT_PATTERN.into());
    }
    if !explicit_mode && options.action == "run" && options.filter.is_some() && !legacy_count {
        options.mode = Mode::Full;
        options.legacy_filter = true;
    }
    Ok(Some(options))
}
fn matcher(options: &Options, default: &str, off: bool) -> Result<Matcher, regex::Error> {
    let pattern = options.filter.as_deref().unwrap_or(default);
    let pattern = if pattern == "*" && !options.literal {
        ""
    } else {
        pattern
    };
    Matcher::new(pattern, options.literal, off)
}
fn prefix(path: &std::path::Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = store::read_file(path)?;
    let mut bytes = Vec::new();
    file.take(limit as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn diagnostics(
    run: &runner::Run,
    options: &Options,
) -> Result<(String, bool), Box<dyn std::error::Error>> {
    let mut bytes = prefix(&run.path.join("stderr.bin"), options.max_bytes)?;
    let remaining = options.max_bytes.saturating_sub(bytes.len());
    let selected = query::select(
        &run.path.join("stdout.bin"),
        &matcher(options, DEFAULT_PATTERN, options.no_stdout)?,
        Mode::Full,
        0,
        u64::MAX,
        remaining,
    )?;
    if !selected.output.is_empty() {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"[stdout diagnostics]\n");
        bytes.extend_from_slice(&selected.output);
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let truncated = run.metadata["stderr_bytes"].as_u64().unwrap_or(0) > options.max_bytes as u64
        || selected.truncated;
    if truncated {
        text.push_str("grent: diagnostics truncated; full available output retained\n");
    }
    if let Some(reason) = run.metadata["reason"].as_str().filter(|s| !s.is_empty()) {
        text.push_str(reason);
        text.push('\n');
    }
    text.push_str(&format!(
        "grent: exit {}; run {}\n",
        run.metadata["exit_code"], run.id
    ));
    Ok((text, truncated))
}
fn response(
    options: &Options,
    run: &runner::Run,
    reading: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    let source_code = run.metadata["exit_code"]
        .as_i64()
        .ok_or("missing source exit status")? as i32;
    let incomplete = run.metadata["incomplete"].as_bool().unwrap_or(true);
    let legacy_all = options.action == "run"
        && options.legacy_filter
        && options.filter.as_deref() == Some("*")
        && !options.literal
        && !options.failure_only
        && !options.no_stdout
        && !options.json;
    if legacy_all && !incomplete {
        io::copy(
            &mut store::read_file(&run.path.join("stdout.bin"))?,
            &mut io::stdout().lock(),
        )?;
        io::copy(
            &mut store::read_file(&run.path.join("stderr.bin"))?,
            &mut io::stderr().lock(),
        )?;
        return Ok(source_code);
    }
    let mut result = json!({"exit_code":if reading {0} else {source_code},"source_exit_code":source_code,"run_id":run.id,"mode":options.mode.name(),"incomplete":incomplete,"truncated":false});
    if !reading && source_code != 0 {
        let (text, truncated) = diagnostics(run, options)?;
        result["truncated"] = json!(truncated);
        result["diagnostics"] = json!(text);
        if options.json {
            println!("{result}");
        } else {
            eprint!("{text}");
        }
        return Ok(source_code);
    }
    if incomplete && matches!(options.mode, Mode::Count | Mode::Exists | Mode::Check) {
        result["exit_code"] = json!(125);
        result["error"] = json!("cannot give an exact answer from incomplete output");
        if options.json {
            println!("{result}");
        } else {
            eprintln!(
                "grent: cannot give an exact answer from incomplete output; run {}",
                run.id
            );
        }
        return Ok(125);
    }
    let mode = if options.failure_only {
        Mode::Check
    } else {
        options.mode
    };
    if mode == Mode::Check {
        result["status"] = json!("ok");
        if options.json {
            println!("{result}");
        } else {
            println!("ok");
        }
        return Ok(0);
    }
    let selection = query::select(
        &run.path.join(format!("{}.bin", options.stream)),
        &matcher(options, "", options.no_stdout && options.stream == "stdout")?,
        mode,
        options.offset,
        if mode == Mode::Full {
            u64::MAX
        } else {
            options.lines
        },
        options.max_bytes,
    )?;
    result["truncated"] = json!(selection.truncated);
    match mode {
        Mode::Count => result["count"] = json!(selection.count),
        Mode::Exists => result["exists"] = json!(selection.count > 0),
        _ => {
            result["output"] = json!(String::from_utf8_lossy(&selection.output));
            result["matched_lines"] = json!(selection.count);
        }
    }
    if options.json {
        println!("{result}");
    } else {
        match mode {
            Mode::Count => println!("{}", selection.count),
            Mode::Exists => println!("{}", selection.count > 0),
            _ => {
                io::stdout().lock().write_all(&selection.output)?;
                if options.legacy_filter {
                    let selected = query::select(
                        &run.path.join("stderr.bin"),
                        &matcher(options, "", false)?,
                        Mode::Full,
                        0,
                        u64::MAX,
                        options.max_bytes,
                    )?;
                    io::stderr().lock().write_all(&selected.output)?;
                    if selected.truncated {
                        eprintln!("\ngrent: stderr truncated; run {}", run.id);
                    }
                }
                if selection.truncated || incomplete {
                    eprintln!("\ngrent: output truncated or incomplete; run {}", run.id);
                }
            }
        }
    }
    Ok(0)
}
fn run(options: Options) -> Result<i32, Box<dyn std::error::Error>> {
    // Validate filters before creating a run or launching any command.
    let _validated = matcher(&options, "", false)?;
    match options.action.as_str() {
        "forget" => {
            store::forget(
                &options.store,
                options.id.as_deref().ok_or("missing run ID")?,
            )?;
            if options.json {
                println!("{}", json!({"status":"ok"}));
            } else {
                println!("ok");
            }
            Ok(0)
        }
        "prune" => {
            let count = store::prune(&options.store, options.hours)?;
            if options.json {
                println!("{}", json!({"removed":count}));
            } else {
                println!("{count}");
            }
            Ok(0)
        }
        "read" => {
            let id = options.id.as_deref().ok_or("missing run ID")?;
            let path = store::run_path(&options.store, id)?;
            let metadata = store::metadata(&path)?;
            response(
                &options,
                &runner::Run {
                    id: id.into(),
                    path,
                    metadata,
                },
                true,
            )
        }
        _ => {
            let run = runner::execute(
                &options.store,
                &options.command,
                Duration::from_millis(options.timeout_ms),
                options.max_log,
            )?;
            response(&options, &run, false)
        }
    }
}
fn main() {
    #[cfg(not(unix))]
    {
        eprintln!("grent currently supports macOS and Linux");
        std::process::exit(125);
    }
    let json_requested = env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json");
    let result = match parse() {
        Ok(Some(options)) => run(options),
        Ok(None) => Ok(0),
        Err(error) => Err(error.into()),
    };
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            if json_requested {
                println!("{}", json!({"exit_code":125,"error":error.to_string()}));
            } else {
                eprintln!("grent: {error}");
            }
            125
        }
    };
    std::process::exit(code);
}
