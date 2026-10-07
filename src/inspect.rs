//! 納品物（動画・WAV・MP3）の形式・長さ・音量を検査し、機械可読な結果を作る。
//!
//! 検査対象は読み取りのみで開く。聴感・字幕の正しさ・映像の見た目・原本ハッシュの
//! 受領時との照合は自動では判定できないため、未確認項目として結果に残す。
use crate::{
    media::{self, Tools, hidden_command},
    presentation::VideoPreset,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const SCHEMA: &str = "karaoke-maker.delivery-inspection/1";
/// 元音源との長さの差がこの秒数未満であること。
pub const DURATION_TOLERANCE: f64 = 0.25;
/// 商用納品のtrue peak上限（dBTP）。
pub const PEAK_LIMIT_DBTP: f64 = -1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Video,
    Wav,
    Mp3,
}

#[derive(Clone, Debug)]
pub struct Deliverable {
    pub kind: Kind,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Serialize)]
pub struct FileReport {
    pub kind: Kind,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub duration_seconds: Option<f64>,
    pub decoded_seconds: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub checks: Vec<Check>,
    pub passed: bool,
}

#[derive(Debug, Serialize)]
pub struct SourceReport {
    pub path: String,
    pub sha256: String,
    pub duration_seconds: f64,
}

#[derive(Debug, Serialize)]
pub struct ManualCheck {
    pub id: &'static str,
    pub description: &'static str,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub inspected_at_unix: u64,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub preset: VideoPreset,
    pub criteria: Criteria,
    pub source: SourceReport,
    pub files: Vec<FileReport>,
    /// 自動検査の結果のみ。納品可否ではない。
    pub automatic_result: &'static str,
    /// 人手の確認が済んでいないため、常にfalse。
    pub manual_review_complete: bool,
    pub manual_checks: Vec<ManualCheck>,
    pub note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Criteria {
    pub duration_tolerance_seconds: f64,
    pub true_peak_limit_dbtp: f64,
    pub video: String,
    pub wav: &'static str,
    pub mp3: &'static str,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.files.iter().all(|f| f.passed)
    }
}

fn manual_checks() -> Vec<ManualCheck> {
    [
        ("source_sha256_vs_receipt", "原本のSHA-256を受領時の記録と照合する（本結果のsourceのsha256は今回の計算値）"),
        ("full_listening", "全編をヘッドホンとスピーカーで試聴し、欠落・揺れ・金属音・クリック・音量の跳びがないか確認する"),
        ("full_visual_check", "全編と冒頭・サビ・終盤のフレームを目視し、字幕の欠け・画面外・重なりがないか確認する"),
        ("lyrics_proofread", "歌詞の正本と字幕を全行照合し、開始時刻と間奏の扱いを確認する"),
        ("player_cue_test", "新しいプレイヤーで再生・頭出しできるか確認する"),
        ("delivery_scope", "正しい案件のファイルか、権利・用途・クレジット・実績掲載許可を照合する"),
    ]
    .into_iter()
    .map(|(id, description)| ManualCheck { id, description, status: "unchecked" })
    .collect()
}

fn display(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\")
        .map(str::to_owned)
        .unwrap_or(text)
}

fn sha256(path: &Path) -> Result<(String, u64)> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("{} を読み取れませんでした。", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn number(v: &Value) -> Option<f64> {
    v.as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| v.as_f64())
        .filter(|n| n.is_finite())
}

fn rate(v: &Value) -> Option<f64> {
    let (a, b) = v.as_str()?.split_once('/')?;
    let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
    (b != 0.0).then_some(a / b)
}

fn first_line(tool: &Path) -> String {
    hidden_command(tool)
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn check(
    checks: &mut Vec<Check>,
    id: &'static str,
    passed: bool,
    expected: impl Into<String>,
    actual: impl Into<String>,
) {
    checks.push(Check {
        id,
        passed,
        expected: expected.into(),
        actual: actual.into(),
    });
}

fn ffprobe(tools: &Tools, path: &Path) -> Result<Value> {
    let output = hidden_command(&tools.ffprobe)
        .args(["-v", "error", "-protocol_whitelist", "file,pipe"])
        .args(["-show_format", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .context("ffprobeを起動できませんでした。")?;
    ensure!(
        output.status.success(),
        "ffprobeが読み取れません: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

struct Decode {
    clean: bool,
    seconds: Option<f64>,
    messages: String,
}

/// 映像・音声を最後まで復号し、エラーが1件でもあれば不合格にする。
fn decode_all(tools: &Tools, path: &Path) -> Result<Decode> {
    let output = hidden_command(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-xerror"])
        .args(["-protocol_whitelist", "file,pipe", "-i"])
        .arg(path)
        .args(["-map", "0:v?", "-map", "0:a?", "-f", "null"])
        .args(["-progress", "pipe:1", "-nostats", "-"])
        .output()
        .context("ffmpegを起動できませんでした。")?;
    let messages = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let seconds = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("out_time_us="))
        .filter_map(|v| v.trim().parse::<f64>().ok())
        .next_back()
        .map(|us| us / 1e6);
    Ok(Decode {
        clean: output.status.success() && messages.is_empty(),
        seconds,
        messages,
    })
}

fn inspect_file(
    tools: &Tools,
    item: &Deliverable,
    source_duration: f64,
    preset: VideoPreset,
) -> Result<FileReport> {
    ensure!(
        item.path.is_file(),
        "検査するファイルが見つかりません: {}",
        item.path.display()
    );
    let path = item.path.canonicalize()?;
    let (before, bytes) = sha256(&path)?;
    let mut checks = Vec::new();
    let mut duration = None;
    let mut decoded_seconds = None;
    let mut peak = None;
    match ffprobe(tools, &path) {
        Err(error) => check(
            &mut checks,
            "readable",
            false,
            "ffprobeで読める",
            format!("{error:#}"),
        ),
        Ok(data) => {
            check(&mut checks, "readable", true, "ffprobeで読める", "読めた");
            let streams = data["streams"].as_array().cloned().unwrap_or_default();
            let of = |t: &str| {
                streams.iter().find(|s| {
                    s["codec_type"] == t && s["disposition"]["attached_pic"].as_i64() != Some(1)
                })
            };
            let (video, audio) = (of("video"), of("audio"));
            let format_name = data["format"]["format_name"].as_str().unwrap_or("");
            duration = number(&data["format"]["duration"]);
            let text = |s: Option<&Value>, key: &str| {
                s.and_then(|s| s[key].as_str()).unwrap_or("なし").to_owned()
            };
            match item.kind {
                Kind::Video => {
                    let (w, h) = preset.dimensions();
                    check(
                        &mut checks,
                        "container_mp4",
                        format_name.split(',').any(|f| f == "mp4"),
                        "mp4",
                        format_name,
                    );
                    check(
                        &mut checks,
                        "video_stream_present",
                        video.is_some(),
                        "映像ストリームあり",
                        if video.is_some() { "あり" } else { "なし" },
                    );
                    check(
                        &mut checks,
                        "video_codec_h264",
                        text(video, "codec_name") == "h264",
                        "h264",
                        text(video, "codec_name"),
                    );
                    check(
                        &mut checks,
                        "pixel_format_yuv420p",
                        text(video, "pix_fmt") == "yuv420p",
                        "yuv420p",
                        text(video, "pix_fmt"),
                    );
                    let size = video.map(|v| {
                        (
                            v["width"].as_u64().unwrap_or(0),
                            v["height"].as_u64().unwrap_or(0),
                        )
                    });
                    check(
                        &mut checks,
                        "dimensions",
                        size == Some((w as u64, h as u64)),
                        format!("{w}x{h}"),
                        size.map(|(a, b)| format!("{a}x{b}"))
                            .unwrap_or_else(|| "なし".into()),
                    );
                    let fps = video.map(|v| (rate(&v["r_frame_rate"]), rate(&v["avg_frame_rate"])));
                    let is30 = |r: Option<f64>| r.is_some_and(|r| (r - 30.0).abs() < 0.01);
                    check(
                        &mut checks,
                        "frame_rate_30",
                        fps.is_some_and(|(a, b)| is30(a) && is30(b)),
                        "30fps（r_frame_rateとavg_frame_rateの両方）",
                        fps.map(|(a, b)| format!("r={a:?} avg={b:?}"))
                            .unwrap_or_else(|| "なし".into()),
                    );
                    check(
                        &mut checks,
                        "audio_codec_aac",
                        text(audio, "codec_name") == "aac",
                        "aac",
                        text(audio, "codec_name"),
                    );
                    for (id, stream) in [
                        ("video_stream_duration_matches_source", video),
                        ("audio_stream_duration_matches_source", audio),
                    ] {
                        if let Some(d) = stream.and_then(|s| number(&s["duration"])) {
                            check(
                                &mut checks,
                                id,
                                (d - source_duration).abs() < DURATION_TOLERANCE,
                                format!(
                                    "元音源{source_duration:.3}秒との差{DURATION_TOLERANCE}秒未満"
                                ),
                                format!("{d:.3}秒"),
                            );
                        }
                    }
                }
                Kind::Wav => {
                    check(
                        &mut checks,
                        "container_wav",
                        format_name == "wav",
                        "wav",
                        format_name,
                    );
                    check(
                        &mut checks,
                        "audio_codec_pcm_s24le",
                        text(audio, "codec_name") == "pcm_s24le",
                        "pcm_s24le（24bit PCM）",
                        text(audio, "codec_name"),
                    );
                    let rate = audio.and_then(|a| number(&a["sample_rate"]));
                    check(
                        &mut checks,
                        "sample_rate_48000",
                        rate == Some(48000.0),
                        "48000Hz",
                        rate.map(|r| format!("{r}Hz"))
                            .unwrap_or_else(|| "なし".into()),
                    );
                    let channels = audio.and_then(|a| a["channels"].as_u64());
                    check(
                        &mut checks,
                        "channels_stereo",
                        channels == Some(2),
                        "2ch",
                        channels
                            .map(|c| format!("{c}ch"))
                            .unwrap_or_else(|| "なし".into()),
                    );
                }
                Kind::Mp3 => {
                    check(
                        &mut checks,
                        "container_mp3",
                        format_name == "mp3",
                        "mp3",
                        format_name,
                    );
                    check(
                        &mut checks,
                        "audio_codec_mp3",
                        text(audio, "codec_name") == "mp3",
                        "mp3",
                        text(audio, "codec_name"),
                    );
                }
            }
            check(
                &mut checks,
                "audio_stream_present",
                audio.is_some(),
                "音声ストリームあり",
                if audio.is_some() { "あり" } else { "なし" },
            );
            check(
                &mut checks,
                "duration_matches_source",
                duration.is_some_and(|d| (d - source_duration).abs() < DURATION_TOLERANCE),
                format!("元音源{source_duration:.3}秒との差{DURATION_TOLERANCE}秒未満"),
                duration
                    .map(|d| format!("{d:.3}秒"))
                    .unwrap_or_else(|| "不明".into()),
            );
            let decode = decode_all(tools, &path)?;
            check(
                &mut checks,
                "decodes_cleanly",
                decode.clean,
                "全編を復号してエラーなし",
                if decode.clean {
                    "エラーなし".into()
                } else {
                    let one_line = decode.messages.lines().collect::<Vec<_>>().join(" / ");
                    one_line.chars().take(300).collect::<String>()
                },
            );
            decoded_seconds = decode.seconds;
            check(
                &mut checks,
                "decoded_length_matches_container",
                matches!((duration, decode.seconds), (Some(d), Some(s)) if (d - s).abs() < DURATION_TOLERANCE),
                "復号できた長さがコンテナの長さと一致（差0.25秒未満）",
                format!(
                    "コンテナ{}秒 / 復号{}秒",
                    duration.map_or("不明".into(), |d| format!("{d:.3}")),
                    decoded_seconds.map_or("不明".into(), |d| format!("{d:.3}"))
                ),
            );
            if audio.is_some() {
                match media::true_peak(tools, &path) {
                    Ok(p) => {
                        peak = p.is_finite().then_some(p);
                        check(
                            &mut checks,
                            "true_peak_limit",
                            p.is_finite() && p <= PEAK_LIMIT_DBTP,
                            format!("{PEAK_LIMIT_DBTP}dBTP以下（無音ではない）"),
                            if p.is_finite() {
                                format!("{p:.2}dBTP")
                            } else {
                                "無音".into()
                            },
                        );
                    }
                    Err(error) => check(
                        &mut checks,
                        "true_peak_limit",
                        false,
                        format!("{PEAK_LIMIT_DBTP}dBTP以下"),
                        format!("測定できません: {error:#}"),
                    ),
                }
            }
        }
    }
    let (after, _) = sha256(&path)?;
    check(
        &mut checks,
        "file_unchanged_by_inspection",
        before == after,
        "検査前後でSHA-256が同じ",
        if before == after {
            "同じ"
        } else {
            "変化した"
        },
    );
    let passed = checks.iter().all(|c| c.passed);
    Ok(FileReport {
        kind: item.kind,
        path: display(&path),
        bytes,
        sha256: before,
        duration_seconds: duration,
        decoded_seconds,
        true_peak_dbtp: peak,
        checks,
        passed,
    })
}

/// 入力の誤り（ファイルなし・元音源が読めない等）はErr。納品物の不合格は結果に記録する。
pub fn inspect(
    tools: &Tools,
    source: &Path,
    deliverables: &[Deliverable],
    preset: VideoPreset,
) -> Result<Report> {
    ensure!(
        !deliverables.is_empty(),
        "検査する納品物を1つ以上指定してください。"
    );
    ensure!(
        source.is_file(),
        "元音源が見つかりません: {}",
        source.display()
    );
    let source = source.canonicalize()?;
    let mut seen = vec![source.clone()];
    for item in deliverables {
        let path = item.path.canonicalize().with_context(|| {
            format!("検査するファイルが見つかりません: {}", item.path.display())
        })?;
        if seen.contains(&path) {
            bail!(
                "同じファイルを元音源や別の納品物として重ねて指定できません: {}",
                path.display()
            );
        }
        seen.push(path);
    }
    let source_probe = media::probe(tools, &source)?;
    let (source_hash, _) = sha256(&source)?;
    let files = deliverables
        .iter()
        .map(|item| inspect_file(tools, item, source_probe.duration, preset))
        .collect::<Result<Vec<_>>>()?;
    let (after, _) = sha256(&source)?;
    ensure!(after == source_hash, "検査中に元音源が変更されました。");
    let mut report = Report {
        schema: SCHEMA,
        inspected_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        ffmpeg: first_line(&tools.ffmpeg),
        ffprobe: first_line(&tools.ffprobe),
        preset,
        criteria: Criteria {
            duration_tolerance_seconds: DURATION_TOLERANCE,
            true_peak_limit_dbtp: PEAK_LIMIT_DBTP,
            video: format!(
                "mp4 / H.264 yuv420p / {}x{} / 30fps / AAC",
                preset.dimensions().0,
                preset.dimensions().1
            ),
            wav: "WAV PCM 24bit / 48kHz / ステレオ",
            mp3: "MP3",
        },
        source: SourceReport {
            path: display(&source),
            sha256: source_hash,
            duration_seconds: source_probe.duration,
        },
        files,
        automatic_result: "fail",
        manual_review_complete: false,
        manual_checks: manual_checks(),
        note: "自動検査は形式・長さ・全編復号・true peakのみ。聴感品質・字幕の正しさ・映像の見た目は判定しておらず、manual_checksの確認が済むまで納品可としない。",
    };
    if report.passed() {
        report.automatic_result = "pass";
    }
    Ok(report)
}
