use crate::Progress;
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, time::Duration};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, ValueEnum, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SeparationModel {
    Standard,
    FineTuned,
    FineTunedResidual,
    MelBand,
    #[default]
    MelBandFp16,
}

fn model_file(
    cache: &Path,
    name: &str,
    url: &str,
    size: u64,
    sha: &str,
    progress: &Progress,
) -> Result<std::path::PathBuf> {
    use std::io::Write;
    fn valid(path: &Path, size: u64, expected: &str) -> Result<bool> {
        if !path.is_file() || fs::metadata(path)?.len() != size {
            return Ok(false);
        }
        let mut digest = Sha256::new();
        let mut reader = fs::File::open(path)?;
        let mut buffer = [0u8; 65536];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        Ok(format!("{:x}", digest.finalize()) == expected)
    }
    fs::create_dir_all(cache)?;
    let path = cache.join(name);
    if valid(&path, size, sha)? {
        return Ok(path);
    }
    progress.set(format!("分離モデル{name}を取得しています。"));
    let temporary = cache.join(format!("{}.part", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(1200))
            .build()?
            .get(url)
            .send()?
            .error_for_status()?;
        let mut writer = fs::File::create(&temporary)?;
        let mut buffer = [0u8; 65536];
        let mut count = 0u64;
        loop {
            progress.check()?;
            let n = response.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            ensure!(count <= size, "分離モデルの大きさが不正です。");
            writer.write_all(&buffer[..n])?;
        }
        writer.sync_all()?;
        drop(writer);
        ensure!(
            valid(&temporary, size, sha)?,
            "分離モデルの照合に失敗しました。"
        );
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(path)
}

pub fn separate_using(
    left: &[f32],
    right: &[f32],
    cache: &Path,
    selected: SeparationModel,
    progress: &Progress,
) -> Result<Stems> {
    if matches!(
        selected,
        SeparationModel::MelBand | SeparationModel::MelBandFp16
    ) {
        if selected == SeparationModel::MelBandFp16 {
            let path = model_file(
                cache,
                "melband_roformer_kim_fp16.onnx",
                "https://huggingface.co/Ryan5453/unblend/resolve/30b749fd691312b1e4b3f8fe79df0b51cde836cd/melband_roformer_kim_fp16.onnx",
                473549669,
                "7600639eee90a7e2732b71af90618defac0cacaca5ddfed7b783babfdf8474bf",
                progress,
            )?;
            return crate::roformer::separate(left, right, &path, progress);
        }
        let path = model_file(
            cache,
            "mel_band_roformer_kim_T801.onnx",
            "https://github.com/santiquiroz/port-bs-roformer-onnx/releases/download/models-v1.0/mel_band_roformer_kim_T801.onnx",
            931054408,
            "1b8afd7780d8a234527748821dee6bc746d346f2088751c12fd48e8c873f625a",
            progress,
        )?;
        return crate::roformer::separate(left, right, &path, progress);
    }
    separate_demucs(left, right, cache, selected, progress)
}

pub const MODEL_URL: &str =
    "https://huggingface.co/set-soft/audio_separation/resolve/main/Demucs/htdemucs.safetensors";
pub const MODEL_SHA256: &str = "8193504cdfb3943adaf039b8acb524a46e87ebf232c383ac7a32c80a6578423e";
const MODEL_SIZE: usize = 84_030_696;

pub struct Stems {
    pub instrumental_left: Vec<f32>,
    pub instrumental_right: Vec<f32>,
    pub vocals_left: Vec<f32>,
    pub vocals_right: Vec<f32>,
}

fn verify(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() == MODEL_SIZE && format!("{:x}", Sha256::digest(bytes)) == MODEL_SHA256,
        "分離モデルの検証に失敗しました。モデルを取得し直してください。"
    );
    Ok(())
}

pub fn ensure_weights(cache: &Path, progress: &Progress) -> Result<Vec<u8>> {
    fs::create_dir_all(cache)?;
    let path = cache.join("htdemucs.safetensors");
    if path.is_file() {
        let bytes = fs::read(&path)?;
        if verify(&bytes).is_ok() {
            return Ok(bytes);
        }
        // A damaged cache is never passed to the inference engine.
        fs::remove_file(&path)?;
    }
    progress.set("初回の分離モデルを取得しています（約84MB）。");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    let mut response = client
        .get(MODEL_URL)
        .send()
        .context("分離モデルを取得できませんでした。初回はインターネット接続が必要です。")?
        .error_for_status()?;
    let mut bytes = Vec::with_capacity(MODEL_SIZE);
    let mut chunk = [0u8; 65536];
    loop {
        progress.check()?;
        let count = response.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        ensure!(
            bytes.len() <= MODEL_SIZE,
            "取得した分離モデルの大きさが一致しません。"
        );
    }
    verify(&bytes)?;
    let temp = cache.join(format!("{}.part", uuid::Uuid::new_v4()));
    fs::write(&temp, &bytes)?;
    if path.exists() {
        fs::remove_file(&temp)?;
    } else {
        fs::rename(&temp, &path)?;
    }
    Ok(bytes)
}

#[cfg(feature = "gpu")]
pub fn separate(left: &[f32], right: &[f32], cache: &Path, progress: &Progress) -> Result<Stems> {
    separate_demucs(left, right, cache, SeparationModel::Standard, progress)
}

#[cfg(feature = "gpu")]
fn separate_demucs(
    left: &[f32],
    right: &[f32],
    cache: &Path,
    selected: SeparationModel,
    progress: &Progress,
) -> Result<Stems> {
    use burn::backend::wgpu::{RuntimeOptions, Wgpu, graphics::AutoGraphicsApi, init_setup};
    use demucs_core::{
        Demucs, ModelOptions,
        listener::{ForwardEvent, ForwardListener},
        model::metadata::StemId,
    };
    struct Listener(Progress);
    impl ForwardListener for Listener {
        fn on_event(&mut self, event: ForwardEvent) {
            if let ForwardEvent::ChunkStarted { index, total } = event {
                self.0.set(format!(
                    "歌声と伴奏を分離しています（{} / {total}）。",
                    index + 1
                ));
            }
        }
        fn is_cancelled(&self) -> bool {
            self.0.check().is_err()
        }
    }
    ensure!(
        left.len() == right.len() && !left.is_empty(),
        "音源が空か、左右の長さが異なります。"
    );
    let bytes = if selected == SeparationModel::Standard {
        ensure_weights(cache, progress)?
    } else {
        let path = model_file(
            cache,
            "htdemucs_ft.safetensors",
            "https://huggingface.co/set-soft/audio_separation/resolve/main/Demucs/htdemucs_ft.safetensors",
            336125008,
            "255c2650d26537ce4887c9c4cf08c6d4896fad2fecc0b78dc5b875b117bcc575",
            progress,
        )?;
        fs::read(path)?
    };
    progress.check()?;
    progress.set("歌声と伴奏を分離しています。初回はGPUの準備に時間がかかります。");
    let device = Default::default();
    static GPU_INIT: std::sync::Once = std::sync::Once::new();
    GPU_INIT.call_once(|| {
        init_setup::<AutoGraphicsApi>(
            &device,
            RuntimeOptions {
                tasks_max: 128,
                ..Default::default()
            },
        );
    });
    let kind = match selected {
        SeparationModel::Standard => ModelOptions::FourStem,
        SeparationModel::FineTuned => ModelOptions::FineTuned(vec![
            StemId::Drums,
            StemId::Bass,
            StemId::Other,
            StemId::Vocals,
        ]),
        _ => ModelOptions::FineTuned(vec![StemId::Vocals]),
    };
    let model = Demucs::<Wgpu>::from_bytes(kind, &bytes, device)
        .context("分離モデルを読み込めませんでした。")?;
    drop(bytes);
    let stems = pollster::block_on(model.separate_with_listener(
        left,
        right,
        44100,
        &mut Listener(progress.clone()),
    ))?;
    progress.check()?;
    ensure!(stems.len() == 4, "分離結果が不足しています。");
    let mut result = Stems {
        instrumental_left: vec![0.0; left.len()],
        instrumental_right: vec![0.0; left.len()],
        vocals_left: Vec::new(),
        vocals_right: Vec::new(),
    };
    for stem in stems {
        ensure!(
            stem.left.len() == left.len() && stem.right.len() == right.len(),
            "分離結果の長さが曲と一致しません。"
        );
        if stem.id == StemId::Vocals {
            result.vocals_left = stem.left;
            result.vocals_right = stem.right;
        } else {
            for (out, value) in result.instrumental_left.iter_mut().zip(stem.left) {
                *out += value;
            }
            for (out, value) in result.instrumental_right.iter_mut().zip(stem.right) {
                *out += value;
            }
        }
    }
    ensure!(
        result.vocals_left.len() == left.len(),
        "歌声の分離結果がありません。"
    );
    if selected == SeparationModel::FineTunedResidual {
        result.instrumental_left = left
            .iter()
            .zip(&result.vocals_left)
            .map(|(x, v)| x - v)
            .collect();
        result.instrumental_right = right
            .iter()
            .zip(&result.vocals_right)
            .map(|(x, v)| x - v)
            .collect();
    }
    Ok(result)
}

#[cfg(not(feature = "gpu"))]
fn separate_demucs(
    _left: &[f32],
    _right: &[f32],
    _cache: &Path,
    _selected: SeparationModel,
    _progress: &Progress,
) -> Result<Stems> {
    anyhow::bail!("このビルドにはDemucs分離が含まれていません。")
}

#[cfg(not(feature = "gpu"))]
pub fn separate(
    _left: &[f32],
    _right: &[f32],
    _cache: &Path,
    _progress: &Progress,
) -> Result<Stems> {
    anyhow::bail!("このビルドには音源分離が含まれていません。通常のreleaseビルドを使ってください。")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_or_different_model_is_rejected() {
        assert!(verify(b"not a model").is_err());
        assert!(verify(&vec![0; MODEL_SIZE]).is_err());
    }
}
