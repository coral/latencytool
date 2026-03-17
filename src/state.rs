use cpal::StreamInstant;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppMode {
    Idle,
    Calibrating,
    Measuring,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeasurementPhase {
    Idle,
    Playing,
    Listening,
    Detected,
}

pub struct SharedState {
    pub mode: AppMode,
    pub phase: MeasurementPhase,

    // Device selection (indices into enumerated device lists)
    pub output_device_idx: Option<usize>,
    pub input_device_idx: Option<usize>,

    // Probe playback
    pub probe_play_requested: bool,
    pub probe_playing: bool,
    pub probe_play_sample_idx: usize,
    pub emission_write_pos: usize,

    // Hardware timestamps for latency computation
    pub playback_instant: Option<StreamInstant>,
    pub output_callback_instant: Option<StreamInstant>,
    pub playback_frame_offset: u32,

    // Capture
    pub capture_buffer: Vec<f32>,
    pub capture_write_pos: usize,
    pub capture_sample_counter: u64,

    // Detection results
    pub last_ncc_peak: f32,
    pub last_latency_ms: Option<f64>,
    pub last_output_delay_ms: Option<f64>,
    pub last_lag_ms: Option<f64>,

    // Calibration
    pub calibration_measurements: Vec<f64>,
    pub calibration_count: usize,
    pub system_offset_ms: f64,

    // Measurement results
    pub measurements: VecDeque<f64>,
    pub measurement_count: u64,
    pub miss_count: u64,

    // Timing
    pub phase_start_sample: u64,
    pub listen_timeout_samples: u64,
    pub inter_probe_gap_samples: u64,

    // Audio params
    pub output_sample_rate: u32,
    pub input_sample_rate: u32,

    // Control
    pub error_message: Option<String>,
    pub streams_active: bool,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            mode: AppMode::Idle,
            phase: MeasurementPhase::Idle,
            output_device_idx: None,
            input_device_idx: None,
            probe_play_requested: false,
            probe_playing: false,
            probe_play_sample_idx: 0,
            emission_write_pos: 0,
            playback_instant: None,
            output_callback_instant: None,
            playback_frame_offset: 0,
            capture_buffer: vec![0.0; 48000 * 10],
            capture_write_pos: 0,
            capture_sample_counter: 0,
            last_ncc_peak: 0.0,
            last_latency_ms: None,
            last_output_delay_ms: None,
            last_lag_ms: None,
            calibration_measurements: Vec::new(),
            calibration_count: 5,
            system_offset_ms: 0.0,
            measurements: VecDeque::with_capacity(1000),
            measurement_count: 0,
            miss_count: 0,
            phase_start_sample: 0,
            listen_timeout_samples: 48000 * 5,
            inter_probe_gap_samples: 48000,
            output_sample_rate: 48000,
            input_sample_rate: 48000,
            error_message: None,
            streams_active: false,
        }
    }

    pub fn stats(&self) -> Option<Stats> {
        if self.measurements.is_empty() {
            return None;
        }
        let mut sorted: Vec<f64> = self.measurements.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = sorted.len();
        let min = sorted[0];
        let max = sorted[n - 1];
        let avg = sorted.iter().sum::<f64>() / n as f64;
        let p50 = percentile(&sorted, 50.0);
        let p95 = percentile(&sorted, 95.0);
        Some(Stats {
            min,
            max,
            avg,
            p50,
            p95,
            count: self.measurement_count,
            misses: self.miss_count,
        })
    }

    pub fn reset_measurements(&mut self) {
        self.measurements.clear();
        self.measurement_count = 0;
        self.miss_count = 0;
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let frac = rank - lower as f64;
    sorted[lower] * (1.0 - frac) + sorted[upper] * frac
}

pub struct Stats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
    pub count: u64,
    pub misses: u64,
}
