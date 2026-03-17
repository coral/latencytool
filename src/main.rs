mod app;
mod audio;
mod config;
mod detection;
mod probe;
mod state;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--calibrate" || a == "-calibrate") {
        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("info"),
        )
        .format_timestamp_millis()
        .init();
        cli::run_calibration();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([500.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Audio Latency Tool",
        options,
        Box::new(|cc| Ok(Box::new(app::LatencyApp::new(cc)))),
    )
}

mod cli {
    use cpal::traits::DeviceTrait;
    use crate::app::compute_latency_from_timestamps;
    use crate::audio;
    use crate::config::Config;
    use crate::detection;
    use crate::probe::Probe;
    use crate::state::{MeasurementPhase, SharedState};
    use log::info;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    pub fn run_calibration() {
        let probe = Arc::new(Probe::load());
        info!("Probe loaded: {} samples at {}Hz ({:.1}ms)",
            probe.samples.len(), probe.sample_rate,
            probe.samples.len() as f64 / probe.sample_rate as f64 * 1000.0);

        let state = Arc::new(Mutex::new(SharedState::new()));

        let config = Config::load();
        let output_devices = audio::enumerate_output_devices();
        let input_devices = audio::enumerate_input_devices();

        let out_idx = config
            .output_device
            .as_ref()
            .and_then(|name| output_devices.iter().position(|d| &d.name == name))
            .or_else(|| if output_devices.is_empty() { None } else { Some(0) });

        let in_idx = config
            .input_device
            .as_ref()
            .and_then(|name| input_devices.iter().position(|d| &d.name == name))
            .or_else(|| if input_devices.is_empty() { None } else { Some(0) });

        let Some(out_idx) = out_idx else {
            eprintln!("No output device found");
            return;
        };
        let Some(in_idx) = in_idx else {
            eprintln!("No input device found");
            return;
        };

        let out_device = &output_devices[out_idx];
        let in_device = &input_devices[in_idx];
        info!("Output: {}", out_device.name);
        info!("Input: {}", in_device.name);

        let out_rate = out_device
            .device
            .default_output_config()
            .map(|c: cpal::SupportedStreamConfig| c.sample_rate().0)
            .unwrap_or(48000);
        let probe_samples = Arc::new(probe.resampled(out_rate));

        let (_out_stream, out_sr) =
            audio::start_output_stream(&out_device.device, probe_samples, state.clone())
                .expect("Failed to start output stream");
        let (_in_stream, in_sr) =
            audio::start_input_stream(&in_device.device, state.clone())
                .expect("Failed to start input stream");

        info!("Output rate: {}Hz, Input rate: {}Hz", out_sr, in_sr);

        {
            let mut st = state.lock().unwrap();
            st.output_sample_rate = out_sr;
            st.input_sample_rate = in_sr;
            let buf_size = (in_sr as usize) * 10;
            st.capture_buffer = vec![0.0; buf_size];
            st.capture_write_pos = 0;
            st.streams_active = true;
            st.listen_timeout_samples = (in_sr as u64) * 5;
            st.inter_probe_gap_samples = in_sr as u64;
        }

        let template_envelope = probe.envelope(in_sr);
        let template_filtered = probe.filtered(in_sr);

        let calibration_count = 10;
        let mut measurements = Vec::new();
        let mut miss_count = 0u32;
        let mut output_delay_ms = 0.0;

        println!("Starting calibration ({calibration_count} measurements)...");

        for i in 0..calibration_count {
            {
                let mut st = state.lock().unwrap();
                st.phase = MeasurementPhase::Playing;
                st.probe_play_requested = true;
            }

            loop {
                thread::sleep(Duration::from_millis(10));
                let st = state.lock().unwrap();
                if st.phase == MeasurementPhase::Listening {
                    break;
                }
            }

            let mut detected = false;
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(5);

            while start.elapsed() < timeout {
                thread::sleep(Duration::from_millis(50));

                let (capture_snapshot, playback_instant, output_callback_instant,
                     playback_frame_offset) = {
                    let st = state.lock().unwrap();
                    let buf_len = st.capture_buffer.len();
                    let emission_pos = st.emission_write_pos % buf_len;
                    let write_pos = st.capture_write_pos % buf_len;
                    let buf = if write_pos > emission_pos {
                        st.capture_buffer[emission_pos..write_pos].to_vec()
                    } else if st.capture_write_pos > st.emission_write_pos {
                        let mut v = st.capture_buffer[emission_pos..].to_vec();
                        v.extend_from_slice(&st.capture_buffer[..write_pos]);
                        v
                    } else {
                        vec![]
                    };
                    (buf, st.playback_instant, st.output_callback_instant,
                     st.playback_frame_offset)
                };

                if capture_snapshot.is_empty() {
                    continue;
                }

                let result = detection::detect_probe(
                    &capture_snapshot,
                    &template_envelope,
                    &template_filtered,
                    in_sr,
                    5000.0,
                );

                if let Some((lag_samples, ncc_peak)) = result {
                    let ts_result = compute_latency_from_timestamps(
                        lag_samples,
                        in_sr,
                        out_sr,
                        playback_instant,
                        output_callback_instant,
                        playback_frame_offset,
                    );
                    output_delay_ms = ts_result.output_delay_ms;
                    println!(
                        "  [{}/{}] latency={:.1}ms (lag={:.1}ms - playout={:.1}ms) ncc={:.3}",
                        i + 1, calibration_count,
                        ts_result.latency_ms, ts_result.lag_ms, ts_result.output_delay_ms,
                        ncc_peak
                    );
                    measurements.push(ts_result.latency_ms);
                    detected = true;
                    break;
                }
            }

            if !detected {
                println!("  [{}/{}] TIMEOUT (no detection)", i + 1, calibration_count);
                miss_count += 1;
            }

            thread::sleep(Duration::from_secs(1));
        }

        println!("\n--- Calibration Results ---");
        println!("Detections: {}/{}", measurements.len(), calibration_count);
        println!("Misses: {}", miss_count);
        println!("Playout delay: {:.1}ms", output_delay_ms);

        if !measurements.is_empty() {
            measurements.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let min = measurements[0];
            let max = measurements[measurements.len() - 1];
            let median = measurements[measurements.len() / 2];
            let avg = measurements.iter().sum::<f64>() / measurements.len() as f64;
            let variance = measurements.iter().map(|x| (x - avg).powi(2)).sum::<f64>()
                / measurements.len() as f64;
            let stddev = variance.sqrt();

            println!("Min:    {:.1}ms", min);
            println!("Median: {:.1}ms", median);
            println!("Avg:    {:.1}ms", avg);
            println!("Max:    {:.1}ms", max);
            println!("StdDev: {:.2}ms", stddev);
            println!("Range:  {:.1}ms", max - min);
            println!("All: {:?}", measurements.iter().map(|x| format!("{:.1}", x)).collect::<Vec<_>>());
        }
    }
}
