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
    storage_id(id)?;
    Ok(())
}

// Return the checked component so callers cannot accidentally use the raw input
// when constructing storage paths. Keep existing canonical UUID filenames.
pub fn storage_id(id: &str) -> Result<String> {
    if id.contains("..") || id.contains('/') || id.contains('\\') {
        anyhow::bail!("曲の識別子が不正です。");
    }
    let parsed = Uuid::parse_str(id)?;
    let canonical = parsed.to_string();
    ensure!(canonical == id, "曲の識別子が不正です。");
    Ok(canonical)
}

fn storage_hash(hash: &str) -> Result<String> {
    if hash.contains("..") || hash.contains('/') || hash.contains('\\') {
        anyhow::bail!("保存した音源の情報を確認できませんでした。");
    }
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "保存した音源の情報を確認できませんでした。"
    );
    Ok(hash.to_owned())
}

fn directory(root: &Path, project: &str, clip: &str, create: bool) -> Result<PathBuf> {
    let project = storage_id(project)?;
    let clip = storage_id(clip)?;
    let root = root.canonicalize()?;
    let mut current = root.clone();
    for name in ["audio", project.as_str(), clip.as_str()] {
        if name.contains("..") {
            return Err(anyhow::anyhow!("保存先を確認できませんでした。"));
        }
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

#[derive(Debug)]
struct IncompleteMp3;
impl std::fmt::Display for IncompleteMp3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MP3の音声が欠落または破損しています。公式画面から保存し直してください。")
    }
}
impl std::error::Error for IncompleteMp3 {}

pub(crate) fn is_incomplete(error: &anyhow::Error) -> bool {
    error.downcast_ref::<IncompleteMp3>().is_some()
}

// Xing/Info frame counts describe audio frames, excluding the tag frame.
// Match FFmpeg's MP3 demuxer offsets; do not infer completeness from an
// estimated duration when this optional header is absent.
// LAME VbrTag.c deliberately keeps this offset unchanged even with CRC
// protection (it subtracts the CRC size from sideinfo_len for the tag).
struct Mp3Declaration {
    frames: Option<u64>,
    stream_bytes: Option<usize>,
    offset: usize,
}

fn declared_frames(bytes: &[u8]) -> Option<Mp3Declaration> {
    let mut offset = 0;
    while bytes.get(offset..offset + 3)? == b"ID3" {
        let header = bytes.get(offset..offset + 10)?;
        if header[6..10].iter().any(|b| b & 0x80 != 0) {
            return None;
        }
        let size = header[6..10]
            .iter()
            .fold(0usize, |n, b| (n << 7) | usize::from(*b));
        offset += 10
            + size
            + if header[3] == 4 && header[5] & 0x10 != 0 {
                10
            } else {
                0
            };
    }
    let header = bytes.get(offset..offset + 4)?;
    let version = (header[1] >> 3) & 3;
    if header[0] != 0xff || header[1] & 0xe0 != 0xe0 || version == 1 || (header[1] >> 1) & 3 != 1 {
        return None;
    }
    let mono = header[3] >> 6 == 3;
    let side_info = match (version == 3, mono) {
        (true, false) => 32,
        (true, true) | (false, false) => 17,
        (false, true) => 9,
    };
    let tag = bytes.get(offset + 4 + side_info..)?;
    if tag.len() < 8 || (&tag[..4] != b"Xing" && &tag[..4] != b"Info") {
        return None;
    }
    let mut field = 8;
    let mut read = || {
        let value = u32::from_be_bytes(tag.get(field..field + 4)?.try_into().ok()?);
        field += 4;
        Some(value)
    };
    let frames = if tag[7] & 1 != 0 {
        read().filter(|v| *v > 0).map(u64::from)
    } else {
        None
    };
    let stream_bytes = if tag[7] & 2 != 0 {
        read().filter(|v| *v > 0).map(|v| v as usize)
    } else {
        None
    };
    Some(Mp3Declaration {
        frames,
        stream_bytes,
        offset,
    })
}

fn wait_for_media(
    child: &mut std::process::Child,
    started: Instant,
) -> Result<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(
                    "MP3の検査を完了できませんでした。保存し直したファイルを選んでください。"
                );
            }
        }
    }
}

fn mp3_duration(dir: &Path, input: &Path, bytes: &[u8]) -> Result<f64> {
    let tools = crate::media::Tools::discover();
    let (output_guard, output) = temporary(dir, "probe")?;
    let mut child = crate::media::hidden_command(&tools.ffprobe)
        .args(["-v", "error", "-protocol_whitelist", "file,pipe", "-count_packets", "-show_entries",
            "format=format_name,duration:stream=codec_type,codec_name,nb_read_packets:stream_disposition=attached_pic",
            "-of", "json"])
        .arg(input)
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
        .context("MP3の検査に必要なffprobeを起動できませんでした。")?;
    let started = Instant::now();
    let status = wait_for_media(&mut child, started)?;
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
    // Count packets rather than decoded frames: valid short MPEG-2/2.5 files
    // can have a final frame consisting entirely of gapless encoder padding.
    let packets = audio[0]["nb_read_packets"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok());
    if packets.is_none_or(|frames| frames == 0)
        || declared_frames(bytes).is_some_and(|expected| {
            expected
                .frames
                .is_some_and(|frames| packets != Some(frames))
                || expected
                    .stream_bytes
                    .is_some_and(|size| size > bytes.len() - expected.offset)
        })
    {
        return Err(IncompleteMp3.into());
    }
    let mut decoder = crate::media::hidden_command(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-xerror",
            "-err_detect",
            "explode",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(input)
        .args(["-map", "0:a:0", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("MP3の検査に必要なffmpegを起動できませんでした。")?;
    if !wait_for_media(&mut decoder, Instant::now())?.success() {
        return Err(IncompleteMp3.into());
    }
    Ok(duration)
}

pub(crate) fn validate_saved(path: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_MP3 as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_MP3, "MP3は16MB以内で選んでください。");
    mp3_duration(
        path.parent().context("音源の保存先がありません。")?,
        path,
        &bytes,
    )?;
    Ok(())
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
    let duration_seconds = mp3_duration(&dir, &staging.0, bytes)?;
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
    let hash = storage_hash(&audio.sha256)?;
    ensure!(
        audio.bytes > 0
            && audio.bytes <= MAX_MP3 as u64
            && audio.duration_seconds.is_finite()
            && audio.duration_seconds > 0.0
            && audio.duration_seconds <= crate::media::MAX_DURATION,
        "保存した音源の情報を確認できませんでした。"
    );
    let requested = directory(root, project, clip, false)?.join(format!("{hash}.mp3"));
    let path = requested.canonicalize()?;
    if !path.starts_with(root.canonicalize()?.join("audio")) {
        return Err(anyhow::anyhow!("保存した音源を確認できませんでした。"));
    }
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && path == requested,
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
pub(crate) mod tests {
    use super::*;
    const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
    const CLIP: &str = "00000000-0000-4000-8000-000000000002";

    pub(crate) fn synthetic_variants(dir: &Path) -> (Vec<u8>, Vec<Vec<u8>>) {
        let original = dir.join("synthetic.mp3");
        assert!(
            crate::media::hidden_command(&crate::media::Tools::discover().ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=5",
                    "-c:a",
                    "libmp3lame",
                    "-b:a",
                    "192k"
                ])
                .arg(&original)
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(&original).unwrap();
        let packets = crate::media::hidden_command(&crate::media::Tools::discover().ffprobe)
            .args([
                "-v",
                "error",
                "-show_packets",
                "-show_entries",
                "packet=pos",
                "-of",
                "json",
            ])
            .arg(&original)
            .output()
            .unwrap();
        assert!(packets.status.success());
        let packets: serde_json::Value = serde_json::from_slice(&packets.stdout).unwrap();
        let packets = packets["packets"].as_array().unwrap();
        let boundary = packets[packets.len() / 2]["pos"]
            .as_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut damaged = bytes.clone();
        damaged[boundary..boundary + 1000].fill(0);
        let mut invalid_side_info = bytes.clone();
        invalid_side_info[boundary + 4..boundary + 24].fill(0xff);
        let variants = vec![
            bytes[..boundary + 100].to_vec(),
            bytes[..boundary].to_vec(),
            damaged,
            bytes[..bytes.len() - 100].to_vec(),
            invalid_side_info,
        ];
        (bytes, variants)
    }

    #[test]
    fn rejects_truncated_and_corrupt_mp3_without_committing_audio() {
        let temp = tempfile::tempdir().unwrap();
        let (normal, variants) = synthetic_variants(temp.path());
        for (index, bytes) in variants.iter().enumerate() {
            assert!(
                import_mp3(temp.path(), PROJECT, CLIP, bytes).is_err(),
                "accepted invalid variant {index}"
            );
            assert_eq!(
                fs::read_dir(directory(temp.path(), PROJECT, CLIP, false).unwrap())
                    .unwrap()
                    .count(),
                0
            );
        }
        let audio = import_mp3(temp.path(), PROJECT, CLIP, &normal).unwrap();
        assert_eq!(
            fs::read(verified_path(temp.path(), PROJECT, CLIP, &audio).unwrap()).unwrap(),
            normal
        );
    }

    #[test]
    fn xing_tag_offset_stays_fixed_for_a_crc_protected_tag_frame() {
        let temp = tempfile::tempdir().unwrap();
        let (mut normal, variants) = synthetic_variants(temp.path());
        let declaration = declared_frames(&normal).unwrap();
        normal[declaration.offset + 1] &= !1;
        assert_eq!(declared_frames(&normal).unwrap().frames, declaration.frames);
        let clip = Uuid::new_v4().to_string();
        assert!(import_mp3(temp.path(), PROJECT, &clip, &normal).is_ok());
        let mut truncated = variants[1].clone();
        truncated[declaration.offset + 1] &= !1;
        assert!(import_mp3(temp.path(), PROJECT, CLIP, &truncated).is_err());
    }

    #[test]
    fn accepts_short_cbr_vbr_and_headerless_mp3_at_supported_sample_rates() {
        let temp = tempfile::tempdir().unwrap();
        for (index, (rate, channels, xing, vbr)) in [
            (44100, 1, true, false),
            (48000, 2, true, true),
            (22050, 1, true, true),
            (24000, 2, true, false),
            (11025, 1, true, true),
            (12000, 2, true, false),
            (44100, 2, false, true),
            (22050, 1, false, false),
        ]
        .into_iter()
        .enumerate()
        {
            let source = temp.path().join(format!("normal-{index}.mp3"));
            let mut encoder = crate::media::hidden_command(&crate::media::Tools::discover().ffmpeg);
            encoder
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=0.2",
                    "-c:a",
                    "libmp3lame",
                    "-ar",
                ])
                .arg(rate.to_string())
                .arg("-ac")
                .arg(channels.to_string())
                .args(["-write_xing", if xing { "1" } else { "0" }]);
            if vbr {
                encoder.args(["-q:a", "4"]);
            } else {
                encoder.args(["-b:a", "64k"]);
            }
            assert!(encoder.arg(&source).status().unwrap().success());
            let bytes = fs::read(&source).unwrap();
            assert_eq!(declared_frames(&bytes).is_some(), xing);
            let clip = Uuid::new_v4().to_string();
            let audio = import_mp3(temp.path(), PROJECT, &clip, &bytes).unwrap();
            assert_eq!(
                fs::read(verified_path(temp.path(), PROJECT, &clip, &audio).unwrap()).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn rejects_paths_oversize_empty_and_fake_mp3() {
        let temp = tempfile::tempdir().unwrap();
        assert!(import_mp3(temp.path(), "../outside", CLIP, b"test").is_err());
        assert!(import_mp3(temp.path(), PROJECT, "../outside", b"test").is_err());
        for invalid in [
            "/outside",
            "C:\\outside",
            "\\\\server\\share",
            "..\\outside",
        ] {
            assert!(import_mp3(temp.path(), invalid, CLIP, b"test").is_err());
            assert!(import_mp3(temp.path(), PROJECT, invalid, b"test").is_err());
        }
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
