//! Measure the selected font through the same FFmpeg/libass renderer used for export.
//! Fontsize is a line metric, not a glyph-width estimate.
use anyhow::{Result, ensure};
pub fn scaled_image(path: &std::path::Path, width: u32, height: u32) -> Result<(u32, u32)> {
    let output = crate::media::hidden_command(crate::media::Tools::discover().ffmpeg)
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-f",
            "image2",
            "-pattern_type",
            "none",
            "-i",
        ])
        .arg(path)
        .args(["-vf"])
        .arg(format!(
            "scale=w={width}:h={height}:force_original_aspect_ratio=decrease"
        ))
        .args([
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "pipe:1",
        ])
        .output()?;
    ensure!(
        output.status.success()
            && output.stdout.starts_with(b"\x89PNG\r\n\x1a\n")
            && output.stdout.len() >= 24,
        "ロゴ画像の実寸を確認できません。"
    );
    Ok((
        u32::from_be_bytes(output.stdout[16..20].try_into()?),
        u32::from_be_bytes(output.stdout[20..24].try_into()?),
    ))
}
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("measure.ass"));
        let _ = fs::remove_dir(&self.0);
    }
}

pub fn measure(font: &str, size: u32, outline: u32, text: &str) -> Result<(u32, u32)> {
    measure_styled(font, size, outline, true, text)
}

pub fn measure_styled(
    font: &str,
    size: u32,
    outline: u32,
    bold: bool,
    text: &str,
) -> Result<(u32, u32)> {
    type Cache = HashMap<(String, u32, u32, bool, String), (u32, u32)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let key = (font.to_owned(), size, outline, bold, text.to_owned());
    if let Some(value) = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(&key)
    {
        return Ok(*value);
    }
    let dir = std::env::temp_dir().join(format!("karaoke-ass-measure-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir)?;
    let scratch = Scratch(dir);
    // Use the output Fontsize directly. With matching PlayRes there is no extra scaling.
    let width = 8192u32;
    let height = (size * 12 + 128)
        .max(size * text.lines().count() as u32 * 2 + 128)
        .max(512);
    let escaped = text
        .replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace('<', "＜")
        .replace('>', "＞")
        .replace('\r', "")
        .replace('\n', "\\N");
    let weight = if bold { -1 } else { 0 };
    // Match the export anchors: subtitles are bottom-centred, titles top-centred.
    // A left-aligned ink width alone misses the centring effect of leading/trailing spaces.
    let alignment = if bold { 2 } else { 8 };
    // Export scales the base 1px lyric shadow with the base 44 Fontsize; this is a
    // style parameter, never a conversion from Fontsize to glyph width.
    let shadow = if bold {
        (size as f64 / 44.0).round() as u32
    } else {
        0
    };
    let ass = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nWrapStyle: 2\nScaledBorderAndShadow: yes\n[V4+ Styles]\nFormat: Name,Fontname,Fontsize,PrimaryColour,SecondaryColour,OutlineColour,BackColour,Bold,Italic,Underline,StrikeOut,ScaleX,ScaleY,Spacing,Angle,BorderStyle,Outline,Shadow,Alignment,MarginL,MarginR,MarginV,Encoding\nStyle: M,{font},{size},&H00FFFFFF,&H00FFFFFF,&H00FFFFFF,&H00FFFFFF,{weight},0,0,0,100,100,0,0,1,{outline},{shadow},{alignment},0,0,64,1\n[Events]\nFormat: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text\nDialogue: 0,0:00:00.00,0:00:01.00,M,,0,0,0,,{escaped}\n"
    );
    fs::write(scratch.0.join("measure.ass"), ass)?;
    let output = crate::media::hidden_command(crate::media::Tools::discover().ffmpeg)
        .current_dir(&scratch.0)
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=black:s={width}x{height}:d=0.1"))
        .args([
            "-vf",
            "ass=measure.ass",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "字幕フォントの実寸計測に失敗しました。"
    );
    ensure!(
        output.stdout.len() == (width * height) as usize,
        "字幕計測フレームが不完全です。"
    );
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    let mut found = false;
    for (i, v) in output.stdout.iter().enumerate() {
        if *v >= 10 {
            let x = i as u32 % width;
            let y = i as u32 / width;
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            found = true;
        }
    }
    ensure!(
        !found || (y0 > 1 && y1 < height - 2),
        "字幕の計測範囲を超えました。"
    );
    let result = if found {
        let centred_width = if x0 <= 1 || x1 >= width - 2 {
            width
        } else {
            2 * ((width / 2).saturating_sub(x0)).max(x1.saturating_sub(width / 2) + 1)
        };
        // Include bearing/leading relative to the actual top or bottom ASS anchor.
        (
            centred_width,
            if bold {
                (height - 64).saturating_sub(y0) + 1
            } else {
                y1.saturating_sub(64) + 1
            },
        )
    } else {
        (0, 0)
    };
    CACHE.get().unwrap().lock().unwrap().insert(key, result);
    Ok(result)
}
