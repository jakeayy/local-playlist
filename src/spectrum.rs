//! Turns recent audio samples into smoothed, log-spaced frequency bands for
//! visualizations. Self-contained (a small radix-2 FFT) so it builds for the
//! browser too without another dependency.

use std::f32::consts::PI;

/// FFT size: ~46 ms at 44.1 kHz, 21.5 Hz per bin.
pub const FFT_LEN: usize = 2048;

/// How bands are measured and smoothed.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub min_hz: f32,
    pub max_hz: f32,
    /// Level range mapped to 0..1 (after the slope).
    pub floor_db: f32,
    pub ceil_db: f32,
    /// Tilt per octave around 1 kHz. Music has less energy up high: a positive
    /// slope evens the bands out, zero keeps the bass dominant.
    pub slope_db_per_octave: f32,
    /// Smoothing time constants (seconds) of the lowest and highest band;
    /// bands in between interpolate. Bass moves a lot from frame to frame
    /// (few FFT bins per band, beats longer than the FFT), so it needs more.
    pub bass_rise_secs: f32,
    pub bass_fall_secs: f32,
    pub treble_rise_secs: f32,
    pub treble_fall_secs: f32,
    /// Passes of a [1 2 1] blur across neighbouring bands.
    pub blur_passes: usize,
    /// Levels below this (0..1) count as silence; the rest is rescaled to 0..1
    /// and raised to `curve`, so quiet parts stay calm and loud ones stand out.
    pub gate: f32,
    pub curve: f32,
    /// How much of each band's recent average (over `adapt_secs`) to subtract,
    /// 0..1. Sustained notes then settle low and only hits (kicks) stand out.
    pub adapt: f32,
    pub adapt_secs: f32,
}

impl Tuning {
    /// Even, lively bars.
    pub const BARS: Tuning = Tuning {
        min_hz: 30.0,
        max_hz: 16_000.0,
        floor_db: -72.0,
        ceil_db: -12.0,
        slope_db_per_octave: 3.0,
        bass_rise_secs: 0.06,
        bass_fall_secs: 0.24,
        treble_rise_secs: 0.015,
        treble_fall_secs: 0.14,
        blur_passes: 1,
        gate: 0.0,
        curve: 1.0,
        adapt: 0.0,
        adapt_secs: 1.0,
    };

    /// Bass-led ring that jumps on hits: no treble boost, mostly bass and
    /// mids, relative to the recent average, gated.
    pub const CIRCLE: Tuning = Tuning {
        min_hz: 30.0,
        max_hz: 6_000.0,
        floor_db: -50.0,
        ceil_db: -6.0,
        slope_db_per_octave: 0.0,
        bass_rise_secs: 0.06,
        bass_fall_secs: 0.2,
        treble_rise_secs: 0.04,
        treble_fall_secs: 0.2,
        blur_passes: 2,
        gate: 0.1,
        curve: 1.4,
        adapt: 0.8,
        adapt_secs: 1.2,
    };
}

/// Upper edge of what counts as bass for [`Spectrum::bass`].
const BASS_HZ: f32 = 140.0;
const BASS_LEVEL_RISE_SECS: f32 = 0.03;
const BASS_LEVEL_FALL_SECS: f32 = 0.2;
/// Peak markers hold, then fall at this speed (full height per second).
const PEAK_HOLD_SECS: f32 = 0.35;
const PEAK_FALL_PER_SEC: f32 = 0.6;

pub struct Spectrum {
    tuning: Tuning,
    window: Vec<f32>,
    /// Sum of the window (for normalising magnitudes).
    window_sum: f32,
    twiddles: Vec<(f32, f32)>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Magnitude per FFT bin (first half).
    mags: Vec<f32>,
    /// Smoothed band levels, 0..1, low to high.
    bands: Vec<f32>,
    peaks: Vec<f32>,
    peak_age: Vec<f32>,
    /// Recent average level per band (for `Tuning::adapt`).
    avg: Vec<f32>,
    /// Bass level of the latest frame (unsmoothed) and smoothed.
    bass_raw: f32,
    bass: f32,
}

impl Spectrum {
    pub fn new(bands: usize, tuning: Tuning) -> Self {
        let window: Vec<f32> =
            (0..FFT_LEN).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_LEN - 1) as f32).cos()).collect();
        let window_sum = window.iter().sum();
        let twiddles = (0..FFT_LEN / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f32 / FFT_LEN as f32;
                (a.cos(), a.sin())
            })
            .collect();
        Self {
            tuning,
            window,
            window_sum,
            twiddles,
            re: vec![0.0; FFT_LEN],
            im: vec![0.0; FFT_LEN],
            mags: vec![0.0; FFT_LEN / 2],
            bands: vec![0.0; bands],
            peaks: vec![0.0; bands],
            peak_age: vec![0.0; bands],
            avg: vec![0.0; bands],
            bass_raw: 0.0,
            bass: 0.0,
        }
    }

    pub fn bands(&self) -> &[f32] {
        &self.bands
    }

    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// Bass level (0..1) of the latest frame, unsmoothed: jumps on kicks.
    pub fn bass_raw(&self) -> f32 {
        self.bass_raw
    }

    /// Smoothed bass level (0..1), for pulsing.
    pub fn bass(&self) -> f32 {
        self.bass
    }

    /// True once everything has fallen back to zero (nothing left to animate).
    pub fn is_silent(&self) -> bool {
        self.bands.iter().chain(&self.peaks).all(|&v| v < 0.002)
    }

    /// Frequency range of band `b` (of `n`) at sample rate `rate`.
    fn band_range(&self, b: usize, n: usize, rate: u32) -> (f32, f32) {
        let t = &self.tuning;
        let ratio = (t.max_hz.min(rate as f32 / 2.0) / t.min_hz).ln();
        (t.min_hz * (ratio * b as f32 / n as f32).exp(), t.min_hz * (ratio * (b + 1) as f32 / n as f32).exp())
    }

    /// Advance by `dt` seconds. `samples` holds the latest [`FFT_LEN`] mono
    /// samples at `rate` Hz, or `None` when nothing is playing (bars fall).
    pub fn update(&mut self, samples: Option<(&[f32], u32)>, dt: f32) {
        let dt = dt.clamp(0.0, 0.25);
        let t = self.tuning;
        let n = self.bands.len();
        let mut targets = vec![0.0; n];
        let mut bass_bands = 0;
        if let Some((samples, rate)) = samples {
            self.analyse(samples);
            let bin_hz = rate as f32 / FFT_LEN as f32;
            for (b, target) in targets.iter_mut().enumerate() {
                let (lo, hi) = self.band_range(b, n, rate);
                let mag = self.band_magnitude(lo / bin_hz, hi / bin_hz);
                let center = (lo * hi).sqrt();
                let db = 20.0 * mag.max(1e-9).log10() + t.slope_db_per_octave * (center / 1000.0).log2();
                let mut level = ((db - t.floor_db) / (t.ceil_db - t.floor_db)).clamp(0.0, 1.0);
                if t.adapt > 0.0 {
                    let avg = &mut self.avg[b];
                    *avg += (level - *avg) * (1.0 - (-dt / t.adapt_secs).exp());
                    let base = t.adapt * *avg;
                    level = ((level - base) / (1.0 - base).max(0.05)).clamp(0.0, 1.0);
                }
                *target = ((level - t.gate) / (1.0 - t.gate)).clamp(0.0, 1.0).powf(t.curve);
                if center <= BASS_HZ {
                    bass_bands = b + 1;
                }
            }
        }
        self.bass_raw = targets[..bass_bands].iter().sum::<f32>() / bass_bands.max(1) as f32;
        let k = |secs: f32| 1.0 - (-dt / secs).exp();
        let bass_k = k(if self.bass_raw > self.bass { BASS_LEVEL_RISE_SECS } else { BASS_LEVEL_FALL_SECS });
        self.bass += (self.bass_raw - self.bass) * bass_k;
        // Blend each band with its neighbours so adjacent bands don't jitter apart.
        for _ in 0..t.blur_passes {
            targets = (0..n)
                .map(|b| {
                    let (l, r) = (targets[b.saturating_sub(1)], targets[(b + 1).min(n - 1)]);
                    0.25 * l + 0.5 * targets[b] + 0.25 * r
                })
                .collect();
        }
        let levels = self.bands.iter_mut().zip(&mut self.peaks).zip(&mut self.peak_age);
        for (b, (((v, peak), age), target)) in levels.zip(targets).enumerate() {
            let f = b as f32 / (n - 1).max(1) as f32;
            let (rise, fall) = (
                k(t.bass_rise_secs + (t.treble_rise_secs - t.bass_rise_secs) * f),
                k(t.bass_fall_secs + (t.treble_fall_secs - t.bass_fall_secs) * f),
            );
            *v += (target - *v) * if target > *v { rise } else { fall };
            if *v >= *peak {
                *peak = *v;
                *age = 0.0;
            } else {
                *age += dt;
                if *age > PEAK_HOLD_SECS {
                    *peak = (*peak - PEAK_FALL_PER_SEC * dt).max(*v);
                }
            }
        }
    }

    /// Strongest bin between fractional bins `lo` and `hi`; bands narrower
    /// than a bin interpolate between the two nearest bins instead.
    fn band_magnitude(&self, lo: f32, hi: f32) -> f32 {
        let last = self.mags.len() - 1;
        let (a, b) = (lo.ceil() as usize, (hi.floor() as usize).min(last));
        if a <= b {
            return self.mags[a..=b].iter().copied().fold(0.0, f32::max);
        }
        let c = ((lo + hi) / 2.0).min(last as f32);
        let i = c.floor() as usize;
        let f = c - i as f32;
        self.mags[i] * (1.0 - f) + self.mags[(i + 1).min(last)] * f
    }

    /// Windowed FFT of `samples` into `self.mags` (amplitude, full scale = 1).
    fn analyse(&mut self, samples: &[f32]) {
        let len = samples.len().min(FFT_LEN);
        // Right-align: the newest samples matter most.
        let start = FFT_LEN - len;
        self.re[..start].fill(0.0);
        for i in 0..len {
            self.re[start + i] = samples[samples.len() - len + i] * self.window[start + i];
        }
        self.im.fill(0.0);
        fft(&mut self.re, &mut self.im, &self.twiddles);
        let scale = 2.0 / self.window_sum;
        for (k, m) in self.mags.iter_mut().enumerate() {
            *m = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() * scale;
        }
    }
}

/// In-place iterative radix-2 FFT; `re.len()` must be a power of two and
/// `twiddles[k] = e^(-2πik/N)` for `k < N/2`.
fn fft(re: &mut [f32], im: &mut [f32], twiddles: &[(f32, f32)]) {
    let n = re.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let half = size / 2;
        let step = n / size;
        for start in (0..n).step_by(size) {
            for k in 0..half {
                let (wr, wi) = twiddles[k * step];
                let (a, b) = (start + k, start + k + half);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        size *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints how a tuning behaves on real songs: per region of the spectrum,
    /// the average level, how often it's high, and the frame-to-frame jitter.
    /// `LP_CALIBRATE=file1:file2 cargo test --lib calibrate -- --ignored --nocapture`
    #[test]
    #[cfg(feature = "desktop")]
    #[ignore = "needs audio files"]
    fn calibrate() {
        use rodio::Source as _;
        let files = std::env::var("LP_CALIBRATE").unwrap();
        for (name, tuning, n) in [("bars", Tuning::BARS, 64), ("circle", Tuning::CIRCLE, 40)] {
            println!("== {name}");
            for file in files.split(':') {
                let d = rodio::Decoder::try_from(std::fs::File::open(file).unwrap()).unwrap();
                let (ch, rate) = (d.channels().get() as usize, d.sample_rate().get());
                let all: Vec<f32> = d.collect();
                let mono: Vec<f32> = all.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
                let mut s = Spectrum::new(n, tuning);
                let step = rate as usize / 60;
                let regions = [(0, n / 4), (n / 4, n / 2), (n / 2, 3 * n / 4), (3 * n / 4, n)];
                let (mut sum, mut high, mut jitter, mut frames) = ([0f32; 4], [0f32; 4], [0f32; 4], 0f32);
                let mut sq = [0f32; 4];
                let mut prev = vec![0.0; n];
                let mut end = FFT_LEN;
                while end <= mono.len() {
                    s.update(Some((&mono[end - FFT_LEN..end], rate)), 1.0 / 60.0);
                    for (r, &(a, b)) in regions.iter().enumerate() {
                        let k = (b - a) as f32;
                        let m = s.bands()[a..b].iter().sum::<f32>() / k;
                        sum[r] += m;
                        sq[r] += m * m;
                        high[r] += s.bands()[a..b].iter().filter(|&&v| v > 0.6).count() as f32 / k;
                        jitter[r] +=
                            s.bands()[a..b].iter().zip(&prev[a..b]).map(|(x, y)| (x - y).abs()).sum::<f32>() / k;
                    }
                    prev.copy_from_slice(s.bands());
                    frames += 1.0;
                    end += step;
                }
                let short: String = file.rsplit('/').next().unwrap().chars().take(28).collect();
                let f = |a: [f32; 4], m: f32| a.map(|x| format!("{:5.2}", x / frames * m)).join(" ");
                let std: [f32; 4] =
                    std::array::from_fn(|r| (sq[r] / frames - (sum[r] / frames).powi(2)).max(0.0).sqrt() * frames);
                println!(
                    "{short:28} mean [{}]  swing [{}]  >0.6 [{}]  jitter% [{}]",
                    f(sum, 1.0),
                    f(std, 1.0),
                    f(high, 1.0),
                    f(jitter, 100.0)
                );
            }
        }
    }

    fn sine(hz: f32, amp: f32, rate: u32) -> Vec<f32> {
        (0..FFT_LEN).map(|i| amp * (2.0 * PI * hz * i as f32 / rate as f32).sin()).collect()
    }

    #[test]
    fn sine_peaks_in_its_bin() {
        let mut s = Spectrum::new(48, Tuning::BARS);
        let rate = 44_100;
        // Bin 93 exactly.
        let hz = 93.0 * rate as f32 / FFT_LEN as f32;
        s.analyse(&sine(hz, 0.5, rate));
        let max = s.mags.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        assert_eq!(max.0, 93);
        // Amplitude is recovered (Hann window, bin-centred).
        assert!((max.1 - 0.5).abs() < 0.02, "{}", max.1);
    }

    #[test]
    fn bands_follow_frequency_and_fall_when_silent() {
        let mut s = Spectrum::new(48, Tuning::BARS);
        let rate = 48_000;
        let samples = sine(1000.0, 0.5, rate);
        for _ in 0..30 {
            s.update(Some((&samples, rate)), 1.0 / 60.0);
        }
        let loudest = s.bands().iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let (lo, hi) = s.band_range(loudest, 48, rate);
        assert!(lo <= 1000.0 * 1.06 && hi >= 1000.0 / 1.06, "band {loudest}: {lo}..{hi} Hz");
        // A lone tone is partly blended into its neighbours.
        assert!(s.bands()[loudest] > 0.7);
        assert!(s.bands()[0] < 0.1);
        for _ in 0..240 {
            s.update(None, 1.0 / 60.0);
        }
        assert!(s.is_silent());
    }
}
