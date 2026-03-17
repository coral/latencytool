use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Mutex;

use crate::dsp::{self, BANDPASS_HIGH, BANDPASS_LOW, ENVELOPE_RATE};

static PROBE_WAV: &[u8] = include_bytes!("../assets/probe3.wav");

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
        let filtered = dsp::bandpass_filter(&resampled, BANDPASS_LOW, BANDPASS_HIGH, sample_rate);
        let envelope = dsp::extract_envelope(&filtered, sample_rate, ENVELOPE_RATE);

        cache.insert(sample_rate, CachedProbe { filtered, envelope });
    }
}
