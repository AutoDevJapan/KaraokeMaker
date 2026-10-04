use anyhow::{Result, ensure};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub struct AudioMetrics {
    pub sample_rate: u32,
    pub bits: u16,
    pub duration: f64,
    pub rms_dbfs: f64,
    pub sample_peak_dbfs: f64,
    pub low_20_150_db: f64,
    pub mid_2000_8000_db: f64,
    pub high_8000_20000_db: f64,
}

pub fn measure(path: &Path) -> Result<AudioMetrics> {
    use realfft::RealFftPlanner;
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    ensure!(spec.channels == 2, "ステレオWAVを指定してください。");
    let values: Vec<f64> = if spec.sample_format == hound::SampleFormat::Float {
        reader
            .samples::<f32>()
            .map(|v| v.map(|x| x as f64))
            .collect::<Result<_, _>>()?
    } else {
        reader
            .samples::<i32>()
            .map(|v| v.map(|x| x as f64 / 2f64.powi(spec.bits_per_sample as i32 - 1)))
            .collect::<Result<_, _>>()?
    };
    ensure!(!values.is_empty(), "音源が空です。");
    let samples = values.len() / 2;
    let rms = (values.iter().map(|v| v * v).sum::<f64>() / values.len() as f64).sqrt();
    let peak = values.iter().map(|v| v.abs()).fold(0.0f64, f64::max);
    const FFT: usize = 4096;
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT);
    let mut time = fft.make_input_vec();
    let mut frequency = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut power = vec![0.0f64; FFT / 2 + 1];
    let mut count = 0usize;
    for channel in 0..2 {
        for start in (0..samples.saturating_sub(FFT)).step_by(2048) {
            for i in 0..FFT {
                time[i] = values[(start + i) * 2 + channel]
                    * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FFT as f64).cos());
            }
            fft.process_with_scratch(&mut time, &mut frequency, &mut scratch)?;
            for (p, z) in power.iter_mut().zip(&frequency) {
                *p += z.norm_sqr();
            }
            count += 1;
        }
    }
    let band = |min: f64, max: f64| {
        let a = (min / spec.sample_rate as f64 * FFT as f64).ceil() as usize;
        let b = (max / spec.sample_rate as f64 * FFT as f64).floor() as usize;
        let sum = power[a.min(power.len() - 1)..=b.min(power.len() - 1)]
            .iter()
            .sum::<f64>()
            / (count.max(1) as f64 * FFT as f64 * FFT as f64);
        10.0 * sum.max(1e-30).log10()
    };
    Ok(AudioMetrics {
        sample_rate: spec.sample_rate,
        bits: spec.bits_per_sample,
        duration: samples as f64 / spec.sample_rate as f64,
        rms_dbfs: 20.0 * rms.max(1e-15).log10(),
        sample_peak_dbfs: 20.0 * peak.max(1e-15).log10(),
        low_20_150_db: band(20.0, 150.0),
        mid_2000_8000_db: band(2000.0, 8000.0),
        high_8000_20000_db: band(8000.0, 20000.0),
    })
}
