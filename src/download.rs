//! Shared state of one background audio download (read by the UI and the
//! streaming player).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::storage::find_file;

pub struct Download {
    progress: AtomicU32, // f32 bits, 0..1
    result: Mutex<Option<Result<PathBuf, String>>>,
    /// Folder yt-dlp writes into while downloading.
    pub tmp_dir: PathBuf,
    pub key: String,
    /// Set to stop the download (kills yt-dlp).
    pub cancel: Arc<AtomicBool>,
}

pub const CANCELLED: &str = "cancelled";

pub enum DlState {
    Running,
    Done(PathBuf),
    Failed(String),
}

impl Download {
    pub fn new(tmp_dir: PathBuf, key: String) -> Self {
        Self { progress: AtomicU32::new(0), result: Mutex::new(None), tmp_dir, key, cancel: Default::default() }
    }

    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// Monotonic: non-negative f32 bit patterns order like the floats.
    pub fn set_progress(&self, p: f32) {
        self.progress.fetch_max(p.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn finish(&self, r: Result<PathBuf, String>) {
        *self.result.lock().unwrap() = Some(r);
    }

    pub fn state(&self) -> DlState {
        match &*self.result.lock().unwrap() {
            None => DlState::Running,
            Some(Ok(p)) => DlState::Done(p.clone()),
            Some(Err(e)) => DlState::Failed(e.clone()),
        }
    }

    pub fn result(&self) -> Option<Result<PathBuf, String>> {
        self.result.lock().unwrap().clone()
    }

    /// The file yt-dlp is currently writing, once it exists.
    pub fn partial_file(&self) -> Option<PathBuf> {
        find_file(&self.tmp_dir, &self.key)
    }
}
