use anyhow::Result;
#[cfg(all(feature = "ort", windows))]
use anyhow::ensure;

#[cfg(feature = "ort")]
static INITIALIZED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

#[cfg(feature = "ort")]
fn existing_root() -> Result<Option<std::path::PathBuf>> {
    let adjacent = std::env::current_exe()?
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("実行先を確認できません。"))?;
    let fallback = std::path::PathBuf::from("D:/CC/Tools/onnxruntime-cuda/1.24.4/runtime");
    let local = crate::runtime_setup::local_root(&adjacent.join("data/models"))?;
    for root in [adjacent.clone(), adjacent.join("runtime"), local, fallback] {
        if crate::runtime_setup::required_dlls_present(&root)? {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

#[cfg(feature = "ort")]
fn initialize_at(root: &std::path::Path) -> Result<()> {
    let mut initialized = INITIALIZED
        .lock()
        .map_err(|_| anyhow::anyhow!("実行部品の初期化状態を確認できません。"))?;
    if *initialized {
        return Ok(());
    }
    eprintln!("ONNX実行部品: {}", root.display());
    #[cfg(windows)]
    {
        use windows::{
            Win32::System::LibraryLoader::{
                AddDllDirectory, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, SetDefaultDllDirectories,
            },
            core::HSTRING,
        };
        let name = HSTRING::from(root.display().to_string());
        unsafe {
            SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)?;
            let cookie = AddDllDirectory(&name);
            ensure!(!cookie.is_null(), "実行部品の場所を登録できません。");
        }
    }
    ort::init_from(root.join("onnxruntime.dll").display().to_string()).commit()?;
    *initialized = true;
    Ok(())
}

#[cfg(feature = "ort")]
pub fn initialize(_cuda: bool) -> Result<()> {
    if *INITIALIZED
        .lock()
        .map_err(|_| anyhow::anyhow!("実行部品の初期化状態を確認できません。"))?
    {
        return Ok(());
    }
    let root = existing_root()?
        .ok_or_else(|| anyhow::anyhow!("高音質の分離に必要なCUDA実行部品がありません。"))?;
    initialize_at(&root)
}

/// Install in the app's data folder, including fixed-source verification and original notices.
#[cfg(feature = "ort")]
pub fn setup_runtime(
    models: &std::path::Path,
    progress: &crate::Progress,
) -> Result<std::path::PathBuf> {
    let root = crate::runtime_setup::install(models, progress)?;
    initialize_at(&root)?;
    Ok(root)
}

#[cfg(not(feature = "ort"))]
pub fn setup_runtime(
    _models: &std::path::Path,
    _progress: &crate::Progress,
) -> Result<std::path::PathBuf> {
    anyhow::bail!("このビルドにはONNX実行部品の接続が含まれていません。")
}

#[cfg(feature = "ort")]
pub fn initialize_for_model(model: &std::path::Path, progress: &crate::Progress) -> Result<()> {
    let models = model
        .parent()
        .ok_or_else(|| anyhow::anyhow!("モデルの保存先を確認できません。"))?;
    let local = crate::runtime_setup::local_root(models)?;
    if local.exists() {
        setup_runtime(models, progress)?;
    } else if let Some(existing) = existing_root()? {
        initialize_at(&existing)?;
    } else {
        setup_runtime(models, progress)?;
    }
    Ok(())
}

#[cfg(not(feature = "ort"))]
pub fn initialize(_cuda: bool) -> Result<()> {
    anyhow::bail!("このビルドにはONNX実行部品の接続が含まれていません。")
}
