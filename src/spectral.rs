use anyhow::{Result, ensure};
use realfft::{RealFftPlanner, num_complex::Complex};

pub const FFT: usize = 2048;
pub const HOP: usize = 441;
pub const SAMPLES: usize = 352800;
pub const FRAMES: usize = 801;
pub const BINS: usize = 1025;

pub fn reflect_index(index: isize, length: usize) -> usize {
    if length < 2 {
        return 0;
    }
    let period = (length as isize - 1) * 2;
    let folded = index.rem_euclid(period);
    if folded < length as isize {
        folded as usize
    } else {
        (period - folded) as usize
    }
}

pub fn spectrum(left: &[f32], right: &[f32]) -> Result<Vec<f32>> {
    ensure!(
        left.len() == SAMPLES && right.len() == SAMPLES,
        "RoFormer区間の長さが不正です。"
    );
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT);
    let mut time = fft.make_input_vec();
    let mut frequency = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut result = vec![0f32; BINS * 2 * FRAMES * 2];
    for (channel, audio) in [left, right].iter().enumerate() {
        for frame in 0..FRAMES {
            for (i, v) in time.iter_mut().enumerate() {
                let position = frame as isize * HOP as isize + i as isize - FFT as isize / 2;
                let w = 0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / FFT as f64).cos());
                *v = audio[reflect_index(position, SAMPLES)] as f64 * w;
            }
            fft.process_with_scratch(&mut time, &mut frequency, &mut scratch)?;
            for (bin, z) in frequency.iter().enumerate() {
                let offset = ((bin * 2 + channel) * FRAMES + frame) * 2;
                result[offset] = z.re as f32;
                result[offset + 1] = z.im as f32;
            }
        }
    }
    Ok(result)
}

pub fn synthesize(spec: &[f32], mask: &[f32], zero_dc: bool) -> Result<(Vec<f32>, Vec<f32>)> {
    ensure!(
        spec.len() == BINS * 2 * FRAMES * 2 && mask.len() == spec.len(),
        "RoFormerのマスク形状が不正です。"
    );
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_inverse(FFT);
    let mut time = fft.make_output_vec();
    let mut frequency = fft.make_input_vec();
    let mut scratch = fft.make_scratch_vec();
    let window: Vec<f64> = (0..FFT)
        .map(|i| 0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / FFT as f64).cos()))
        .collect();
    let length = FFT + HOP * (FRAMES - 1);
    let mut output = Vec::new();
    for channel in 0..2 {
        let mut wave = vec![0.0f64; length];
        let mut weight = vec![0.0f64; length];
        for frame in 0..FRAMES {
            for (bin, z) in frequency.iter_mut().enumerate() {
                let offset = ((bin * 2 + channel) * FRAMES + frame) * 2;
                let a = spec[offset] as f64;
                let b = spec[offset + 1] as f64;
                let c = mask[offset] as f64;
                let d = mask[offset + 1] as f64;
                *z = Complex::new(a * c - b * d, a * d + b * c);
                if bin == 0 || bin == BINS - 1 {
                    z.im = 0.0;
                }
                if zero_dc && bin == 0 {
                    *z = Complex::new(0.0, 0.0);
                }
            }
            fft.process_with_scratch(&mut frequency, &mut time, &mut scratch)?;
            for i in 0..FFT {
                let offset = frame * HOP + i;
                wave[offset] += time[i] / FFT as f64 * window[i];
                weight[offset] += window[i] * window[i];
            }
        }
        let mut signal = Vec::with_capacity(SAMPLES);
        for i in FFT / 2..FFT / 2 + SAMPLES {
            ensure!(weight[i] > 0.0, "逆変換の重なりが不足しています。");
            let value = wave[i] / weight[i];
            ensure!(value.is_finite(), "分離結果が不正です。");
            signal.push(value as f32);
        }
        output.push(signal);
    }
    Ok((output.remove(0), output.remove(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_mask_reconstructs_both_channels_and_high_frequencies() {
        let left: Vec<f32> = (0..SAMPLES)
            .map(|i| (i as f32 * std::f32::consts::TAU * 18000.0 / 44100.0).sin() * 0.2)
            .collect();
        let right: Vec<f32> = (0..SAMPLES)
            .map(|i| (i as f32 * std::f32::consts::TAU * 80.0 / 44100.0).sin() * 0.4)
            .collect();
        let spec = spectrum(&left, &right).unwrap();
        let mut mask = vec![0f32; spec.len()];
        for i in (0..mask.len()).step_by(2) {
            mask[i] = 1.0;
        }
        let (a, b) = synthesize(&spec, &mask, false).unwrap();
        let error = a
            .iter()
            .zip(&left)
            .chain(b.iter().zip(&right))
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(error < 1e-6, "FFT roundtrip error {error}");
        assert_eq!(reflect_index(-1, 5), 1);
        assert_eq!(reflect_index(5, 5), 3);
    }
}
