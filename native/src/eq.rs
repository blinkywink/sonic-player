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
}

impl Settings {
    pub fn set_normalizer(&self, enabled: bool) {
        self.compressor.store(enabled, Ordering::Relaxed);
    }

    pub fn set_preamp(&self, preamp: f32) {
        self.preamp.store(preamp.to_bits(), Ordering::Relaxed);
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
    peak: f32,
    reduction: f32,
    attack: f32,
    release: f32,
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
            peak: 0.0,
            reduction: 0.0,
            attack: 0.0,
            release: 0.0,
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
        self.rate = rate;
        self.attack = 1.0 - (-1.0 / (rate as f32 * 0.003)).exp();
        self.release = 1.0 - (-1.0 / (rate as f32 * 0.250)).exp();
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
            self.peak = self.peak.max(sample.abs());
            sample *= 10.0_f32.powf(self.reduction / 20.0);
        }
        self.channel += 1;
        if self.channel == self.delays.len() {
            self.channel = 0;
            if self.compressor {
                // The original Web Audio compressor uses a -24 dB threshold,
                // 30 dB knee, 12:1 ratio, 3 ms attack and 250 ms release.
                let over = 20.0 * self.peak.max(0.000_001).log10() + 24.0;
                let amount = if over < -15.0 {
                    0.0
                } else if over > 15.0 {
                    over
                } else {
                    (over + 15.0).powi(2) / 60.0
                };
                let target = (1.0 / 12.0 - 1.0) * amount;
                let speed = if target < self.reduction {
                    self.attack
                } else {
                    self.release
                };
                self.reduction += (target - self.reduction) * speed;
                self.peak = 0.0;
            } else {
                self.reduction = 0.0;
                self.peak = 0.0;
            }
        }
        // Apply the user gain after normalization so compression can't undo it.
        Some(sample * self.preamp)
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
        self.delays.fill([Delay::default(); 20]);
        self.channel = 0;
        self.remaining = 0;
        self.peak = 0.0;
        self.reduction = 0.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

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
