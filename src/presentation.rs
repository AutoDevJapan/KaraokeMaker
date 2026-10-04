use anyhow::{Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum VideoPreset {
    #[default]
    Hd,
    FullHd,
    Vertical,
}

impl VideoPreset {
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Hd => (1280, 720),
            Self::FullHd => (1920, 1080),
            Self::Vertical => (1080, 1920),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VisualStyle {
    pub background: String,
    pub accent: String,
    pub font: String,
}

impl Default for VisualStyle {
    fn default() -> Self {
        Self {
            background: "142e27".into(),
            accent: "ceffa4".into(),
            font: "Yu Gothic".into(),
        }
    }
}

impl VisualStyle {
    pub fn validate(&self) -> Result<()> {
        for color in [&self.background, &self.accent] {
            ensure!(
                color.len() == 6 && color.bytes().all(|c| c.is_ascii_hexdigit()),
                "配色は6桁のRGB（例: 142e27）にしてください。"
            );
        }
        ensure!(
            !self.font.trim().is_empty()
                && self.font.chars().count() <= 80
                && !self
                    .font
                    .chars()
                    .any(|c| c.is_control() || matches!(c, ',' | '{' | '}' | '\\')),
            "フォント名が不正です。"
        );
        Ok(())
    }

    pub fn ass_accent(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "&H00{}{}{}",
            &self.accent[4..6],
            &self.accent[2..4],
            &self.accent[..2]
        ))
    }
}
