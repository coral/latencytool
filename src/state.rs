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

pub struct ProbePlayback {
    pub requested: bool,
    pub playing: bool,
    pub sample_idx: usize,
    pub emission_write_pos: usize,
}

pub struct Timestamps {
    pub playback: Option<StreamInstant>,
    pub output_callback: Option<StreamInstant>,
    pub frame_offset: u32,
}

pub struct CaptureState {
    pub buffer: Vec<f32>,
    pub write_pos: usize,
    pub sample_counter: u64,
}

pub struct DetectionResults {
    pub ncc_peak: f32,
    pub latency_ms: Option<f64>,
    pub output_delay_ms: Option<f64>,
    pub lag_ms: Option<f64>,
}

pub struct CalibrationState {
    pub measurements: Vec<f64>,
    pub count: usize,
    pub system_offset_ms: f64,
}

pub struct MeasurementResults {
    pub values: VecDeque<f64>,
    pub count: u64,
    pub miss_count: u64,
}

pub struct SharedState {
    pub mode: AppMode,
    pub phase: MeasurementPhase,

    // Device selection (indices into enumerated device lists)
    pub output_device_idx: Option<usize>,
    pub input_device_idx: Option<usize>,

    pub probe: ProbePlayback,
    pub timestamps: Timestamps,
    pub capture: CaptureState,
    pub detection: DetectionResults,
    pub calibration: CalibrationState,
    pub measurement: MeasurementResults,

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
            probe: ProbePlayback {
                requested: false,
                playing: false,
                sample_idx: 0,
                emission_write_pos: 0,
            },
            timestamps: Timestamps {
                playback: None,
                output_callback: None,
                frame_offset: 0,
            },
            capture: CaptureState {
                buffer: vec![0.0; 48000 * 10],
                write_pos: 0,
                sample_counter: 0,
            },
            detection: DetectionResults {
                ncc_peak: 0.0,
                latency_ms: None,
                output_delay_ms: None,
                lag_ms: None,
            },
            calibration: CalibrationState {
                measurements: Vec::new(),
                count: 5,
                system_offset_ms: 0.0,
            },
            measurement: MeasurementResults {
                values: VecDeque::with_capacity(1000),
                count: 0,
                miss_count: 0,
            },
            phase_start_sample: 0,
            listen_timeout_samples: 48000 * 5,
            inter_probe_gap_samples: 48000,
            output_sample_rate: 48000,
            input_sample_rate: 48000,
            error_message: None,
            streams_active: false,
        }
    }

    /// Extract the capture buffer contents from emission_write_pos to capture_write_pos,
    /// unwrapping the circular buffer.
    pub fn capture_snapshot(&self) -> Vec<f32> {
        let buf_len = self.capture.buffer.len();
        let emission_pos = self.probe.emission_write_pos % buf_len;
        let write_pos = self.capture.write_pos % buf_len;
        if write_pos > emission_pos {
            self.capture.buffer[emission_pos..write_pos].to_vec()
        } else if self.capture.write_pos > self.probe.emission_write_pos {
            let mut v = self.capture.buffer[emission_pos..].to_vec();
            v.extend_from_slice(&self.capture.buffer[..write_pos]);
            v
        } else {
            vec![]
        }
    }

    pub fn stats(&self) -> Option<Stats> {
        if self.measurement.values.is_empty() {
            return None;
        }
        let mut sorted: Vec<f64> = self.measurement.values.iter().copied().collect();
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
            count: self.measurement.count,
            misses: self.measurement.miss_count,
        })
    }

    pub fn reset_measurements(&mut self) {
        self.measurement.values.clear();
        self.measurement.count = 0;
        self.measurement.miss_count = 0;
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
