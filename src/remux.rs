//! Fragmented MP4/M4A → plain MP4, without ffmpeg.
//!
//! Without ffmpeg, yt-dlp saves YouTube/SoundCloud audio as it is streamed:
//! fragmented MP4 (an empty sample table in `moov`, then `moof`+`mdat` pairs).
//! This app plays that fine, but some players don't. [`defragment`] rebuilds
//! the sample table and writes `ftyp`, `moov`, one `mdat`. Nothing is
//! re-encoded: codec setup (`stsd`), edit lists and sample data are copied
//! byte for byte.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use memmap2::Mmap;

// ------------------------------------------------------------------ reading

#[derive(Clone, Copy)]
struct Atom {
    kind: [u8; 4],
    start: usize,
    /// First byte after the header.
    body: usize,
    end: usize,
}

impl Atom {
    fn is(&self, kind: &[u8; 4]) -> bool {
        &self.kind == kind
    }
}

fn atoms(data: &[u8], start: usize, end: usize) -> Result<Vec<Atom>> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        let size = u32_at(data, pos)? as u64;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (size, header) = match size {
            0 => ((end - pos) as u64, 8),
            1 => (u64_at(data, pos + 8)?, 16),
            s => (s, 8),
        };
        let atom_end = pos.checked_add(size as usize).context("box too large")?;
        ensure!(size >= header as u64 && atom_end <= end, "truncated or corrupt '{}' box", fourcc(&kind));
        out.push(Atom { kind, start: pos, body: pos + header, end: atom_end });
        pos = atom_end;
    }
    Ok(out)
}

fn child(data: &[u8], parent: Atom, kind: &[u8; 4]) -> Result<Option<Atom>> {
    Ok(atoms(data, parent.body, parent.end)?.into_iter().find(|a| a.is(kind)))
}

fn need(data: &[u8], parent: Atom, kind: &[u8; 4]) -> Result<Atom> {
    child(data, parent, kind)?.with_context(|| format!("missing '{}' box", fourcc(kind)))
}

fn fourcc(k: &[u8; 4]) -> String {
    String::from_utf8_lossy(k).into_owned()
}

fn u32_at(d: &[u8], pos: usize) -> Result<u32> {
    let b = d.get(pos..pos + 4).context("unexpected end of data")?;
    Ok(u32::from_be_bytes(b.try_into().unwrap()))
}

fn u64_at(d: &[u8], pos: usize) -> Result<u64> {
    let b = d.get(pos..pos + 8).context("unexpected end of data")?;
    Ok(u64::from_be_bytes(b.try_into().unwrap()))
}

/// Cursor over a box body.
struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
    end: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32> {
        ensure!(self.pos + 4 <= self.end, "box too short");
        let v = u32_at(self.d, self.pos)?;
        self.pos += 4;
        Ok(v)
    }

    fn u64(&mut self) -> Result<u64> {
        ensure!(self.pos + 8 <= self.end, "box too short");
        let v = u64_at(self.d, self.pos)?;
        self.pos += 8;
        Ok(v)
    }
}

fn reader(d: &[u8], a: Atom) -> Reader<'_> {
    Reader { d, pos: a.body, end: a.end }
}

/// Whether `data` is an MP4 made of movie fragments.
pub fn is_fragmented(data: &[u8]) -> bool {
    atoms(data, 0, data.len()).is_ok_and(|top| top.iter().any(|a| a.is(b"moov")) && top.iter().any(|a| a.is(b"moof")))
}

// ------------------------------------------------------------------ samples

#[derive(Clone, Copy, Default)]
struct Defaults {
    description: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

struct Sample {
    size: u32,
    duration: u32,
    /// Composition time offset (signed in version 1 `trun`s).
    cto: i64,
    sync: bool,
}

/// Contiguous samples from one `trun`.
struct Chunk {
    offset: usize,
    description: u32,
    samples: Vec<Sample>,
}

impl Chunk {
    fn len(&self) -> usize {
        self.samples.iter().map(|s| s.size as usize).sum()
    }
}

struct Track {
    id: u32,
    trak: Atom,
    timescale: u32,
    chunks: Vec<Chunk>,
}

const NON_SYNC: u32 = 0x1_0000;

fn track_id(data: &[u8], trak: Atom) -> Result<u32> {
    let tkhd = need(data, trak, b"tkhd")?;
    let version = data[tkhd.body];
    u32_at(data, tkhd.body + if version == 1 { 20 } else { 12 })
}

fn mdhd_timescale(data: &[u8], trak: Atom) -> Result<u32> {
    let mdhd = need(data, need(data, trak, b"mdia")?, b"mdhd")?;
    let version = data[mdhd.body];
    u32_at(data, mdhd.body + if version == 1 { 20 } else { 12 })
}

/// Read every fragment's samples into `tracks`.
fn collect(data: &[u8], top: &[Atom], tracks: &mut [Track], trex: &[(u32, Defaults)]) -> Result<()> {
    for moof in top.iter().filter(|a| a.is(b"moof")) {
        // Data of a traf without an explicit base follows the previous one's.
        let mut prev_end = moof.start;
        for traf in atoms(data, moof.body, moof.end)?.into_iter().filter(|a| a.is(b"traf")) {
            let tfhd = need(data, traf, b"tfhd")?;
            let mut r = reader(data, tfhd);
            let flags = r.u32()? & 0xFF_FFFF;
            let id = r.u32()?;
            let mut d = trex.iter().find(|(t, _)| *t == id).map(|(_, d)| *d).unwrap_or_default();
            let mut base = if flags & 0x2_0000 != 0 { moof.start } else { prev_end };
            if flags & 0x1 != 0 {
                base = r.u64()? as usize;
            }
            if flags & 0x2 != 0 {
                d.description = r.u32()?;
            }
            if flags & 0x8 != 0 {
                d.duration = r.u32()?;
            }
            if flags & 0x10 != 0 {
                d.size = r.u32()?;
            }
            if flags & 0x20 != 0 {
                d.flags = r.u32()?;
            }
            let track = tracks.iter_mut().find(|t| t.id == id).with_context(|| format!("fragment for unknown track {id}"))?;
            let mut next = base;
            for trun in atoms(data, traf.body, traf.end)?.into_iter().filter(|a| a.is(b"trun")) {
                let mut r = reader(data, trun);
                let vf = r.u32()?;
                let (version, flags) = (vf >> 24, vf & 0xFF_FFFF);
                let count = r.u32()? as usize;
                if flags & 0x1 != 0 {
                    let off = r.u32()? as i32 as i64;
                    next = usize::try_from(base as i64 + off).context("bad data offset")?;
                }
                let first_flags = if flags & 0x4 != 0 { Some(r.u32()?) } else { None };
                let per_sample = [0x100, 0x200, 0x400, 0x800].iter().filter(|&&f| flags & f != 0).count() * 4;
                ensure!(
                    if per_sample > 0 { count * per_sample <= trun.end - r.pos } else { count <= 50_000_000 },
                    "bad sample count"
                );
                let mut samples = Vec::with_capacity(count);
                for i in 0..count {
                    let duration = if flags & 0x100 != 0 { r.u32()? } else { d.duration };
                    let size = if flags & 0x200 != 0 { r.u32()? } else { d.size };
                    let mut sflags = if flags & 0x400 != 0 { r.u32()? } else { d.flags };
                    if i == 0
                        && let Some(f) = first_flags
                    {
                        sflags = f;
                    }
                    let cto = if flags & 0x800 != 0 {
                        let v = r.u32()?;
                        if version == 0 { v as i64 } else { v as i32 as i64 }
                    } else {
                        0
                    };
                    samples.push(Sample { size, duration, cto, sync: sflags & NON_SYNC == 0 });
                }
                let chunk = Chunk { offset: next, description: d.description.max(1), samples };
                let len = chunk.len();
                ensure!(next.checked_add(len).is_some_and(|e| e <= data.len()), "sample data outside the file");
                next += len;
                if !chunk.samples.is_empty() {
                    track.chunks.push(chunk);
                }
            }
            prev_end = next;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ writing

fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

fn full_atom(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.extend_from_slice(&((version as u32) << 24 | flags).to_be_bytes());
    p.extend_from_slice(payload);
    atom(kind, &p)
}

/// A container box rebuilt from its children (copied unless `replace` returns
/// new bytes; `Some(empty)` drops the child).
fn rebuild(
    data: &[u8],
    parent: Atom,
    mut replace: impl FnMut(Atom) -> Result<Option<Vec<u8>>>,
    extra: &[u8],
) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    for c in atoms(data, parent.body, parent.end)? {
        match replace(c)? {
            Some(bytes) => payload.extend_from_slice(&bytes),
            None => payload.extend_from_slice(&data[c.start..c.end]),
        }
    }
    payload.extend_from_slice(extra);
    Ok(atom(&parent.kind, &payload))
}

/// Copy of a `mvhd`/`tkhd`/`mdhd` with its duration replaced.
fn with_duration(data: &[u8], a: Atom, duration: u64) -> Vec<u8> {
    let mut bytes = data[a.start..a.end].to_vec();
    let version = data[a.body];
    let rel = a.body - a.start;
    // Offset of the duration field after version/flags, per box and version.
    let at = match (&a.kind, version) {
        (b"tkhd", 1) => 4 + 8 + 8 + 4 + 4,
        (b"tkhd", _) => 4 + 4 + 4 + 4 + 4,
        (_, 1) => 4 + 8 + 8 + 4,
        _ => 4 + 4 + 4 + 4,
    };
    let pos = rel + at;
    if version == 1 {
        bytes[pos..pos + 8].copy_from_slice(&duration.to_be_bytes());
    } else {
        bytes[pos..pos + 4].copy_from_slice(&(duration.min(u32::MAX as u64) as u32).to_be_bytes());
    }
    bytes
}

/// The track's sample table (`stbl`), with chunks at `offsets`.
fn sample_table(data: &[u8], stbl: Atom, track: &Track, offsets: &[u64], co64: bool) -> Result<Vec<u8>> {
    let samples = || track.chunks.iter().flat_map(|c| &c.samples);
    let count = samples().count() as u32;

    let mut stts: Vec<(u32, u32)> = Vec::new();
    for s in samples() {
        match stts.last_mut() {
            Some((n, d)) if *d == s.duration => *n += 1,
            _ => stts.push((1, s.duration)),
        }
    }
    let table = |entries: &[(u32, u32)]| {
        let mut p = (entries.len() as u32).to_be_bytes().to_vec();
        for (a, b) in entries {
            p.extend_from_slice(&a.to_be_bytes());
            p.extend_from_slice(&b.to_be_bytes());
        }
        p
    };

    let mut new = Vec::new();
    new.extend(full_atom(b"stts", 0, 0, &table(&stts)));

    if samples().any(|s| s.cto != 0) {
        let mut ctts: Vec<(u32, i64)> = Vec::new();
        for s in samples() {
            match ctts.last_mut() {
                Some((n, c)) if *c == s.cto => *n += 1,
                _ => ctts.push((1, s.cto)),
            }
        }
        let negative = ctts.iter().any(|(_, c)| *c < 0);
        let entries: Vec<(u32, u32)> = ctts.iter().map(|&(n, c)| (n, c as i32 as u32)).collect();
        new.extend(full_atom(b"ctts", negative as u8, 0, &table(&entries)));
    }

    if samples().any(|s| !s.sync) {
        let sync: Vec<u32> = samples().enumerate().filter(|(_, s)| s.sync).map(|(i, _)| i as u32 + 1).collect();
        let mut p = (sync.len() as u32).to_be_bytes().to_vec();
        for i in sync {
            p.extend_from_slice(&i.to_be_bytes());
        }
        new.extend(full_atom(b"stss", 0, 0, &p));
    }

    let mut stsc: Vec<(u32, u32, u32)> = Vec::new();
    for (i, c) in track.chunks.iter().enumerate() {
        let entry = (c.samples.len() as u32, c.description);
        if stsc.last().is_none_or(|&(_, n, d)| (n, d) != entry) {
            stsc.push((i as u32 + 1, entry.0, entry.1));
        }
    }
    let mut p = (stsc.len() as u32).to_be_bytes().to_vec();
    for (first, n, d) in stsc {
        for v in [first, n, d] {
            p.extend_from_slice(&v.to_be_bytes());
        }
    }
    new.extend(full_atom(b"stsc", 0, 0, &p));

    let first = samples().next().map_or(0, |s| s.size);
    let mut p = Vec::new();
    if samples().all(|s| s.size == first) {
        p.extend_from_slice(&first.to_be_bytes());
        p.extend_from_slice(&count.to_be_bytes());
    } else {
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&count.to_be_bytes());
        for s in samples() {
            p.extend_from_slice(&s.size.to_be_bytes());
        }
    }
    new.extend(full_atom(b"stsz", 0, 0, &p));

    let mut p = (offsets.len() as u32).to_be_bytes().to_vec();
    for &o in offsets {
        if co64 {
            p.extend_from_slice(&o.to_be_bytes());
        } else {
            p.extend_from_slice(&(o as u32).to_be_bytes());
        }
    }
    new.extend(full_atom(if co64 { b"co64" } else { b"stco" }, 0, 0, &p));

    // Keep the description and anything else (sample groups…); replace the
    // (empty) tables.
    const TABLES: [&[u8; 4]; 8] = [b"stts", b"ctts", b"stss", b"stsc", b"stsz", b"stz2", b"stco", b"co64"];
    rebuild(data, stbl, |c| Ok(TABLES.iter().any(|t| c.is(t)).then(Vec::new)), &new)
}

/// Rebuild a fragmented MP4 as a plain one, in memory (see [`defragment_into`]).
pub fn defragment(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    defragment_into(data, &mut out)?;
    Ok(out)
}

/// Rebuild a fragmented MP4 as a plain one (see the module docs), written to
/// `out` as it's produced: the sample data is copied straight from `data`.
pub fn defragment_into(data: &[u8], out: &mut impl Write) -> Result<()> {
    let top = atoms(data, 0, data.len()).context("not an MP4 file")?;
    let moov = *top.iter().find(|a| a.is(b"moov")).context("missing 'moov' box")?;
    let mvhd = need(data, moov, b"mvhd")?;
    let movie_timescale = u32_at(data, mvhd.body + if data[mvhd.body] == 1 { 20 } else { 12 })?;

    let mut trex = Vec::new();
    if let Some(mvex) = child(data, moov, b"mvex")? {
        for t in atoms(data, mvex.body, mvex.end)?.into_iter().filter(|a| a.is(b"trex")) {
            let mut r = reader(data, t);
            r.u32()?;
            let id = r.u32()?;
            let d = Defaults { description: r.u32()?, duration: r.u32()?, size: r.u32()?, flags: r.u32()? };
            trex.push((id, d));
        }
    }
    let mut tracks = Vec::new();
    for trak in atoms(data, moov.body, moov.end)?.into_iter().filter(|a| a.is(b"trak")) {
        tracks.push(Track {
            id: track_id(data, trak)?,
            trak,
            timescale: mdhd_timescale(data, trak)?.max(1),
            chunks: Vec::new(),
        });
    }
    collect(data, &top, &mut tracks, &trex)?;
    ensure!(tracks.iter().any(|t| !t.chunks.is_empty()), "no samples found");

    // All chunks in file order (keeps tracks interleaved as they were).
    let mut order: Vec<(usize, usize)> =
        tracks.iter().enumerate().flat_map(|(t, tr)| (0..tr.chunks.len()).map(move |c| (t, c))).collect();
    order.sort_by_key(|&(t, c)| tracks[t].chunks[c].offset);
    let payload: usize = order.iter().map(|&(t, c)| tracks[t].chunks[c].len()).sum();

    let audio_only = tracks.iter().all(|t| {
        need(data, t.trak, b"mdia")
            .and_then(|m| need(data, m, b"hdlr"))
            .is_ok_and(|h| data.get(h.body + 8..h.body + 12) == Some(b"soun"))
    });
    let mut ftyp = Vec::new();
    let brands: &[&[u8; 4]] = if audio_only { &[b"M4A ", b"M4A ", b"isom", b"mp42"] } else { &[b"isom", b"isom", b"iso2", b"mp41"] };
    for (i, b) in brands.iter().enumerate() {
        ftyp.extend_from_slice(*b);
        if i == 0 {
            ftyp.extend_from_slice(&0x200u32.to_be_bytes());
        }
    }
    let ftyp = atom(b"ftyp", &ftyp);

    let big_mdat = payload as u64 + 8 > u32::MAX as u64;
    let mdat_header = if big_mdat { 16 } else { 8 };
    // Offsets are fixed-width, so the moov's size doesn't depend on them:
    // build it once with zeros to learn where the data will start.
    let build_moov = |data_start: u64, co64: bool| -> Result<Vec<u8>> {
        let mut offsets: Vec<Vec<u64>> = tracks.iter().map(|t| vec![0; t.chunks.len()]).collect();
        let mut pos = data_start;
        for &(t, c) in &order {
            offsets[t][c] = pos;
            pos += tracks[t].chunks[c].len() as u64;
        }
        let media_duration = |t: &Track| t.chunks.iter().flat_map(|c| &c.samples).map(|s| s.duration as u64).sum::<u64>();
        let movie_duration = |t: &Track| media_duration(t) * movie_timescale as u64 / t.timescale as u64;
        let longest = tracks.iter().map(movie_duration).max().unwrap_or(0);
        rebuild(
            data,
            moov,
            |c| {
                if c.is(b"mvex") {
                    return Ok(Some(Vec::new()));
                }
                if c.is(b"mvhd") {
                    return Ok(Some(with_duration(data, c, longest)));
                }
                if !c.is(b"trak") {
                    return Ok(None);
                }
                let (i, track) = tracks.iter().enumerate().find(|(_, t)| t.trak.start == c.start).unwrap();
                let trak = rebuild(
                    data,
                    c,
                    |c| {
                        if c.is(b"tkhd") {
                            return Ok(Some(with_duration(data, c, movie_duration(track))));
                        }
                        if !c.is(b"mdia") {
                            return Ok(None);
                        }
                        Ok(Some(rebuild(
                            data,
                            c,
                            |c| {
                                if c.is(b"mdhd") {
                                    return Ok(Some(with_duration(data, c, media_duration(track))));
                                }
                                if !c.is(b"minf") {
                                    return Ok(None);
                                }
                                Ok(Some(rebuild(
                                    data,
                                    c,
                                    |c| {
                                        c.is(b"stbl").then(|| sample_table(data, c, track, &offsets[i], co64)).transpose()
                                    },
                                    &[],
                                )?))
                            },
                            &[],
                        )?))
                    },
                    &[],
                )?;
                Ok(Some(trak))
            },
            &[],
        )
    };
    let probe = build_moov(0, false)?;
    let start = (ftyp.len() + probe.len() + mdat_header) as u64;
    let co64 = start + payload as u64 > u32::MAX as u64;
    let probe = if co64 { build_moov(0, true)? } else { probe };
    let start = (ftyp.len() + probe.len() + mdat_header) as u64;
    let moov = build_moov(start, co64)?;
    debug_assert_eq!(moov.len(), probe.len());

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    if big_mdat {
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&(payload as u64 + 16).to_be_bytes())?;
    } else {
        out.write_all(&(payload as u32 + 8).to_be_bytes())?;
        out.write_all(b"mdat")?;
    }
    for &(t, c) in &order {
        let chunk = &tracks[t].chunks[c];
        out.write_all(&data[chunk.offset..chunk.offset + chunk.len()])?;
    }
    Ok(())
}

/// Defragment `path` in place if it's a fragmented MP4. The new file is
/// written next to it and renamed over it, so a reader that has the old one
/// open (the player, while streaming) keeps reading intact data.
/// Returns whether the file was rewritten.
///
/// Neither version is held in memory: the file is memory-mapped (read from
/// the page cache) and the new one is written out as it's produced, so long
/// mixes don't cost their size in RAM.
pub fn defragment_file(path: &Path) -> Result<bool> {
    let file = File::open(path)?;
    // SAFETY: the file is our own finished download, which nothing truncates
    // or rewrites in place while it's mapped (it's only ever replaced by
    // renaming another file over it).
    let data = unsafe { Mmap::map(&file)? };
    if !is_fragmented(&data) {
        return Ok(false);
    }
    replace_via_temp(path, move |tmp| {
        let mut out = BufWriter::new(File::create(tmp)?);
        defragment_into(&data, &mut out)?;
        out.flush()?;
        drop(out);
        // Unmapped before the rename (Windows can't replace a mapped file).
        drop(data);
        drop(file);
        let written = File::open(tmp)?;
        // SAFETY: as above; the temporary file is ours alone.
        let check = unsafe { Mmap::map(&written)? };
        if is_fragmented(&check) {
            bail!("defragmenting didn't produce a plain file");
        }
        Ok(())
    })?;
    Ok(true)
}

/// Produce the new version of `path` in a temporary file next to it (`write`
/// gets its path), then rename it over `path`. Readers that already have the
/// old file open keep reading the old data.
pub fn replace_via_temp(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    // Keep the extension: tag libraries pick the format by it.
    let name = path.file_name().and_then(|n| n.to_str()).context("bad file name")?;
    let tmp = path.with_file_name(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!(".{name}.tmp.{ext}"),
        None => format!(".{name}.tmp"),
    });
    let res = write(&tmp).and_then(|()| Ok(std::fs::rename(&tmp, path)?));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal fragmented audio MP4: one track, `frames` of `sizes`, split
    /// into two fragments with data offsets relative to each `moof`.
    fn fragmented(sizes: &[u32]) -> Vec<u8> {
        let stsd = full_atom(b"stsd", 0, 0, &[0, 0, 0, 1, 0, 0, 0, 8, b'm', b'p', b'4', b'a']);
        let empty = |k: &[u8; 4], n: usize| full_atom(k, 0, 0, &vec![0; n]);
        let stbl = atom(
            b"stbl",
            &[stsd, empty(b"stts", 4), empty(b"stsc", 4), empty(b"stsz", 8), empty(b"stco", 4)].concat(),
        );
        let minf = atom(b"minf", &[full_atom(b"smhd", 0, 0, &[0; 4]), stbl].concat());
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"soun");
        hdlr.extend_from_slice(&[0; 13]);
        let mdhd = full_atom(b"mdhd", 0, 0, &[&[0; 8][..], &44_100u32.to_be_bytes(), &[0; 4], &[0; 4]].concat());
        let mdia = atom(b"mdia", &[mdhd, full_atom(b"hdlr", 0, 0, &hdlr), minf].concat());
        let tkhd = full_atom(b"tkhd", 0, 3, &[&[0; 8][..], &1u32.to_be_bytes(), &[0; 68]].concat());
        let trak = atom(b"trak", &[tkhd, mdia].concat());
        let mvhd = full_atom(b"mvhd", 0, 0, &[&[0; 8][..], &1000u32.to_be_bytes(), &[0; 84]].concat());
        // Defaults: description 1, duration 1024.
        let trex = full_atom(b"trex", 0, 0, &[1u32, 1, 1024, 0, 0].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<_>>());
        let moov = atom(b"moov", &[mvhd, atom(b"mvex", &trex), trak].concat());
        let mut file = [atom(b"ftyp", b"dash\0\0\0\0iso6mp41"), moov].concat();
        let mut byte = 0u8;
        for part in sizes.chunks(sizes.len().div_ceil(2)) {
            // trun with data offset + per-sample sizes; data offset patched below.
            let mut trun = (part.len() as u32).to_be_bytes().to_vec();
            trun.extend_from_slice(&0u32.to_be_bytes());
            for s in part {
                trun.extend_from_slice(&s.to_be_bytes());
            }
            let trun = full_atom(b"trun", 0, 0x201, &trun);
            let tfhd = full_atom(b"tfhd", 0, 0x2_0000, &1u32.to_be_bytes());
            let traf = atom(b"traf", &[tfhd, trun].concat());
            let mut moof = atom(b"moof", &[full_atom(b"mfhd", 0, 0, &[0, 0, 0, 1]), traf].concat());
            // Data offset field: moof(8) mfhd(16) traf(8) tfhd(16) trun(8) flags(4) count(4).
            let off = (moof.len() + 8) as u32;
            moof[8 + 16 + 8 + 16 + 8 + 4 + 4..][..4].copy_from_slice(&off.to_be_bytes());
            let payload: Vec<u8> = part
                .iter()
                .flat_map(|&s| {
                    byte = byte.wrapping_add(1);
                    vec![byte; s as usize]
                })
                .collect();
            file.extend(moof);
            file.extend(atom(b"mdat", &payload));
        }
        file
    }

    #[test]
    fn rebuilds_sample_table() {
        let sizes = [10, 20, 30, 40, 50];
        let input = fragmented(&sizes);
        assert!(is_fragmented(&input));
        let out = defragment(&input).unwrap();
        assert!(!is_fragmented(&out));

        let top = atoms(&out, 0, out.len()).unwrap();
        let kinds: Vec<_> = top.iter().map(|a| fourcc(&a.kind)).collect();
        assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
        let moov = top[1];
        assert!(child(&out, moov, b"mvex").unwrap().is_none());
        let trak = need(&out, moov, b"trak").unwrap();
        let mdia = need(&out, trak, b"mdia").unwrap();
        let stbl = need(&out, need(&out, mdia, b"minf").unwrap(), b"stbl").unwrap();
        assert!(child(&out, stbl, b"stsd").unwrap().is_some(), "description kept");

        // Sizes, in order.
        let stsz = need(&out, stbl, b"stsz").unwrap();
        assert_eq!(u32_at(&out, stsz.body + 8).unwrap(), 5);
        let got: Vec<u32> = (0..5).map(|i| u32_at(&out, stsz.body + 12 + 4 * i).unwrap()).collect();
        assert_eq!(got, sizes);
        // One duration run of 5 × 1024; mdhd duration = 5120.
        let stts = need(&out, stbl, b"stts").unwrap();
        assert_eq!(
            [u32_at(&out, stts.body + 4).unwrap(), u32_at(&out, stts.body + 8).unwrap(), u32_at(&out, stts.body + 12).unwrap()],
            [1, 5, 1024]
        );
        let mdhd = need(&out, mdia, b"mdhd").unwrap();
        assert_eq!(u32_at(&out, mdhd.body + 16).unwrap(), 5120);

        // Chunk offsets point at the right bytes: chunk 1 starts with sample 1
        // (bytes = 1), chunk 2 with sample 4 (bytes = 4).
        let stco = need(&out, stbl, b"stco").unwrap();
        assert_eq!(u32_at(&out, stco.body + 4).unwrap(), 2);
        let c1 = u32_at(&out, stco.body + 8).unwrap() as usize;
        let c2 = u32_at(&out, stco.body + 12).unwrap() as usize;
        assert_eq!(out[c1], 1);
        assert_eq!(out[c1 + 9], 1);
        assert_eq!(out[c1 + 10], 2);
        assert_eq!(out[c2], 4);
        assert_eq!(out[c2 + 40], 5);
        // Every input byte of sample data made it across.
        let mdat = top[2];
        assert_eq!(mdat.end - mdat.body, sizes.iter().sum::<u32>() as usize);
    }

    #[test]
    fn leaves_other_files_alone() {
        assert!(!is_fragmented(b"ID3\x03\0\0\0\0\0\0rest of an mp3"));
        assert!(!is_fragmented(&[]));
        assert!(defragment(b"not an mp4 at all").is_err());
        // A truncated fragmented file is rejected, not mangled.
        let input = fragmented(&[100, 200, 300]);
        assert!(defragment(&input[..input.len() - 50]).is_err());
    }

    /// Real encoder output: fragment an AAC file with ffmpeg the way DASH
    /// streams are, defragment it, and compare the decoded audio with what
    /// ffmpeg's own remux (`-c copy`, the path used when ffmpeg is installed)
    /// gives. (Decoding the fragmented file directly differs at the very end:
    /// ffmpeg ignores the edit list there and plays the encoder padding.)
    #[test]
    #[ignore = "needs ffmpeg"]
    fn matches_ffmpeg_decode() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("lp-remux-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frag = dir.join("frag.m4a");
        let ok = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=7", "-c:a", "aac"])
            .args(["-movflags", "frag_keyframe+empty_moov+default_base_moof", "-frag_duration", "1000000"])
            .arg(&frag)
            .status()
            .unwrap();
        assert!(ok.success());
        let pcm = |p: &Path| {
            Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(p).args(["-f", "s16le", "-"]).output().unwrap()
        };
        let reference = dir.join("ffmpeg.m4a");
        let ok = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&frag)
            .args(["-c", "copy"])
            .arg(&reference)
            .status()
            .unwrap();
        assert!(ok.success());
        let before = pcm(&reference);
        assert!(is_fragmented(&std::fs::read(&frag).unwrap()));
        assert!(defragment_file(&frag).unwrap());
        let after = pcm(&frag);
        assert!(after.stderr.is_empty(), "{}", String::from_utf8_lossy(&after.stderr));
        assert_eq!(before.stdout, after.stdout, "decoded audio differs");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
