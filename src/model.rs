use crate::Progress;
#[cfg(feature = "gpu")]
use anyhow::Context;
use anyhow::{Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Duration};

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
    crate::subtitle_assets::verified_file(
        cache,
        name,
        url,
        size,
        sha,
        Duration::from_secs(1200),
        progress,
    )
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
    let path = crate::subtitle_assets::verified_file(
        cache,
        "htdemucs.safetensors",
        MODEL_URL,
        MODEL_SIZE as u64,
        MODEL_SHA256,
        Duration::from_secs(300),
        progress,
    )?;
    let bytes = fs::read(path)?;
    verify(&bytes)?;
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
    fn standard_model_initial_cancellation_keeps_the_actual_reason() {
        let temp = tempfile::tempdir().unwrap();
        let progress = Progress::default();
        progress
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let error = ensure_weights(temp.path(), &progress).unwrap_err();
        assert!(
            error.to_string().contains("取り消し"),
            "displayed reason: {error}"
        );
        assert!(!temp.path().join("htdemucs.safetensors").exists());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part")
        }));
    }

    #[test]
    fn separation_model_stalled_download_cancels_and_preserves_existing_cache() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::mpsc,
            time::Instant,
        };
        for send_headers in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (ready, received) = mpsc::channel();
            let (release, wait_release) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0u8; 2048];
                assert!(socket.read(&mut request).unwrap() > 0);
                if send_headers {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    socket.flush().unwrap();
                }
                ready.send(()).unwrap();
                let _ = wait_release.recv_timeout(Duration::from_secs(3));
            });
            let temp = tempfile::tempdir().unwrap();
            let original = temp.path().join("model.bin");
            fs::write(&original, b"keep damaged original").unwrap();
            let progress = Progress::default();
            let cancel_progress = progress.clone();
            let cancel = std::thread::spawn(move || {
                received.recv_timeout(Duration::from_secs(5)).unwrap();
                let at = Instant::now();
                cancel_progress
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                at
            });
            let result = model_file(
                temp.path(),
                "model.bin",
                &format!("http://{address}/model"),
                5,
                &format!("{:x}", Sha256::digest(b"model")),
                &progress,
            );
            let cancel_at = cancel.join().unwrap();
            let elapsed = cancel_at.elapsed();
            let _ = release.send(());
            server.join().unwrap();
            assert!(result.is_err());
            assert!(
                elapsed < Duration::from_millis(750),
                "model cancel waited {elapsed:?}; headers={send_headers}"
            );
            assert!(format!("{:#}", result.unwrap_err()).contains("取り消し"));
            assert_eq!(fs::read(original).unwrap(), b"keep damaged original");
            assert!(
                fs::read_dir(temp.path()).unwrap().all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part")),
                "no partial model files"
            );
        }
    }

    #[test]
    fn model_download_verifies_repairs_keeps_bad_bytes_and_reuses_offline() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let expected = format!("{:x}", Sha256::digest(b"model"));
        for body in [
            b"model".as_slice(),
            b"short".as_slice(),
            b"bad".as_slice(),
            b"model!".as_slice(),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body = body.to_vec();
            let succeeds = body == b"model";
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0u8; 2048];
                assert!(socket.read(&mut request).unwrap() > 0);
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                socket.write_all(&body).unwrap();
            });
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("model.bin"), b"keep damaged model").unwrap();
            let result = model_file(
                temp.path(),
                "model.bin",
                &format!("http://{address}/model"),
                5,
                &expected,
                &Progress::default(),
            );
            server.join().unwrap();
            assert_eq!(result.is_ok(), succeeds);
            if succeeds {
                let path = result.unwrap();
                assert_eq!(fs::read(&path).unwrap(), b"model");
                let invalid = fs::read_dir(temp.path())
                    .unwrap()
                    .filter_map(|entry| {
                        let entry = entry.unwrap();
                        entry
                            .file_name()
                            .to_string_lossy()
                            .contains(".invalid-")
                            .then(|| entry.path())
                    })
                    .collect::<Vec<_>>();
                assert_eq!(invalid.len(), 1);
                assert_eq!(fs::read(&invalid[0]).unwrap(), b"keep damaged model");
                // The server has finished: this can pass only by using verified cache.
                assert_eq!(
                    model_file(
                        temp.path(),
                        "model.bin",
                        &format!("http://{address}/offline"),
                        5,
                        &expected,
                        &Progress::default()
                    )
                    .unwrap(),
                    path
                );
            } else {
                assert_eq!(
                    fs::read(temp.path().join("model.bin")).unwrap(),
                    b"keep damaged model"
                );
            }
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part")
            }));
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let progress = Progress::default();
        progress
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let temp = tempfile::tempdir().unwrap();
        assert!(
            format!(
                "{:#}",
                model_file(
                    temp.path(),
                    "model.bin",
                    &format!("http://{}/model", listener.local_addr().unwrap()),
                    5,
                    &expected,
                    &progress
                )
                .unwrap_err()
            )
            .contains("取り消し")
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!temp.path().join("model.bin").exists());
    }

    #[test]
    fn truncated_or_different_model_is_rejected() {
        assert!(verify(b"not a model").is_err());
        assert!(verify(&vec![0; MODEL_SIZE]).is_err());
    }
}
