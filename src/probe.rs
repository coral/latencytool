use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type, Q_BUTTERWORTH_F32};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Mutex;

static PROBE_WAV: &[u8] = include_bytes!("../assets/probe3.wav");

const BANDPASS_LOW: f32 = 500.0;
const BANDPASS_HIGH: f32 = 3500.0;
const ENVELOPE_LPF: f32 = 100.0;
const ENVELOPE_RATE: u32 = 1000;

pub struct Probe {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// Cache: sample_rate -> (filtered, envelope)
    cache: Mutex<HashMap<u32, CachedProbe>>,
}

#[derive(Clone)]
struct CachedProbe {
    filtered: Vec<f32>,
    envelope: Vec<f32>,
}

impl Probe {
    pub fn load() -> Self {
        let cursor = Cursor::new(PROBE_WAV);
        let mut reader = hound::WavReader::new(cursor).expect("Failed to read probe WAV");
        let spec = reader.spec();
        let channels = spec.channels as usize;
        let sample_rate = spec.sample_rate;

        let raw_samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let max_val = (1u32 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.unwrap() as f32 / max_val)
                    .collect()
            }
            hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        };

        // Convert to mono by averaging channels
        let mono: Vec<f32> = raw_samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect();

        Probe {
            samples: mono,
            sample_rate,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Resample probe to target sample rate using rubato
    pub fn resampled(&self, target_rate: u32) -> Vec<f32> {
        if self.sample_rate == target_rate {
            return self.samples.clone();
        }

        use rubato::{
            Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
            WindowFunction,
        };

        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        let ratio = target_rate as f64 / self.sample_rate as f64;
        let mut resampler = SincFixedIn::<f32>::new(
            ratio,
            2.0,
            params,
            self.samples.len(),
            1, // mono
        )
        .expect("Failed to create resampler");

        let input = vec![self.samples.clone()];
        let output = resampler.process(&input, None).expect("Resampling failed");
        output.into_iter().next().unwrap()
    }

    /// Get or compute cached envelope at the given sample rate
    pub fn envelope(&self, sample_rate: u32) -> Vec<f32> {
        self.ensure_cached(sample_rate);
        self.cache.lock().unwrap()[&sample_rate].envelope.clone()
    }

    /// Get or compute cached bandpass-filtered signal at the given sample rate
    pub fn filtered(&self, sample_rate: u32) -> Vec<f32> {
        self.ensure_cached(sample_rate);
        self.cache.lock().unwrap()[&sample_rate].filtered.clone()
    }

    fn ensure_cached(&self, sample_rate: u32) {
        let mut cache = self.cache.lock().unwrap();
        if cache.contains_key(&sample_rate) {
            return;
        }

        let resampled = self.resampled(sample_rate);
        let filtered = bandpass_filter(&resampled, BANDPASS_LOW, BANDPASS_HIGH, sample_rate);
        let envelope = extract_envelope(&filtered, sample_rate, ENVELOPE_RATE);

        cache.insert(sample_rate, CachedProbe { filtered, envelope });
    }
}

fn bandpass_filter(signal: &[f32], low_hz: f32, high_hz: f32, sample_rate: u32) -> Vec<f32> {
    let fs = sample_rate.hz();

    let hp_coeffs =
        Coefficients::<f32>::from_params(Type::HighPass, fs, low_hz.hz(), Q_BUTTERWORTH_F32)
            .unwrap();
    let mut hp1 = DirectForm1::<f32>::new(hp_coeffs);
    let mut hp2 = DirectForm1::<f32>::new(hp_coeffs);

    let lp_coeffs =
        Coefficients::<f32>::from_params(Type::LowPass, fs, high_hz.hz(), Q_BUTTERWORTH_F32)
            .unwrap();
    let mut lp1 = DirectForm1::<f32>::new(lp_coeffs);
    let mut lp2 = DirectForm1::<f32>::new(lp_coeffs);

    signal
        .iter()
        .map(|&x| {
            let y = hp1.run(x);
            let y = hp2.run(y);
            let y = lp1.run(y);
            lp2.run(y)
        })
        .collect()
}

fn extract_envelope(signal: &[f32], sample_rate: u32, target_rate: u32) -> Vec<f32> {
    let fs = sample_rate.hz();

    let lp_coeffs =
        Coefficients::<f32>::from_params(Type::LowPass, fs, ENVELOPE_LPF.hz(), Q_BUTTERWORTH_F32)
            .unwrap();
    let mut lp1 = DirectForm1::<f32>::new(lp_coeffs);
    let mut lp2 = DirectForm1::<f32>::new(lp_coeffs);

    let decimation = sample_rate as usize / target_rate as usize;
    if decimation == 0 {
        return vec![];
    }

    let mut envelope = Vec::with_capacity(signal.len() / decimation + 1);

    for (i, &x) in signal.iter().enumerate() {
        let rectified = x.abs();
        let y = lp1.run(rectified);
        let y = lp2.run(y);

        if i % decimation == 0 {
            envelope.push(y);
        }
    }

    envelope
}
