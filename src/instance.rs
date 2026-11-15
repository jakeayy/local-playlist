//! One copy of the desktop app at a time. Closing the window can leave the
//! app playing in the tray, so starting it again is a normal way to get the
//! window back: a second copy asks the running one to show its window and
//! exits, rather than becoming a second player saving the same library.
//!
//! The running copy holds a lock on `<data dir>/instance.lock` (released by
//! the system however the app ends). A second copy leaves a `show-window`
//! file next to it, which the running one picks up on its next tick.

use std::fs::{File, TryLockError};
use std::path::PathBuf;

use crate::model::Paths;

/// Keeps the lock for as long as it lives.
pub struct Lock(#[allow(dead_code)] Option<File>);

pub enum Start {
    /// No other copy runs: go ahead, keeping the lock.
    First(Lock),
    /// Another copy runs and was asked to show its window.
    Running,
}

fn request_file(paths: &Paths) -> PathBuf {
    paths.data.join("show-window")
}

pub fn start(paths: &Paths) -> Start {
    let file = File::options().create(true).write(true).truncate(false).open(paths.data.join("instance.lock"));
    let file = match file {
        Ok(f) => f,
        Err(e) => {
            // Better to run than to refuse because of the lock file.
            eprintln!("can't check for a running copy: {e}");
            return Start::First(Lock(None));
        }
    };
    match file.try_lock() {
        Ok(()) => {
            // A request left over from before isn't meant for this run.
            let _ = std::fs::remove_file(request_file(paths));
            Start::First(Lock(Some(file)))
        }
        Err(TryLockError::WouldBlock) => {
            if let Err(e) = File::create(request_file(paths)) {
                eprintln!("can't ask the running copy to show its window: {e}");
            }
            Start::Running
        }
        Err(TryLockError::Error(e)) => {
            eprintln!("can't check for a running copy: {e}");
            Start::First(Lock(Some(file)))
        }
    }
}

/// Whether another copy asked this one to show its window (once per request).
pub fn show_requested(paths: &Paths) -> bool {
    std::fs::remove_file(request_file(paths)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_copy_asks_the_first() {
        let dir = std::env::temp_dir().join(format!("lp-instance-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths { data: dir.clone(), cache: dir.clone() };
        let first = start(&paths);
        assert!(matches!(first, Start::First(_)));
        assert!(!show_requested(&paths));
        assert!(matches!(start(&paths), Start::Running));
        assert!(show_requested(&paths));
        assert!(!show_requested(&paths), "a request is taken once");
        drop(first);
        assert!(matches!(start(&paths), Start::First(_)), "the lock goes with the first copy");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
