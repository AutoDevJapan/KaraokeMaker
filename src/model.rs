use crate::Progress;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, time::Duration};

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
    let bytes = ensure_weights(cache, progress)?;
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
    let model = Demucs::<Wgpu>::from_bytes(ModelOptions::FourStem, &bytes, device)
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
    Ok(result)
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
