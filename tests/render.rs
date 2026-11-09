use karaoke_maker::{
    Progress,
    media::{self, Options, Tools},
};
use std::fs;

#[test]
fn render_preserves_48khz_high_band_and_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let mut writer = hound::WavWriter::create(
        &source,
        hound::WavSpec {
            channels: 2,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    let source_samples: Vec<f32> = (0..48000)
        .map(|i| (i as f32 * std::f32::consts::TAU * 23000.0 / 48000.0).sin() * 0.1)
        .collect();
    for &v in &source_samples {
        writer.write_sample(v).unwrap();
        writer.write_sample(v).unwrap();
    }
    writer.finalize().unwrap();
    let bytes = fs::read(&source).unwrap();
    let dir = temp.path().join("out");
    fs::create_dir(&dir).unwrap();
    media::render(
        &source,
        &dir,
        &Options {
            sample_rate: 48000,
            ..Options::default()
        },
        &Progress::default(),
    )
    .unwrap();
    let mut reader = hound::WavReader::open(dir.join("karaoke.wav")).unwrap();
    assert_eq!(reader.spec().sample_rate, 48000);
    let output: Vec<f32> = reader
        .samples::<i32>()
        .map(|v| v.unwrap() as f32 / 8388608.0)
        .step_by(2)
        .collect();
    assert_eq!(output.len(), source_samples.len());
    let error = output
        .iter()
        .zip(&source_samples)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(error < 2e-6, "23kHz waveform changed: {error}");
    assert_eq!(fs::read(source).unwrap(), bytes);
}

#[test]
fn word_timing_burns_visible_subtitles_and_exports_48khz_audio() {
    use karaoke_maker::{alignment::Word, presentation::VideoPreset};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let signal: Vec<f32> = (0..44100 * 2)
        .map(|i| (i as f32 * 0.05).sin() * 0.1)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    let output = temp.path().join("out");
    fs::create_dir(&output).unwrap();
    let words = vec![
        Word {
            text: "Read".into(),
            start: 0.2,
            end: 0.6,
            confidence: 1.0,
            join_previous: false,
            line_index: 0,
        },
        Word {
            text: "me".into(),
            start: 0.6,
            end: 1.0,
            confidence: 1.0,
            join_previous: false,
            line_index: 0,
        },
    ];
    media::render(
        &source,
        &output,
        &Options {
            title: "test".into(),
            preset: VideoPreset::FullHd,
            words,
            ..Options::default()
        },
        &Progress::default(),
    )
    .unwrap();
    let reader = hound::WavReader::open(output.join("karaoke.wav")).unwrap();
    assert_eq!(reader.spec().sample_rate, 48000);
    assert_eq!(reader.spec().bits_per_sample, 24);
    let ass = fs::read_to_string(output.join("lyrics.ass")).unwrap();
    assert!(ass.contains("{\\kf40}Read {\\kf40}me"));
    let srt = fs::read_to_string(output.join("lyrics.srt")).unwrap();
    assert!(srt.contains("00:00:00,200 --> 00:00:01,000\nRead me"));
    let frame = media::hidden_command(Tools::discover().ffmpeg)
        .args(["-v", "error", "-ss", "0.4", "-i"])
        .arg(output.join("karaoke.mp4"))
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
    let region = &frame.stdout[1920 * 700 * 3..1920 * 1000 * 3];
    assert!(
        region
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|p| p.iter().all(|&v| v > 128))
            .count()
            > 500
    );
}

fn solid_png(path: &std::path::Path, size: &str, color: &str) {
    let status = media::hidden_command(Tools::discover().ffmpeg)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c={color}:s={size}"))
        .args(["-frames:v", "1"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn template_render_keeps_long_japanese_subtitles_inside_margins_in_both_orientations() {
    use karaoke_maker::{presentation::VideoPreset, template::Template};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let signal: Vec<f32> = (0..44100 * 2)
        .map(|i| (i as f32 * 0.05).sin() * 0.1)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    // Self-made flat fixtures: a blue background and a red logo.
    let background = temp.path().join("background.png");
    let logo = temp.path().join("logo.png");
    solid_png(&background, "1280x720", "0x203060");
    solid_png(&logo, "200x200", "0xc02020");
    let template = |lines: u8| Template {
        version: 1,
        project: "客先A".into(),
        style: Default::default(),
        background_image: Some(background.clone()),
        logo: Some(logo.clone()),
        horizontal_margin: 0.1,
        vertical_margin: 0.1,
        max_lines: lines,
    };
    let options = |preset, template| Options {
        title: "テンプレート".into(),
        preset,
        lyrics:
            "[00:00.20]吾輩は猫である名前はまだ無いどこで生れたかとんと見当がつかぬ\n[00:01.80]\n"
                .into(),
        template: Some(template),
        project: Some("客先A".into()),
        ..Options::default()
    };
    for (preset, width, height, margins) in [
        // left, right, top, bottom in pixels, from the saved margin and the platform safe area.
        (
            VideoPreset::FullHd,
            1920usize,
            1080usize,
            [192usize, 192, 108, 120],
        ),
        (VideoPreset::Vertical, 1080, 1920, [108, 160, 192, 320]),
    ] {
        let output = temp.path().join(format!("{width}"));
        fs::create_dir(&output).unwrap();
        media::render(
            &source,
            &output,
            &options(preset, template(2)),
            &Progress::default(),
        )
        .unwrap();
        let frame = media::hidden_command(Tools::discover().ffmpeg)
            .args(["-v", "error", "-ss", "0.8", "-i"])
            .arg(output.join("karaoke.mp4"))
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
        assert_eq!(frame.stdout.len(), width * height * 3);
        let bright = |x: usize, y: usize| {
            let at = (y * width + x) * 3;
            frame.stdout[at..at + 3].iter().all(|&v| v > 200)
        };
        let [left, right, top, bottom] = margins;
        let mut inside = 0;
        let mut outside = 0;
        let mut spots = Vec::new();
        for y in 0..height {
            for x in 0..width {
                if !bright(x, y) {
                    continue;
                }
                if x + 4 < left || x > width - right + 4 || y + 4 < top || y > height - bottom + 4 {
                    outside += 1;
                    spots.push((x, y));
                } else {
                    inside += 1;
                }
            }
        }
        assert!(
            inside > 2000,
            "{width}: subtitle text is missing ({inside})"
        );
        assert_eq!(
            outside,
            0,
            "{width}: bright pixels outside the saved margins x {:?}..{:?} y {:?}..{:?}",
            spots.iter().map(|s| s.0).min(),
            spots.iter().map(|s| s.0).max(),
            spots.iter().map(|s| s.1).min(),
            spots.iter().map(|s| s.1).max()
        );
        // The flat background image and the logo are really in the frame.
        let pixel = |x: usize, y: usize| {
            let at = (y * width + x) * 3;
            [frame.stdout[at], frame.stdout[at + 1], frame.stdout[at + 2]]
        };
        assert!(
            pixel(width / 10, height / 2)[2] > pixel(width / 10, height / 2)[0] + 40,
            "{width}: background image"
        );
        // The logo's top-right corner sits on the corner of the safe area (margins included).
        let logo_pixel = pixel(width - right - 8, top + 8);
        assert!(
            logo_pixel[0] > 150 && logo_pixel[1] < 80,
            "{width}: logo {logo_pixel:?}"
        );
        let outside_logo = pixel(width - right + 8, top + 8);
        assert!(
            outside_logo[0] < 100,
            "{width}: logo reaches past the right safe edge {outside_logo:?}"
        );
    }
    // One line is not enough for this lyric, and another project's template is refused.
    let refused = temp.path().join("refused");
    fs::create_dir(&refused).unwrap();
    assert!(
        media::render(
            &source,
            &refused,
            &options(VideoPreset::FullHd, template(1)),
            &Progress::default()
        )
        .is_err()
    );
    let mut other = options(VideoPreset::FullHd, template(2));
    other.project = Some("別の客先".into());
    assert!(media::render(&source, &refused, &other, &Progress::default()).is_err());
}

#[test]
fn truncated_template_background_fails_quickly_instead_of_looping_forever() {
    use karaoke_maker::{presentation::VideoPreset, template::Template};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let signal: Vec<f32> = (0..44100 * 2)
        .map(|i| (i as f32 * 0.05).sin() * 0.1)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    // A PNG cut off after the header passes the signature check but cannot be decoded.
    let full = temp.path().join("full.png");
    solid_png(&full, "800x450", "0x203060");
    let cut = temp.path().join("cut.png");
    fs::write(&cut, &fs::read(&full).unwrap()[..60]).unwrap();
    let output = temp.path().join("out");
    fs::create_dir(&output).unwrap();
    let options = Options {
        lyrics: "[00:00.20]あ\n[00:01.00]\n".into(),
        template: Some(Template {
            version: 1,
            project: "客先A".into(),
            style: Default::default(),
            background_image: Some(cut),
            logo: None,
            horizontal_margin: 0.1,
            vertical_margin: 0.1,
            max_lines: 2,
        }),
        project: Some("客先A".into()),
        preset: VideoPreset::Hd,
        ..Options::default()
    };
    let progress = Progress::default();
    // Stop a run that never ends so that the test itself cannot hang.
    let cancel = progress.cancelled.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(25));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    assert!(media::render(&source, &output, &options, &progress).is_err());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "a broken background must be refused quickly, took {:?}",
        started.elapsed()
    );
}

#[test]
fn convert_checks_the_template_line_limit_before_slow_work() {
    use karaoke_maker::template::Template;
    let template = |lines: u8| Template {
        version: 1,
        project: "客先A".into(),
        style: Default::default(),
        background_image: None,
        logo: None,
        horizontal_margin: 0.1,
        vertical_margin: 0.1,
        max_lines: lines,
    };
    let long = "あ".repeat(60);
    let options = |template: Option<Template>| Options {
        lyrics: format!(
            "[00:01.00]{long}
[00:02.00]
"
        ),
        template,
        project: Some("客先A".into()),
        ..Options::default()
    };
    assert!(media::check_lyrics_layout(&options(Some(template(1))), 3.0).is_err());
    assert!(media::check_lyrics_layout(&options(Some(template(4))), 3.0).is_ok());
    assert!(media::check_lyrics_layout(&options(None), 3.0).is_ok());
}
#[test]
fn still_background_keeps_the_output_length_at_other_tempos() {
    use karaoke_maker::{presentation::VideoPreset, template::Template};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let signal: Vec<f32> = (0..44100 * 2)
        .map(|i| (i as f32 * 0.05).sin() * 0.1)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    // A name with a percent sign is read as a plain file, not as a numbered image sequence.
    let plain = temp.path().join("plain.png");
    solid_png(&plain, "800x450", "0x203060");
    let background = temp.path().join("100%d.png");
    fs::rename(&plain, &background).unwrap();
    for tempo in [0.8, 1.25] {
        let output = temp.path().join(format!("out-{tempo}"));
        fs::create_dir(&output).unwrap();
        let options = Options {
            tempo,
            lyrics: "[00:00.20]あ\n[00:01.00]\n".into(),
            template: Some(Template {
                version: 1,
                project: "客先A".into(),
                style: Default::default(),
                background_image: Some(background.clone()),
                logo: None,
                horizontal_margin: 0.1,
                vertical_margin: 0.1,
                max_lines: 2,
            }),
            project: Some("客先A".into()),
            preset: VideoPreset::Hd,
            ..Options::default()
        };
        media::render(&source, &output, &options, &Progress::default()).unwrap();
        let video = media::probe(&Tools::discover(), &output.join("karaoke.mp4")).unwrap();
        assert!(
            (video.duration - 2.0 / tempo).abs() < 0.25,
            "{tempo}: {}",
            video.duration
        );
    }
}

#[test]
fn loud_audio_keeps_headroom_after_wav_mp3_and_aac_export() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original.wav");
    let signal: Vec<f32> = (0..44100 * 2)
        .map(|i| (i as f32 * std::f32::consts::TAU * 19000.0 / 44100.0).sin() * 0.97)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    let original = fs::read(&source).unwrap();
    let dir = temp.path().join("output");
    fs::create_dir(&dir).unwrap();
    media::write_stereo(&dir.join("instrumental-raw.wav"), &signal, &signal).unwrap();
    media::write_stereo(&dir.join("vocals.wav"), &signal, &signal).unwrap();
    let tools = Tools::discover();
    assert!(media::true_peak(&tools, &source).unwrap() > -1.0);
    media::export(
        &tools,
        &source,
        &dir,
        &media::probe(&tools, &source).unwrap(),
        &Options::default(),
        2.0,
        &Progress::default(),
    )
    .unwrap();
    assert!(media::true_peak(&tools, &dir.join("karaoke.wav")).unwrap() <= -3.0);
    for name in ["karaoke.mp3", "karaoke.mp4"] {
        assert!(
            media::true_peak(&tools, &dir.join(name)).unwrap() < 0.0,
            "{name}"
        );
    }
    assert_eq!(fs::read(source).unwrap(), original);
}

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
        ("vertical-4k", 2160, 3840, "vertical-uhd"),
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
        if preset == "vertical-4k" {
            assert!(ass.contains(",2,160,320,640,1"));
            assert!(ass.contains(",8,160,320,320,1"));
        }
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
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|pixel| pixel.iter().all(|&channel| channel > 128))
            .count();
        assert!(
            bright > 200,
            "字幕の明るい画素がありません: {preset}: {bright}"
        );
    }
    assert_eq!(fs::read_dir(output).unwrap().count(), 5);
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
fn subtitle_revision_preserves_adjusted_audio_and_original_files() {
    use karaoke_maker::{presentation::VideoPreset, revision};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original.wav");
    let signal: Vec<f32> = (0..44100 * 4)
        .map(|i| (i as f32 * std::f32::consts::TAU * 261.6256 / 44100.0).sin() * 0.2)
        .collect();
    media::write_stereo(&source, &signal, &signal).unwrap();
    let previous = temp.path().join("previous");
    fs::create_dir(&previous).unwrap();
    media::write_stereo(&previous.join("instrumental-raw.wav"), &signal, &signal).unwrap();
    media::write_stereo(&previous.join("vocals.wav"), &signal, &signal).unwrap();
    let tools = Tools::discover();
    let options = Options {
        key: 3,
        tempo: 1.25,
        sample_rate: 44100,
        lyrics: "[00:00.50]before\n[00:02.00]next".into(),
        ..Options::default()
    };
    media::export(
        &tools,
        &source,
        &previous,
        &media::probe(&tools, &source).unwrap(),
        &options,
        4.0,
        &Progress::default(),
    )
    .unwrap();
    fs::write(
        previous.join("settings.json"),
        serde_json::to_vec(&options).unwrap(),
    )
    .unwrap();
    let before_audio = fs::read(previous.join("karaoke.wav")).unwrap();
    let before_video = fs::read(previous.join("karaoke.mp4")).unwrap();
    assert_eq!(
        revision::editable_lrc(&options).unwrap(),
        "[00:00.40]before\n[00:01.60]next\n"
    );
    let revised = temp.path().join("revised");
    fs::create_dir(&revised).unwrap();
    revision::render_revision(
        &previous,
        &revised,
        "[00:00.40]corrected\n[00:01.60]\n",
        VideoPreset::FullHd,
        &Progress::default(),
    )
    .unwrap();
    assert_eq!(
        fs::read(previous.join("karaoke.wav")).unwrap(),
        before_audio
    );
    assert_eq!(
        fs::read(previous.join("karaoke.mp4")).unwrap(),
        before_video
    );
    assert!(
        (media::probe(&tools, &revised.join("karaoke.wav"))
            .unwrap()
            .duration
            - 3.2)
            .abs()
            < 0.01
    );
    assert!(
        (media::probe(&tools, &revised.join("vocals.wav"))
            .unwrap()
            .duration
            - 3.2)
            .abs()
            < 0.02
    );
    let mut reader = hound::WavReader::open(revised.join("karaoke.wav")).unwrap();
    let samples: Vec<i32> = reader
        .samples::<i32>()
        .map(Result::unwrap)
        .step_by(2)
        .collect();
    let center = &samples[22050..samples.len() - 22050];
    let crossings = center.windows(2).filter(|p| p[0] <= 0 && p[1] > 0).count();
    let frequency = crossings as f64 / (center.len() as f64 / 44100.0);
    assert!((frequency - 311.1269).abs() < 1.5, "{frequency}");
    assert!(!temp.path().join("models").exists());
    assert_eq!(
        fs::read_to_string(revised.join("lyrics.srt")).unwrap(),
        "1\n00:00:00,400 --> 00:00:01,600\ncorrected\n\n"
    );
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
        sample_rate: 44100,
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
