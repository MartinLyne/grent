use crate::store;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

pub struct Run {
    pub id: String,
    pub path: PathBuf,
    pub metadata: Value,
}
struct Spool {
    bytes: u64,
    error: Option<String>,
}
fn spool(
    mut input: impl Read,
    mut dest: File,
    limit: u64,
    failed: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
) -> Spool {
    let mut bytes = 0u64;
    let mut buf = [0u8; 16384];
    let result = (|| -> io::Result<()> {
        loop {
            if stopped.load(Ordering::SeqCst) {
                break;
            }
            let size = match input.read(&mut buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                other => other?,
            };
            if size == 0 {
                break;
            }
            let keep = (limit.saturating_sub(bytes)).min(size as u64) as usize;
            dest.write_all(&buf[..keep])?;
            bytes += keep as u64;
            if keep < size {
                overflow.store(true, Ordering::SeqCst);
                break;
            }
        }
        dest.flush()
    })();
    let error = result.err().map(|e| e.to_string());
    if error.is_some() {
        failed.store(true, Ordering::SeqCst);
    }
    Spool { bytes, error }
}
fn status_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(125)
    }
}
fn kill_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: a negative child process-group ID targets only that group; no pointers.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(unix)]
fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: fcntl operates on a live pipe descriptor with integer flags.
    unsafe {
        let flags = libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
pub fn execute(
    store_path: &Path,
    command: &[OsString],
    timeout: Duration,
    max_bytes: u64,
) -> io::Result<Run> {
    store::prune(store_path, store::RETENTION_HOURS)?;
    let (id, path) = store::create_run(store_path)?;
    let stdout = store::create_file(&path.join("stdout.bin"))?;
    let stderr = store::create_file(&path.join("stderr.bin"))?;
    let interrupted = Arc::new(AtomicBool::new(false));
    let terminated = Arc::new(AtomicBool::new(false));
    let int_registration =
        signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
    let term_registration =
        signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone())?;
    struct Signals(signal_hook::SigId, signal_hook::SigId);
    impl Drop for Signals {
        fn drop(&mut self) {
            signal_hook::low_level::unregister(self.0);
            signal_hook::low_level::unregister(self.1);
        }
    }
    let _signals = Signals(int_registration, term_registration);
    let start = Instant::now();
    let mut builder = Command::new(&command[0]);
    builder
        .args(&command[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    builder.process_group(0);
    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(error) => {
            let metadata = json!({"schema_version":1,"exit_code":125,"incomplete":true,"reason":format!("could not start command: {error}"),"stdout_bytes":0,"stderr_bytes":0,"finished_at":store::now(),"duration_ms":start.elapsed().as_secs_f64()*1000.0});
            store::save_metadata(&path, &metadata)?;
            return Ok(Run { id, path, metadata });
        }
    };
    let pid = child.id();
    struct Guard(u32);
    impl Drop for Guard {
        fn drop(&mut self) {
            kill_group(self.0);
        }
    }
    // On early I/O failure or panic, do not abandon children in our process group.
    let _guard = Guard(pid);
    let failed = Arc::new(AtomicBool::new(false));
    let overflow = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let input_out = child.stdout.take().expect("configured stdout pipe");
    let input_err = child.stderr.take().expect("configured stderr pipe");
    #[cfg(unix)]
    {
        nonblocking(&input_out)?;
        nonblocking(&input_err)?;
    }
    let (f, o, stop) = (failed.clone(), overflow.clone(), stopped.clone());
    let out_worker = thread::spawn(move || spool(input_out, stdout, max_bytes, f, o, stop));
    let (f, o, stop) = (failed.clone(), overflow.clone(), stopped.clone());
    let err_worker = thread::spawn(move || spool(input_err, stderr, max_bytes, f, o, stop));
    let mut status = None;
    let mut forced = None;
    loop {
        if status.is_none() {
            status = child.try_wait()?;
        }
        if interrupted.load(Ordering::SeqCst) {
            forced = Some((130, "interrupted"));
        } else if terminated.load(Ordering::SeqCst) {
            forced = Some((143, "terminated"));
        } else if failed.load(Ordering::SeqCst) {
            forced = Some((125, "could not retain command output"));
        } else if overflow.load(Ordering::SeqCst) {
            forced = Some((125, "retained output limit exceeded"));
        }
        if forced.is_some() {
            kill_group(pid);
            stopped.store(true, Ordering::SeqCst);
            break;
        }
        if status.is_some() && out_worker.is_finished() && err_worker.is_finished() {
            break;
        }
        if start.elapsed() >= timeout {
            forced = Some((124, "command or output pipes timed out"));
            kill_group(pid);
            stopped.store(true, Ordering::SeqCst);
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let status = match status {
        Some(value) => value,
        None => child.wait()?,
    };
    let out = out_worker
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))?;
    let err = err_worker
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))?;
    // Readers can discover a final error between the last flag check and joining.
    if forced.is_none()
        && (out.error.is_some() || err.error.is_some() || overflow.load(Ordering::SeqCst))
    {
        forced = Some((125, "retained output incomplete"));
    }
    let exit_code = forced.map(|v| v.0).unwrap_or_else(|| status_code(status));
    let reason = forced
        .map(|v| v.1.to_string())
        .or(out.error)
        .or(err.error)
        .unwrap_or_default();
    let metadata = json!({"schema_version":1,"exit_code":exit_code,"incomplete":forced.is_some(),"reason":reason,"stdout_bytes":out.bytes,"stderr_bytes":err.bytes,"finished_at":store::now(),"duration_ms":start.elapsed().as_secs_f64()*1000.0});
    store::save_metadata(&path, &metadata)?;
    Ok(Run { id, path, metadata })
}
