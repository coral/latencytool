use cpal::traits::DeviceTrait;
use cpal::Stream;
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::audio::{
    self, enumerate_input_devices, enumerate_output_devices, AudioDeviceInfo,
};
use crate::config::Config;
use crate::detection;
use crate::latency::compute_latency_from_timestamps;
use crate::probe::Probe;
use crate::state::{AppMode, MeasurementPhase, SharedState};

pub struct LatencyApp {
    state: Arc<Mutex<SharedState>>,
    output_devices: Vec<AudioDeviceInfo>,
    input_devices: Vec<AudioDeviceInfo>,
    probe: Arc<Probe>,
    _output_stream: Option<Stream>,
    _input_stream: Option<Stream>,
    detection_running: bool,
    resampled_probe: Option<Arc<Vec<f32>>>,
    show_save_dialog: bool,
    save_filename: String,
    save_message: Option<String>,
}

impl LatencyApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let state = Arc::new(Mutex::new(SharedState::new()));
        let output_devices = enumerate_output_devices();
        let input_devices = enumerate_input_devices();
        let probe = Arc::new(Probe::load());

        let config = Config::load();
        if let Some(ref name) = config.output_device
            && let Some(idx) = output_devices.iter().position(|d| &d.name == name)
        {
            state.lock().unwrap().output_device_idx = Some(idx);
        }
        if let Some(ref name) = config.input_device
            && let Some(idx) = input_devices.iter().position(|d| &d.name == name)
        {
            state.lock().unwrap().input_device_idx = Some(idx);
        }

        Self {
            state,
            output_devices,
            input_devices,
            probe,
            _output_stream: None,
            _input_stream: None,
            detection_running: false,
            resampled_probe: None,
            show_save_dialog: false,
            save_filename: String::new(),
            save_message: None,
        }
    }

    fn start_streams(&mut self) {
        if self.state.lock().unwrap().streams_active {
            return;
        }

        let out_idx = {
            let st = self.state.lock().unwrap();
            st.output_device_idx
        };
        let in_idx = {
            let st = self.state.lock().unwrap();
            st.input_device_idx
        };

        let Some(out_idx) = out_idx else {
            self.set_error("Select an output device first");
            return;
        };
        let Some(in_idx) = in_idx else {
            self.set_error("Select an input device first");
            return;
        };

        let out_device = &self.output_devices[out_idx].device;
        let probe_samples = Arc::new(self.probe.resampled(
            out_device
                .default_output_config()
                .map(|c| c.sample_rate().0)
                .unwrap_or(48000),
        ));
        self.resampled_probe = Some(probe_samples.clone());

        match audio::start_output_stream(out_device, probe_samples, self.state.clone()) {
            Ok((stream, rate)) => {
                self._output_stream = Some(stream);
                let mut st = self.state.lock().unwrap();
                st.output_sample_rate = rate;
                st.listen_timeout_samples = (rate as u64) * 5;
                st.inter_probe_gap_samples = rate as u64;
            }
            Err(e) => {
                self.set_error(&format!("Output stream error: {e}"));
                return;
            }
        }

        let in_device = &self.input_devices[in_idx].device;
        match audio::start_input_stream(in_device, self.state.clone()) {
            Ok((stream, rate)) => {
                self._input_stream = Some(stream);
                let mut st = self.state.lock().unwrap();
                st.input_sample_rate = rate;
                let buf_size = (rate as usize) * 10;
                st.capture.buffer = vec![0.0; buf_size];
                st.capture.write_pos = 0;
                st.streams_active = true;
            }
            Err(e) => {
                self.set_error(&format!("Input stream error: {e}"));
                return;
            }
        }

        if !self.detection_running {
            self.start_detection_thread();
        }
    }

    fn stop_streams(&mut self) {
        let mut st = self.state.lock().unwrap();
        st.probe.playing = false;
        st.probe.requested = false;
        st.phase = MeasurementPhase::Idle;
        st.mode = AppMode::Idle;
    }

    fn start_calibration(&mut self) {
        {
            let mut st = self.state.lock().unwrap();
            st.mode = AppMode::Calibrating;
            st.calibration.measurements.clear();
            st.phase = MeasurementPhase::Idle;
            st.error_message = None;
        }
        self.start_streams();
        let mut st = self.state.lock().unwrap();
        if st.streams_active {
            st.phase = MeasurementPhase::Playing;
            st.probe.requested = true;
        }
    }

    fn start_measuring(&mut self) {
        {
            let mut st = self.state.lock().unwrap();
            st.mode = AppMode::Measuring;
            st.reset_measurements();
            st.phase = MeasurementPhase::Idle;
            st.error_message = None;
        }
        self.start_streams();
        let mut st = self.state.lock().unwrap();
        if st.streams_active {
            st.phase = MeasurementPhase::Playing;
            st.probe.requested = true;
        }
    }

    fn stop(&mut self) {
        self.stop_streams();
    }

    fn save_device_config(&self) {
        let st = self.state.lock().unwrap();
        let config = Config {
            output_device: st
                .output_device_idx
                .and_then(|i| self.output_devices.get(i))
                .map(|d| d.name.clone()),
            input_device: st
                .input_device_idx
                .and_then(|i| self.input_devices.get(i))
                .map(|d| d.name.clone()),
        };
        drop(st);
        config.save();
    }

    fn set_error(&self, msg: &str) {
        let mut st = self.state.lock().unwrap();
        st.error_message = Some(msg.to_string());
    }

    fn save_measurements_csv(&self, filename: &str) -> Result<(), String> {
        let st = self.state.lock().unwrap();
        if st.measurement.values.is_empty() {
            return Err("No measurements to save".to_string());
        }

        let path = if filename.ends_with(".csv") {
            filename.to_string()
        } else {
            format!("{}.csv", filename)
        };

        let mut contents = String::from("measurement,latency_ms\n");
        for (i, &val) in st.measurement.values.iter().enumerate() {
            contents.push_str(&format!("{},{:.2}\n", i + 1, val));
        }
        drop(st);

        std::fs::write(&path, contents).map_err(|e| format!("Failed to write {}: {}", path, e))
    }

    fn start_detection_thread(&mut self) {
        self.detection_running = true;
        let state = self.state.clone();
        let probe = self.probe.clone();

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(50));

                let (should_detect, input_rate, output_rate, capture_snapshot, mode, phase,
                     playback_instant, output_callback_instant, playback_frame_offset) = {
                    let st = state.lock().unwrap();
                    if !st.streams_active {
                        break;
                    }
                    let should = st.phase == MeasurementPhase::Listening;
                    let buf = st.capture_snapshot();
                    (
                        should,
                        st.input_sample_rate,
                        st.output_sample_rate,
                        buf,
                        st.mode,
                        st.phase,
                        st.timestamps.playback,
                        st.timestamps.output_callback,
                        st.timestamps.frame_offset,
                    )
                };

                if phase == MeasurementPhase::Idle && mode == AppMode::Idle {
                    continue;
                }

                if phase == MeasurementPhase::Listening {
                    let st = state.lock().unwrap();
                    let elapsed = st.capture.sample_counter.saturating_sub(st.phase_start_sample);
                    if elapsed > st.listen_timeout_samples {
                        drop(st);
                        let mut st = state.lock().unwrap();
                        st.measurement.miss_count += 1;
                        st.phase = MeasurementPhase::Playing;
                        st.probe.requested = true;
                        continue;
                    }
                }

                if !should_detect {
                    continue;
                }

                let template_envelope = probe.envelope(input_rate);
                let template_filtered = probe.filtered(input_rate);

                let result = detection::detect_probe(
                    &capture_snapshot,
                    &template_envelope,
                    &template_filtered,
                    input_rate,
                    5000.0,
                );

                if let Some((lag_samples, ncc_peak)) = result {
                    let ts_result = compute_latency_from_timestamps(
                        lag_samples,
                        input_rate,
                        output_rate,
                        playback_instant,
                        output_callback_instant,
                        playback_frame_offset,
                    );

                    let mut st = state.lock().unwrap();
                    st.detection.ncc_peak = ncc_peak;
                    st.detection.latency_ms = Some(ts_result.latency_ms);
                    st.detection.output_delay_ms = Some(ts_result.output_delay_ms);
                    st.detection.lag_ms = Some(ts_result.lag_ms);
                    st.phase = MeasurementPhase::Detected;

                    match st.mode {
                        AppMode::Calibrating => {
                            st.calibration.measurements.push(ts_result.latency_ms);
                            let done = st.calibration.measurements.len() >= st.calibration.count;
                            if done {
                                let mut cal = st.calibration.measurements.clone();
                                cal.sort_by(|a, b| a.partial_cmp(b).unwrap());
                                st.calibration.system_offset_ms = cal[cal.len() / 2];
                                st.mode = AppMode::Idle;
                                st.phase = MeasurementPhase::Idle;
                            } else {
                                st.phase = MeasurementPhase::Playing;
                                st.probe.requested = true;
                            }
                        }
                        AppMode::Measuring => {
                            let adjusted = (ts_result.latency_ms - st.calibration.system_offset_ms).max(0.0);
                            st.measurement.values.push_back(adjusted);
                            st.measurement.count += 1;
                            while st.measurement.values.len() > 1000 {
                                st.measurement.values.pop_front();
                            }
                            st.phase = MeasurementPhase::Playing;
                            st.probe.requested = true;
                        }
                        AppMode::Idle => {}
                    }
                }
            }
        });
    }

    fn render_device_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Output Device:");
            let current_out = self.state.lock().unwrap().output_device_idx;
            let mut selected = current_out.unwrap_or(0);
            let names: Vec<String> =
                self.output_devices.iter().map(|d| d.name.clone()).collect();
            egui::ComboBox::from_id_salt("output_device")
                .selected_text(
                    names
                        .get(selected)
                        .cloned()
                        .unwrap_or_else(|| "None".to_string()),
                )
                .show_ui(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        if ui.selectable_value(&mut selected, i, name).changed() {
                            self.state.lock().unwrap().output_device_idx = Some(selected);
                            self.save_device_config();
                        }
                    }
                });
            if current_out.is_none() && !self.output_devices.is_empty() {
                self.state.lock().unwrap().output_device_idx = Some(0);
                self.save_device_config();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Input Device: ");
            let current_in = self.state.lock().unwrap().input_device_idx;
            let mut selected = current_in.unwrap_or(0);
            let names: Vec<String> =
                self.input_devices.iter().map(|d| d.name.clone()).collect();
            egui::ComboBox::from_id_salt("input_device")
                .selected_text(
                    names
                        .get(selected)
                        .cloned()
                        .unwrap_or_else(|| "None".to_string()),
                )
                .show_ui(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        if ui.selectable_value(&mut selected, i, name).changed() {
                            self.state.lock().unwrap().input_device_idx = Some(selected);
                            self.save_device_config();
                        }
                    }
                });
            if current_in.is_none() && !self.input_devices.is_empty() {
                self.state.lock().unwrap().input_device_idx = Some(0);
                self.save_device_config();
            }
        });
    }

    fn render_controls(&mut self, ui: &mut egui::Ui) {
        let mode = {
            let st = self.state.lock().unwrap();
            st.mode
        };

        ui.horizontal(|ui| {
            let is_running = mode != AppMode::Idle;
            if !is_running {
                if ui.button("Calibrate").clicked() {
                    self.start_calibration();
                }
                if ui.button("Measure").clicked() {
                    self.start_measuring();
                }
            } else if ui.button("Stop").clicked() {
                self.stop();
            }
        });
    }

    fn render_status(&self, ui: &mut egui::Ui) {
        let st = self.state.lock().unwrap();

        ui.horizontal(|ui| {
            ui.label("Mode:");
            ui.strong(match st.mode {
                AppMode::Idle => "Idle",
                AppMode::Calibrating => "Calibrating",
                AppMode::Measuring => "Measuring",
            });
        });

        ui.horizontal(|ui| {
            ui.label("Phase:");
            ui.strong(match st.phase {
                MeasurementPhase::Idle => "Idle",
                MeasurementPhase::Playing => "Playing probe...",
                MeasurementPhase::Listening => "Listening...",
                MeasurementPhase::Detected => "Detected!",
            });
        });

        if st.calibration.system_offset_ms > 0.0 {
            ui.horizontal(|ui| {
                ui.label("System offset (calibration):");
                ui.strong(format!("{:.1} ms", st.calibration.system_offset_ms));
            });
        }

        if st.mode == AppMode::Calibrating {
            ui.horizontal(|ui| {
                ui.label("Calibration progress:");
                ui.strong(format!(
                    "{}/{}",
                    st.calibration.measurements.len(),
                    st.calibration.count
                ));
            });
        }

        // Timing breakdown
        if st.detection.latency_ms.is_some() || st.detection.lag_ms.is_some() {
            ui.separator();
            ui.heading("Timing");
            egui::Grid::new("timing_grid")
                .num_columns(2)
                .spacing([40.0, 4.0])
                .show(ui, |ui| {
                    if let Some(lag) = st.detection.lag_ms {
                        ui.label("Detection lag:");
                        ui.label(format!("{:.1} ms", lag));
                        ui.end_row();
                    }
                    if let Some(delay) = st.detection.output_delay_ms {
                        ui.label("Playout delay:");
                        ui.label(format!("{:.1} ms", delay));
                        ui.end_row();
                    }
                    if let Some(latency) = st.detection.latency_ms {
                        ui.label("Round-trip latency:");
                        ui.strong(format!("{:.1} ms", latency));
                        ui.end_row();
                    }
                    ui.label("NCC confidence:");
                    ui.label(format!("{:.3}", st.detection.ncc_peak));
                    ui.end_row();
                    ui.label("Output sample rate:");
                    ui.label(format!("{} Hz", st.output_sample_rate));
                    ui.end_row();
                    ui.label("Input sample rate:");
                    ui.label(format!("{} Hz", st.input_sample_rate));
                    ui.end_row();
                });
        }

        ui.separator();

        // Statistics
        if let Some(stats) = st.stats() {
            ui.heading("Measurement Statistics");
            egui::Grid::new("stats_grid")
                .num_columns(2)
                .spacing([40.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Min:");
                    ui.label(format!("{:.1} ms", stats.min));
                    ui.end_row();
                    ui.label("Avg:");
                    ui.label(format!("{:.1} ms", stats.avg));
                    ui.end_row();
                    ui.label("P50:");
                    ui.label(format!("{:.1} ms", stats.p50));
                    ui.end_row();
                    ui.label("P95:");
                    ui.label(format!("{:.1} ms", stats.p95));
                    ui.end_row();
                    ui.label("Max:");
                    ui.label(format!("{:.1} ms", stats.max));
                    ui.end_row();
                    ui.label("Count:");
                    ui.label(format!("{}", stats.count));
                    ui.end_row();
                    ui.label("Misses:");
                    ui.label(format!("{}", stats.misses));
                    ui.end_row();
                });
        }

        // Error display
        if let Some(ref err) = st.error_message {
            ui.separator();
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn render_save_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_save_dialog {
            return;
        }
        let mut open = true;
        egui::Window::new("Save Results")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Filename:");
                    ui.text_edit_singleline(&mut self.save_filename);
                    ui.label(".csv");
                });
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && !self.save_filename.is_empty() {
                        match self.save_measurements_csv(&self.save_filename) {
                            Ok(()) => {
                                let path = if self.save_filename.ends_with(".csv") {
                                    self.save_filename.clone()
                                } else {
                                    format!("{}.csv", self.save_filename)
                                };
                                self.save_message = Some(format!("Saved to {}", path));
                                self.show_save_dialog = false;
                            }
                            Err(e) => {
                                self.save_message = Some(e);
                                self.show_save_dialog = false;
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_save_dialog = false;
                    }
                });
            });
        if !open {
            self.show_save_dialog = false;
        }
    }
}

impl eframe::App for LatencyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(100));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Audio Latency Measurement Tool");
            ui.separator();

            self.render_device_selector(ui);
            ui.separator();

            self.render_controls(ui);
            ui.separator();

            self.render_status(ui);

            // Save message
            if let Some(ref msg) = self.save_message {
                ui.separator();
                ui.label(msg.as_str());
            }

            // Save button — bottom right
            let has_measurements = !self.state.lock().unwrap().measurement.values.is_empty();

            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::BOTTOM), |ui| {
                if ui.add_enabled(has_measurements, egui::Button::new("Save")).clicked() {
                    self.show_save_dialog = true;
                    self.save_filename.clear();
                    self.save_message = None;
                }
            });
        });

        self.render_save_dialog(ctx);
    }
}
