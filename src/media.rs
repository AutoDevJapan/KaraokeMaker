use crate::presentation::{VideoPreset, VisualStyle};
use crate::{Progress, lyrics};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const MAX_DURATION: f64 = 15.0 * 60.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub title: String,
    pub key: i32,
    pub tempo: f64,
    pub lyrics: String,
    pub original_video: bool,
    #[serde(default)]
    pub preset: VideoPreset,
    #[serde(default)]
    pub style: VisualStyle,
    #[serde(default = "legacy_separation")]
    pub separation: crate::model::SeparationModel,
    #[serde(default = "legacy_sample_rate")]
    pub sample_rate: u32,
    #[serde(default)]
    pub words: Vec<crate::alignment::Word>,
    #[serde(default)]
    pub automatic_subtitles: bool,
}
fn legacy_sample_rate() -> u32 {
    44100
}
fn legacy_separation() -> crate::model::SeparationModel {
    crate::model::SeparationModel::Standard
}
impl Default for Options {
    fn default() -> Self {
        Self {
            title: "個人用カラオケ".into(),
            key: 0,
            tempo: 1.0,
            lyrics: String::new(),
            original_video: true,
            preset: VideoPreset::default(),
            style: VisualStyle::default(),
            separation: crate::model::SeparationModel::MelBandFp16,
            sample_rate: 48000,
            words: Vec::new(),
            automatic_subtitles: true,
        }
    }
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.title.chars().count() <= 120,
            "曲名は120文字以内にしてください。"
        );
        ensure!(
            (-6..=6).contains(&self.key),
            "キーは-6から+6の間にしてください。"
        );
        ensure!(
            self.tempo.is_finite() && (0.5..=1.5).contains(&self.tempo),
            "速度は0.5から1.5の間にしてください。"
        );
        ensure!(
            self.lyrics.len() <= 64 * 1024,
            "歌詞は64KB以内にしてください。"
        );
        if self.lyrics.trim().starts_with('[') || self.lyrics.trim().is_empty() {
            lyrics::parse_lrc(&self.lyrics)?;
        }
        self.style.validate()?;
        ensure!(
            [44100, 48000].contains(&self.sample_rate),
            "音声は44.1kHzまたは48kHzにしてください。"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}
impl Tools {
    pub fn discover() -> Self {
        let base = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let find = |name: &str| {
            let file = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            };
            let adjacent = base.as_ref().map(|p| p.join(&file));
            adjacent
                .filter(|p| p.is_file())
                .unwrap_or_else(|| file.into())
        };
        Self {
            ffmpeg: find("ffmpeg"),
            ffprobe: find("ffprobe"),
        }
    }
    pub fn check(&self) -> Result<()> {
        for path in [&self.ffmpeg, &self.ffprobe] {
            let status = hidden_command(path)
                .args(["-version"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .with_context(|| {
                    format!(
                        "{} が見つかりません。FFmpegをインストールしてください。",
                        path.display()
                    )
                })?;
            ensure!(status.success(), "FFmpegを起動できませんでした。");
        }
        for filter in ["rubberband", "ass", "ebur128"] {
            let output = hidden_command(&self.ffmpeg)
                .args(["-hide_banner", "-h", &format!("filter={filter}")])
                .output()?;
            ensure!(
                String::from_utf8_lossy(&output.stdout).contains(&format!("Filter {filter}")),
                "FFmpegの{filter}フィルターが必要です。"
            );
        }
        Ok(())
    }
}

pub fn hidden_command(path: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command.stdin(Stdio::null());
    command
}

#[derive(Debug)]
pub struct Probe {
    pub duration: f64,
    pub video: bool,
    pub video_index: Option<u64>,
}
pub fn probe(tools: &Tools, input: &Path) -> Result<Probe> {
    let output = hidden_command(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(input)
        .output()
        .context("音楽・動画の情報を読み取れませんでした。")?;
    ensure!(
        output.status.success(),
        "読み込めないファイルです。MP3・WAV・FLAC・M4A・MP4などを選んでください。"
    );
    let data: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let streams = data["streams"]
        .as_array()
        .context("音声を含むファイルを選んでください。")?;
    ensure!(
        streams.iter().any(|s| s["codec_type"] == "audio"),
        "ファイルに音声がありません。"
    );
    let video_stream = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64() != Some(1)
    });
    let video_index = video_stream.and_then(|s| s["index"].as_u64());
    let video = video_index.is_some();
    let duration = data["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .context("曲の長さを読み取れませんでした。")?;
    ensure!(
        duration.is_finite() && duration > 0.0,
        "曲の長さを読み取れませんでした。"
    );
    Ok(Probe {
        duration,
        video,
        video_index,
    })
}

fn run(tools: &Tools, dir: &Path, args: &[String], progress: &Progress) -> Result<()> {
    progress.check()?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("ffmpeg.log"))?;
    let mut child = hidden_command(&tools.ffmpeg)
        .current_dir(dir)
        .args(["-hide_banner", "-v", "warning", "-nostdin", "-n"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .context("FFmpegを起動できませんでした。")?;
    let started = Instant::now();
    loop {
        if progress.check().is_err() || started.elapsed() > Duration::from_secs(3600) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("処理が取り消されたか、1時間の処理時間を超えました。");
        }
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "音源・映像の変換に失敗しました。詳細は出力フォルダのffmpeg.logにあります。"
            );
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

pub fn decode(
    tools: &Tools,
    input: &Path,
    dir: &Path,
    progress: &Progress,
) -> Result<(Vec<f32>, Vec<f32>)> {
    decode_bounded(tools, input, dir, progress, MAX_DURATION)
}

fn decode_bounded(
    tools: &Tools,
    input: &Path,
    dir: &Path,
    progress: &Progress,
    max_duration: f64,
) -> Result<(Vec<f32>, Vec<f32>)> {
    run(
        tools,
        dir,
        &[
            "-protocol_whitelist".into(),
            "file,pipe".into(),
            "-i".into(),
            input.display().to_string(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-af".into(),
            "aresample=async=1:first_pts=0".into(),
            "-ar".into(),
            "44100".into(),
            "-ac".into(),
            "2".into(),
            "-c:a".into(),
            "pcm_f32le".into(),
            "decoded.wav".into(),
        ],
        progress,
    )?;
    let mut reader = hound::WavReader::open(dir.join("decoded.wav"))?;
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut samples = reader.samples::<f32>();
    while let Some(l) = samples.next() {
        let l = l?;
        let r = samples
            .next()
            .context("音源の左右の長さが一致しません。")??;
        ensure!(
            l.is_finite() && r.is_finite(),
            "音源に不正なサンプルが含まれています。"
        );
        left.push(l);
        right.push(r);
        ensure!(
            left.len() <= max_duration as usize * 44100,
            "音源が処理できる長さを超えています。"
        );
    }
    ensure!(!left.is_empty(), "音源が空です。");
    Ok((left, right))
}

pub fn write_stereo(path: &Path, left: &[f32], right: &[f32]) -> Result<()> {
    ensure!(
        left.len() == right.len() && !left.is_empty(),
        "分離した音源の長さが不正です。"
    );
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44100,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?,
        spec,
    )?;
    for (&l, &r) in left.iter().zip(right) {
        ensure!(
            l.is_finite() && r.is_finite(),
            "分離結果に不正なサンプルが含まれています。"
        );
        writer.write_sample(l)?;
        writer.write_sample(r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Oversampled peak measurement, rather than the largest stored sample.
pub fn true_peak(tools: &Tools, input: &Path) -> Result<f64> {
    let output = hidden_command(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-af",
            "ebur128=peak=true",
            "-f",
            "null",
            "-",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "音源のピークを測定できませんでした。"
    );
    let log = String::from_utf8_lossy(&output.stderr);
    let summary = log
        .rsplit_once("True peak:")
        .context("音源のピークを読み取れませんでした。")?
        .1;
    let value = summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("Peak:"))
        .and_then(|value| value.split_whitespace().next())
        .context("音源のピークを読み取れませんでした。")?;
    let peak = value.parse::<f64>()?;
    ensure!(
        peak.is_finite() || peak == f64::NEG_INFINITY,
        "音源のピークが不正です。"
    );
    Ok(peak)
}

fn preserve_headroom(tools: &Tools, dir: &Path, progress: &Progress) -> Result<()> {
    progress.check()?;
    progress.set("再生時に歪まないよう音量を確認しています。");
    let peak = true_peak(tools, &dir.join("karaoke.wav"))?;
    if peak > -3.0 {
        // Keep dynamics intact. Leave room for MP3/AAC intersample overshoot.
        let gain = -3.1 - peak;
        fs::rename(dir.join("karaoke.wav"), dir.join("karaoke-unchecked.wav"))?;
        run(
            tools,
            dir,
            &[
                "-i".into(),
                "karaoke-unchecked.wav".into(),
                "-af".into(),
                format!("volume={gain:.3}dB"),
                "-c:a".into(),
                "pcm_s24le".into(),
                "karaoke.wav".into(),
            ],
            progress,
        )?;
        fs::remove_file(dir.join("karaoke-unchecked.wav"))?;
    }
    progress.check()?;
    Ok(())
}

pub fn export(
    tools: &Tools,
    input: &Path,
    dir: &Path,
    source: &Probe,
    options: &Options,
    duration: f64,
    progress: &Progress,
) -> Result<()> {
    options.validate()?;
    for name in ["karaoke.wav", "karaoke.mp3", "karaoke.mp4"] {
        ensure!(
            !dir.join(name).exists(),
            "出力先に{name}がすでにあります。別のフォルダを使ってください。"
        );
    }
    let mut cues = if options.words.is_empty() {
        lyrics::parse_lrc(&options.lyrics)?
    } else {
        lyrics::word_cues(&options.words)
    };
    if !options.words.is_empty() {
        cues.retain(|cue| cue.seconds < duration);
    }
    let ass = lyrics::make_ass_styled(
        &options.title,
        &cues,
        duration,
        options.tempo,
        options.preset,
        &options.style,
    )?;
    let ass = if options.words.is_empty() {
        ass
    } else {
        lyrics::make_ass_words(
            &options.title,
            &options.words,
            duration,
            options.tempo,
            options.preset,
            &options.style,
        )?
    };
    fs::write(dir.join("lyrics.ass"), ass)?;
    fs::write(
        dir.join("lyrics.srt"),
        lyrics::make_srt(&cues, duration, options.tempo)?,
    )?;
    fs::write(
        dir.join("lyrics.lrc"),
        if options.words.is_empty() {
            options.lyrics.clone()
        } else {
            lyrics::to_lrc(&cues)
        },
    )?;
    if !options.words.is_empty() {
        fs::write(
            dir.join("words.json"),
            serde_json::to_vec_pretty(&options.words)?,
        )?;
    }
    let pitch = 2.0_f64.powf(options.key as f64 / 12.0);
    let mut audio_args = vec!["-i".into(), "instrumental-raw.wav".into()];
    // At the original key/speed, avoid phase-vocoder and limiter processing.
    // Peak safety is still measured and handled by preserve_headroom below.
    if options.key != 0 || options.tempo != 1.0 {
        audio_args.extend(["-af".into(),format!("rubberband=tempo={}:pitch={pitch}:channels=together,alimiter=limit=0.98:level=false:latency=true",options.tempo)]);
    }
    audio_args.extend([
        "-c:a".into(),
        "pcm_f32le".into(),
        "-ar".into(),
        options.sample_rate.to_string(),
        "adjusted-audio.wav".into(),
    ]);
    run(tools, dir, &audio_args, progress)?;
    let peak = true_peak(tools, &dir.join("adjusted-audio.wav"))?;
    let mut encode = vec!["-i".into(), "adjusted-audio.wav".into()];
    if peak > -3.0 {
        encode.extend(["-af".into(), format!("volume={:.3}dB", -3.1 - peak)]);
    }
    encode.extend(["-c:a".into(), "pcm_s24le".into(), "karaoke.wav".into()]);
    run(tools, dir, &encode, progress)?;
    fs::remove_file(dir.join("adjusted-audio.wav"))?;
    preserve_headroom(tools, dir, progress)?;
    run(
        tools,
        dir,
        &[
            "-i".into(),
            "karaoke.wav".into(),
            "-c:a".into(),
            "libmp3lame".into(),
            "-b:a".into(),
            "320k".into(),
            "karaoke.mp3".into(),
        ],
        progress,
    )?;
    progress.set("カラオケ映像を作っています。");
    let mut args = Vec::<String>::new();
    let (width, height) = options.preset.dimensions();
    let (crf, maxrate, bufsize) = options.preset.encoding();
    let background = &options.style.background;
    if source.video && options.original_video {
        args.extend([
            "-protocol_whitelist".into(),
            "file,pipe".into(),
            "-i".into(),
            input.display().to_string(),
        ]);
    } else {
        args.extend([
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("color=c=0x{background}:s={width}x{height}:r=30"),
        ]);
    }
    let video_map = if source.video && options.original_video {
        format!("0:{}", source.video_index.unwrap())
    } else {
        "0:v:0".into()
    };
    args.extend([
        "-i".into(), "karaoke.wav".into(), "-map".into(), video_map,
        "-map".into(), "1:a:0".into(), "-vf".into(),
        format!("setpts=PTS/{},scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=0x{background},setsar=1,tpad=stop_mode=clone:stop_duration=1800,ass=lyrics.ass",options.tempo),
        "-c:v".into(), "libx264".into(), "-preset".into(), "fast".into(),
        "-crf".into(), crf.to_string(), "-maxrate".into(), maxrate.into(),
        "-bufsize".into(), bufsize.into(), "-pix_fmt".into(), "yuv420p".into(),
        "-r".into(), "30".into(),
        "-c:a".into(), "aac".into(), "-b:a".into(), "384k".into(),
        "-t".into(), format!("{:.6}",duration/options.tempo),
        "-movflags".into(), "+faststart".into(), "karaoke.mp4".into()
    ]);
    run(tools, dir, &args, progress)?;
    for name in ["karaoke.mp3", "karaoke.mp4"] {
        progress.check()?;
        ensure!(
            true_peak(tools, &dir.join(name))? < 0.0,
            "出力音源のピークが大きすぎます。音量を確認してください。"
        );
    }
    let audio = probe(tools, &dir.join("karaoke.wav"))?;
    let video = probe(tools, &dir.join("karaoke.mp4"))?;
    let expected = duration / options.tempo;
    ensure!(
        video.video
            && (video.duration - expected).abs() < 0.25
            && (audio.duration - expected).abs() < 0.25,
        "出力の長さが曲と一致しません。結果を確認してください。"
    );
    for name in ["karaoke.wav", "karaoke.mp3", "karaoke.mp4"] {
        ensure!(
            fs::metadata(dir.join(name))?.len() > 44,
            "出力ファイルが空です。"
        );
    }
    Ok(())
}

/// Render supplied audio (original mix or approved instrumental) without AI separation.
/// Inputs remain untouched; the CLI allocates a fresh output directory per revision.
pub fn render(input: &Path, dir: &Path, options: &Options, progress: &Progress) -> Result<()> {
    render_bounded(input, dir, options, progress, MAX_DURATION)
}

pub(crate) fn render_retimed_audio(
    input: &Path,
    dir: &Path,
    options: &Options,
    progress: &Progress,
) -> Result<()> {
    // A 15-minute original at the supported minimum tempo has 30 minutes of output.
    render_bounded(input, dir, options, progress, MAX_DURATION / 0.5)
}

fn render_bounded(
    input: &Path,
    dir: &Path,
    options: &Options,
    progress: &Progress,
    max_duration: f64,
) -> Result<()> {
    options.validate()?;
    let input = input
        .canonicalize()
        .context("入力ファイルが見つかりません。")?;
    let dir = dir.canonicalize()?;
    let tools = Tools::discover();
    tools.check()?;
    let source = probe(&tools, &input)?;
    ensure!(
        source.duration <= max_duration,
        "音源が処理できる長さを超えています。"
    );
    run(
        &tools,
        &dir,
        &[
            "-protocol_whitelist".into(),
            "file,pipe".into(),
            "-i".into(),
            input.display().to_string(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-af".into(),
            "aresample=async=1:first_pts=0".into(),
            "-ar".into(),
            options.sample_rate.to_string(),
            "-ac".into(),
            "2".into(),
            "-t".into(),
            format!("{:.3}", max_duration + 0.1),
            "-c:a".into(),
            "pcm_f32le".into(),
            "instrumental-raw.wav".into(),
        ],
        progress,
    )?;
    let duration = probe(&tools, &dir.join("instrumental-raw.wav"))?.duration;
    ensure!(
        duration <= max_duration,
        "音源が処理できる長さを超えています。"
    );
    export(&tools, &input, &dir, &source, options, duration, progress)?;
    fs::write(
        dir.join("settings.json"),
        serde_json::to_vec_pretty(options)?,
    )?;
    fs::remove_file(dir.join("instrumental-raw.wav"))?;
    progress.set("指定した音源で字幕付き映像ができました。");
    Ok(())
}

pub(crate) fn retime_vocals(dir: &Path, tempo: f64, progress: &Progress) -> Result<()> {
    run(
        &Tools::discover(),
        dir,
        &[
            "-i".into(),
            "revision-vocals.wav".into(),
            "-af".into(),
            format!("rubberband=tempo={tempo}:channels=together"),
            "-c:a".into(),
            "pcm_f32le".into(),
            "vocals.wav".into(),
        ],
        progress,
    )
}

pub fn convert(
    input: &Path,
    dir: &Path,
    cache: &Path,
    options: &Options,
    progress: &Progress,
) -> Result<()> {
    options.validate()?;
    let input = input
        .canonicalize()
        .context("入力ファイルが見つかりません。")?;
    let dir = dir.canonicalize()?;
    if !options.lyrics.trim().is_empty() {
        fs::write(dir.join("reference-lyrics.txt"), &options.lyrics)?;
    }
    let tools = Tools::discover();
    tools.check()?;
    progress.set("音源を読み込んでいます。");
    let source = probe(&tools, &input)?;
    ensure!(
        source.duration <= MAX_DURATION,
        "曲は15分以内にしてください。"
    );
    let (left, right) = decode(&tools, &input, &dir, progress)?;
    let duration = left.len() as f64 / 44100.0;
    // Validate timestamps before spending time on inference.
    if options.lyrics.trim().starts_with('[') || options.lyrics.trim().is_empty() {
        lyrics::make_ass(
            &options.title,
            &lyrics::parse_lrc(&options.lyrics)?,
            duration,
            options.tempo,
        )?;
    }
    progress.set("歌声と伴奏を分離しています。初回はモデルを取得します。");
    let stems = crate::model::separate_using(&left, &right, cache, options.separation, progress)?;
    write_stereo(
        &dir.join("vocals.wav"),
        &stems.vocals_left,
        &stems.vocals_right,
    )?;
    if matches!(
        options.separation,
        crate::model::SeparationModel::MelBand
            | crate::model::SeparationModel::MelBandFp16
            | crate::model::SeparationModel::FineTunedResidual
    ) {
        run(&tools, &dir, &[
            "-protocol_whitelist".into(),"file,pipe".into(),"-i".into(),input.display().to_string(),
            "-i".into(),"vocals.wav".into(),"-filter_complex".into(),
            "[0:a:0]aresample=48000:async=1:first_pts=0[a];[1:a:0]aresample=48000[v];[a][v]amix=inputs=2:weights='1 -1':normalize=0:duration=first".into(),
            "-vn".into(),"-ac".into(),"2".into(),"-c:a".into(),"pcm_f32le".into(),"instrumental-raw.wav".into(),
        ],progress)?;
    } else {
        write_stereo(
            &dir.join("instrumental-raw.wav"),
            &stems.instrumental_left,
            &stems.instrumental_right,
        )?;
    }
    let mut effective = options.clone();
    if effective.words.is_empty()
        && ((!effective.lyrics.trim().is_empty() && !effective.lyrics.trim().starts_with('['))
            || (effective.automatic_subtitles && effective.lyrics.trim().is_empty()))
    {
        let result = crate::alignment::prepare(
            &dir.join("vocals.wav"),
            &dir,
            &effective.lyrics,
            duration,
            progress,
        );
        match result {
            Ok(words) => {
                effective.words = words;
                effective.lyrics = lyrics::to_lrc(&lyrics::word_cues(&effective.words));
            }
            Err(error) => {
                progress.check()?;
                fs::write(
                    dir.join("subtitle-warning.txt"),
                    "歌詞の時刻を確定できませんでした。時刻付きの歌詞を入力して作り直してください。",
                )?;
                fs::write(dir.join("alignment-error.log"), format!("{error:#}"))?;
                effective.lyrics.clear();
            }
        }
        effective.automatic_subtitles = false;
    }
    progress.set("キーと速度を調整しています。");
    export(
        &tools, &input, &dir, &source, &effective, duration, progress,
    )?;
    fs::write(
        dir.join("settings.json"),
        serde_json::to_vec_pretty(&effective)?,
    )?;
    for name in ["decoded.wav", "instrumental-raw.wav"] {
        if let Err(error) = fs::remove_file(dir.join(name)) {
            use std::io::Write;
            if let Ok(mut log) = fs::OpenOptions::new()
                .append(true)
                .open(dir.join("ffmpeg.log"))
            {
                let _ = writeln!(log, "一時ファイル{name}の削除に失敗しました: {error}");
            }
        }
    }
    progress.set("音源と映像ができました。");
    Ok(())
}
