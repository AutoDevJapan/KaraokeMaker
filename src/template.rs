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
    #[serde(default)]
    pub placement: Option<Placement>,
}

/// Normalized rectangles on the output frame, shared across resolutions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Pixel edges inside the requested box. Rendering and validation share this rounding.
    pub(crate) fn pixel_bounds(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        (
            (self.x * width as f64).ceil() as u32,
            (self.y * height as f64).ceil() as u32,
            ((self.x + self.width) * width as f64).floor() as u32,
            ((self.y + self.height) * height as f64).floor() as u32,
        )
    }
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.x + self.width <= 1.0
            && self.y + self.height <= 1.0
    }
    fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub title: Rect,
    pub subtitles: Rect,
    #[serde(default)]
    pub avoid: Vec<Rect>,
}

impl Placement {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.title.valid() && self.subtitles.valid() && self.avoid.iter().all(Rect::valid),
            "配置の矩形は画面内の有限な値で指定してください。"
        );
        ensure!(
            !self.title.overlaps(&self.subtitles),
            "タイトルと字幕の配置が重なっています。"
        );
        ensure!(
            self.avoid
                .iter()
                .all(|r| !r.overlaps(&self.title) && !r.overlaps(&self.subtitles)),
            "配置が回避領域と重なっています。"
        );
        Ok(())
    }
}

impl Template {
    pub fn validate(&self, project: &str) -> Result<()> {
        ensure!(self.version == 1, "対応していないテンプレート版です。");
        ensure!(
            !project.trim().is_empty() && self.project == project,
            "別の制作プロジェクトのテンプレートです。"
        );
        self.style.validate()?;
        if let Some(placement) = &self.placement {
            placement.validate()?;
        }
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
            // Only PNG or JPEG files by name and by file signature, so a renamed playlist
            // or video is refused before ffmpeg opens it. This is not a full image check:
            // ffmpeg is also told to read the file as a still image (-f image2).
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            ensure!(
                matches!(extension.as_deref(), Some("png" | "jpg" | "jpeg")),
                "素材はPNGまたはJPEGの画像にしてください。"
            );
            let mut head = [0u8; 8];
            let read = fs::File::open(path)
                .and_then(|mut file| std::io::Read::read(&mut file, &mut head))?;
            let png = head[..read].starts_with(b"\x89PNG\r\n\x1a\n");
            let jpeg = head[..read].starts_with(&[0xff, 0xd8, 0xff]);
            ensure!(
                if extension.as_deref() == Some("png") {
                    png
                } else {
                    jpeg
                },
                "素材の拡張子と中身（PNGかJPEG）が一致しません。"
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
    #[test]
    fn placement_roundtrips_and_rejects_overlap_while_old_templates_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut value = template();
        let old = serde_json::to_value(&value).unwrap();
        let mut old = old.as_object().unwrap().clone();
        old.remove("placement");
        let restored: Template = serde_json::from_value(old.into()).unwrap();
        assert!(restored.placement.is_none());
        value.placement = Some(Placement {
            title: Rect {
                x: 0.2,
                y: 0.15,
                width: 0.6,
                height: 0.15,
            },
            subtitles: Rect {
                x: 0.2,
                y: 0.6,
                width: 0.6,
                height: 0.2,
            },
            avoid: vec![Rect {
                x: 0.3,
                y: 0.35,
                width: 0.3,
                height: 0.2,
            }],
        });
        let path = dir.path().join("placement.json");
        value.save_new(&path, "project-a").unwrap();
        let restored = Template::load(&path, "project-a").unwrap();
        assert_eq!(restored.placement.unwrap().avoid[0].y, 0.35);
        value.placement.as_mut().unwrap().avoid[0].y = 0.6;
        assert!(value.validate("project-a").is_err());
        value.placement.as_mut().unwrap().avoid[0].y = f64::NAN;
        assert!(value.validate("project-a").is_err());
    }
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
            placement: None,
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
    #[test]
    fn only_png_or_jpeg_files_are_accepted_as_assets() {
        let dir = tempfile::tempdir().unwrap();
        let mut value = template();
        for (name, accepted) in [
            ("logo.png", true),
            ("logo.JPG", true),
            ("list.m3u8", false),
            ("movie.mp4", false),
            ("noext", false),
        ] {
            let path = dir.path().join(name);
            let content: &[u8] = if name.to_ascii_lowercase().ends_with("png") {
                b"\x89PNG\r\n\x1a\nrest"
            } else {
                &[0xff, 0xd8, 0xff, 0xe0, 0, 0]
            };
            fs::write(&path, content).unwrap();
            value.logo = Some(path);
            assert_eq!(value.validate("project-a").is_ok(), accepted, "{name}");
        }
        // A playlist renamed to .png is refused by its content.
        let fake = dir.path().join("playlist.png");
        fs::write(&fake, b"#EXTM3U\nhttp://example.invalid/a.ts\n").unwrap();
        value.logo = Some(fake);
        assert!(value.validate("project-a").is_err());
        // A JPEG named .png and a PNG named .jpg are refused: the name must match the content.
        for (name, content) in [
            ("jpeg-as.png", &[0xffu8, 0xd8, 0xff, 0xe0, 0, 0][..]),
            ("png-as.jpg", &b"\x89PNG\r\n\x1a\nrest"[..]),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, content).unwrap();
            value.logo = Some(path);
            assert!(value.validate("project-a").is_err(), "{name}");
        }
    }
}
