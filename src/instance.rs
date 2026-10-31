//! Keeping one hyprhopd per Hyprland session. The running copy holds a lock on a file
//! that stores its pid; a new copy stops that pid and takes the lock over. The kernel
//! drops a lock when its process dies, however it dies, so a crash leaves nothing stale.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::hyprland;

/// How long the old copy gets to exit after each signal.
const STOP_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

/// Makes this the only running hyprhopd, stopping any copy already running. The lock
/// lasts as long as the returned file stays open.
pub fn claim() -> Result<File, String> {
    let dir = hyprland::socket_dir().map_err(|err| err.to_string())?;
    let path = dir.join("hyprhopd.lock");
    claim_at(&path).map_err(|err| format!("cannot take over {}: {err}", path.display()))
}

fn claim_at(path: &Path) -> io::Result<File> {
    // Not truncated on open: until we hold the lock, the pid inside is the running copy's.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if !try_lock(&file)? {
        replace(&file, path)?;
    }
    file.set_len(0)?;
    write!(file, "{}", std::process::id())?;
    Ok(file)
}

/// Stops the copy holding the lock and waits until the lock is ours.
fn replace(file: &File, path: &Path) -> io::Result<()> {
    let Some(pid) = holder(file, path)? else {
        // It exited on its own while we looked.
        return Ok(());
    };
    info!("replacing the running hyprhopd (pid {pid})");
    if stop(file, pid, libc::SIGTERM)? {
        return Ok(());
    }
    warn!("hyprhopd (pid {pid}) didn't exit, killing it");
    if stop(file, pid, libc::SIGKILL)? {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "hyprhopd (pid {pid}) is still running"
    )))
}

/// The pid of the running copy, or None if the lock came free meanwhile. A copy that just
/// took the lock may not have written its pid yet, so the file can briefly hold nothing
/// or a dead copy's pid; checking the process's name keeps us from signalling a stranger.
fn holder(file: &File, path: &Path) -> io::Result<Option<i32>> {
    let deadline = Instant::now() + STOP_WAIT;
    loop {
        if try_lock(file)? {
            return Ok(None);
        }
        let pid = parse_pid(&fs::read_to_string(path)?);
        if let Some(pid) = pid.filter(|&pid| is_hyprhopd(pid)) {
            return Ok(Some(pid));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("the running copy's pid isn't readable"));
        }
        thread::sleep(POLL);
    }
}

/// Sends `signal` and waits up to STOP_WAIT for the lock, returning whether we got it.
fn stop(file: &File, pid: i32, signal: libc::c_int) -> io::Result<bool> {
    // Safe: kill only reads its arguments. A process that already exited is fine (ESRCH);
    // its lock is gone or about to be.
    unsafe { libc::kill(pid, signal) };
    let deadline = Instant::now() + STOP_WAIT;
    while Instant::now() < deadline {
        if try_lock(file)? {
            return Ok(true);
        }
        thread::sleep(POLL);
    }
    Ok(false)
}

fn try_lock(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// Only positive pids: kill treats 0 and negative numbers as whole process groups.
fn parse_pid(text: &str) -> Option<i32> {
    text.trim().parse().ok().filter(|&pid: &i32| pid > 0)
}

fn is_hyprhopd(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|name| name.trim() == "hyprhopd")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_positive_pids() {
        assert_eq!(parse_pid("1234\n"), Some(1234));
        assert_eq!(parse_pid(""), None);
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("-1"), None);
        assert_eq!(parse_pid("hyprhopd"), None);
    }

    #[test]
    fn the_lock_is_held_until_the_file_closes() {
        let dir = std::env::temp_dir().join(format!("hyprhopd-lock-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hyprhopd.lock");
        fs::write(&path, "a dead copy's pid").unwrap();

        let claimed = claim_at(&path).unwrap();
        assert_eq!(
            parse_pid(&fs::read_to_string(&path).unwrap()),
            Some(std::process::id() as i32)
        );
        // Another open of the same file is what a second copy would do.
        let other = File::open(&path).unwrap();
        assert!(!try_lock(&other).unwrap());

        drop(claimed);
        assert!(try_lock(&other).unwrap());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_hyprhopd_counts_as_the_running_copy() {
        // The test binary is called hyprhopd-<hash>, not hyprhopd.
        assert!(!is_hyprhopd(std::process::id() as i32));
        assert!(!is_hyprhopd(i32::MAX));
    }
}
