use karaoke_maker::{
    inspect::{self, Deliverable, FileReport, Kind, Report},
    media::Tools,
    presentation::VideoPreset,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn ffmpeg(args: &[&str]) {
    let output = Command::new(Tools::discover().ffmpeg)
        .args(["-hide_banner", "-v", "error", "-nostdin", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

/// 3秒の合成音声（440Hz・約-18dBFS）。
fn source(dir: &Path) -> PathBuf {
    let out = path(dir, "source.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:r=48000:d=3",
        "-ac",
        "2",
        "-c:a",
        "pcm_s24le",
        out.to_str().unwrap(),
    ]);
    out
}

fn wav(dir: &Path, name: &str, filter: &str, codec: &str, rate: &str, channels: &str) -> PathBuf {
    let out = path(dir, name);
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:r=48000:d=3",
        "-af",
        filter,
        "-c:a",
        codec,
        "-ar",
        rate,
        "-ac",
        channels,
        out.to_str().unwrap(),
    ]);
    out
}

fn good_wav(dir: &Path) -> PathBuf {
    wav(dir, "good.wav", "volume=0dB", "pcm_s24le", "48000", "2")
}

fn mp3(dir: &Path) -> PathBuf {
    let out = path(dir, "good.mp3");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:r=48000:d=3",
        "-ac",
        "2",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "192k",
        out.to_str().unwrap(),
    ]);
    out
}

fn video(dir: &Path, name: &str, size: &str, fps: &str, pix: &str, seconds: &str) -> PathBuf {
    let out = path(dir, name);
    let color = format!("color=c=blue:s={size}:r={fps}:d={seconds}");
    let sine = format!("sine=f=440:r=48000:d={seconds}");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &color,
        "-f",
        "lavfi",
        "-i",
        &sine,
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        pix,
        "-r",
        fps,
        "-c:a",
        "aac",
        "-ac",
        "2",
        "-movflags",
        "+faststart",
        out.to_str().unwrap(),
    ]);
    out
}

fn good_video(dir: &Path) -> PathBuf {
    video(dir, "good.mp4", "1280x720", "30", "yuv420p", "3")
}

fn run(source: &Path, kind: Kind, file: &Path, preset: VideoPreset) -> Report {
    inspect::inspect(
        &Tools::discover(),
        source,
        &[Deliverable {
            kind,
            path: file.into(),
        }],
        preset,
    )
    .unwrap()
}

fn failed(file: &FileReport) -> Vec<&'static str> {
    file.checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.id)
        .collect()
}

#[test]
fn conforming_deliverables_pass_and_inputs_stay_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let items = [
        Deliverable {
            kind: Kind::Video,
            path: good_video(dir),
        },
        Deliverable {
            kind: Kind::Wav,
            path: good_wav(dir),
        },
        Deliverable {
            kind: Kind::Mp3,
            path: mp3(dir),
        },
    ];
    let source = source(dir);
    let before: Vec<_> = items.iter().map(|i| fs::read(&i.path).unwrap()).collect();
    let source_before = fs::read(&source).unwrap();
    let report = inspect::inspect(&Tools::discover(), &source, &items, VideoPreset::Hd).unwrap();
    for file in &report.files {
        assert!(file.passed, "{:?}: {:?}", file.kind, failed(file));
        assert!(file.true_peak_dbtp.unwrap() <= -1.0);
        assert!((file.decoded_seconds.unwrap() - 3.0).abs() < 0.25);
    }
    assert_eq!(report.automatic_result, "pass");
    // 人手の確認は自動合格にしない。
    assert!(!report.manual_review_complete);
    assert!(
        report.manual_checks.len() >= 5
            && report.manual_checks.iter().all(|m| m.status == "unchecked")
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schema"], inspect::SCHEMA);
    assert_eq!(json["files"][0]["sha256"].as_str().unwrap().len(), 64);
    for (item, bytes) in items.iter().zip(before) {
        assert_eq!(fs::read(&item.path).unwrap(), bytes);
    }
    assert_eq!(fs::read(&source).unwrap(), source_before);
}

#[test]
fn video_format_defects_are_each_detected() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    let hd = good_video(dir);
    // 寸法: 720pを縦1080pの基準で検査。
    let report = run(&source, Kind::Video, &hd, VideoPreset::Vertical);
    assert_eq!(failed(&report.files[0]), ["dimensions"]);
    assert_eq!(report.automatic_result, "fail");
    // フレームレート。
    let fps25 = video(dir, "fps25.mp4", "1280x720", "25", "yuv420p", "3");
    assert_eq!(
        failed(&run(&source, Kind::Video, &fps25, VideoPreset::Hd).files[0]),
        ["frame_rate_30"]
    );
    // ピクセル形式。
    let yuv444 = video(dir, "yuv444.mp4", "1280x720", "30", "yuv444p", "3");
    assert_eq!(
        failed(&run(&source, Kind::Video, &yuv444, VideoPreset::Hd).files[0]),
        ["pixel_format_yuv420p"]
    );
    // 長さ: 元音源3秒に対して2秒。
    let short = video(dir, "short.mp4", "1280x720", "30", "yuv420p", "2");
    let report = run(&source, Kind::Video, &short, VideoPreset::Hd);
    let ids = failed(&report.files[0]);
    assert!(ids.contains(&"duration_matches_source"), "{ids:?}");
    assert!(
        ids.contains(&"video_stream_duration_matches_source"),
        "{ids:?}"
    );
}

#[test]
fn audio_format_and_peak_defects_are_each_detected() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    let wav16 = wav(dir, "wav16.wav", "volume=0dB", "pcm_s16le", "44100", "1");
    let ids = failed(&run(&source, Kind::Wav, &wav16, VideoPreset::Hd).files[0]);
    assert_eq!(
        ids,
        [
            "audio_codec_pcm_s24le",
            "sample_rate_48000",
            "channels_stereo"
        ]
    );
    // 約-0.1dBTPは不合格、約-1.6dBTPは合格（-1dBTPの境界）。
    let hot = wav(dir, "hot.wav", "volume=21dB", "pcm_s24le", "48000", "2");
    let report = run(&source, Kind::Wav, &hot, VideoPreset::Hd);
    assert_eq!(failed(&report.files[0]), ["true_peak_limit"]);
    assert!(report.files[0].true_peak_dbtp.unwrap() > -1.0);
    let loud = wav(dir, "loud.wav", "volume=19.5dB", "pcm_s24le", "48000", "2");
    let report = run(&source, Kind::Wav, &loud, VideoPreset::Hd);
    assert!(report.files[0].passed, "{:?}", failed(&report.files[0]));
    // 無音は合格にしない。
    let silent = wav(
        dir,
        "silent.wav",
        "volume=-200dB",
        "pcm_s24le",
        "48000",
        "2",
    );
    let report = run(&source, Kind::Wav, &silent, VideoPreset::Hd);
    assert_eq!(failed(&report.files[0]), ["true_peak_limit"]);
    // MP3をWAVとして納品。
    let ids = failed(&run(&source, Kind::Wav, &mp3(dir), VideoPreset::Hd).files[0]);
    assert!(
        ids.contains(&"container_wav") && ids.contains(&"audio_codec_pcm_s24le"),
        "{ids:?}"
    );
}

#[test]
fn corrupt_and_truncated_files_fail() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    // 動画の中央を壊す。
    let broken = good_video(dir);
    let mut bytes = fs::read(&broken).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 4000] {
        *b = 0xA5;
    }
    fs::write(&broken, bytes).unwrap();
    let report = run(&source, Kind::Video, &broken, VideoPreset::Hd);
    assert!(!report.files[0].passed);
    assert_eq!(report.automatic_result, "fail");
    assert!(
        failed(&report.files[0]).contains(&"decodes_cleanly"),
        "{:?}",
        failed(&report.files[0])
    );
    // WAVを途中で切る（ヘッダーは3秒のまま）。
    let cut = good_wav(dir);
    let bytes = fs::read(&cut).unwrap();
    fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
    let report = run(&source, Kind::Wav, &cut, VideoPreset::Hd);
    assert!(!report.files[0].passed, "truncated WAV passed");
    // 空ファイル。
    let empty = path(dir, "empty.mp4");
    fs::write(&empty, b"").unwrap();
    let report = run(&source, Kind::Video, &empty, VideoPreset::Hd);
    assert_eq!(failed(&report.files[0])[0], "readable");
}

#[test]
fn input_errors_are_errors_not_reports() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    let good = good_wav(dir);
    let tools = Tools::discover();
    let item = |p: &Path| Deliverable {
        kind: Kind::Wav,
        path: p.into(),
    };
    assert!(inspect::inspect(&tools, &source, &[], VideoPreset::Hd).is_err());
    assert!(
        inspect::inspect(
            &tools,
            &source,
            &[item(&path(dir, "missing.wav"))],
            VideoPreset::Hd
        )
        .is_err()
    );
    assert!(
        inspect::inspect(
            &tools,
            &path(dir, "missing-source.wav"),
            &[item(&good)],
            VideoPreset::Hd
        )
        .is_err()
    );
    assert!(inspect::inspect(&tools, &source, &[item(&source)], VideoPreset::Hd).is_err());
    assert!(
        inspect::inspect(
            &tools,
            &source,
            &[item(&good), item(&good)],
            VideoPreset::Hd
        )
        .is_err()
    );
    let not_audio = path(dir, "source.txt");
    fs::write(&not_audio, "text").unwrap();
    assert!(inspect::inspect(&tools, &not_audio, &[item(&good)], VideoPreset::Hd).is_err());
}

#[test]
fn cli_writes_json_and_exits_nonzero_only_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_karaoke-maker-cli"))
            .arg("--data")
            .arg(dir.join("data"))
            .arg("inspect-delivery")
            .args(args)
            .output()
            .unwrap()
    };
    let (src, good) = (source.to_str().unwrap(), good_wav(dir));
    let report_path = path(dir, "ok.json");
    let out = cli(&[
        "--source",
        src,
        "--wav",
        good.to_str().unwrap(),
        "--output",
        report_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(json["automatic_result"], "pass");
    assert_eq!(json["manual_review_complete"], false);
    // 既存の結果は上書きしない。
    let again = cli(&[
        "--source",
        src,
        "--wav",
        good.to_str().unwrap(),
        "--output",
        report_path.to_str().unwrap(),
    ]);
    assert!(!again.status.success());
    // 不合格: 終了コード非0、JSONは残る。
    let hot = wav(dir, "hot.wav", "volume=21dB", "pcm_s24le", "48000", "2");
    let fail_path = path(dir, "fail.json");
    let out = cli(&[
        "--source",
        src,
        "--wav",
        hot.to_str().unwrap(),
        "--output",
        fail_path.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&fs::read(&fail_path).unwrap()).unwrap();
    assert_eq!(json["automatic_result"], "fail");
    assert_eq!(
        json["files"][0]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["passed"] == false)
            .count(),
        1
    );
    // 納品物の指定なし・存在しないファイルも非0。
    assert!(!cli(&["--source", src]).status.success());
    assert!(
        !cli(&[
            "--source",
            src,
            "--mp3",
            path(dir, "none.mp3").to_str().unwrap()
        ])
        .status
        .success()
    );
}

#[test]
fn padded_tail_and_missing_audio_are_failures_with_json_and_nonzero_exit() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    // 映像だけ0.9秒長い（末尾の余白）。音声は3秒。
    let padded = path(dir, "padded.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=1280x720:r=30:d=3.9",
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:r=48000:d=3",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-r",
        "30",
        "-c:a",
        "aac",
        "-ac",
        "2",
        padded.to_str().unwrap(),
    ]);
    let report = run(&source, Kind::Video, &padded, VideoPreset::Hd);
    let ids = failed(&report.files[0]);
    assert!(ids.contains(&"duration_matches_source"), "{ids:?}");
    assert!(
        ids.contains(&"video_stream_duration_matches_source"),
        "{ids:?}"
    );
    // 音声なしの映像のみ。
    let silent = path(dir, "no-audio.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=1280x720:r=30:d=3",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-r",
        "30",
        "-an",
        silent.to_str().unwrap(),
    ]);
    let report = run(&source, Kind::Video, &silent, VideoPreset::Hd);
    let ids = failed(&report.files[0]);
    assert!(
        ids.contains(&"audio_stream_present") && ids.contains(&"audio_codec_aac"),
        "{ids:?}"
    );
    assert!(report.files[0].true_peak_dbtp.is_none());
    // CLIは両方で終了コード1、JSONは残る。
    for (file, expect) in [
        (&padded, "duration_matches_source"),
        (&silent, "audio_stream_present"),
    ] {
        let json_path = path(dir, &format!("{expect}.json"));
        let out = Command::new(env!("CARGO_BIN_EXE_karaoke-maker-cli"))
            .arg("--data")
            .arg(dir.join("data"))
            .args(["inspect-delivery", "--preset", "hd", "--source"])
            .arg(&source)
            .arg("--video")
            .arg(file)
            .arg("--output")
            .arg(&json_path)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
        assert_eq!(json["automatic_result"], "fail");
        let bad: Vec<_> = json["files"][0]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["passed"] == false)
            .map(|c| c["id"].as_str().unwrap().to_owned())
            .collect();
        assert!(bad.iter().any(|id| id == expect), "{bad:?}");
    }
}

#[test]
fn alternate_default_audio_cannot_hide_an_overload_behind_a_quiet_first_track() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let source = source(dir);
    let movie = path(dir, "alternate-default.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=1280x720:r=30:d=3",
        "-i",
        source.to_str().unwrap(),
        "-f",
        "lavfi",
        "-i",
        "sine=f=1000:r=48000:d=3",
        "-filter_complex",
        "[2:a]volume=12[hot]",
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        "-map",
        "[hot]",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-disposition:a:0",
        "0",
        "-disposition:a:1",
        "default",
        "-t",
        "3",
        movie.to_str().unwrap(),
    ]);
    // The alternate default track overloads even though the first track is quiet.
    let peak = Command::new(Tools::discover().ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(&movie)
        .args([
            "-map",
            "0:a:1",
            "-af",
            "ebur128=peak=true",
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    assert!(peak.status.success());
    let stderr = String::from_utf8_lossy(&peak.stderr);
    let hot_peak: f64 = stderr
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("Peak:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(hot_peak > 0.0, "{stderr}");
    let report = run(&source, Kind::Video, &movie, VideoPreset::Hd);
    assert!(report.files[0].true_peak_dbtp.unwrap() < -1.0);
    assert!(!report.passed());
    assert_eq!(failed(&report.files[0]), vec!["single_audio_stream"]);

    let output_path = path(dir, "alternate-default.json");
    let result = Command::new(env!("CARGO_BIN_EXE_karaoke-maker-cli"))
        .args(["inspect-delivery", "--source"])
        .arg(&source)
        .arg("--video")
        .arg(&movie)
        .args(["--preset", "hd", "--output"])
        .arg(&output_path)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&fs::read(output_path).unwrap()).unwrap();
    assert_eq!(json["automatic_result"], "fail");
}
