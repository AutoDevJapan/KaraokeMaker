use crate::{
    Progress, lyrics,
    media::{self, Options},
    presentation::VideoPreset,
};
use anyhow::{Result, ensure};
use std::{fs, path::Path};

/// The editor displays the timeline of the already playable audio, including tempo changes.
pub fn editable_lrc(options: &Options) -> Result<String> {
    options.validate()?;
    let mut lrc = String::new();
    let cues = if !options.words.is_empty() {
        lyrics::word_cues(&options.words)
    } else {
        lyrics::parse_lrc(&options.lyrics)?
    };
    for cue in cues {
        let cs = (cue.seconds / options.tempo * 100.0).round() as u64;
        for line in cue.text.split('\n') {
            lrc.push_str(&format!(
                "[{:02}:{:02}.{:02}]{}\n",
                cs / 6000,
                cs / 100 % 60,
                cs % 100,
                line
            ));
        }
    }
    Ok(lrc)
}

pub fn render_revision(
    source: &Path,
    dir: &Path,
    text: &str,
    preset: VideoPreset,
    progress: &Progress,
) -> Result<()> {
    let previous: Options = serde_json::from_slice(&fs::read(source.join("settings.json"))?)?;
    previous.validate()?;
    let options = Options {
        title: previous.title,
        lyrics: text.into(),
        preset,
        style: previous.style,
        sample_rate: previous.sample_rate,
        original_video: false,
        ..Options::default()
    };
    options.validate()?;
    for (input, output) in [
        ("karaoke.wav", "revision-audio.wav"),
        ("vocals.wav", "revision-vocals.wav"),
    ] {
        progress.check()?;
        let mut reader = fs::File::open(source.join(input))?;
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(output))?;
        std::io::copy(&mut reader, &mut writer)?;
    }
    media::render_retimed_audio(&dir.join("revision-audio.wav"), dir, &options, progress)?;
    media::retime_vocals(dir, previous.tempo, progress)?;
    for name in ["revision-audio.wav", "revision-vocals.wav"] {
        fs::remove_file(dir.join(name))?;
    }
    ensure!(
        dir.join("vocals.wav").is_file(),
        "歌声の確認用ファイルがありません。"
    );
    Ok(())
}
