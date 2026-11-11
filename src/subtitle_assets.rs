use crate::Progress;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

/// One owned asset-download runtime per app process, including CUDA setup.
/// The default OS DNS resolver uses
/// blocking tasks; cancellation must not drop a short-lived runtime and wait for
/// getaddrinfo. Bound those tasks across retries instead of abandoning a new
/// thread pool for every file. DNS tasks hold only host names, never cache files.
struct NetworkRuntime {
    runtime: Mutex<(Option<tokio::runtime::Runtime>, bool)>,
}

impl NetworkRuntime {
    const fn new() -> Self {
        Self {
            runtime: Mutex::new((None, false)),
        }
    }

    fn handle(&self) -> Result<tokio::runtime::Handle> {
        let mut state = self
            .runtime
            .lock()
            .map_err(|_| anyhow::anyhow!("字幕の通信状態を確認できません。"))?;
        ensure!(!state.1, "字幕の通信処理は終了しています。");
        if state.0.is_none() {
            state.0 = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .max_blocking_threads(2)
                    .thread_name("karaoke-subtitle-network")
                    .enable_all()
                    .build()?,
            );
        }
        Ok(state.0.as_ref().unwrap().handle().clone())
    }

    fn shutdown(&self) {
        let runtime = if let Ok(mut state) = self.runtime.lock() {
            state.1 = true;
            state.0.take()
        } else {
            None
        };
        // App workers have returned before this call. An OS DNS lookup cannot
        // be forcibly cancelled; its at-most-two workers may finish afterwards.
        // They cannot publish files, and app exit never waits indefinitely.
        if let Some(runtime) = runtime {
            runtime.shutdown_timeout(Duration::from_millis(100));
        }
    }
}

static NETWORK: NetworkRuntime = NetworkRuntime::new();

pub(crate) fn shutdown() {
    NETWORK.shutdown();
}

async fn cancel_pending<T>(
    future: impl std::future::Future<Output = Result<T>>,
    progress: &Progress,
) -> Result<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(50)) => progress.check()?,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Source {
    name: String,
    url: String,
    size: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct Manifest {
    ctc: Vec<Source>,
    whisper: Vec<Source>,
    whisper_sdk: Sdk,
}

#[derive(Deserialize)]
struct Sdk {
    directory: String,
    archive: Source,
    files: Vec<SdkFile>,
}

#[derive(Deserialize)]
struct SdkFile {
    entry: String,
    name: String,
    size: u64,
    sha256: String,
}

pub(crate) struct CtcAssets {
    pub model: PathBuf,
    pub vocab: PathBuf,
}

pub(crate) struct WhisperAssets {
    pub executable: PathBuf,
    pub model: PathBuf,
}

fn manifest() -> Result<Manifest> {
    Ok(serde_json::from_str(include_str!(
        "../resources/subtitle-assets.json"
    ))?)
}

fn resolve_files_with(
    cache: &Path,
    sources: &[Source],
    progress: &Progress,
    fetch: &mut impl FnMut(&Source, &Path, &Progress) -> Result<()>,
) -> Result<Vec<PathBuf>> {
    ensure!(!sources.is_empty(), "字幕の実行部品の一覧が空です。");
    let _lock = setup_lock(cache, progress)?;
    let mut files = Vec::new();
    for source in sources {
        progress.check()?;
        validate_source(source)?;
        let path = cache.join(&source.name);
        if verified(&path, source.size, &source.sha256)?.is_none() {
            let part = cache.join(format!(".download-{}.part", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                fetch(source, &part, progress)?;
                ensure!(
                    verified(&part, source.size, &source.sha256)?.is_some(),
                    "取得ファイルのサイズかSHA-256が一致しません：{}",
                    source.name
                );
                preserve_invalid(&path)?;
                fs::rename(&part, &path)?;
                Ok(())
            })();
            cleanup(result, &part)?;
        }
        files.push(path);
    }
    Ok(files)
}

pub(crate) fn ctc(models: &Path, progress: &Progress) -> Result<CtcAssets> {
    let sources = manifest()?.ctc;
    let cache = models.join("subtitles/ctc-729c1a6");
    let files = resolve_files_with(&cache, &sources, progress, &mut download)?;
    let named = |name: &str| -> Result<PathBuf> {
        Ok(files[sources
            .iter()
            .position(|source| source.name == name)
            .context("字幕モデルの定義がありません。")?]
        .clone())
    };
    Ok(CtcAssets {
        model: named("model_quantized.onnx")?,
        vocab: named("vocab.json")?,
    })
}

pub(crate) fn whisper(models: &Path, progress: &Progress) -> Result<WhisperAssets> {
    ensure!(
        cfg!(windows),
        "字幕候補の自動準備はWindows向けです。時刻付き歌詞でも作成できます。"
    );
    let manifest = manifest()?;
    let cache = models.join("subtitles/whisper-v1.9.2");
    resolve_files_with(&cache, &manifest.whisper, progress, &mut download)?;
    let executable = install_sdk_with(&cache, &manifest.whisper_sdk, progress, &mut download)?;
    Ok(WhisperAssets {
        executable,
        model: cache.join("ggml-large-v3-turbo-q5_0.bin"),
    })
}

fn leaf(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

fn validate_source(source: &Source) -> Result<()> {
    ensure!(
        leaf(&source.name)
            && source.size > 0
            && source.sha256.len() == 64
            && source.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "取得ファイルの定義が不正です：{}",
        source.name
    );
    Ok(())
}

fn setup_lock(cache: &Path, progress: &Progress) -> Result<fs::File> {
    fs::create_dir_all(cache)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join("setup.lock"))?;
    loop {
        progress.check()?;
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(100)),
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn verified(path: &Path, size: u64, expected: &str) -> Result<Option<fs::File>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        !metadata.file_type().is_symlink() && metadata.is_file(),
        "字幕の実行部品が通常のファイルではありません：{}",
        path.display()
    );
    if metadata.len() != size {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(Some(file))
}

fn preserve_invalid(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                !metadata.file_type().is_symlink(),
                "字幕の保存先がリンクです。"
            );
            let saved = path.with_file_name(format!(
                "{}.invalid-{}",
                path.file_name()
                    .context("保存先がありません。")?
                    .to_string_lossy(),
                uuid::Uuid::new_v4()
            ));
            fs::rename(path, &saved).with_context(|| {
                format!("不完全な字幕の実行部品を保持できません：{}", path.display())
            })?;
            eprintln!(
                "不完全な字幕の実行部品を{}へ保持しました。",
                saved.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn cleanup(result: Result<()>, temporary: &Path) -> Result<()> {
    let removed = match fs::symlink_metadata(temporary) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(temporary),
        Ok(_) => fs::remove_file(temporary),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    match (result, removed) {
        (Err(error), Err(removal)) => Err(anyhow::anyhow!(
            "{error:#}。未完了ファイル{}の回収にも失敗しました：{removal}",
            temporary.display()
        )),
        (Ok(()), Err(removal)) => Err(anyhow::anyhow!(
            "準備後のファイル{}を回収できません：{removal}",
            temporary.display()
        )),
        (result, Ok(())) => result,
    }
}

fn download(source: &Source, destination: &Path, progress: &Progress) -> Result<()> {
    download_with_client(&NETWORK, source, destination, progress, || {
        Ok(reqwest::Client::builder()
            .timeout(Duration::from_secs(1200))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(30))
            .build()?)
    })
}

/// Separation models share verified cache publication and cancellable transfer.
pub(crate) fn verified_file(
    cache: &Path,
    name: &str,
    url: &str,
    size: u64,
    sha256: &str,
    timeout: Duration,
    progress: &Progress,
) -> Result<PathBuf> {
    let source = Source {
        name: name.into(),
        url: url.into(),
        size,
        sha256: sha256.into(),
    };
    let mut paths =
        resolve_files_with(cache, &[source], progress, &mut |source, path, progress| {
            progress.set(format!("分離モデルを初回取得しています：{}", source.name));
            download_to(
                &NETWORK,
                &source.name,
                &source.url,
                source.size,
                path,
                progress,
                || {
                    Ok(reqwest::Client::builder()
                        .timeout(timeout)
                        .connect_timeout(Duration::from_secs(30))
                        .read_timeout(Duration::from_secs(30))
                        .build()?)
                },
            )
        })?;
    paths.pop().context("分離モデルの保存先を確認できません。")
}

/// Shared by subtitle assets and CUDA setup. Call from a blocking app worker;
/// cancelling a request must not drop a short-lived runtime or wait for OS DNS.
#[cfg(any(feature = "ort", test))]
pub(crate) fn download_file(
    name: &str,
    url: &str,
    expected_size: u64,
    destination: &Path,
    progress: &Progress,
) -> Result<()> {
    download_to(
        &NETWORK,
        name,
        url,
        expected_size,
        destination,
        progress,
        || {
            Ok(reqwest::Client::builder()
                .timeout(Duration::from_secs(1200))
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(30))
                .build()?)
        },
    )
}

fn download_with_client(
    network: &NetworkRuntime,
    source: &Source,
    destination: &Path,
    progress: &Progress,
    client: impl FnOnce() -> Result<reqwest::Client>,
) -> Result<()> {
    validate_source(source)?;
    progress.set(format!(
        "字幕の実行部品を初回準備しています：{}",
        source.name
    ));
    download_to(
        network,
        &source.name,
        &source.url,
        source.size,
        destination,
        progress,
        client,
    )
}

fn download_to(
    network: &NetworkRuntime,
    name: &str,
    url: &str,
    expected_size: u64,
    destination: &Path,
    progress: &Progress,
    client: impl FnOnce() -> Result<reqwest::Client>,
) -> Result<()> {
    progress.check()?;
    network.handle()?.block_on(async {
        let transfer = async {
            let mut response = client()?.get(url).send().await?.error_for_status()?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            let mut total = 0;
            while let Some(bytes) = response.chunk().await? {
                progress.check()?;
                total += bytes.len() as u64;
                ensure!(
                    total <= expected_size,
                    "取得ファイルのサイズが固定値を超えました：{name}"
                );
                file.write_all(&bytes)?;
                if total / (8 * 1024 * 1024) != (total - bytes.len() as u64) / (8 * 1024 * 1024) {
                    progress.set(format!(
                        "実行部品を準備しています：{name}（{} / {} MB）",
                        total / 1_000_000,
                        expected_size / 1_000_000
                    ));
                }
            }
            file.sync_all()?;
            progress.check()
        };
        cancel_pending(transfer, progress).await
    })
}

fn sdk_valid(root: &Path, sdk: &Sdk) -> Result<bool> {
    for item in &sdk.files {
        ensure!(
            leaf(&item.name) && item.size > 0,
            "字幕実行部品の格納先が不正です。"
        );
        if verified(&root.join(&item.name), item.size, &item.sha256)?.is_none() {
            return Ok(false);
        }
    }
    Ok(!sdk.files.is_empty())
}

fn install_sdk_with(
    cache: &Path,
    sdk: &Sdk,
    progress: &Progress,
    fetch: &mut impl FnMut(&Source, &Path, &Progress) -> Result<()>,
) -> Result<PathBuf> {
    ensure!(
        leaf(&sdk.directory)
            && !sdk.files.is_empty()
            && sdk.files.iter().any(|file| file.name == "whisper-cli.exe"),
        "字幕実行部品の一覧が不正です。"
    );
    let root = cache.join(&sdk.directory);
    // A complete installation must work offline without requiring the download archive.
    // Publication is serialized, so installed bytes are only changed by the holder.
    let lock = setup_lock(cache, progress)?;
    if sdk_valid(&root, sdk)? {
        return Ok(root.join("whisper-cli.exe"));
    }
    drop(lock);
    let archive_path =
        resolve_files_with(cache, std::slice::from_ref(&sdk.archive), progress, fetch)?.remove(0);
    let _lock = setup_lock(cache, progress)?;
    if sdk_valid(&root, sdk)? {
        return Ok(root.join("whisper-cli.exe"));
    }
    let stage = cache.join(format!(".setup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage)?;
    let result = (|| -> Result<()> {
        let file = verified(&archive_path, sdk.archive.size, &sdk.archive.sha256)?
            .context("字幕実行部品のアーカイブが一致しません。")?;
        let mut archive = zip::ZipArchive::new(file)?;
        for item in &sdk.files {
            progress.check()?;
            ensure!(leaf(&item.name), "字幕実行部品のファイル名が不正です。");
            let mut entry = archive.by_name(&item.entry)?;
            ensure!(
                entry.size() == item.size
                    && entry.enclosed_name().is_some()
                    && entry
                        .unix_mode()
                        .is_none_or(|mode| mode & 0o170000 != 0o120000),
                "字幕実行部品の展開対象が不正です。"
            );
            let destination = stage.join(&item.name);
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            let written = std::io::copy(&mut entry.by_ref().take(item.size + 1), &mut out)?;
            out.sync_all()?;
            drop(out);
            ensure!(
                written == item.size && verified(&destination, item.size, &item.sha256)?.is_some(),
                "字幕実行部品の展開結果が一致しません：{}",
                item.name
            );
        }
        ensure!(
            sdk_valid(&stage, sdk)?,
            "字幕実行部品の導入結果が一致しません。"
        );
        preserve_invalid(&root)?;
        fs::rename(&stage, &root)?;
        Ok(())
    })();
    cleanup(result, &stage)?;
    Ok(root.join("whisper-cli.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn subtitle_models_follow_selected_app_data_and_fetch_only_once() {
        let data = tempfile::tempdir().unwrap();
        let cache = data.path().join("data/models");
        let source = Source {
            name: "model_quantized.onnx".into(),
            url: "https://example.invalid/model".into(),
            size: 5,
            sha256: format!("{:x}", Sha256::digest(b"model")),
        };
        let mut calls = 0;
        let mut fetch = |_: &Source, dest: &Path, _: &Progress| -> Result<()> {
            calls += 1;
            fs::write(dest, b"model")?;
            Ok(())
        };
        let files = resolve_files_with(
            &cache,
            std::slice::from_ref(&source),
            &Progress::default(),
            &mut fetch,
        )
        .unwrap();
        assert_eq!(
            files,
            [cache.join("model_quantized.onnx")],
            "subtitle models must use selected app data, not a fixed developer drive"
        );
        assert_eq!(fs::read(&files[0]).unwrap(), b"model");
        let files_again =
            resolve_files_with(&cache, &[source], &Progress::default(), &mut fetch).unwrap();
        assert_eq!(files_again, files);
        assert_eq!(calls, 1);
    }

    fn source() -> Source {
        Source {
            name: "model.bin".into(),
            url: "https://example.invalid/not-requested".into(),
            size: 5,
            sha256: format!("{:x}", Sha256::digest(b"model")),
        }
    }

    #[test]
    fn invalid_downloads_and_cancellation_preserve_existing_assets_and_remove_partials() {
        for bad in [
            b"wrong".as_slice(),
            b"short".as_slice(),
            b"sixsix".as_slice(),
        ] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("model.bin"), b"old").unwrap();
            let error = resolve_files_with(
                temp.path(),
                &[source()],
                &Progress::default(),
                &mut |_, path, _| {
                    fs::write(path, bad)?;
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("SHA-256"));
            assert_eq!(fs::read(temp.path().join("model.bin")).unwrap(), b"old");
            assert_eq!(
                fs::read_dir(temp.path()).unwrap().count(),
                2,
                "only old file and setup.lock remain"
            );
        }
        let temp = tempfile::tempdir().unwrap();
        let progress = Progress::default();
        progress
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut requested = false;
        assert!(
            resolve_files_with(temp.path(), &[source()], &progress, &mut |_, _, _| {
                requested = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!requested);
        assert!(!temp.path().join("model.bin").exists());
    }

    #[test]
    fn repaired_cache_preserves_the_bad_bytes_and_never_traverses_source_names() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("model.bin"), b"bad").unwrap();
        resolve_files_with(
            temp.path(),
            &[source()],
            &Progress::default(),
            &mut |_, path, _| {
                fs::write(path, b"model")?;
                Ok(())
            },
        )
        .unwrap();
        let retained: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .map(|item| item.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("invalid-")
            })
            .collect();
        assert_eq!(retained.len(), 1);
        assert_eq!(fs::read(&retained[0]).unwrap(), b"bad");
        let mut invalid = source();
        invalid.name = "../escape.bin".into();
        let mut requested = false;
        assert!(
            resolve_files_with(
                temp.path(),
                &[invalid],
                &Progress::default(),
                &mut |_, _, _| {
                    requested = true;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!requested);
        assert!(
            resolve_files_with(
                temp.path(),
                &[],
                &Progress::default(),
                &mut |_, _, _| Ok(())
            )
            .is_err()
        );
    }

    fn sdk_fixture() -> (Sdk, Vec<u8>) {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("Release/whisper-cli.exe", b"synthetic exe".as_slice()),
            ("Release/whisper.dll", b"synthetic dll".as_slice()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        (
            Sdk {
                directory: "test-sdk".into(),
                archive: Source {
                    name: "test.zip".into(),
                    url: "https://example.invalid/sdk".into(),
                    size: bytes.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                },
                files: [
                    ("whisper-cli.exe", b"synthetic exe".as_slice()),
                    ("whisper.dll", b"synthetic dll".as_slice()),
                ]
                .into_iter()
                .map(|(name, bytes)| SdkFile {
                    entry: format!("Release/{name}"),
                    name: name.into(),
                    size: bytes.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                })
                .collect(),
            },
            bytes,
        )
    }

    #[test]
    fn sdk_extracts_only_verified_files_reuses_then_repairs_without_replacing_bad_data() {
        let temp = tempfile::tempdir().unwrap();
        let (sdk, bytes) = sdk_fixture();
        let mut calls = 0;
        let mut fetch = |_: &Source, path: &Path, _: &Progress| {
            calls += 1;
            fs::write(path, &bytes).map_err(Into::into)
        };
        let executable =
            install_sdk_with(temp.path(), &sdk, &Progress::default(), &mut fetch).unwrap();
        assert_eq!(executable, temp.path().join("test-sdk/whisper-cli.exe"));
        assert_eq!(fs::read(&executable).unwrap(), b"synthetic exe");
        assert_eq!(
            install_sdk_with(temp.path(), &sdk, &Progress::default(), &mut fetch).unwrap(),
            executable
        );
        fs::remove_file(temp.path().join("test.zip")).unwrap();
        assert_eq!(
            install_sdk_with(
                temp.path(),
                &sdk,
                &Progress::default(),
                &mut |_, _, _| anyhow::bail!("complete installation must be usable offline")
            )
            .unwrap(),
            executable
        );
        fs::write(&executable, b"bad").unwrap();
        install_sdk_with(temp.path(), &sdk, &Progress::default(), &mut fetch).unwrap();
        assert_eq!(
            calls, 2,
            "archive is downloaded only when installation actually needs repair"
        );
        assert_eq!(fs::read(&executable).unwrap(), b"synthetic exe");
        let old = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("test-sdk.invalid-")
            })
            .unwrap();
        assert_eq!(fs::read(old.join("whisper-cli.exe")).unwrap(), b"bad");
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".setup-")
        }));
    }

    #[test]
    fn sdk_member_hash_mismatch_does_not_publish_or_erase_old_installation() {
        let temp = tempfile::tempdir().unwrap();
        let (mut sdk, bytes) = sdk_fixture();
        fs::create_dir(temp.path().join("test-sdk")).unwrap();
        fs::write(temp.path().join("test-sdk/keep.txt"), b"old").unwrap();
        sdk.files[1].sha256 = "0".repeat(64);
        assert!(
            install_sdk_with(
                temp.path(),
                &sdk,
                &Progress::default(),
                &mut |_, path, _| {
                    fs::write(path, &bytes)?;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(
            fs::read(temp.path().join("test-sdk/keep.txt")).unwrap(),
            b"old"
        );
        assert!(!temp.path().join("test-sdk/whisper-cli.exe").exists());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".setup-")
        }));
    }

    #[test]
    fn pinned_manifest_retains_expected_models_sdk_and_original_notices() {
        let manifest = manifest().unwrap();
        assert!(
            manifest
                .ctc
                .iter()
                .any(|s| s.name == "model_quantized.onnx")
        );
        assert!(manifest.ctc.iter().any(|s| s.name == "vocab.json"));
        assert!(manifest.ctc.iter().any(|s| s.name == "LICENSE.ctc"));
        assert!(manifest.whisper.iter().any(|s| s.name == "LICENSE.model"));
        assert!(
            manifest
                .whisper
                .iter()
                .any(|s| s.name == "LICENSE.whisper-cpp")
        );
        for source in manifest
            .ctc
            .iter()
            .chain(&manifest.whisper)
            .chain([&manifest.whisper_sdk.archive])
        {
            validate_source(source).unwrap();
        }
        assert_eq!(manifest.whisper_sdk.files.len(), 13);
    }

    #[test]
    fn stalled_headers_or_body_cancel_promptly_and_preserve_existing_cache() {
        use std::{net::TcpListener, sync::mpsc, time::Instant};
        for send_headers in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (ready, received) = mpsc::channel();
            let (release, wait_release) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0u8; 2048];
                assert!(stream.read(&mut request).unwrap() > 0);
                if send_headers {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    stream.flush().unwrap();
                }
                ready.send(()).unwrap();
                let _ = wait_release.recv_timeout(Duration::from_secs(4));
            });
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("model.bin"), b"old").unwrap();
            let mut source = source();
            source.url = format!("http://{address}/model");
            let progress = Progress::default();
            let cancelled = progress.clone();
            let cancel = std::thread::spawn(move || {
                received.recv_timeout(Duration::from_secs(5)).unwrap();
                let start = Instant::now();
                cancelled
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                start
            });
            let result = resolve_files_with(temp.path(), &[source], &progress, &mut download);
            let cancel_at = cancel.join().unwrap();
            let elapsed = cancel_at.elapsed();
            let _ = release.send(());
            server.join().unwrap();
            assert!(result.is_err());
            assert!(
                elapsed < Duration::from_secs(2),
                "cancel waited {elapsed:?} with headers={send_headers}"
            );
            assert!(format!("{:#}", result.unwrap_err()).contains("取り消し"));
            assert_eq!(fs::read(temp.path().join("model.bin")).unwrap(), b"old");
            assert_eq!(
                fs::read_dir(temp.path()).unwrap().count(),
                2,
                "old model and setup.lock only"
            );
        }
    }

    #[test]
    fn cancelled_blocking_resolution_releases_call_and_cache_lock_before_resolver_finishes() {
        use std::{sync::mpsc, time::Instant};
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("model.bin"), b"old").unwrap();
        let (begun, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let progress = Progress::default();
        let cancelled = progress.clone();
        let cancel = std::thread::spawn(move || {
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            cancelled
                .cancelled
                .store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let fetch_started = Instant::now();
        let network = NetworkRuntime::new();
        let mut blocking = Some((begun, released, done));
        let result = resolve_files_with(
            temp.path(),
            &[source()],
            &progress,
            &mut |_, _path, progress| {
                let (begun, released, done) = blocking.take().unwrap();
                network.handle()?.block_on(async {
                    tokio::task::spawn_blocking(move || {
                        begun.send(()).unwrap();
                        let _ = released.recv_timeout(Duration::from_millis(1200));
                        done.send(()).unwrap();
                    });
                    cancel_pending(std::future::pending::<Result<()>>(), progress).await
                })
            },
        );
        cancel.join().unwrap();
        let elapsed = fetch_started.elapsed();
        let _ = release.send(());
        finished.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(result.is_err());
        assert!(
            elapsed < Duration::from_millis(600),
            "cancel waited for blocking resolver: {elapsed:?}"
        );
        assert_eq!(fs::read(temp.path().join("model.bin")).unwrap(), b"old");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(temp.path().join("setup.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        network.shutdown();
    }

    #[test]
    fn resolver_retries_are_bounded_cancel_or_timeout_release_files_and_shutdown_is_finite() {
        use std::{
            net::{IpAddr, Ipv4Addr, SocketAddr},
            sync::{
                Arc, Condvar,
                atomic::{AtomicUsize, Ordering},
            },
            time::Instant,
        };
        struct Resolver {
            gate: Arc<(Mutex<bool>, Condvar)>,
            entered: Arc<AtomicUsize>,
            active: Arc<AtomicUsize>,
        }
        struct Abort(tokio::task::AbortHandle);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        impl reqwest::dns::Resolve for Resolver {
            fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
                let gate = self.gate.clone();
                let entered = self.entered.clone();
                let active = self.active.clone();
                Box::pin(async move {
                    let task = tokio::task::spawn_blocking(move || {
                        entered.fetch_add(1, Ordering::SeqCst);
                        active.fetch_add(1, Ordering::SeqCst);
                        let (lock, wake) = &*gate;
                        let closed = lock.lock().unwrap();
                        let _ = wake
                            .wait_timeout_while(closed, Duration::from_secs(5), |open| !*open)
                            .unwrap();
                        active.fetch_sub(1, Ordering::SeqCst);
                        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9);
                        Ok::<reqwest::dns::Addrs, Box<dyn std::error::Error + Send + Sync>>(
                            Box::new(std::iter::once(address)),
                        )
                    });
                    let _abort = Abort(task.abort_handle());
                    task.await
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                })
            }
        }
        let network = NetworkRuntime::new();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("model.bin"), b"old").unwrap();
        for attempt in 0..5 {
            let progress = Progress::default();
            let cancelled = progress.clone();
            let cancel = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(70));
                cancelled.cancelled.store(true, Ordering::Relaxed);
            });
            let mut source = source();
            source.url = "http://cancelled-dns.invalid/model".into();
            let resolver = Arc::new(Resolver {
                gate: gate.clone(),
                entered: entered.clone(),
                active: active.clone(),
            });
            let start = Instant::now();
            let result = resolve_files_with(
                temp.path(),
                &[source],
                &progress,
                &mut |source, path, progress| {
                    download_with_client(&network, source, path, progress, || {
                        Ok(reqwest::Client::builder()
                            .no_proxy()
                            .dns_resolver(resolver.clone())
                            .timeout(Duration::from_secs(2))
                            .build()?)
                    })
                },
            );
            cancel.join().unwrap();
            assert!(result.is_err());
            assert!(
                start.elapsed() < Duration::from_millis(700),
                "cancel attempt{attempt} waited for resolver"
            );
            assert!(active.load(Ordering::SeqCst) <= 2);
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(temp.path().join("setup.lock"))
                .unwrap();
            lock.try_lock().unwrap();
        }
        assert_eq!(
            entered.load(Ordering::SeqCst),
            2,
            "retries must not spawn unbounded DNS workers"
        );
        // Both DNS workers still run. Timeout and app shutdown must remain finite.
        let mut source = source();
        source.url = "http://timeout-dns.invalid/model".into();
        let resolver = Arc::new(Resolver {
            gate: gate.clone(),
            entered: entered.clone(),
            active: active.clone(),
        });
        let start = Instant::now();
        let result = resolve_files_with(
            temp.path(),
            &[source],
            &Progress::default(),
            &mut |source, path, progress| {
                download_with_client(&network, source, path, progress, || {
                    Ok(reqwest::Client::builder()
                        .no_proxy()
                        .dns_resolver(resolver.clone())
                        .timeout(Duration::from_millis(80))
                        .build()?)
                })
            },
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(700));
        let start = Instant::now();
        network.shutdown();
        assert!(start.elapsed() < Duration::from_millis(600));
        assert!(
            network.handle().is_err(),
            "shutdown must not recreate the runtime"
        );
        assert_eq!(
            active.load(Ordering::SeqCst),
            2,
            "remaining DNS is distinct from completed cancellation"
        );
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let deadline = Instant::now() + Duration::from_secs(2);
        while active.load(Ordering::SeqCst) != 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            entered.load(Ordering::SeqCst),
            2,
            "cancelled queued DNS must never run afterwards"
        );
        assert_eq!(fs::read(temp.path().join("model.bin")).unwrap(), b"old");
    }

    #[test]
    fn network_download_accepts_exact_bytes_and_rejects_truncation_or_excess() {
        use std::net::TcpListener;
        for body in [b"model".as_slice(), b"bad".as_slice(), b"sixsix".as_slice()] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body = body.to_vec();
            let should_pass = body == b"model";
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
            let mut source = source();
            source.url = format!("http://{address}/model");
            let result =
                resolve_files_with(temp.path(), &[source], &Progress::default(), &mut download);
            server.join().unwrap();
            assert_eq!(result.is_ok(), should_pass);
            if should_pass {
                assert_eq!(fs::read(temp.path().join("model.bin")).unwrap(), b"model");
            } else {
                assert!(!temp.path().join("model.bin").exists());
            }
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part")
            }));
        }
    }

    #[test]
    #[ignore = "requires an explicitly supplied official ZIP; never downloads or starts an executable"]
    fn official_whisper_sdk_installs_from_verified_local_archive_and_reuses_offline() {
        let archive = std::env::var_os("KARAOKE_WHISPER_SDK_ARCHIVE")
            .expect("set KARAOKE_WHISPER_SDK_ARCHIVE to the fixed official v1.9.2 ZIP");
        let sdk = manifest().unwrap().whisper_sdk;
        assert!(
            verified(Path::new(&archive), sdk.archive.size, &sdk.archive.sha256)
                .unwrap()
                .is_some()
        );
        let temp = tempfile::tempdir().unwrap();
        let mut fetches = 0;
        let executable = install_sdk_with(
            temp.path(),
            &sdk,
            &Progress::default(),
            &mut |_, path, _| {
                fetches += 1;
                fs::copy(&archive, path)?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(fetches, 1);
        assert!(sdk_valid(executable.parent().unwrap(), &sdk).unwrap());
        assert_eq!(
            fs::read_dir(executable.parent().unwrap()).unwrap().count(),
            sdk.files.len()
        );
        fs::remove_file(temp.path().join(&sdk.archive.name)).unwrap();
        assert_eq!(
            install_sdk_with(
                temp.path(),
                &sdk,
                &Progress::default(),
                &mut |_, _, _| anyhow::bail!("offline reuse fetched again")
            )
            .unwrap(),
            executable
        );
    }
}
