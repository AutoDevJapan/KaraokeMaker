use anyhow::{Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum VideoPreset {
    #[default]
    Hd,
    #[value(name = "1080p", alias = "full-hd")]
    FullHd,
    #[value(name = "1440p")]
    Qhd,
    #[value(name = "4k")]
    Uhd,
    Vertical,
    #[value(name = "vertical-4k")]
    VerticalUhd,
}

impl VideoPreset {
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Hd => (1280, 720),
            Self::FullHd => (1920, 1080),
            Self::Qhd => (2560, 1440),
            Self::Uhd => (3840, 2160),
            Self::Vertical => (1080, 1920),
            Self::VerticalUhd => (2160, 3840),
        }
    }

    /// Constrained quality: static backgrounds stay small, detailed video gets more bits.
    pub fn encoding(self) -> (u32, &'static str, &'static str) {
        match self {
            Self::Hd => (20, "8M", "16M"),
            Self::FullHd | Self::Vertical => (20, "12M", "24M"),
            Self::Qhd => (18, "20M", "40M"),
            Self::Uhd | Self::VerticalUhd => (18, "40M", "80M"),
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
