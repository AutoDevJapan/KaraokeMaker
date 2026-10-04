use anyhow::Result;
#[cfg(feature = "ort")]
use anyhow::ensure;

#[cfg(feature = "ort")]
pub fn initialize(_cuda: bool) -> Result<()> {
    let adjacent = std::env::current_exe()?
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("実行先を確認できません。"))?;
    let full = |root: &std::path::Path| {
        root.join("onnxruntime.dll").is_file()
            && [
                "onnxruntime_providers_cuda.dll",
                "cudart64_12.dll",
                "cublas64_12.dll",
                "cublasLt64_12.dll",
                "cudnn64_9.dll",
            ]
            .iter()
            .all(|file| root.join(file).is_file())
    };
    let fallback = std::path::PathBuf::from("D:/CC/Tools/onnxruntime-cuda/1.24.4/runtime");
    let root = if full(&adjacent) { adjacent } else { fallback };
    ensure!(
        full(&root),
        "高音質の分離に必要なCUDA実行部品がありません。アプリの導入先を確認してください。"
    );
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
    Ok(())
}

#[cfg(not(feature = "ort"))]
pub fn initialize(_cuda: bool) -> Result<()> {
    anyhow::bail!("このビルドにはONNX実行部品の接続が含まれていません。")
}
