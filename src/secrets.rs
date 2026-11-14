//! Encrypted local storage for platform session cookies.
//!
//! Cookies are serialized to JSON and encrypted with ChaCha20-Poly1305. The
//! 256-bit key lives in the OS credential store (Secret Service / Windows
//! Credential Manager / macOS Keychain). If no credential store is available,
//! the key falls back to a 0600 file next to the data, and the UI says so.

use std::collections::BTreeMap;
use std::path::PathBuf;

#[cfg(feature = "desktop")]
use anyhow::Context as _;
use anyhow::{Result, bail};
#[cfg(feature = "desktop")]
use chacha20poly1305::aead::{Aead, KeyInit};
#[cfg(feature = "desktop")]
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};

use crate::model::{Paths, Platform};

#[cfg(feature = "desktop")]
const KEYRING_SERVICE: &str = "local-playlist";
#[cfg(feature = "desktop")]
const KEYRING_USER: &str = "cookie-encryption-key";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    /// Unix timestamp, `None` for session cookies.
    #[serde(default)]
    pub expires: Option<i64>,
}

fn root_path() -> String {
    "/".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub sessions: BTreeMap<String, Vec<StoredCookie>>,
    /// Selected YouTube channel (Brand Account); `None` = default channel.
    #[serde(default)]
    pub youtube_channel: Option<crate::sources::youtube::Channel>,
}

fn platform_key(p: Platform) -> &'static str {
    match p {
        Platform::YouTube => "youtube",
        Platform::SoundCloud => "soundcloud",
    }
}

impl Credentials {
    pub fn get(&self, p: Platform) -> Option<&[StoredCookie]> {
        self.sessions.get(platform_key(p)).map(Vec::as_slice).filter(|c| !c.is_empty())
    }

    pub fn set(&mut self, p: Platform, cookies: Vec<StoredCookie>) {
        self.sessions.insert(platform_key(p).into(), cookies);
    }

    pub fn remove(&mut self, p: Platform) {
        self.sessions.remove(platform_key(p));
        if p == Platform::YouTube {
            self.youtube_channel = None;
        }
    }

    pub fn yt_auth(&self) -> Option<crate::sources::youtube::YtAuth<'_>> {
        Some(crate::sources::youtube::YtAuth {
            cookies: self.get(Platform::YouTube)?,
            page_id: self.youtube_channel.as_ref().and_then(|c| c.page_id.as_deref()),
        })
    }
}

/// Where the key ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStorage {
    Keyring,
    File,
}

pub struct SecretStore {
    #[allow(dead_code)]
    file: PathBuf,
    #[allow(dead_code)]
    key_file: PathBuf,
}

/// Builds without the desktop app (browser, server) never store platform logins.
#[cfg(not(feature = "desktop"))]
impl SecretStore {
    pub fn new(paths: &Paths) -> Self {
        Self { file: paths.data.clone(), key_file: paths.data.clone() }
    }

    pub fn load(&self) -> Result<(Credentials, Option<KeyStorage>)> {
        Ok((Credentials::default(), None))
    }

    pub fn save(&self, _: &Credentials) -> Result<KeyStorage> {
        bail!("logins are only available in the desktop app")
    }
}

#[cfg(feature = "desktop")]
impl SecretStore {
    pub fn new(paths: &Paths) -> Self {
        Self { file: paths.data.join("credentials.bin"), key_file: paths.data.join("credentials.key") }
    }

    fn key(&self, create: bool) -> Result<Option<([u8; 32], KeyStorage)>> {
        // 1. OS credential store.
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            match entry.get_secret() {
                Ok(k) if k.len() == 32 => return Ok(Some((k.try_into().unwrap(), KeyStorage::Keyring))),
                _ if !self.key_file.exists() && create => {
                    let k = random_key()?;
                    if entry.set_secret(&k).is_ok() && entry.get_secret().is_ok_and(|r| r == k) {
                        return Ok(Some((k, KeyStorage::Keyring)));
                    }
                }
                _ => {}
            }
        }
        // 2. Fallback key file.
        if let Ok(bytes) = std::fs::read(&self.key_file)
            && bytes.len() == 32
        {
            return Ok(Some((bytes.try_into().unwrap(), KeyStorage::File)));
        }
        if !create {
            return Ok(None);
        }
        let k = random_key()?;
        write_private(&self.key_file, &k)?;
        Ok(Some((k, KeyStorage::File)))
    }

    pub fn load(&self) -> Result<(Credentials, Option<KeyStorage>)> {
        let Ok(blob) = std::fs::read(&self.file) else { return Ok((Credentials::default(), None)) };
        let Some((key, storage)) = self.key(false)? else {
            bail!("saved logins can't be decrypted (encryption key missing from the keyring)");
        };
        if blob.len() < 12 {
            bail!("credentials file is corrupt");
        }
        let cipher = ChaCha20Poly1305::new(&Key::from(key));
        let nonce: [u8; 12] = blob[..12].try_into().unwrap();
        let plain = cipher
            .decrypt(&Nonce::from(nonce), &blob[12..])
            .map_err(|_| anyhow::anyhow!("saved logins can't be decrypted"))?;
        Ok((serde_json::from_slice(&plain)?, Some(storage)))
    }

    pub fn save(&self, creds: &Credentials) -> Result<KeyStorage> {
        let (key, storage) = self.key(true)?.context("no encryption key")?;
        let cipher = ChaCha20Poly1305::new(&Key::from(key));
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("rng: {e}"))?;
        let ct = cipher
            .encrypt(&Nonce::from(nonce), serde_json::to_vec(creds)?.as_slice())
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        let mut blob = nonce.to_vec();
        blob.extend(ct);
        write_private(&self.file, &blob)?;
        Ok(storage)
    }
}

#[cfg(feature = "desktop")]
fn random_key() -> Result<[u8; 32]> {
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).map_err(|e| anyhow::anyhow!("rng: {e}"))?;
    Ok(k)
}

#[cfg(not(target_family = "wasm"))]
/// Write a file readable only by the current user.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Parse user-provided cookies: Netscape `cookies.txt` (as exported by browser
/// extensions) or a raw `name=value; name2=value2` Cookie header.
pub fn parse_cookies(text: &str, platform: Platform) -> Result<Vec<StoredCookie>> {
    let default_domain = match platform {
        Platform::YouTube => ".youtube.com",
        Platform::SoundCloud => ".soundcloud.com",
    };
    let relevant = |domain: &str| {
        let d = domain.trim_start_matches('.');
        match platform {
            Platform::YouTube => d == "youtube.com" || d.ends_with(".youtube.com"),
            Platform::SoundCloud => d == "soundcloud.com" || d.ends_with(".soundcloud.com"),
        }
    };

    let text = text.trim();
    let mut cookies = Vec::new();
    let looks_netscape = text.lines().any(|l| l.split('\t').count() >= 7);
    if looks_netscape {
        for line in text.lines() {
            let (line, http_only) = match line.strip_prefix("#HttpOnly_") {
                Some(rest) => (rest, true),
                None => (line, false),
            };
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 7 || !relevant(f[0]) {
                continue;
            }
            let expires = f[4].trim().parse::<i64>().ok().filter(|e| *e > 0);
            cookies.push(StoredCookie {
                domain: f[0].to_string(),
                path: f[2].to_string(),
                secure: f[3].eq_ignore_ascii_case("TRUE"),
                expires,
                name: f[5].to_string(),
                value: f[6].trim_end_matches(['\r', '\n']).to_string(),
                http_only,
            });
        }
    } else {
        let header = text.strip_prefix("Cookie:").unwrap_or(text);
        for pair in header.split(';') {
            let Some((name, value)) = pair.split_once('=') else { continue };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            cookies.push(StoredCookie {
                name: name.to_string(),
                value: value.trim().to_string(),
                domain: default_domain.to_string(),
                path: "/".into(),
                secure: true,
                http_only: false,
                expires: None,
            });
        }
    }

    let required: &[&str] = match platform {
        Platform::YouTube => &["SAPISID", "__Secure-3PAPISID"],
        Platform::SoundCloud => &["oauth_token"],
    };
    if !cookies.iter().any(|c| required.contains(&c.name.as_str())) {
        bail!(
            "No {} session cookie found (expected {}). Make sure you copied cookies while logged in.",
            platform.label(),
            required.join(" or ")
        );
    }
    Ok(cookies)
}

/// `Cookie:` header value for requests to the given host.
pub fn cookie_header(cookies: &[StoredCookie], host: &str) -> String {
    cookies
        .iter()
        .filter(|c| {
            let d = c.domain.trim_start_matches('.');
            host == d || host.ends_with(&format!(".{d}"))
        })
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn find<'a>(cookies: &'a [StoredCookie], name: &str) -> Option<&'a str> {
    cookies.iter().find(|c| c.name == name).map(|c| c.value.as_str())
}

/// Netscape cookies.txt for handing a session to yt-dlp.
pub fn to_netscape(cookies: &[StoredCookie]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for c in cookies {
        let domain =
            if c.domain.starts_with('.') || c.domain.is_empty() { c.domain.clone() } else { format!(".{}", c.domain) };
        out.push_str(&format!(
            "{}{}\tTRUE\t{}\t{}\t{}\t{}\t{}\n",
            if c.http_only { "#HttpOnly_" } else { "" },
            domain,
            c.path,
            if c.secure { "TRUE" } else { "FALSE" },
            c.expires.unwrap_or(0),
            c.name,
            c.value
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netscape_and_header() {
        let txt = "# Netscape HTTP Cookie File\n\
            .youtube.com\tTRUE\t/\tTRUE\t1893456000\tSAPISID\tabc/def\n\
            #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tSID\txyz\n\
            .google.com\tTRUE\t/\tTRUE\t0\tNID\tnope\n";
        let c = parse_cookies(txt, Platform::YouTube).unwrap();
        assert_eq!(c.len(), 2);
        assert!(c[1].http_only);
        assert_eq!(find(&c, "SAPISID"), Some("abc/def"));
        assert_eq!(cookie_header(&c, "www.youtube.com"), "SAPISID=abc/def; SID=xyz");

        let c = parse_cookies("Cookie: oauth_token=2-123; sc_anonymous_id=x", Platform::SoundCloud).unwrap();
        assert_eq!(find(&c, "oauth_token"), Some("2-123"));
        assert!(parse_cookies("foo=bar", Platform::SoundCloud).is_err());

        let round = parse_cookies(&to_netscape(&c), Platform::SoundCloud).unwrap();
        assert_eq!(round.len(), c.len());
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn encrypt_roundtrip_with_file_key() {
        let dir = std::env::temp_dir().join(format!("lp-secret-test-{}", std::process::id()));
        let paths = Paths { data: dir.clone(), cache: dir.join("cache") };
        let store = SecretStore::new(&paths);
        // Force file-key mode by pre-creating the key file.
        write_private(&store.key_file, &random_key().unwrap()).unwrap();
        let mut creds = Credentials::default();
        creds.set(Platform::SoundCloud, parse_cookies("oauth_token=t", Platform::SoundCloud).unwrap());
        store.save(&creds).unwrap();
        let (loaded, _) = store.load().unwrap();
        assert_eq!(loaded.get(Platform::SoundCloud).unwrap()[0].value, "t");
        let _ = std::fs::remove_dir_all(dir);
    }
}
