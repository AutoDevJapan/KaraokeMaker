use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
pub fn read_folder(folder: &Path) -> Result<Vec<(String, String)>> {
    use windows::{
        Globalization::Language,
        Graphics::Imaging::BitmapDecoder,
        Media::Ocr::OcrEngine,
        Storage::{FileAccessMode, StorageFile},
        core::HSTRING,
    };
    let language = Language::CreateLanguage(&HSTRING::from("en-US"))?;
    let engine = OcrEngine::TryCreateFromLanguage(&language)
        .or_else(|_| OcrEngine::TryCreateFromUserProfileLanguages())?;
    let mut files = std::fs::read_dir(folder)?.collect::<Result<Vec<_>, _>>()?;
    files.sort_by_key(|entry| entry.file_name());
    let mut results = Vec::new();
    for entry in files {
        if entry.path().extension().is_none_or(|ext| ext != "png") {
            continue;
        }
        let absolute = entry.path().canonicalize()?;
        let path = absolute
            .display()
            .to_string()
            .trim_start_matches("\\\\?\\")
            .to_string();
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))?.get()?;
        let stream = file.OpenAsync(FileAccessMode::Read)?.get()?;
        let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
        let bitmap = decoder.GetSoftwareBitmapAsync()?.get()?;
        let result = engine.RecognizeAsync(&bitmap)?.get()?;
        results.push((
            entry.file_name().to_string_lossy().to_string(),
            result.Text()?.to_string(),
        ));
    }
    Ok(results)
}

#[cfg(not(windows))]
pub fn read_folder(_folder: &Path) -> Result<Vec<(String, String)>> {
    anyhow::bail!("字幕画像の読み取りはWindowsで利用できます。")
}
