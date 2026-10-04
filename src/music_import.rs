//! Import an official download without opening arbitrary source paths.
//! Names supplied by a browser never become filesystem paths.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const MAX_MP3: usize = 16 * 1024 * 1024;
pub const MAX_FORM: usize = MAX_MP3 + 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportedAudio {
    pub sha256: String,
    pub bytes: u64,
    pub duration_seconds: f64,
}

pub fn canonical_id(id: &str) -> Result<()> {
    ensure!(
        Uuid::parse_str(id)?.to_string() == id,
        "曲の識別子が不正です。"
    );
    Ok(())
}

fn directory(root: &Path, project: &str, clip: &str, create: bool) -> Result<PathBuf> {
    canonical_id(project)?;
    canonical_id(clip)?;
    let root = root.canonicalize()?;
    let mut current = root.clone();
    for name in ["audio", project, clip] {
        current.push(name);
        if create && !current.exists() {
            fs::create_dir(&current)?;
        }
        let metadata = fs::symlink_metadata(&current)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "保存先を確認できませんでした。"
        );
        ensure!(
            current.canonicalize()? == current && current.starts_with(&root),
            "保存先がアプリのフォルダー外にあります。"
        );
    }
    Ok(current)
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temporary(dir: &Path, extension: &str) -> Result<(Temporary, fs::File)> {
    let path = dir.join(format!("{}.{}", Uuid::new_v4(), extension));
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    Ok((Temporary(path), file))
}

fn mp3_duration(dir: &Path, input: &Path) -> Result<f64> {
    let tools = crate::media::Tools::discover();
    let (output_guard, output) = temporary(dir, "probe")?;
    let mut child = crate::media::hidden_command(&tools.ffprobe)
        .args(["-v", "error", "-protocol_whitelist", "file,pipe", "-show_entries",
            "format=format_name,duration:stream=codec_type,codec_name:stream_disposition=attached_pic",
            "-of", "json"])
        .arg(input)
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
        .context("MP3の検査に必要なffprobeを起動できませんでした。")?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(
                    "MP3の検査を完了できませんでした。保存し直したファイルを選んでください。"
                );
            }
        }
    };
    ensure!(status.success(), "読み込めるMP3を選んでください。");
    ensure!(
        fs::metadata(&output_guard.0)?.len() <= 64 * 1024,
        "音源の情報が大きすぎます。"
    );
    let data: serde_json::Value = serde_json::from_slice(&fs::read(&output_guard.0)?)?;
    ensure!(
        data["format"]["format_name"] == "mp3",
        "Sunoから保存したMP3を選んでください。"
    );
    let streams = data["streams"].as_array().context("音声がありません。")?;
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    ensure!(
        audio.len() == 1 && audio[0]["codec_name"] == "mp3",
        "MP3の音声がありません。"
    );
    ensure!(
        streams.iter().all(|s| s["codec_type"] == "audio"
            || (s["codec_type"] == "video" && s["disposition"]["attached_pic"] == 1)),
        "音楽のMP3を選んでください。"
    );
    let duration = data["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .context("曲の長さを確認できませんでした。")?;
    ensure!(
        duration.is_finite() && duration > 0.0 && duration <= crate::media::MAX_DURATION,
        "曲は15分以内のMP3を選んでください。"
    );
    Ok(duration)
}

pub fn import_mp3(root: &Path, project: &str, clip: &str, bytes: &[u8]) -> Result<ImportedAudio> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_MP3,
        "MP3は16MB以内で選んでください。"
    );
    let dir = directory(root, project, clip, true)?;
    let (staging, mut file) = temporary(&dir, "mp3")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let duration_seconds = mp3_duration(&dir, &staging.0)?;
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    let result = ImportedAudio {
        sha256,
        bytes: bytes.len() as u64,
        duration_seconds,
    };
    let destination = dir.join(format!("{}.mp3", result.sha256));
    // Hard-link creation is atomic and refuses to overwrite an existing download.
    match fs::hard_link(&staging.0, &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verified_path(root, project, clip, &result)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(result)
}

pub fn verified_path(
    root: &Path,
    project: &str,
    clip: &str,
    audio: &ImportedAudio,
) -> Result<PathBuf> {
    ensure!(
        audio.sha256.len() == 64
            && audio
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "保存した音源の情報を確認できませんでした。"
    );
    ensure!(
        audio.bytes > 0
            && audio.bytes <= MAX_MP3 as u64
            && audio.duration_seconds.is_finite()
            && audio.duration_seconds > 0.0
            && audio.duration_seconds <= crate::media::MAX_DURATION,
        "保存した音源の情報を確認できませんでした。"
    );
    let path = directory(root, project, clip, false)?.join(format!("{}.mp3", audio.sha256));
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && path.canonicalize()? == path,
        "保存した音源を確認できませんでした。"
    );
    ensure!(
        metadata.len() == audio.bytes,
        "保存した音源の大きさが変わっています。"
    );
    let mut bytes = Vec::new();
    fs::File::open(&path)?
        .take(MAX_MP3 as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == audio.bytes
            && format!("{:x}", Sha256::digest(&bytes)) == audio.sha256,
        "保存した音源の内容が変わっています。"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
    const CLIP: &str = "00000000-0000-4000-8000-000000000002";

    #[test]
    fn rejects_paths_oversize_empty_and_fake_mp3() {
        let temp = tempfile::tempdir().unwrap();
        assert!(import_mp3(temp.path(), "../outside", CLIP, b"test").is_err());
        assert!(import_mp3(temp.path(), PROJECT, "../outside", b"test").is_err());
        assert!(import_mp3(temp.path(), PROJECT, CLIP, &[]).is_err());
        assert!(import_mp3(temp.path(), PROJECT, CLIP, &vec![0; MAX_MP3 + 1]).is_err());
        assert!(import_mp3(temp.path(), PROJECT, CLIP, b"fake mp3").is_err());
        let dir = directory(temp.path(), PROJECT, CLIP, false).unwrap();
        assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
    }

    #[test]
    fn imports_real_mp3_idempotently_and_detects_modified_media() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.mp3");
        let tools = crate::media::Tools::discover();
        let status = crate::media::hidden_command(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.2",
                "-c:a",
                "libmp3lame",
            ])
            .arg(&original)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = fs::read(&original).unwrap();
        let audio = import_mp3(temp.path(), PROJECT, CLIP, &bytes).unwrap();
        let again = import_mp3(temp.path(), PROJECT, CLIP, &bytes).unwrap();
        assert_eq!(audio.sha256, again.sha256);
        assert!(audio.duration_seconds > 0.0 && audio.duration_seconds < 1.0);
        let imported = verified_path(temp.path(), PROJECT, CLIP, &audio).unwrap();
        assert_eq!(fs::read(&imported).unwrap(), bytes);
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert_eq!(fs::read_dir(imported.parent().unwrap()).unwrap().count(), 1);
        let mut changed = bytes.clone();
        changed[0] ^= 1;
        fs::write(imported, changed).unwrap();
        assert!(verified_path(temp.path(), PROJECT, CLIP, &audio).is_err());
        assert!(verified_path(temp.path(), CLIP, PROJECT, &audio).is_err());
        let mut invalid = audio;
        invalid.sha256 = "../outside".into();
        assert!(verified_path(temp.path(), PROJECT, CLIP, &invalid).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_redirected_storage() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("audio")).unwrap();
        assert!(import_mp3(temp.path(), PROJECT, CLIP, b"test").is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
