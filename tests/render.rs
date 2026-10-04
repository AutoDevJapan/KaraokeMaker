use karaoke_maker::{
    Progress,
    media::{self, Options, Tools},
};
use std::fs;

#[test]
fn cli_renders_original_mix_and_revisions_in_full_hd_and_vertical_without_a_model() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original.wav");
    let samples: Vec<f32> = (0..44100)
        .map(|i| (i as f32 * std::f32::consts::TAU * 440.0 / 44100.0).sin() * 0.1)
        .collect();
    media::write_stereo(&source, &samples, &samples).unwrap();
    let original = fs::read(&source).unwrap();
    let lrc = temp.path().join("lyrics.lrc");
    let style = temp.path().join("style.json");
    fs::write(
        &style,
        r#"{"background":"301020","accent":"abcdef","font":"DejaVu Sans"}"#,
    )
    .unwrap();
    let data = temp.path().join("data");
    let output = temp.path().join("results");
    for (preset, width, height, text) in [
        ("1080p", 1920, 1080, "first"),
        ("1440p", 2560, 1440, "qhd"),
        ("4k", 3840, 2160, "uhd"),
        ("vertical", 1080, 1920, "revised"),
    ] {
        fs::write(&lrc, format!("[00:00.10]{text}\n[00:00.80]\n")).unwrap();
        let result = media::hidden_command(env!("CARGO_BIN_EXE_karaoke-maker-cli"))
            .arg("--data")
            .arg(&data)
            .arg("render")
            .arg(&source)
            .arg("--output")
            .arg(&output)
            .args(["--title", "test", "--preset", preset])
            .arg("--lyrics")
            .arg(&lrc)
            .arg("--style")
            .arg(&style)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let dir = std::path::PathBuf::from(String::from_utf8(result.stdout).unwrap().trim());
        let probe = media::hidden_command(Tools::discover().ffprobe)
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(dir.join("karaoke.mp4"))
            .output()
            .unwrap();
        assert!(probe.status.success());
        let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let streams = json["streams"].as_array().unwrap();
        let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        assert_eq!(video["width"], width);
        assert_eq!(video["height"], height);
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(video["pix_fmt"], "yuv420p");
        assert_eq!(video["r_frame_rate"], "30/1");
        assert_eq!(
            streams.iter().find(|s| s["codec_type"] == "audio").unwrap()["codec_name"],
            "aac"
        );
        assert_eq!(
            fs::read_to_string(dir.join("lyrics.lrc")).unwrap(),
            format!("[00:00.10]{text}\n[00:00.80]\n")
        );
        assert_eq!(
            fs::read_to_string(dir.join("lyrics.srt")).unwrap(),
            format!("1\n00:00:00,100 --> 00:00:00,800\n{text}\n\n")
        );
        let ass = fs::read_to_string(dir.join("lyrics.ass")).unwrap();
        assert!(ass.contains("&H00efcdab"));
        assert!(ass.contains(&format!("PlayResX: {width}")));
        assert!(ass.contains(&format!("PlayResY: {height}")));
        // Inspect decoded pixels in the lyric area: dimensions and sidecar files alone
        // would still pass if the ASS filter were accidentally removed.
        let frame = media::hidden_command(Tools::discover().ffmpeg)
            .args(["-v", "error", "-ss", "0.4", "-i"])
            .arg(dir.join("karaoke.mp4"))
            .args([
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(frame.status.success());
        assert_eq!(frame.stdout.len(), width as usize * height as usize * 3);
        let start = width as usize * (height as usize * 60 / 100) * 3;
        let end = width as usize * (height as usize * 95 / 100) * 3;
        let bright = frame.stdout[start..end]
            .chunks_exact(3)
            .filter(|pixel| pixel.iter().all(|&channel| channel > 128))
            .count();
        assert!(
            bright > 200,
            "字幕の明るい画素がありません: {preset}: {bright}"
        );
    }
    assert_eq!(fs::read_dir(output).unwrap().count(), 4);
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(!data.join("models").exists());
}

#[test]
fn srt_roundtrip_through_ffmpeg_keeps_literal_tags_and_does_not_add_styles() {
    use karaoke_maker::lyrics;
    use std::{io::Write, process::Stdio};
    let cues = lyrics::parse_lrc("[00:00.10]{\\an8}<i>hello</i>\n[00:00.80]").unwrap();
    let srt = lyrics::make_srt(&cues, 1.0, 1.0).unwrap();
    assert!(srt.contains("｛＼an8｝＜i＞hello＜/i＞"));
    let mut child = media::hidden_command(Tools::discover().ffmpeg)
        .args([
            "-v", "error", "-f", "srt", "-i", "pipe:0", "-f", "ass", "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(srt.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoded = String::from_utf8(output.stdout).unwrap();
    let dialogue = decoded
        .lines()
        .find(|line| line.starts_with("Dialogue:"))
        .unwrap();
    assert!(dialogue.contains("｛＼an8｝＜i＞hello＜/i＞"));
    assert!(!dialogue.contains("{\\an8}"));
    assert!(!dialogue.contains("{\\i1}"));
}

#[test]
fn old_settings_keep_hd_output_and_invalid_style_is_rejected() {
    let options: Options = serde_json::from_str(
        r#"{"title":"old","key":0,"tempo":1.0,"lyrics":"","original_video":false}"#,
    )
    .unwrap();
    assert_eq!(options.preset.dimensions(), (1280, 720));
    options.validate().unwrap();
    for style in [
        r#"{"background":"abc,ass=evil"}"#,
        r#"{"accent":"１２３４５６"}"#,
        r#"{"font":"fake\n[Events]"}"#,
        r#"{"font":"fake,100"}"#,
        r#"{"font":""}"#,
        r#"{"accentt":"abcdef"}"#,
    ] {
        let parsed = serde_json::from_str::<karaoke_maker::presentation::VisualStyle>(style);
        assert!(
            parsed.is_err() || parsed.unwrap().validate().is_err(),
            "{style}"
        );
    }
}

#[test]
fn real_ffmpeg_exports_retimed_pitched_audio_and_captioned_video_without_modifying_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original.wav");
    let count = 44100 * 4;
    let signal: Vec<f32> = (0..count)
        .map(|i| (i as f32 * std::f32::consts::TAU * 261.6256 / 44100.0).sin() * 0.2)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    let original = fs::read(&source).unwrap();
    let dir = temp.path().join("output");
    fs::create_dir(&dir).unwrap();
    media::write_stereo(&dir.join("instrumental-raw.wav"), &signal, &signal).unwrap();
    media::write_stereo(
        &dir.join("vocals.wav"),
        &vec![0.0; count],
        &vec![0.0; count],
    )
    .unwrap();
    let tools = Tools::discover();
    tools.check().unwrap();
    let probe = media::probe(&tools, &source).unwrap();
    assert!(!probe.video);
    let options = Options {
        title: "字幕の試験".into(),
        key: 3,
        tempo: 1.25,
        lyrics: "[00:00.50]最初の行\n[00:02.00]次の行".into(),
        original_video: false,
        ..Options::default()
    };
    media::export(
        &tools,
        &source,
        &dir,
        &probe,
        &options,
        4.0,
        &Progress::default(),
    )
    .unwrap();
    assert_eq!(fs::read(&source).unwrap(), original);
    let video = media::probe(&tools, &dir.join("karaoke.mp4")).unwrap();
    assert!(video.video);
    assert!((video.duration - 3.2).abs() < 0.1);
    let ass = fs::read_to_string(dir.join("lyrics.ass")).unwrap();
    assert!(ass.contains("Dialogue: 1,0:00:00.40,0:00:01.60,Lyrics"));
    let mut reader = hound::WavReader::open(dir.join("karaoke.wav")).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 24);
    assert_eq!(reader.spec().channels, 2);
    let samples: Vec<i32> = reader
        .samples::<i32>()
        .map(Result::unwrap)
        .step_by(2)
        .collect();
    let samples = &samples[44100 / 2..samples.len() - 44100 / 2];
    let crossings = samples
        .windows(2)
        .filter(|pair| pair[0] <= 0 && pair[1] > 0)
        .count();
    let frequency = crossings as f64 / (samples.len() as f64 / 44100.0);
    assert!(
        (frequency - 311.1269).abs() < 1.5,
        "measured pitch {frequency}"
    );
    let before = fs::read(dir.join("karaoke.wav")).unwrap();
    assert!(
        media::export(
            &tools,
            &source,
            &dir,
            &probe,
            &options,
            4.0,
            &Progress::default()
        )
        .is_err()
    );
    assert_eq!(fs::read(dir.join("karaoke.wav")).unwrap(), before);
}
