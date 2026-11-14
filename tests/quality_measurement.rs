use karaoke_maker::{media, quality};
use std::{f64::consts::TAU, fs, path::Path};

const RATE: u32 = 48_000;
const FFT: usize = 4096;

fn write_tone(path: &Path, frames: usize, leading_silence: usize) -> Vec<f64> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    let signal: Vec<f64> = (0..frames)
        .map(|i| {
            if i < leading_silence {
                0.0
            } else {
                (0.25 * (TAU * 8.0 * i as f64 / FFT as f64).sin()) as f32 as f64
            }
        })
        .collect();
    for &value in &signal {
        writer.write_sample(value as f32).unwrap();
        writer.write_sample(value as f32).unwrap();
    }
    writer.finalize().unwrap();
    signal
}

#[test]
fn exact_fft_length_is_measured_instead_of_reported_as_silent() {
    let temp = tempfile::tempdir().unwrap();
    // One sample beyond the boundary is a positive control for the original path.
    for frames in [FFT + 1, FFT] {
        let source = temp.path().join(format!("tone-{frames}.wav"));
        write_tone(&source, frames, 0);
        let before = fs::read(&source).unwrap();
        let result = media::hidden_command(env!("CARGO_BIN_EXE_karaoke-maker-cli"))
            .arg("--data")
            .arg(temp.path().join("data"))
            .arg("measure-audio")
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let metrics: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let band = metrics["low_20_150_db"].as_f64().unwrap();
        assert!(
            (-23.0..-22.0).contains(&band),
            "{frames} frames: actual low-band power {band} dB"
        );
        let direct = quality::measure(&source).unwrap();
        assert_eq!(direct.low_20_150_db, band);
        assert!((direct.duration - frames as f64 / RATE as f64).abs() < 1e-12);
        assert_eq!(fs::read(&source).unwrap(), before);
    }
}

#[test]
fn last_complete_overlapping_window_contributes_to_band_power() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("tail.wav");
    let signal = write_tone(&source, FFT + 2048, 2048);
    // Independent scalar DFT for the 20..150 Hz bins, including the final full window.
    let first_bin = (20.0 / RATE as f64 * FFT as f64).ceil() as usize;
    let last_bin = (150.0 / RATE as f64 * FFT as f64).floor() as usize;
    let mut power = 0.0;
    for start in [0, 2048] {
        for bin in first_bin..=last_bin {
            let (mut re, mut im) = (0.0, 0.0);
            for i in 0..FFT {
                let sample = signal[start + i] * (0.5 - 0.5 * (TAU * i as f64 / FFT as f64).cos());
                let angle = TAU * bin as f64 * i as f64 / FFT as f64;
                re += sample * angle.cos();
                im -= sample * angle.sin();
            }
            power += re * re + im * im;
        }
    }
    let expected = 10.0 * (power / (2.0 * (FFT * FFT) as f64)).log10();
    let actual = quality::measure(&source).unwrap().low_20_150_db;
    assert!(
        (actual - expected).abs() < 1e-7,
        "actual={actual}, scalar DFT={expected}"
    );
}
