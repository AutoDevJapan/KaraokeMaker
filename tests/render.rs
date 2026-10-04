use karaoke_maker::{
    Progress,
    media::{self, Options, Tools},
};
use std::fs;

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
