//! Private, bounded, locally retained command output.
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
pub const HARD_LOG_LIMIT: u64 = 1024 * 1024 * 1024;
pub const RETENTION_HOURS: u64 = 168;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn private_dir(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid(
            "store and run paths must be real directories, not symlinks",
        ));
    }
    #[cfg(unix)]
    {
        // SAFETY: getuid has no arguments or memory preconditions.
        if metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0 {
            return Err(invalid(
                "store and run directories must be owned by you with mode 700",
            ));
        }
    }
    Ok(())
}
pub fn prepare(path: &Path) -> io::Result<()> {
    if !path.try_exists()? {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
    }
    private_dir(path)
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 17
        && id.starts_with('r')
        && id[1..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn run_path(store: &Path, id: &str) -> io::Result<PathBuf> {
    if !valid_id(id) {
        return Err(invalid("invalid run ID"));
    }
    private_dir(store)?;
    let path = store.join(id);
    private_dir(&path)?;
    Ok(path)
}
pub fn create_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}
pub fn read_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("retained file is not a regular file"));
    }
    #[cfg(unix)]
    {
        // SAFETY: getuid has no arguments or memory preconditions.
        if metadata.uid() != unsafe { libc::getuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(invalid(
                "retained files must be private, owned by you, and not hard-linked",
            ));
        }
    }
    Ok(file)
}
pub fn create_run(store: &Path) -> io::Result<(String, PathBuf)> {
    prepare(store)?;
    for attempt in 0..100u64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let id = format!(
            "r{:016x}",
            nanos ^ ((std::process::id() as u64) << 32) ^ attempt
        );
        let path = store.join(&id);
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => return Ok((id, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(invalid("could not allocate unique run ID"))
}
pub fn save_metadata(path: &Path, metadata: &Value) -> io::Result<()> {
    let temp = path.join("metadata.tmp");
    let mut file = create_file(&temp)?;
    file.write_all(serde_json::to_string(metadata)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(temp, path.join("metadata.json"))
}
pub fn metadata(path: &Path) -> io::Result<Value> {
    let file = read_file(&path.join("metadata.json"))?;
    if file.metadata()?.len() > 64 * 1024 {
        return Err(invalid("run metadata too large"));
    }
    let mut data = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut data)?;
    let result: Value = serde_json::from_str(&data)?;
    if result["schema_version"] != 1
        || !result["exit_code"].is_i64()
        || !result["incomplete"].is_boolean()
    {
        return Err(invalid("invalid or unsupported run metadata"));
    }
    Ok(result)
}
pub fn forget(store: &Path, id: &str) -> io::Result<()> {
    let path = run_path(store, id)?;
    // remove_dir_all does not follow symlinks. The private parent excludes other users.
    fs::remove_dir_all(path)
}
pub fn prune(store: &Path, hours: u64) -> io::Result<usize> {
    prepare(store)?;
    let threshold = now().saturating_sub(hours.saturating_mul(3600));
    let mut count = 0;
    for entry in fs::read_dir(store)? {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !valid_id(&id) || private_dir(&entry.path()).is_err() {
            continue;
        }
        // Only completed, valid runs: never remove a running command's directory.
        if let Ok(meta) = metadata(&entry.path()) {
            if meta["finished_at"]
                .as_u64()
                .is_some_and(|time| time < threshold)
            {
                match forget(store, &id) {
                    Ok(()) => count += 1,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(count)
}
