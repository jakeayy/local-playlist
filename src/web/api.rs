//! Client for the Local Playlist server API. The access token is kept in
//! localStorage; platform cookies are never sent (the web UI has none).

use anyhow::{Result, anyhow, bail};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

use crate::model::Track;
use crate::sources::{Link, Resolved, link_url};

const TOKEN_KEY: &str = "local-playlist.server-token";
/// Error text the UI recognises as "show the server login screen".
pub const UNAUTHORIZED: &str = "Server login required";

pub fn token() -> Option<String> {
    super::storage_get(TOKEN_KEY).filter(|t| !t.is_empty())
}

pub fn logout() {
    super::storage_remove(TOKEN_KEY);
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn fetch(method: &str, url: &str, body: Option<String>) -> Result<(u16, serde_json::Value)> {
    let init = RequestInit::new();
    init.set_method(method);
    if let Some(b) = body {
        init.set_body(&b.into());
    }
    let req = Request::new_with_str_and_init(url, &init).map_err(|_| anyhow!("bad request"))?;
    let headers = req.headers();
    let _ = headers.set("Content-Type", "application/json");
    if let Some(t) = token() {
        let _ = headers.set("Authorization", &format!("Bearer {t}"));
    }
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|_| anyhow!("can't reach the server"))?
        .dyn_into()
        .map_err(|_| anyhow!("bad response"))?;
    let status = resp.status();
    let text = JsFuture::from(resp.text().map_err(|_| anyhow!("bad response"))?)
        .await
        .map_err(|_| anyhow!("bad response"))?
        .as_string()
        .unwrap_or_default();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if status == 401 && !url.ends_with("/api/login") {
        logout();
        bail!(UNAUTHORIZED);
    }
    Ok((status, json))
}

fn api_error(status: u16, json: &serde_json::Value) -> anyhow::Error {
    anyhow!("{}", json["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("server error {status}")))
}

pub async fn login(password: String) -> Result<()> {
    let body = serde_json::json!({ "password": password }).to_string();
    let (status, json) = fetch("POST", "/api/login", Some(body)).await?;
    match json["token"].as_str() {
        Some(t) if status == 200 => super::storage_set(TOKEN_KEY, t),
        _ => Err(api_error(status, &json)),
    }
}

/// Whether the stored token is still accepted (server restarts invalidate it).
pub async fn check() -> bool {
    token().is_some() && fetch("GET", "/api/check", None).await.is_ok_and(|(s, _)| s == 200)
}

pub async fn resolve(link: &Link, as_collection: bool) -> Result<Resolved> {
    let url = format!("/api/resolve?url={}&collection={}", enc(&link_url(link)), if as_collection { 1 } else { 0 });
    let (status, json) = fetch("GET", &url, None).await?;
    if status != 200 {
        return Err(api_error(status, &json));
    }
    Ok(serde_json::from_value(json)?)
}

/// SponsorBlock segments of a YouTube video, looked up by the server.
pub async fn segments(video_id: &str) -> Result<Vec<crate::sponsorblock::Segment>> {
    let (status, json) = fetch("GET", &format!("/api/segments?id={}", enc(video_id)), None).await?;
    if status != 200 {
        return Err(api_error(status, &json));
    }
    Ok(serde_json::from_value(json)?)
}

/// Lyrics of a track, looked up (and cached) by the server.
pub async fn lyrics(track: &Track) -> Result<Option<crate::lyrics::Lyrics>> {
    let url = format!(
        "/api/lyrics?platform={}&id={}&title={}&artist={}{}",
        track.platform.short(),
        enc(&track.source_id),
        enc(&track.title),
        enc(&track.artist),
        track.duration_secs.map(|d| format!("&duration={d}")).unwrap_or_default()
    );
    let (status, json) = fetch("GET", &url, None).await?;
    if status != 200 {
        return Err(api_error(status, &json));
    }
    Ok(serde_json::from_value(json)?)
}

/// Cover image bytes of a track, fetched (and cached) by the server.
pub async fn cover(platform: crate::model::Platform, id: &str, size: crate::cover::Size) -> Result<Vec<u8>> {
    let url = format!("/api/cover?platform={}&id={}&size={}", platform.short(), enc(id), size.param());
    let req = Request::new_with_str(&url).map_err(|_| anyhow!("bad request"))?;
    if let Some(t) = token() {
        let _ = req.headers().set("Authorization", &format!("Bearer {t}"));
    }
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let resp: Response = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|_| anyhow!("can't reach the server"))?
        .dyn_into()
        .map_err(|_| anyhow!("bad response"))?;
    if resp.status() != 200 {
        bail!("no cover (server error {})", resp.status());
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|_| anyhow!("bad response"))?)
        .await
        .map_err(|_| anyhow!("bad response"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Stream URL for a track. `<audio>` can't send headers, so the token goes
/// in the query (same origin only; the server sends no-referrer).
pub fn audio_url(track: &Track) -> String {
    format!("/api/audio?url={}&token={}", enc(&track.url), enc(&token().unwrap_or_default()))
}
