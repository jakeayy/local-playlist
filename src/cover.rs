//! Cover art: YouTube thumbnails and SoundCloud artwork, cached on disk, and
//! (desktop) embedded into downloaded files so other players show it too.

#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

#[cfg(not(target_family = "wasm"))]
use anyhow::bail;
#[cfg(any(not(target_family = "wasm"), feature = "ui"))]
use anyhow::{Context as _, Result};

#[cfg(not(target_family = "wasm"))]
use crate::model::{Paths, Platform};
#[cfg(not(target_family = "wasm"))]
use crate::sources::http;

#[cfg(not(target_family = "wasm"))]
/// Whether `id` is a valid platform id (also keeps it safe as a file name).
pub fn valid_id(platform: Platform, id: &str) -> bool {
    match platform {
        Platform::YouTube => crate::sources::youtube::is_video_id(id),
        Platform::SoundCloud => !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Which version of a cover to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// As large as available (visualizer, embedding).
    Full,
    /// ~100–320 px, a few KB (track list thumbnails).
    Small,
}

impl Size {
    pub fn param(self) -> &'static str {
        match self {
            Size::Full => "full",
            Size::Small => "small",
        }
    }

    pub fn from_param(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Size::Full),
            "small" => Some(Size::Small),
            _ => None,
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn cache_file(paths: &Paths, platform: Platform, id: &str, size: Size) -> PathBuf {
    let suffix = if size == Size::Small { "-small" } else { "" };
    paths.cache.join("covers").join(format!("{}-{id}{suffix}", platform.short()))
}

#[cfg(not(target_family = "wasm"))]
/// The cover image (JPEG/PNG/WebP bytes) of a track, from the cache or fetched.
pub fn get(paths: &Paths, platform: Platform, id: &str, size: Size) -> Result<Vec<u8>> {
    if !valid_id(platform, id) {
        bail!("invalid id");
    }
    let file = cache_file(paths, platform, id, size);
    if let Ok(bytes) = std::fs::read(&file)
        && !bytes.is_empty()
    {
        return Ok(bytes);
    }
    let bytes = match platform {
        Platform::YouTube => youtube(id, size)?,
        Platform::SoundCloud => {
            let url = crate::sources::soundcloud::artwork_url(id, paths)?.context("this track has no artwork")?;
            // `-large` is 100×100; t500x500 is the biggest generally available.
            let url = if size == Size::Full { url.replace("-large.", "-t500x500.") } else { url };
            fetch(&url)?.context("artwork not found")?
        }
    };
    if image_format(&bytes).is_none() {
        bail!("unsupported image format");
    }
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
    Ok(bytes)
}

#[cfg(not(target_family = "wasm"))]
/// Largest available thumbnail without letterboxing (`maxresdefault` is
/// missing on older/smaller videos; `hqdefault` has black bars), or the
/// 320×180 `mqdefault` for [`Size::Small`].
fn youtube(id: &str, size: Size) -> Result<Vec<u8>> {
    let names: &[&str] = if size == Size::Full { &["maxresdefault", "mqdefault"] } else { &["mqdefault"] };
    for name in names {
        if let Some(bytes) = fetch(&format!("https://i.ytimg.com/vi/{id}/{name}.jpg"))? {
            return Ok(bytes);
        }
    }
    bail!("no thumbnail")
}

#[cfg(not(target_family = "wasm"))]
/// GET an image; `None` on 404.
fn fetch(url: &str) -> Result<Option<Vec<u8>>> {
    let mut resp = http().get(url).call()?;
    match resp.status().as_u16() {
        200 => Ok(Some(resp.body_mut().with_config().limit(8 << 20).read_to_vec()?)),
        404 => Ok(None),
        s => bail!("HTTP {s} for {url}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}

impl Format {
    pub fn mime(self) -> &'static str {
        match self {
            Format::Jpeg => "image/jpeg",
            Format::Png => "image/png",
            Format::Webp => "image/webp",
        }
    }
}

pub fn image_format(bytes: &[u8]) -> Option<Format> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some(Format::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(Format::Png),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(Format::Webp),
        _ => None,
    }
}

/// Luma above this counts as picture, not a black bar (JPEG bars aren't exactly 0).
#[cfg(feature = "ui")]
const BAR_LUMA: u8 = 40;
/// A row/column is a bar when at most this fraction of its pixels is brighter.
#[cfg(feature = "ui")]
const BAR_MAX_BRIGHT: f32 = 0.02;
/// Never trim more than this fraction from one side (dark pictures stay whole).
#[cfg(feature = "ui")]
const MAX_TRIM: f32 = 0.45;

/// Cover art ready to show or embed: black letterbox/pillarbox bars (e.g. a
/// 4:3 video in a 16:9 thumbnail) trimmed, centre-cropped to a square and
/// scaled down to at most `max_side` pixels.
#[cfg(feature = "ui")]
pub fn square(bytes: &[u8], max_side: u32) -> Result<image::RgbaImage> {
    use image::imageops::{self, FilterType};
    let img = image::load_from_memory(bytes).context("unreadable cover image")?.into_rgba8();
    let (x0, y0, x1, y1) = content_bounds(&img);
    let (w, h) = (x1 - x0, y1 - y0);
    let side = w.min(h);
    let (x, y) = (x0 + (w - side) / 2, y0 + (h - side) / 2);
    let cropped = imageops::crop_imm(&img, x, y, side, side).to_image();
    Ok(if side > max_side { imageops::resize(&cropped, max_side, max_side, FilterType::Triangle) } else { cropped })
}

/// The picture inside any dark bars: (left, top, right, bottom), exclusive.
#[cfg(feature = "ui")]
fn content_bounds(img: &image::RgbaImage) -> (u32, u32, u32, u32) {
    let (w, h) = img.dimensions();
    let bright = |x: u32, y: u32| {
        let p = img.get_pixel(x, y).0;
        (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000 > BAR_LUMA as u32
    };
    let row_is_bar = |y: u32| ((0..w).filter(|&x| bright(x, y)).count() as f32) <= BAR_MAX_BRIGHT * w as f32;
    let col_is_bar = |x: u32| ((0..h).filter(|&y| bright(x, y)).count() as f32) <= BAR_MAX_BRIGHT * h as f32;
    let (max_x, max_y) = ((w as f32 * MAX_TRIM) as u32, (h as f32 * MAX_TRIM) as u32);
    let top = (0..max_y).find(|&y| !row_is_bar(y)).unwrap_or(0);
    let bottom = h - (0..max_y).find(|&i| !row_is_bar(h - 1 - i)).unwrap_or(0);
    let left = (0..max_x).find(|&x| !col_is_bar(x)).unwrap_or(0);
    let right = w - (0..max_x).find(|&i| !col_is_bar(w - 1 - i)).unwrap_or(0);
    (left, top, right, bottom)
}

/// The cover's dominant vivid colour as (hue, saturation), both 0..1, or `None`
/// for a mostly grey picture. Dark, washed-out and near-white pixels count less,
/// so a bright logo on black wins over the black.
#[cfg(feature = "ui")]
pub fn dominant_hue(img: &image::RgbaImage) -> Option<(f32, f32)> {
    const BINS: usize = 36;
    let small = image::imageops::thumbnail(img, 48, 48);
    let mut weight = [0f32; BINS];
    // Per bin: weighted sums of the hue (as a vector) and of the saturation.
    let mut sum = [(0f32, 0f32, 0f32); BINS];
    for p in small.pixels() {
        let (h, s, l) = rgb_to_hsl(p.0[0], p.0[1], p.0[2]);
        let w = s * s * (1.0 - ((l - 0.5).abs() * 2.2)).max(0.0);
        if w <= 0.0 {
            continue;
        }
        let bin = ((h * BINS as f32) as usize).min(BINS - 1);
        weight[bin] += w;
        let a = h * std::f32::consts::TAU;
        sum[bin].0 += w * a.cos();
        sum[bin].1 += w * a.sin();
        sum[bin].2 += w * s;
    }
    let total: f32 = weight.iter().sum();
    if total / (small.width() * small.height()) as f32 <= 0.015 {
        return None;
    }
    // Strongest neighbourhood of three bins (hues wrap around).
    let around = |b: usize| [(b + BINS - 1) % BINS, b, (b + 1) % BINS];
    let best = (0..BINS).max_by(|&a, &b| {
        let w = |c: usize| around(c).iter().map(|&i| weight[i]).sum::<f32>();
        w(a).total_cmp(&w(b))
    })?;
    let (mut x, mut y, mut s, mut w) = (0f32, 0f32, 0f32, 0f32);
    for i in around(best) {
        x += sum[i].0;
        y += sum[i].1;
        s += sum[i].2;
        w += weight[i];
    }
    let hue = y.atan2(x).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    Some((hue, s / w))
}

/// (hue, saturation, lightness), all 0..1.
#[cfg(feature = "ui")]
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s.min(1.0), l)
}

/// Inverse of [`rgb_to_hsl`].
#[cfg(feature = "ui")]
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(1.0) * 6.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    (to(r), to(g), to(b))
}

/// Encode as JPEG (for embedding into audio files).
#[cfg(feature = "desktop")]
pub fn to_jpeg(img: &image::RgbaImage) -> Result<Vec<u8>> {
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).into_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&rgb)?;
    Ok(out)
}

/// Embed `cover` (JPEG, already square) as the front cover of a downloaded
/// audio file, replacing any. Pure Rust (no ffmpeg); works for MP4/M4A, MP3,
/// Ogg Vorbis/Opus and FLAC. The tagged copy is renamed over the file, so a
/// player reading it meanwhile isn't disturbed.
#[cfg(feature = "desktop")]
pub fn embed(file: &std::path::Path, cover: &[u8]) -> Result<()> {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile as _, TaggedFileExt as _};
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::Tag;

    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if !matches!(ext.as_str(), "m4a" | "mp4" | "mp3" | "ogg" | "oga" | "opus" | "flac") {
        bail!("can't embed cover art into .{ext} files");
    }
    crate::remux::replace_via_temp(file, |tmp| {
        std::fs::copy(file, tmp)?;
        let mut tagged = lofty::probe::Probe::open(tmp)?.guess_file_type()?.read()?;
        if tagged.primary_tag().is_none() {
            let kind = tagged.primary_tag_type();
            tagged.insert_tag(Tag::new(kind));
        }
        let tag = tagged.primary_tag_mut().context("no tag")?;
        // Replace whatever art is there (MP4 doesn't store picture types, so a
        // previous cover reads back as "Other").
        while !tag.pictures().is_empty() {
            tag.remove_picture(0);
        }
        tag.push_picture(Picture::new_unchecked(PictureType::CoverFront, Some(MimeType::Jpeg), None, cover.to_vec()));
        tagged.save_to_path(tmp, WriteOptions::default())?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Paths;

    #[test]
    fn ids_are_checked() {
        assert!(valid_id(Platform::YouTube, "dQw4w9WgXcQ"));
        assert!(!valid_id(Platform::YouTube, "../../etc/x"));
        assert!(valid_id(Platform::SoundCloud, "1234567"));
        assert!(!valid_id(Platform::SoundCloud, "12/34"));
        assert!(!valid_id(Platform::SoundCloud, ""));
    }

    /// A 160×90 "thumbnail" with a 120×90 picture in it (4:3 pillarboxed in
    /// 16:9), plus 10px letterbox bars inside that.
    #[cfg(feature = "ui")]
    fn boxed_picture() -> image::RgbaImage {
        image::RgbaImage::from_fn(160, 90, |x, y| {
            let inside = (20..140).contains(&x) && (10..80).contains(&y);
            // Noisy near-black bars, like JPEG artifacts.
            if inside {
                image::Rgba([200, 120, (x * 2) as u8, 255])
            } else {
                image::Rgba([(x % 3 * 7) as u8, 6, 9, 255])
            }
        })
    }

    #[test]
    #[cfg(feature = "ui")]
    fn finds_picture_inside_bars() {
        assert_eq!(content_bounds(&boxed_picture()), (20, 10, 140, 80));
        // A picture without bars is left alone; so is an all-dark one.
        let plain = image::RgbaImage::from_pixel(50, 40, image::Rgba([90, 90, 90, 255]));
        assert_eq!(content_bounds(&plain), (0, 0, 50, 40));
        let dark = image::RgbaImage::from_pixel(50, 40, image::Rgba([5, 5, 5, 255]));
        assert_eq!(content_bounds(&dark), (0, 0, 50, 40));
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn squares_without_bars() {
        let mut png = Vec::new();
        boxed_picture().write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let sq = square(&png, 64).unwrap();
        assert_eq!(sq.dimensions(), (64, 64));
        // Every edge pixel comes from the picture, not a bar.
        for i in 0..64 {
            for p in [sq.get_pixel(i, 0), sq.get_pixel(i, 63), sq.get_pixel(0, i), sq.get_pixel(63, i)] {
                assert!(p.0[0] > 150, "bar left at the edge: {p:?}");
            }
        }
        assert!(image_format(&to_jpeg(&sq).unwrap()) == Some(Format::Jpeg));
    }

    #[test]
    #[cfg(feature = "ui")]
    fn finds_dominant_colour() {
        // Mostly black with a teal logo and a little red: teal wins.
        let img = image::RgbaImage::from_fn(100, 100, |x, y| {
            if (30..70).contains(&x) && (30..70).contains(&y) {
                image::Rgba([20, 180, 170, 255])
            } else if x < 5 {
                image::Rgba([200, 30, 30, 255])
            } else {
                image::Rgba([8, 8, 8, 255])
            }
        });
        let (h, s) = dominant_hue(&img).unwrap();
        assert!((h - 0.49).abs() < 0.03, "hue {h}");
        assert!(s > 0.6, "saturation {s}");
        // Greyscale: nothing to pick.
        let grey = image::RgbaImage::from_fn(50, 50, |x, _| image::Rgba([(x * 5) as u8; 4]));
        assert_eq!(dominant_hue(&grey), None);
    }

    #[test]
    #[cfg(feature = "ui")]
    fn hsl_round_trips() {
        for rgb in [(255, 0, 0), (12, 200, 99), (103, 80, 164), (208, 188, 255), (40, 40, 40)] {
            let (h, s, l) = rgb_to_hsl(rgb.0, rgb.1, rgb.2);
            let back = hsl_to_rgb(h, s, l);
            for (a, b) in [(rgb.0, back.0), (rgb.1, back.1), (rgb.2, back.2)] {
                assert!((a as i32 - b as i32).abs() <= 1, "{rgb:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn sniffs_formats() {
        assert_eq!(image_format(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(image_format(b"\x89PNG\r\n"), Some(Format::Png));
        assert_eq!(image_format(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::Webp));
        assert_eq!(image_format(b"<html>"), None);
    }

    /// Embeds with lofty into each format and checks with ffmpeg that the
    /// cover is there and the audio decodes exactly as before. (ffmpeg only
    /// creates and checks the files.)
    #[test]
    #[cfg(feature = "desktop")]
    #[ignore = "needs ffmpeg"]
    fn embeds_into_each_format() {
        use std::process::Command;
        let dir = test_paths().cache.join("embed");
        std::fs::create_dir_all(&dir).unwrap();
        let pic = dir.join("pic.jpg");
        let ok = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=180x180", "-frames:v", "1"])
            .arg(&pic)
            .status()
            .unwrap();
        assert!(ok.success());
        let cover = std::fs::read(&pic).unwrap();
        let pcm = |p: &std::path::Path| {
            Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(p)
                .args(["-map", "0:a", "-f", "s16le", "-"])
                .output()
                .unwrap()
        };
        for (ext, codec) in
            [("m4a", "aac"), ("mp3", "libmp3lame"), ("opus", "libopus"), ("ogg", "libvorbis"), ("flac", "flac")]
        {
            let file = dir.join(format!("song.{ext}"));
            let ok = Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=duration=1", "-c:a", codec])
                .arg(&file)
                .status()
                .unwrap();
            assert!(ok.success(), "{ext}");
            let before = pcm(&file).stdout;
            embed(&file, &cover).unwrap();
            let after = pcm(&file);
            assert!(after.stderr.is_empty(), "{ext}: {}", String::from_utf8_lossy(&after.stderr));
            assert_eq!(before, after.stdout, "{ext}: audio changed");
            let probe = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "stream=codec_type,width,height:stream_disposition=attached_pic",
                ])
                .args(["-of", "compact"])
                .arg(&file)
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&probe.stdout);
            assert!(out.contains("width=180|height=180") && out.contains("attached_pic=1"), "{ext}: {out}");
            // Embedding again replaces the cover instead of adding another.
            embed(&file, &cover).unwrap();
            let again = Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
                .arg(&file)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&again.stdout).matches("video").count(), 1, "{ext}");
        }
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 6, "temporary files left behind: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_paths() -> Paths {
        let dir = std::env::temp_dir().join(format!("lp-cover-test-{}", std::process::id()));
        Paths { data: dir.join("data"), cache: dir.join("cache") }
    }

    #[test]
    #[ignore = "network"]
    fn live_youtube_and_soundcloud() {
        let paths = test_paths();
        let yt = get(&paths, Platform::YouTube, "dQw4w9WgXcQ", Size::Full).unwrap();
        assert_eq!(image_format(&yt), Some(Format::Jpeg));
        // Cached the second time.
        assert!(cache_file(&paths, Platform::YouTube, "dQw4w9WgXcQ", Size::Full).exists());
        let small = get(&paths, Platform::YouTube, "dQw4w9WgXcQ", Size::Small).unwrap();
        assert!(small.len() < yt.len() / 2, "{} vs {}", small.len(), yt.len());
        let link = crate::sources::parse_link("https://soundcloud.com/forss/sets/soulhack").unwrap();
        let crate::sources::Resolved::Collection { tracks, .. } =
            crate::sources::soundcloud::resolve(&link, &paths, None).unwrap()
        else {
            panic!()
        };
        let sc = get(&paths, Platform::SoundCloud, &tracks[0].source_id, Size::Full).unwrap();
        assert!(image_format(&sc).is_some());
        let sc_small = get(&paths, Platform::SoundCloud, &tracks[0].source_id, Size::Small).unwrap();
        assert!(sc_small.len() < sc.len());
        let _ = std::fs::remove_dir_all(paths.cache.parent().unwrap());
    }
}
