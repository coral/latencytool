# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build                              # dev build
cargo build --release                    # release build
cargo run                                # launch GUI
RUST_LOG=info cargo run -- --calibrate   # CLI calibration mode (10 measurements)
```

No tests exist yet. Rust edition is 2024.

## Architecture

Audio latency measurement tool: plays a probe signal, captures it back, and computes round-trip latency using cross-correlation and CoreAudio hardware timestamps.

### Signal Flow

1. **Probe playback** (`probe.rs`, `audio.rs`): Embedded WAV (`assets/probe3.wav`) is resampled to output device rate, played through CPAL output stream. Output callback records hardware timestamps (`playback_instant`, `output_callback_instant`, `playback_frame_offset`).

2. **Capture** (`audio.rs`): CPAL input stream writes to circular buffer in `SharedState`. Multi-channel input averaged to mono.

3. **Detection** (`detection.rs`): Two-stage detection:
   - **Coarse**: Bandpass filter (500-3500Hz) → envelope extraction at 1kHz → FFT-based NCC → top N candidates
   - **Fine**: Full-rate (48kHz) time-domain NCC in ±10ms window around each coarse candidate → pick highest fine NCC → parabolic interpolation for sub-sample accuracy
   - Fine NCC threshold is 0.92; below that the detection is rejected as a miss

4. **Latency computation** (`app.rs`): `latency = lag_samples/input_rate - output_playout_delay`

### Modules

- `main.rs` — Entry point, CLI calibration loop (`mod cli`), GUI launch
- `app.rs` — egui GUI (`LatencyApp`), detection thread, `compute_latency_from_timestamps()`
- `audio.rs` — CPAL device enumeration, output/input stream setup
- `probe.rs` — Probe loading, resampling, caches bandpass-filtered signal and envelope per sample rate
- `detection.rs` — `detect_probe()` (coarse+fine), bandpass filter, envelope extraction, NCC
- `state.rs` — `SharedState` (shared between audio callbacks, detection thread, and UI), `AppMode`, `MeasurementPhase`
- `config.rs` — Device selection persistence (JSON in user config dir)

### Key Design Points

- `SharedState` is `Arc<Mutex<...>>` shared across audio callbacks, detection thread, and GUI
- `CachedProbe` stores both `filtered` (bandpass at full rate) and `envelope` (1kHz), keyed by sample rate
- Detection thread polls every 50ms, extracts capture snapshot from `emission_write_pos` to `capture_write_pos`
- GUI and CLI use the same `detect_probe()` and `compute_latency_from_timestamps()` functions
