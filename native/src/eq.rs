use rodio::{Source, source::SeekError};
use std::{
    f32::consts::TAU,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

pub const FREQUENCIES: [f32; 20] = [
    25.0, 35.0, 49.0, 69.0, 97.0, 137.0, 192.0, 270.0, 380.0, 534.0, 750.0, 1053.0, 1480.0, 2079.0,
    2922.0, 4105.0, 5768.0, 8105.0, 11387.0, 16000.0,
];

pub const PRESETS: [[f32; 20]; 6] = [
    [0.0; 20],
    [
        8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ],
    [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0,
        6.0, 7.0,
    ],
    [
        5.0, 4.0, 3.0, 2.0, 1.0, 0.0, -1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0,
        2.0, 2.0,
    ],
    [
        -1.0, 0.0, 1.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ],
    [
        2.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 4.0,
        4.0, 4.0,
    ],
];

pub fn curve(gains: &[f32; 20]) -> (String, String) {
    use std::fmt::Write;
    let mut line = String::new();
    for (i, (&frequency, &gain)) in FREQUENCIES.iter().zip(gains).enumerate() {
        let x = (frequency.log10() - 25.0_f32.log10()) / (16000.0_f32.log10() - 25.0_f32.log10())
            * 1000.0;
        let y = 29.0 - gain.clamp(-12.0, 12.0) / 12.0 * 29.0;
        let _ = write!(line, "{}{x:.2},{y:.2} ", if i == 0 { 'M' } else { 'L' });
    }
    let area = format!("{line} L1000,29 L0,29 Z");
    (line, area)
}

#[derive(Default)]
pub struct Settings {
    gains: [AtomicU32; 20],
    preamp: AtomicU32,
    revision: AtomicU64,
    compressor: AtomicBool,
    /// RMS level of the playing audio in four bands (low → high), for the visualizer.
    levels: [AtomicU32; 4],
    /// Set while a seek is about to happen: output fades to silence so the jump doesn't click.
    seeking: AtomicBool,
}

impl Settings {
    pub fn set_normalizer(&self, enabled: bool) {
        self.compressor.store(enabled, Ordering::Relaxed);
    }

    pub fn set_preamp(&self, preamp: f32) {
        self.preamp.store(preamp.to_bits(), Ordering::Relaxed);
    }

    /// Fade the output out ahead of a seek; call [`Settings::end_seek`] once it has happened.
    pub fn begin_seek(&self) {
        self.seeking.store(true, Ordering::Relaxed);
    }

    pub fn end_seek(&self) {
        self.seeking.store(false, Ordering::Relaxed);
    }

    pub fn levels(&self) -> [f32; 4] {
        self.levels
            .each_ref()
            .map(|level| f32::from_bits(level.load(Ordering::Relaxed)))
    }

    pub fn update(&self, gains: &[f32; 20], preamp: f32) {
        for (value, gain) in self.gains.iter().zip(gains) {
            value.store(gain.to_bits(), Ordering::Relaxed);
        }
        self.set_preamp(preamp);
        self.revision.fetch_add(1, Ordering::Release);
    }
}

#[derive(Clone, Copy, Default)]
struct Delay {
    z1: f32,
    z2: f32,
}

#[derive(Clone, Copy)]
struct Filter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Filter {
    fn peak(frequency: f32, gain: f32, rate: u32) -> Option<Self> {
        if gain.abs() < 0.01 || frequency >= rate as f32 * 0.49 {
            return None;
        }
        let (sin, cos) = (TAU * frequency / rate as f32).sin_cos();
        let alpha = sin / 2.0;
        let a = 10.0f32.powf(gain / 40.0);
        let a0 = 1.0 + alpha / a;
        Some(Self {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / a) / a0,
        })
    }

    fn process(self, value: f32, state: &mut Delay) -> f32 {
        let output = self.b0 * value + state.z1;
        state.z1 = self.b1 * value - self.a1 * output + state.z2;
        state.z2 = self.b2 * value - self.a2 * output;
        output
    }
}

pub struct Equalizer<S> {
    inner: S,
    settings: Arc<Settings>,
    filters: [Option<Filter>; 20],
    delays: Vec<[Delay; 20]>,
    preamp: f32,
    revision: u64,
    rate: u32,
    channel: usize,
    remaining: u16,
    compressor: bool,
    leveler: Leveler,
    meter: Meter,
    /// Output gain for seek fades (0 = silent, 1 = normal) and its per-frame step.
    fade: f32,
    fade_step: f32,
}

/// Volume leveling: one steady gain per song, not compression. It measures the song's
/// loudness (ignoring silence) over the first few seconds, sets a gain that brings it to
/// a common level, then only drifts very slowly, so loud and quiet parts of a song keep
/// their balance. A fast limiter catches the rare peaks that would clip.
#[derive(Default)]
struct Leveler {
    /// Sum of squares of the current frame (all channels) and of the current block.
    frame: f32,
    block: f32,
    block_frames: u32,
    block_len: u32,
    /// Loudness estimate (mean square) and how many loud blocks it has seen.
    estimate: f32,
    blocks: u32,
    /// Gain in dB the leveler is heading to, and the linear gain applied now.
    gain_db: f32,
    gain: f32,
    smoothing: f32,
    limiter: f32,
    limiter_release: f32,
}

impl Leveler {
    /// Level songs are brought to (RMS dBFS), and how far they may be moved.
    const TARGET_DB: f32 = -14.0;
    const MAX_CUT_DB: f32 = 12.0;
    const MAX_BOOST_DB: f32 = 9.0;
    /// Blocks quieter than this are silence and don't count.
    const GATE: f32 = 1e-5;
    /// Output ceiling (-1 dBFS).
    const CEILING: f32 = 0.891;
    /// The first 3 seconds of loud audio set the level quickly.
    const SETTLE_BLOCKS: u32 = 30;

    fn reset(&mut self, rate: u32) {
        let rate = rate.max(1) as f32;
        *self = Self {
            block_len: (rate * 0.1) as u32,
            gain: 1.0,
            smoothing: 1.0 - (-1.0 / (rate * 0.05)).exp(),
            limiter: 1.0,
            limiter_release: 1.0 - (-1.0 / (rate * 0.15)).exp(),
            ..Self::default()
        };
    }

    fn sample(&mut self, sample: f32) -> f32 {
        self.frame += sample * sample;
        let level = sample * self.gain;
        let peak = level.abs() * self.limiter;
        if peak > Self::CEILING {
            self.limiter = Self::CEILING / level.abs().max(1e-9);
        }
        level * self.limiter
    }

    fn end_frame(&mut self, channels: usize) {
        self.block += self.frame / channels.max(1) as f32;
        self.frame = 0.0;
        self.block_frames += 1;
        self.limiter += (1.0 - self.limiter) * self.limiter_release;
        let target = 10.0f32.powf(self.gain_db / 20.0);
        self.gain += (target - self.gain) * self.smoothing;
        if self.block_frames < self.block_len.max(1) {
            return;
        }
        let mean_square = self.block / self.block_frames as f32;
        self.block = 0.0;
        self.block_frames = 0;
        if mean_square < Self::GATE {
            return;
        }
        let settling = self.blocks < Self::SETTLE_BLOCKS;
        if self.blocks == 0 {
            self.estimate = mean_square;
        } else {
            // About 1 s to settle at the start, then a 30 s memory.
            let memory = if settling { 1.0 } else { 30.0 };
            self.estimate += (mean_square - self.estimate) * (1.0 - (-0.1f32 / memory).exp());
        }
        self.blocks = self.blocks.saturating_add(1);
        let wanted = (Self::TARGET_DB - 10.0 * self.estimate.max(1e-9).log10())
            .clamp(-Self::MAX_CUT_DB, Self::MAX_BOOST_DB);
        // Move quickly while settling, then by at most 0.5 dB per second.
        let step = if settling { 1.0 } else { 0.05 };
        self.gain_db += (wanted - self.gain_db).clamp(-step, step);
    }
}

/// Splits the output into four bands with one-pole low-passes and accumulates their energy.
#[derive(Default)]
struct Meter {
    coefficients: [f32; 3],
    lows: [f32; 3],
    energy: [f32; 4],
    frames: u32,
    window: u32,
}

impl Meter {
    const CUTOFFS: [f32; 3] = [180.0, 900.0, 3500.0];

    fn reset(&mut self, rate: u32) {
        *self = Self {
            coefficients: Self::CUTOFFS
                .map(|cutoff| 1.0 - (-TAU * cutoff / rate.max(1) as f32).exp()),
            window: (rate / 40).max(1),
            ..Self::default()
        };
    }

    fn push(&mut self, sample: f32, levels: &[AtomicU32; 4]) {
        for (low, coefficient) in self.lows.iter_mut().zip(self.coefficients) {
            *low += (sample - *low) * coefficient;
        }
        let bands = [
            self.lows[0],
            self.lows[1] - self.lows[0],
            self.lows[2] - self.lows[1],
            sample - self.lows[2],
        ];
        for (energy, band) in self.energy.iter_mut().zip(bands) {
            *energy += band * band;
        }
        self.frames += 1;
        if self.frames >= self.window {
            for (level, energy) in levels.iter().zip(&mut self.energy) {
                level.store(
                    (*energy / self.frames as f32).sqrt().to_bits(),
                    Ordering::Relaxed,
                );
                *energy = 0.0;
            }
            self.frames = 0;
        }
    }
}

impl<S: Source> Equalizer<S> {
    pub fn new(inner: S, settings: Arc<Settings>) -> Self {
        let channels = usize::from(inner.channels()).max(1);
        Self {
            inner,
            settings,
            filters: [None; 20],
            delays: vec![[Delay::default(); 20]; channels],
            preamp: 1.0,
            revision: u64::MAX,
            rate: 0,
            channel: 0,
            remaining: 0,
            compressor: false,
            leveler: Leveler::default(),
            meter: Meter::default(),
            fade: 1.0,
            fade_step: 1.0,
        }
    }

    fn refresh(&mut self) {
        self.compressor = self.settings.compressor.load(Ordering::Relaxed);
        self.preamp =
            10.0f32.powf(f32::from_bits(self.settings.preamp.load(Ordering::Relaxed)) / 20.0);
        let revision = self.settings.revision.load(Ordering::Acquire);
        let rate = self.inner.sample_rate();
        let channels = usize::from(self.inner.channels()).max(1);
        if self.revision == revision && self.rate == rate && self.delays.len() == channels {
            return;
        }
        self.revision = revision;
        // EQ changes land here too; keep the song's level unless the format changed.
        if self.leveler.block_len == 0 || self.rate != rate {
            self.leveler.reset(rate);
        }
        self.rate = rate;
        self.meter.reset(rate);
        // About 12 ms to fade fully out or back in.
        self.fade_step = 1.0 / (rate.max(1) as f32 * 0.012);
        self.channel = 0;
        self.delays.resize(channels, [Delay::default(); 20]);
        for (index, filter) in self.filters.iter_mut().enumerate() {
            let gain = f32::from_bits(self.settings.gains[index].load(Ordering::Relaxed));
            *filter = Filter::peak(FREQUENCIES[index], gain, rate);
        }
        self.delays.fill([Delay::default(); 20]);
    }
}

impl<S: Source> Iterator for Equalizer<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.remaining == 0 {
            self.refresh();
            self.remaining = 256;
        }
        self.remaining -= 1;
        let mut sample = self.inner.next()?;
        for (filter, delay) in self.filters.iter().zip(&mut self.delays[self.channel]) {
            if let Some(filter) = filter {
                sample = filter.process(sample, delay);
            }
        }
        if self.compressor {
            sample = self.leveler.sample(sample);
        }
        self.channel += 1;
        if self.channel == self.delays.len() {
            self.channel = 0;
            if self.compressor {
                self.leveler.end_frame(self.delays.len());
            }
        }
        // Apply the user gain after normalization so compression can't undo it.
        let output = sample * self.preamp * self.fade;
        if self.channel == 0 {
            let target = if self.settings.seeking.load(Ordering::Relaxed) {
                0.0
            } else {
                1.0
            };
            self.fade += (target - self.fade).clamp(-self.fade_step, self.fade_step);
            // Meter one channel per frame; enough for a visual level.
            self.meter.push(output, &self.settings.levels);
        }
        Some(output)
    }
}

impl<S: Source> Source for Equalizer<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(position)?;
        // Start the new position from silence and fade back in.
        self.fade = 0.0;
        self.delays.fill([Delay::default(); 20]);
        self.channel = 0;
        self.remaining = 0;
        // Same song: keep its level, just drop the partly measured frame.
        self.leveler.frame = 0.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    #[test]
    fn leveling_brings_songs_toward_one_level_without_clipping() {
        let settings = Arc::new(Settings::default());
        settings.set_normalizer(true);
        let rate = 48_000usize;
        let tone = |amplitude: f32| -> Vec<f32> {
            (0..rate * 10).map(|i| amplitude * (i as f32 * 0.05).sin()).collect()
        };
        let rms = |samples: &[f32]| {
            (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
        };
        let level = |amplitude: f32| -> Vec<f32> {
            Equalizer::new(SamplesBuffer::new(1, rate as u32, tone(amplitude)), settings.clone())
                .collect()
        };
        let (quiet, loud) = (level(0.04), level(0.95));
        let last_second = |samples: &Vec<f32>| rms(&samples[samples.len() - rate..]);
        // A quiet song is raised (by at most 9 dB), a loud one turned down.
        assert!(last_second(&quiet) > rms(&tone(0.04)) * 2.0);
        assert!(last_second(&loud) < rms(&tone(0.95)) * 0.6);
        // Peaks never pass the -1 dBFS ceiling, even before the level settles.
        assert!(loud.iter().all(|x| x.abs() <= 0.9));
    }

    #[test]
    fn flat_bands_preserve_interleaved_samples() {
        let signal = vec![0.2, -0.3, 0.1, 0.0, -0.9, 0.8];
        let source = SamplesBuffer::new(2, 48_000, signal.clone());
        assert_eq!(
            Equalizer::new(source, Arc::default()).collect::<Vec<_>>(),
            signal
        );
    }

    #[test]
    fn seek_fades_out_and_back_in_without_a_jump() {
        let settings = Arc::new(Settings::default());
        let source = SamplesBuffer::new(1, 48_000, vec![0.5; 48_000]);
        let mut playing = Equalizer::new(source, settings.clone());
        assert!((playing.nth(10).unwrap() - 0.5).abs() < 1e-6);
        settings.begin_seek();
        let faded: Vec<f32> = playing.by_ref().take(1_000).collect();
        // Monotonic fade with small steps, reaching silence within ~12 ms.
        assert!(
            faded
                .windows(2)
                .all(|pair| pair[1] <= pair[0] && pair[0] - pair[1] < 0.01)
        );
        assert_eq!(*faded.last().unwrap(), 0.0);
        playing.try_seek(Duration::from_millis(100)).unwrap();
        settings.end_seek();
        let resumed: Vec<f32> = playing.by_ref().take(1_000).collect();
        assert!(resumed[0] < 0.01);
        assert!(
            resumed
                .windows(2)
                .all(|pair| pair[1] >= pair[0] && pair[1] - pair[0] < 0.01)
        );
        assert!((resumed.last().unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn preamp_changes_amplitude() {
        for normalize in [false, true] {
            let settings = Arc::new(Settings::default());
            settings.set_normalizer(normalize);
            let source = SamplesBuffer::new(1, 48_000, vec![0.2; 50_000]);
            let mut playing = Equalizer::new(source, settings.clone());
            // Let the compressor settle before comparing the same signal at each gain.
            let baseline = playing.nth(47_999).unwrap();
            settings.set_preamp(-6.0);
            let quieter = playing.nth(255).unwrap();
            assert!((quieter / baseline - 10.0_f32.powf(-6.0 / 20.0)).abs() < 0.001);
            settings.set_preamp(6.0);
            let louder = playing.nth(255).unwrap();
            assert!((louder / baseline - 10.0_f32.powf(6.0 / 20.0)).abs() < 0.001);
        }
    }
}
