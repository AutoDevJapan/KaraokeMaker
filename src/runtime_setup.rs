use crate::Progress;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    thread,
    time::Duration,
};

const DIRECTORY: &str = "onnxruntime-cuda-1.24.4";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Package {
    name: String,
    version: String,
    url: String,
    size: u64,
    sha256: String,
    dlls: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct InstalledFile {
    path: PathBuf,
    package: String,
    size: u64,
    sha256: String,
    license: bool,
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    version: u32,
    packages: Vec<Package>,
    files: Vec<InstalledFile>,
}

pub(crate) fn local_root(models: &Path) -> Result<PathBuf> {
    Ok(models
        .parent()
        .context("モデルの保存先を確認できません。")?
        .join("runtime")
        .join(DIRECTORY))
}

fn packages() -> Result<Vec<Package>> {
    Ok(serde_json::from_str(include_str!(
        "../resources/cuda-runtime.json"
    ))?)
}

pub(crate) fn required_dlls_present(root: &Path) -> Result<bool> {
    let packages = packages()?;
    Ok(!packages.is_empty()
        && packages
            .iter()
            .flat_map(|package| &package.dlls)
            .all(|name| root.join(name).is_file()))
}

#[cfg(feature = "ort")]
pub(crate) fn install(models: &Path, progress: &Progress) -> Result<PathBuf> {
    install_with(models, &packages()?, progress, &mut download)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn verified_archive(path: &Path, package: &Package) -> Result<Option<fs::File>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != package.size {
        return Ok(None);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if format!("{:x}", digest.finalize()) != package.sha256 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(Some(file))
}

fn relative_file(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn installed_valid(root: &Path, expected: &[Package]) -> Result<bool> {
    let bytes = match fs::read(root.join("runtime-manifest.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let receipt: Receipt = match serde_json::from_slice(&bytes) {
        Ok(receipt) => receipt,
        Err(_) => return Ok(false),
    };
    if expected.is_empty()
        || receipt.version != 1
        || receipt.packages != expected
        || receipt.files.is_empty()
    {
        return Ok(false);
    }
    for package in expected {
        for dll in &package.dlls {
            if receipt
                .files
                .iter()
                .filter(|file| {
                    file.package == package.name && !file.license && file.path == Path::new(dll)
                })
                .count()
                != 1
            {
                return Ok(false);
            }
        }
        if !receipt
            .files
            .iter()
            .any(|file| file.package == package.name && file.license)
        {
            return Ok(false);
        }
    }
    for file in receipt.files {
        if !relative_file(&file.path) {
            return Ok(false);
        }
        let path = root.join(file.path);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != file.size
            || hash_file(&path)? != file.sha256
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(feature = "ort")]
fn download(package: &Package, destination: &Path, progress: &Progress) -> Result<()> {
    let mut response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1200))
        .build()?
        .get(&package.url)
        .send()?
        .error_for_status()?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        progress.check()?;
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= package.size,
            "実行部品のサイズが公式の固定値を超えました。"
        );
        file.write_all(&buffer[..count])?;
        if total / (8 * 1024 * 1024) != (total - count as u64) / (8 * 1024 * 1024) {
            progress.set(format!(
                "高音質の実行部品を準備しています：{}（{} / {} MB）",
                package.name,
                total / 1_000_000,
                package.size / 1_000_000
            ));
        }
    }
    file.sync_all()?;
    Ok(())
}

fn extract(
    archive: fs::File,
    stage: &Path,
    package: &Package,
    progress: &Progress,
) -> Result<Vec<InstalledFile>> {
    // Parse the same open file whose bytes were hashed, including when a cache name is renamed.
    let mut zip = zip::ZipArchive::new(archive)?;
    let mut installed = Vec::new();
    for index in 0..zip.len() {
        progress.check()?;
        let mut entry = zip.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let leaf = name.rsplit('/').next().unwrap_or_default();
        let dll = package.dlls.iter().any(|dll| dll == leaf)
            && if package.name == "onnxruntime" {
                name.starts_with("runtimes/win-x64/native/")
            } else {
                name.contains("/bin/")
            };
        let lower = leaf.to_ascii_lowercase();
        let license = lower.contains("license")
            || lower.contains("licence")
            || lower.contains("notice")
            || lower == "copying";
        if !dll && !license {
            continue;
        }
        ensure!(
            !name.starts_with('/')
                && !name.contains(':')
                && !name.split('/').any(|part| part == ".."),
            "実行部品の格納パスが不正です。"
        );
        ensure!(
            entry.enclosed_name().is_some()
                && entry
                    .unix_mode()
                    .is_none_or(|mode| mode & 0o170000 != 0o120000),
            "実行部品のリンクを展開できません。"
        );
        let relative = if dll {
            PathBuf::from(leaf)
        } else {
            PathBuf::from("licenses").join(&package.name).join(&name)
        };
        ensure!(relative_file(&relative), "実行部品の格納パスが不正です。");
        let destination = stage.join(&relative);
        fs::create_dir_all(destination.parent().context("展開先を確認できません。")?)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let size = std::io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
        drop(file);
        ensure!(
            size > 0 && size == entry.size(),
            "実行部品の展開サイズが一致しません。"
        );
        installed.push(InstalledFile {
            path: relative,
            package: package.name.clone(),
            size,
            sha256: hash_file(&destination)?,
            license: !dll,
        });
    }
    for dll in &package.dlls {
        ensure!(
            installed
                .iter()
                .filter(|file| !file.license && file.path == Path::new(dll))
                .count()
                == 1,
            "必要な実行部品がありません：{dll}"
        );
    }
    ensure!(
        installed.iter().any(|file| file.license),
        "実行部品のライセンス原文がありません。"
    );
    Ok(installed)
}

fn install_with(
    models: &Path,
    packages: &[Package],
    progress: &Progress,
    fetch: &mut impl FnMut(&Package, &Path, &Progress) -> Result<()>,
) -> Result<PathBuf> {
    ensure!(
        !packages.is_empty() && packages.iter().all(|package| !package.dlls.is_empty()),
        "実行部品の一覧が空です。"
    );
    let root = local_root(models)?;
    let parent = root
        .parent()
        .context("実行部品の保存先を確認できません。")?;
    fs::create_dir_all(parent)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join("setup.lock"))?;
    loop {
        progress.check()?;
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {
                progress.set("高音質の実行部品の初期準備が終わるのを待っています。");
                thread::sleep(Duration::from_millis(100));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    if installed_valid(&root, packages)? {
        return Ok(root);
    }
    if root.exists() {
        ensure!(
            !fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "実行部品の保存先がリンクです。"
        );
        let saved = parent.join(format!("runtime-invalid-{}", uuid::Uuid::new_v4()));
        fs::rename(&root, &saved)
            .with_context(|| format!("不完全な実行部品を退避できません：{}", root.display()))?;
        progress.set(format!(
            "不完全な実行部品を{}へ保持し、導入し直します。",
            saved.display()
        ));
    }
    let downloads = parent.join("packages");
    fs::create_dir_all(&downloads)?;
    let stage = parent.join(format!(".setup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage)?;
    let result = (|| -> Result<()> {
        let mut files = Vec::new();
        for (index, package) in packages.iter().enumerate() {
            progress.check()?;
            progress.set(format!(
                "高音質の実行部品を準備しています（{} / {}）：{}",
                index + 1,
                packages.len(),
                package.name
            ));
            let archive = downloads.join(format!("{}-{}.zip", package.name, package.version));
            let verified = if let Some(file) = verified_archive(&archive, package)? {
                file
            } else {
                if archive.exists() {
                    fs::remove_file(&archive)?;
                }
                let part = stage.join(format!("{}.part", uuid::Uuid::new_v4()));
                fetch(package, &part, progress)?;
                let file = verified_archive(&part, package)?.with_context(|| {
                    format!(
                        "実行部品の公式サイズかSHA-256の照合に失敗しました：{}",
                        package.name
                    )
                })?;
                fs::rename(part, &archive)?;
                file
            };
            files.extend(extract(verified, &stage, package, progress)?);
        }
        let receipt = Receipt {
            version: 1,
            packages: packages.to_vec(),
            files,
        };
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(stage.join("runtime-manifest.json"))?;
        file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
        file.sync_all()?;
        drop(file);
        ensure!(
            installed_valid(&stage, packages)?,
            "実行部品の導入結果が一致しません。"
        );
        fs::rename(&stage, &root)?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(cleanup) = fs::remove_dir_all(&stage) {
            return Err(anyhow::anyhow!(
                "{error:#}。未完了の導入先{}の回収にも失敗しました：{cleanup}",
                stage.display()
            ));
        }
        return Err(error);
    }
    // Completed archives are disposable. A cleanup failure does not invalidate installed DLLs.
    for package in packages {
        let archive = downloads.join(format!("{}-{}.zip", package.name, package.version));
        if let Err(error) = fs::remove_file(&archive) {
            let message = format!(
                "実行部品は導入済みですが、取得ファイル{}の回収に失敗しました：{error}",
                archive.display()
            );
            eprintln!("{message}");
            let log_path = root.join("setup-cleanup-warning.txt");
            let logged = (|| -> std::io::Result<()> {
                let mut log = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)?;
                writeln!(log, "{message}")?;
                log.sync_all()
            })();
            if let Err(log_error) = logged {
                eprintln!(
                    "回収記録{}の保存にも失敗しました：{log_error}",
                    log_path.display()
                );
            }
        }
    }
    progress.set("高音質の実行部品の準備ができました。");
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{fs, io::Write};

    fn fixture() -> (Package, Vec<u8>) {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("official/bin/test.dll", b"fixture binary".as_slice()),
            ("official/LICENSE", b"Original license bytes\n".as_slice()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        (
            Package {
                name: "test-runtime".into(),
                version: "1".into(),
                url: "https://example.invalid/not-requested".into(),
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                dlls: vec!["test.dll".into()],
            },
            bytes,
        )
    }

    #[test]
    fn first_setup_installs_verified_runtime_and_preserves_license_before_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("data/models");
        let (package, archive) = fixture();
        let mut calls = 0;
        let root = install_with(
            &models,
            std::slice::from_ref(&package),
            &Progress::default(),
            &mut |_, path, _| {
                calls += 1;
                fs::write(path, &archive)?;
                Ok(())
            },
        )
        .unwrap();
        assert!(root.starts_with(temp.path().join("data/runtime")));
        assert_eq!(fs::read(root.join("test.dll")).unwrap(), b"fixture binary");
        assert_eq!(
            fs::read(root.join("licenses/test-runtime/official/LICENSE")).unwrap(),
            b"Original license bytes\n"
        );
        assert_eq!(calls, 1);
        let reused = install_with(&models, &[package], &Progress::default(), &mut |_, _, _| {
            panic!("a verified installation must not download again")
        })
        .unwrap();
        assert_eq!(root, reused);
    }

    #[test]
    fn official_pins_are_nonempty_and_cover_all_runtime_dependencies() {
        let packages = packages().unwrap();
        assert_eq!(packages.len(), 5);
        assert_eq!(
            packages
                .iter()
                .map(|package| package.dlls.len())
                .sum::<usize>(),
            15
        );
        for package in packages {
            assert!(
                package.url.starts_with("https://api.nuget.org/")
                    || package
                        .url
                        .starts_with("https://developer.download.nvidia.com/")
            );
            assert!(package.size > 0 && package.sha256.len() == 64);
            assert!(package.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn dependency_probe_requires_the_cuda_provider_and_cudnn_child_libraries() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!required_dlls_present(temp.path()).unwrap());
        for package in packages().unwrap() {
            for dll in package.dlls {
                fs::write(temp.path().join(dll), b"presence fixture, not executable").unwrap();
            }
        }
        assert!(required_dlls_present(temp.path()).unwrap());
        fs::remove_file(temp.path().join("cudnn_ops64_9.dll")).unwrap();
        assert!(!required_dlls_present(temp.path()).unwrap());
    }

    #[test]
    fn corrupt_download_is_not_extracted_or_published_and_preserves_original_files() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("data/models");
        fs::create_dir_all(&models).unwrap();
        fs::write(models.join("existing-model"), b"keep model").unwrap();
        let (package, mut archive) = fixture();
        archive[20] ^= 1;
        let error = install_with(
            &models,
            &[package],
            &Progress::default(),
            &mut |_, path, _| {
                fs::write(path, &archive)?;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("照合"));
        assert!(!local_root(&models).unwrap().exists());
        assert_eq!(
            fs::read(models.join("existing-model")).unwrap(),
            b"keep model"
        );
        assert!(
            fs::read_dir(temp.path().join("data/runtime"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".setup-"))
        );
    }

    #[test]
    fn invalid_cached_dll_is_kept_separately_and_reinstalled_from_verified_source() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("data/models");
        let (package, archive) = fixture();
        let mut fetch = |_: &Package, path: &Path, _: &Progress| -> Result<()> {
            fs::write(path, &archive)?;
            Ok(())
        };
        let root = install_with(
            &models,
            std::slice::from_ref(&package),
            &Progress::default(),
            &mut fetch,
        )
        .unwrap();
        fs::write(root.join("test.dll"), b"fixture binarY").unwrap();
        let repaired = install_with(&models, &[package], &Progress::default(), &mut fetch).unwrap();
        assert_eq!(repaired, root);
        assert_eq!(fs::read(root.join("test.dll")).unwrap(), b"fixture binary");
        let held: Vec<_> = fs::read_dir(root.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("runtime-invalid-")
            })
            .collect();
        assert_eq!(held.len(), 1);
        assert_eq!(
            fs::read(held[0].join("test.dll")).unwrap(),
            b"fixture binarY"
        );
    }

    #[test]
    fn missing_license_and_unsafe_archive_path_are_rejected_before_publication() {
        for entries in [
            vec![("official/bin/test.dll", b"fixture binary".as_slice())],
            vec![
                ("../bin/test.dll", b"fixture binary".as_slice()),
                ("LICENSE", b"license".as_slice()),
            ],
            vec![
                ("official/bin/test.dll", b"fixture binary".as_slice()),
                ("LICENSE", b"".as_slice()),
            ],
        ] {
            let temp = tempfile::tempdir().unwrap();
            let models = temp.path().join("data/models");
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            for (name, bytes) in entries {
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(bytes).unwrap();
            }
            let bytes = zip.finish().unwrap().into_inner();
            let (mut package, _) = fixture();
            package.size = bytes.len() as u64;
            package.sha256 = format!("{:x}", Sha256::digest(&bytes));
            assert!(
                install_with(
                    &models,
                    &[package],
                    &Progress::default(),
                    &mut |_, path, _| {
                        fs::write(path, &bytes)?;
                        Ok(())
                    }
                )
                .is_err()
            );
            assert!(!local_root(&models).unwrap().exists());
            assert!(!temp.path().join("bin/test.dll").exists());
        }
    }

    #[test]
    fn cancelled_setup_cleans_partial_files_and_empty_sources_never_pass() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("data/models");
        let (package, _) = fixture();
        assert!(
            install_with(
                &models,
                &[package],
                &Progress::default(),
                &mut |_, path, _| {
                    fs::write(path, b"partial")?;
                    anyhow::bail!("cancelled fixture download")
                }
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled fixture download")
        );
        assert!(!local_root(&models).unwrap().exists());
        assert!(
            install_with(&models, &[], &Progress::default(), &mut |_, _, _| panic!(
                "empty source must not fetch"
            ))
            .is_err()
        );
        assert!(
            fs::read_dir(temp.path().join("data/runtime"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".setup-"))
        );
    }

    #[test]
    fn extraction_consumes_the_same_verified_archive_after_its_name_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let (package, archive) = fixture();
        let path = temp.path().join("package.zip");
        fs::write(&path, &archive).unwrap();
        let verified = verified_archive(&path, &package).unwrap().unwrap();
        fs::rename(&path, temp.path().join("original.zip")).unwrap();
        fs::write(&path, b"replacement is not verified").unwrap();
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        extract(verified, &stage, &package, &Progress::default()).unwrap();
        assert_eq!(fs::read(stage.join("test.dll")).unwrap(), b"fixture binary");
    }

    #[test]
    fn interrupted_later_package_reuses_the_earlier_verified_archive_on_retry() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("data/models");
        let (first, first_bytes) = fixture();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("official/bin/second.dll", b"second binary".as_slice()),
            ("official/LICENSE", b"second license".as_slice()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        let second_bytes = zip.finish().unwrap().into_inner();
        let second = Package {
            name: "second-runtime".into(),
            dlls: vec!["second.dll".into()],
            size: second_bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&second_bytes)),
            ..first.clone()
        };
        let sources = [first.clone(), second];
        let error = install_with(
            &models,
            &sources,
            &Progress::default(),
            &mut |package, path, _| {
                if package.name != first.name {
                    anyhow::bail!("interrupted second download");
                }
                fs::write(path, &first_bytes)?;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("interrupted second download"));
        assert!(!local_root(&models).unwrap().exists());
        let mut retry_downloads = 0;
        let root = install_with(
            &models,
            &sources,
            &Progress::default(),
            &mut |package, path, _| {
                assert_ne!(
                    package.name, first.name,
                    "the first verified archive must be reused"
                );
                retry_downloads += 1;
                fs::write(path, &second_bytes)?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(retry_downloads, 1);
        assert_eq!(fs::read(root.join("test.dll")).unwrap(), b"fixture binary");
        assert_eq!(fs::read(root.join("second.dll")).unwrap(), b"second binary");
    }
}
