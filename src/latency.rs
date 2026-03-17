use cpal::StreamInstant;
use std::time::Duration;

/// Result of hardware-timestamp-based latency computation.
pub struct TimestampLatency {
    pub latency_ms: f64,
    pub output_delay_ms: f64,
    pub lag_ms: f64,
}

/// Compute latency using hardware timestamps from CoreAudio.
///
/// The capture snapshot starts at `emission_write_pos` (set at output callback time).
/// `lag_samples` into that snapshot, the probe appears. The output buffer delay
/// (playback - callback) represents time before the probe reaches the DAC, so:
///
///   latency = lag_samples/input_rate - (playback - callback) - frame_offset/output_rate
pub fn compute_latency_from_timestamps(
    lag_samples: f64,
    input_rate: u32,
    output_rate: u32,
    playback_instant: Option<StreamInstant>,
    output_callback_instant: Option<StreamInstant>,
    playback_frame_offset: u32,
) -> TimestampLatency {
    let playback_ts = playback_instant.expect("playback_instant must be set before detection");
    let callback_ts = output_callback_instant.expect("output_callback_instant must be set before detection");

    let probe_frame_offset = Duration::from_secs_f64(playback_frame_offset as f64 / output_rate as f64);
    let output_delay = playback_ts.duration_since(&callback_ts)
        .expect("playback must be after callback")
        + probe_frame_offset;

    let lag_secs = lag_samples / input_rate as f64;
    let latency_ms = (lag_secs - output_delay.as_secs_f64()) * 1000.0;

    TimestampLatency {
        latency_ms,
        output_delay_ms: output_delay.as_secs_f64() * 1000.0,
        lag_ms: lag_secs * 1000.0,
    }
}
