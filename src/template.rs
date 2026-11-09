//! Local, project-scoped template settings. Assets are referenced, never uploaded.
use crate::presentation::VisualStyle;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub version: u32,
    pub project: String,
    pub style: VisualStyle,
    pub background_image: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    /// Normalized safe-area margins, independently stored for each orientation.
    pub horizontal_margin: f64,
    pub vertical_margin: f64,
    pub max_lines: u8,
}

impl Template {
    pub fn validate(&self, project: &str) -> Result<()> {
        ensure!(self.version == 1, "対応していないテンプレート版です。");
        ensure!(
            !project.trim().is_empty() && self.project == project,
            "別の制作プロジェクトのテンプレートです。"
        );
        self.style.validate()?;
        for margin in [self.horizontal_margin, self.vertical_margin] {
            ensure!(
                margin.is_finite() && (0.0..=0.4).contains(&margin),
                "余白は0から0.4の範囲にしてください。"
            );
        }
        ensure!(
            (1..=4).contains(&self.max_lines),
            "字幕の行数は1から4にしてください。"
        );
        for path in [&self.background_image, &self.logo].into_iter().flatten() {
            ensure!(
                path.is_absolute() && path.is_file(),
                "素材の絶対パスを確認してください。"
            );
        }
        Ok(())
    }

    pub fn load(path: &Path, project: &str) -> Result<Self> {
        let template: Self = serde_json::from_slice(&fs::read(path)?)?;
        template.validate(project)?;
        Ok(template)
    }

    /// New revisions are new files; existing settings are never overwritten.
    pub fn save_new(&self, path: &Path, project: &str) -> Result<()> {
        self.validate(project)?;
        let bytes = serde_json::to_vec_pretty(self)?;
        write_new(path, |output| output.write_all(&bytes))
    }
}

fn write_new(path: &Path, write: impl FnOnce(&mut fs::File) -> std::io::Result<()>) -> Result<()> {
    write_new_with(path, write, |path| fs::remove_file(path))
}

fn write_new_with(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = write(&mut output).and_then(|()| output.sync_all());
    drop(output);
    if let Err(error) = result {
        if let Err(cleanup) = remove(path) {
            return Err(error).map_err(|error| anyhow::anyhow!("保存に失敗しました: {error}。未完成ファイル {} の回収にも失敗しました: {cleanup}", path.display()));
        }
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template() -> Template {
        Template {
            version: 1,
            project: "project-a".into(),
            style: VisualStyle::default(),
            background_image: None,
            logo: None,
            horizontal_margin: 0.1,
            vertical_margin: 0.15,
            max_lines: 2,
        }
    }
    #[test]
    fn failed_cleanup_reports_original_error_and_remaining_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.json");
        let error = write_new_with(
            &path,
            |file| {
                file.write_all(b"partial")?;
                Err(std::io::Error::other("original write failure"))
            },
            |_| Err(std::io::Error::other("cleanup failure")),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("original write failure"));
        assert!(message.contains("cleanup failure"));
        assert!(message.contains(&path.display().to_string()));
        assert_eq!(fs::read(&path).unwrap(), b"partial");
    }
    #[test]
    fn interrupted_new_write_is_removed_and_existing_file_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("template.json");
        let result = write_new(&path, |output| {
            output.write_all(b"partial")?;
            Err(std::io::Error::other("injected write failure"))
        });
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("injected write failure")
        );
        assert!(!path.exists());
        fs::write(&path, b"original").unwrap();
        let mut called = false;
        assert!(
            write_new(&path, |_| {
                called = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!called);
        assert_eq!(fs::read(path).unwrap(), b"original");
    }
    #[test]
    fn saved_settings_roundtrip_without_overwriting_or_crossing_projects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("template.json");
        template().save_new(&path, "project-a").unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(Template::load(&path, "project-a").unwrap().max_lines, 2);
        assert!(Template::load(&path, "project-b").is_err());
        assert!(template().save_new(&path, "project-a").is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn invalid_layout_version_and_missing_assets_are_rejected_before_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.json");
        let mut value = template();
        value.horizontal_margin = f64::NAN;
        assert!(value.save_new(&path, "project-a").is_err());
        assert!(!path.exists());
        value = template();
        value.version = 2;
        assert!(value.validate("project-a").is_err());
        value = template();
        value.logo = Some(dir.path().join("missing.png"));
        assert!(value.validate("project-a").is_err());
    }
}
