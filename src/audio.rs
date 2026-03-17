use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::{Arc, Mutex};

use crate::state::{MeasurementPhase, SharedState};

pub struct AudioDeviceInfo {
    pub name: String,
    pub device: Device,
}

pub fn enumerate_output_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    if let Ok(output_devices) = host.output_devices() {
        for device in output_devices {
            if let Ok(name) = device.name() {
                devices.push(AudioDeviceInfo { name, device });
            }
        }
    }
    devices
}

pub fn enumerate_input_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    if let Ok(input_devices) = host.input_devices() {
        for device in input_devices {
            if let Ok(name) = device.name() {
                devices.push(AudioDeviceInfo { name, device });
            }
        }
    }
    devices
}

pub fn start_output_stream(
    device: &Device,
    probe_samples: Arc<Vec<f32>>,
    state: Arc<Mutex<SharedState>>,
) -> Result<(Stream, u32), String> {
    let config = device
        .default_output_config()
        .map_err(|e| format!("No default output config: {e}"))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let stream_config: StreamConfig = config.into();

    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let mut st = state.lock().unwrap();
                let frames = data.len() / channels;
                let ts = info.timestamp();

                for frame in 0..frames {
                    let sample = if st.probe.playing {
                        let idx = st.probe.sample_idx;
                        if idx < probe_samples.len() {
                            let s = probe_samples[idx];
                            if idx == 0 {
                                st.probe.emission_write_pos = st.capture.write_pos;
                                st.timestamps.playback = Some(ts.playback);
                                st.timestamps.output_callback = Some(ts.callback);
                                st.timestamps.frame_offset = frame as u32;
                            }
                            st.probe.sample_idx = idx + 1;
                            s
                        } else {
                            st.probe.playing = false;
                            st.phase = MeasurementPhase::Listening;
                            st.phase_start_sample = st.capture.sample_counter;
                            0.0
                        }
                    } else if st.probe.requested {
                        st.probe.requested = false;
                        st.probe.playing = true;
                        st.probe.sample_idx = 0;
                        st.probe.emission_write_pos = st.capture.write_pos;
                        st.timestamps.playback = Some(ts.playback);
                        st.timestamps.output_callback = Some(ts.callback);
                        st.timestamps.frame_offset = frame as u32;
                        if !probe_samples.is_empty() {
                            let s = probe_samples[0];
                            st.probe.sample_idx = 1;
                            s
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };

                    for ch in 0..channels {
                        data[frame * channels + ch] = sample;
                    }
                }
            },
            move |err| {
                eprintln!("Output stream error: {err}");
            },
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {e}"))?;

    stream.play().map_err(|e| format!("Failed to play output stream: {e}"))?;
    Ok((stream, sample_rate))
}

pub fn start_input_stream(
    device: &Device,
    state: Arc<Mutex<SharedState>>,
) -> Result<(Stream, u32), String> {
    let config = device
        .default_input_config()
        .map_err(|e| format!("No default input config: {e}"))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let stream_config: StreamConfig = config.into();

    let stream = device
        .build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut st = state.lock().unwrap();
                let buf_len = st.capture.buffer.len();

                for frame_samples in data.chunks(channels) {
                    let mono: f32 =
                        frame_samples.iter().sum::<f32>() / channels as f32;
                    let pos = st.capture.write_pos % buf_len;
                    st.capture.buffer[pos] = mono;
                    st.capture.write_pos += 1;
                    st.capture.sample_counter += 1;
                }
            },
            move |err| {
                eprintln!("Input stream error: {err}");
            },
            None,
        )
        .map_err(|e| format!("Failed to build input stream: {e}"))?;

    stream.play().map_err(|e| format!("Failed to play input stream: {e}"))?;
    Ok((stream, sample_rate))
}
