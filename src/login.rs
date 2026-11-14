//! Platform login through a webview.
//!
//! The webview runs in a separate executable, `local-playlist-login` (see
//! `src/bin/login.rs`), installed next to the app. That keeps the main GPUI
//! process on its native backend and keeps the webview libraries (WebKitGTK
//! and GTK on Linux) out of its memory entirely. The helper prints the session
//! cookies as JSON on stdout once the user has signed in.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

use crate::model::Platform;
use crate::secrets::StoredCookie;

/// File name of the helper executable.
pub const HELPER: &str = "local-playlist-login";

/// Where the helper is expected: next to the running executable.
fn helper_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot locate own executable")?;
    Ok(exe.with_file_name(format!("{HELPER}{}", std::env::consts::EXE_SUFFIX)))
}

/// Launch the helper and wait for cookies (blocking; run in background).
pub fn login(platform: Platform) -> Result<Vec<StoredCookie>> {
    let helper = helper_path()?;
    if !helper.is_file() {
        bail!(
            "The login window ({}) is missing. It ships next to local-playlist; when building from \
             source, `cargo build --release` builds both.",
            helper.display()
        );
    }
    let arg = match platform {
        Platform::YouTube => "youtube",
        Platform::SoundCloud => "soundcloud",
    };
    let out = std::process::Command::new(&helper)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .output()
        .context("failed to start login window")?;
    match out.status.code() {
        Some(0) => Ok(serde_json::from_slice(&out.stdout).context("login helper returned bad data")?),
        Some(2) => bail!("Login cancelled"),
        _ => bail!(
            "Login failed: {}",
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("unknown error")
        ),
    }
}
