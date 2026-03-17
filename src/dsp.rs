use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type, Q_BUTTERWORTH_F32};

pub const BANDPASS_LOW: f32 = 500.0;
pub const BANDPASS_HIGH: f32 = 3500.0;
pub const ENVELOPE_LPF: f32 = 100.0;
pub const ENVELOPE_RATE: u32 = 1000;

/// Band-pass filter using two cascaded 2nd-order biquad sections (4th order Butterworth)
pub fn bandpass_filter(signal: &[f32], low_hz: f32, high_hz: f32, sample_rate: u32) -> Vec<f32> {
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

/// Extract amplitude envelope: abs(signal) → low-pass at 100Hz → downsample to target_rate
pub fn extract_envelope(signal: &[f32], sample_rate: u32, target_rate: u32) -> Vec<f32> {
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
pub fn normalize(signal: &mut [f32]) {
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
