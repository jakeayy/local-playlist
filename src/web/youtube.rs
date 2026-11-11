//! The parts of the YouTube source the web UI needs (metadata itself is
//! fetched by the server).

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::secrets::StoredCookie;

pub fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

pub fn is_personal_list(list: &str) -> bool {
    matches!(list, "LL" | "LM" | "WL")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub page_id: Option<String>,
}

#[derive(Clone, Copy)]
pub struct YtAuth<'a> {
    pub cookies: &'a [StoredCookie],
    pub page_id: Option<&'a str>,
}

pub fn channels(_: &[StoredCookie]) -> Result<Vec<Channel>> {
    bail!("not available in the web version")
}
