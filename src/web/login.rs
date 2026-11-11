//! Web stand-in: platform logins are desktop-only, so no cookies ever reach
//! a server.

use anyhow::{Result, bail};

use crate::model::Platform;
use crate::secrets::StoredCookie;

pub fn login(_: Platform) -> Result<Vec<StoredCookie>> {
    bail!("Logging in to platforms is only available in the desktop app")
}
