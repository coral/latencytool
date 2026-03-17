use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type, Q_BUTTERWORTH_F32};
use log::{debug, info};
use rustfft::{num_complex::Complex, FftPlanner};

const ENVELOPE_RATE: u32 = 1000;
const BANDPASS_LOW: f32 = 500.0;
const BANDPASS_HIGH: f32 = 3500.0;
const ENVELOPE_LPF: f32 = 100.0;
const NUM_COARSE_CANDIDATES: usize = 5;
const COARSE_CANDIDATE_MIN_SEPARATION: usize = 200; // 200ms at 1000Hz envelope rate
const TRANSIENT_SKIP: usize = 10; // 10 envelope samples = 10ms at 1kHz
const FINE_MARGIN_MS: f64 = 10.0; // search ±10ms around coarse lag
const FINE_NCC_THRESHOLD: f32 = 0.92; // real probe at full rate should easily exceed this

/// Two-stage probe detection: coarse envelope NCC + fine full-rate NCC.
///
/// Stage 1: Bandpass-filter, extract envelope at 1kHz, cross-correlate for coarse lag.
/// Stage 2: Refine with full-rate (48kHz) NCC in a narrow window for sub-sample accuracy.
pub fn detect_probe(
    capture: &[f32],
    template_envelope: &[f32],
    template_filtered: &[f32],
    sample_rate: u32,
    max_latency_ms: f64,
) -> Option<(f64, f32)> {
    // Extract envelope from capture
    let filtered_capture = bandpass_filter(capture, BANDPASS_LOW, BANDPASS_HIGH, sample_rate);
    let env_capture = extract_envelope(&filtered_capture, sample_rate, ENVELOPE_RATE);
    let env_template = template_envelope.to_vec();

    if env_capture.len() < env_template.len() + TRANSIENT_SKIP || env_template.len() < 2 {
        debug!("Envelope too short: capture={} template={}", env_capture.len(), env_template.len());
        return None;
    }

    // Skip filter startup transients from capture envelope
    let env_capture_trimmed = &env_capture[TRANSIENT_SKIP..];

    // Max lag in envelope samples
    let max_lag_envelope = ((max_latency_ms / 1000.0) * ENVELOPE_RATE as f64) as usize;
    let max_lag_envelope = max_lag_envelope.min(env_capture_trimmed.len() - env_template.len());
    if max_lag_envelope == 0 {
        return None;
    }

    // Truncate capture envelope to fixed window for stable normalization
    let norm_len = (max_lag_envelope + env_template.len()).min(env_capture_trimmed.len());
    let mut env_capture_norm = env_capture_trimmed[..norm_len].to_vec();
    let mut env_template_norm = env_template.clone();
    normalize(&mut env_capture_norm);
    normalize(&mut env_template_norm);

    // Compute envelope NCC and find best peak
    let ncc_values = envelope_ncc_values(
        &env_capture_norm,
        &env_template_norm,
        max_lag_envelope,
    );

    let candidates = find_top_peaks(&ncc_values, NUM_COARSE_CANDIDATES, COARSE_CANDIDATE_MIN_SEPARATION);

    if candidates.is_empty() {
        debug!("No envelope candidates found");
        return None;
    }

    if candidates[0].1 < 0.4 {
        debug!("Best envelope NCC {:.3} below threshold 0.4", candidates[0].1);
        return None;
    }

    info!(
        "Coarse candidates: {}",
        candidates
            .iter()
            .map(|(lag, ncc)| format!("{}ms={:.3}", lag + TRANSIENT_SKIP, ncc))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // --- Fine stage: try all coarse candidates, pick best fine NCC ---
    let samples_per_env = sample_rate as f64 / ENVELOPE_RATE as f64;
    let margin_samples = (FINE_MARGIN_MS / 1000.0 * sample_rate as f64) as usize;

    let mut best_fine: Option<(f64, f32)> = None;
    let mut best_fine_coarse_lag = 0usize;

    for &(env_lag, env_ncc) in &candidates {
        if env_ncc < 0.3 {
            break; // remaining candidates are worse
        }
        let coarse_lag_samples = ((env_lag + TRANSIENT_SKIP) as f64 * samples_per_env) as usize;
        if let Some((fine_lag, fine_ncc)) = fine_stage_ncc(
            &filtered_capture,
            template_filtered,
            coarse_lag_samples,
            margin_samples,
        ) {
            info!(
                "  candidate {}ms: fine_ncc={:.4} fine_lag={:.1}ms",
                env_lag + TRANSIENT_SKIP,
                fine_ncc,
                fine_lag / sample_rate as f64 * 1000.0,
            );
            if best_fine.is_none() || fine_ncc > best_fine.unwrap().1 {
                best_fine = Some((fine_lag, fine_ncc));
                best_fine_coarse_lag = env_lag + TRANSIENT_SKIP;
            }
        }
    }

    if let Some((fine_lag, fine_ncc)) = best_fine {
        if fine_ncc < FINE_NCC_THRESHOLD {
            debug!(
                "Best fine NCC {:.3} below threshold {:.2}, rejecting",
                fine_ncc, FINE_NCC_THRESHOLD
            );
            return None;
        }
        let lag_ms = fine_lag / sample_rate as f64 * 1000.0;
        info!(
            "Detection: lag={:.2}ms fine_ncc={:.4} (from coarse {}ms)",
            lag_ms, fine_ncc, best_fine_coarse_lag
        );
        Some((fine_lag, fine_ncc))
    } else {
        debug!("Fine stage failed on all candidates");
        None
    }
}

/// Fine-stage NCC: full-rate cross-correlation in a narrow window around the coarse lag.
/// Returns (sub-sample lag, ncc_peak) with parabolic interpolation for sub-sample accuracy.
fn fine_stage_ncc(
    capture_filtered: &[f32],
    template_filtered: &[f32],
    coarse_lag_samples: usize,
    margin_samples: usize,
) -> Option<(f64, f32)> {
    let tlen = template_filtered.len();
    let start_lag = coarse_lag_samples.saturating_sub(margin_samples);
    let end_lag = (coarse_lag_samples + margin_samples)
        .min(capture_filtered.len().saturating_sub(tlen));

    if start_lag >= end_lag || tlen == 0 {
        return None;
    }

    // Template energy (constant)
    let template_energy: f64 = template_filtered.iter().map(|&x| (x as f64) * (x as f64)).sum();
    if template_energy < 1e-20 {
        return None;
    }

    // Direct time-domain NCC over the narrow window
    let num_lags = end_lag - start_lag + 1;
    let mut ncc_values = vec![0.0f64; num_lags];

    for (i, lag) in (start_lag..=end_lag).enumerate() {
        let window = &capture_filtered[lag..lag + tlen];
        let dot: f64 = window.iter().zip(template_filtered.iter())
            .map(|(&c, &t)| c as f64 * t as f64)
            .sum();
        let window_energy: f64 = window.iter().map(|&x| (x as f64) * (x as f64)).sum();
        let denom = (template_energy * window_energy).sqrt();
        if denom > 1e-20 {
            ncc_values[i] = dot / denom;
        }
    }

    // Find peak
    let mut best_idx = 0;
    let mut best_ncc = ncc_values[0];
    for (i, &v) in ncc_values.iter().enumerate() {
        if v > best_ncc {
            best_ncc = v;
            best_idx = i;
        }
    }

    if best_ncc < 0.3 {
        return None;
    }

    // Parabolic interpolation for sub-sample accuracy
    let fractional_offset = if best_idx > 0 && best_idx < num_lags - 1 {
        let y_minus = ncc_values[best_idx - 1];
        let y_0 = ncc_values[best_idx];
        let y_plus = ncc_values[best_idx + 1];
        let denom = 2.0 * (2.0 * y_0 - y_minus - y_plus);
        if denom.abs() > 1e-12 {
            (y_minus - y_plus) / denom
        } else {
            0.0
        }
    } else {
        0.0
    };

    let precise_lag = (start_lag + best_idx) as f64 + fractional_offset;
    Some((precise_lag, best_ncc as f32))
}

/// Band-pass filter using two cascaded 2nd-order biquad sections (4th order Butterworth)
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

/// Extract amplitude envelope: abs(signal) → low-pass at 30Hz → downsample to target_rate
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

/// Normalize in-place: subtract mean, divide by std dev
fn normalize(signal: &mut [f32]) {
    if signal.is_empty() {
        return;
    }
    let n = signal.len() as f32;
    let mean = signal.iter().sum::<f32>() / n;
    for x in signal.iter_mut() {
        *x -= mean;
    }
    let variance = signal.iter().map(|x| x * x).sum::<f32>() / n;
    let std_dev = variance.sqrt();
    if std_dev > 1e-10 {
        for x in signal.iter_mut() {
            *x /= std_dev;
        }
    }
}

/// FFT-based NCC on envelopes. Returns the full NCC values array.
fn envelope_ncc_values(
    capture_env: &[f32],
    template_env: &[f32],
    max_lag: usize,
) -> Vec<f32> {
    let n = (capture_env.len() + template_env.len() - 1).next_power_of_two();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);

    let mut cap_fft: Vec<Complex<f32>> = capture_env
        .iter()
        .map(|&x| Complex::new(x, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)).take(n - capture_env.len()))
        .collect();
    fft.process(&mut cap_fft);

    let mut tmpl_fft: Vec<Complex<f32>> = template_env
        .iter()
        .map(|&x| Complex::new(x, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)).take(n - template_env.len()))
        .collect();
    fft.process(&mut tmpl_fft);

    let mut product: Vec<Complex<f32>> = cap_fft
        .iter()
        .zip(tmpl_fft.iter())
        .map(|(a, b)| a * b.conj())
        .collect();
    ifft.process(&mut product);

    let scale = 1.0 / n as f32;
    let template_energy: f32 = template_env.iter().map(|x| x * x).sum();
    if template_energy < 1e-10 {
        return vec![0.0; max_lag + 1];
    }
    let template_rms = template_energy.sqrt();

    let tlen = template_env.len();
    let mut running_energy = vec![0.0f32; max_lag + 1];
    let mut energy: f32 = capture_env[..tlen].iter().map(|x| x * x).sum();
    running_energy[0] = energy;
    for lag in 1..=max_lag {
        if lag + tlen - 1 < capture_env.len() {
            energy += capture_env[lag + tlen - 1] * capture_env[lag + tlen - 1];
            energy -= capture_env[lag - 1] * capture_env[lag - 1];
            running_energy[lag] = energy;
        }
    }

    let mut ncc_values = vec![0.0f32; max_lag + 1];
    for lag in 0..=max_lag {
        let local_rms = running_energy[lag].sqrt();
        if local_rms < 1e-6 {
            continue;
        }
        ncc_values[lag] = (product[lag].re * scale) / (template_rms * local_rms);
    }

    ncc_values
}

/// Find top N peaks from NCC values with minimum separation.
fn find_top_peaks(ncc_values: &[f32], top_n: usize, min_separation: usize) -> Vec<(usize, f32)> {
    let max_lag = ncc_values.len().saturating_sub(1);
    let mut peaks = Vec::with_capacity(top_n);
    let mut used = vec![false; ncc_values.len()];

    for _ in 0..top_n {
        let mut best_lag = 0;
        let mut best_ncc: f32 = -1.0;

        for lag in 0..=max_lag {
            if used[lag] {
                continue;
            }
            if ncc_values[lag] > best_ncc {
                best_ncc = ncc_values[lag];
                best_lag = lag;
            }
        }

        if best_ncc <= 0.0 {
            break;
        }

        peaks.push((best_lag, best_ncc));

        let start = best_lag.saturating_sub(min_separation);
        let end = (best_lag + min_separation).min(max_lag);
        for i in start..=end {
            used[i] = true;
        }
    }

    peaks
}
