use karaoke_maker::{
    Progress,
    media::{self, Options, Tools},
};
use std::fs;

#[test]
fn unchanged_key_and_speed_preserve_stereo_transients_to_pcm24_precision() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let left: Vec<f32> = (0..44100)
        .map(|i| {
            let t = i as f32 / 44100.;
            0.07 * (std::f32::consts::TAU * 127.0 * t).sin()
                + 0.045 * (std::f32::consts::TAU * 9837.0 * t).sin()
                + if i % 173 == 0 { 0.2 } else { 0.0 }
        })
        .collect();
    let right: Vec<f32> = left
        .iter()
        .enumerate()
        .map(|(i, x)| -*x * 0.7 + if i % 241 == 0 { 0.12 } else { 0.0 })
        .collect();
    media::write_stereo(&source, &left, &right).unwrap();
    let original = fs::read(&source).unwrap();
    let dir = temp.path().join("out");
    fs::create_dir(&dir).unwrap();
    media::write_stereo(&dir.join("instrumental-raw.wav"), &left, &right).unwrap();
    media::write_stereo(
        &dir.join("vocals.wav"),
        &vec![0.0; 44100],
        &vec![0.0; 44100],
    )
    .unwrap();
    let tools = Tools::discover();
    let probe = media::probe(&tools, &source).unwrap();
    let options = Options {
        original_video: false,
        ..Options::default()
    };
    media::export(
        &tools,
        &source,
        &dir,
        &probe,
        &options,
        1.0,
        &Progress::default(),
    )
    .unwrap();
    let mut reader = hound::WavReader::open(dir.join("karaoke.wav")).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 24);
    assert_eq!(reader.spec().channels, 2);
    let output: Vec<f32> = reader
        .samples::<i32>()
        .map(|s| s.unwrap() as f32 / 8388608.0)
        .collect();
    assert_eq!(output.len(), 88200);
    let mut max_error = 0f32;
    for (pair, (&l, &r)) in output.chunks_exact(2).zip(left.iter().zip(&right)) {
        max_error = max_error.max((pair[0] - l).abs()).max((pair[1] - r).abs());
    }
    assert!(
        max_error <= 2.0 / 8388608.0,
        "unchanged setting altered transients: max sample error={max_error}"
    );
    assert_eq!(fs::read(&source).unwrap(), original);
}
