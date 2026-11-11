use karaoke_maker::media;
use std::{fs, path::Path, process::Output};

fn invoke(executable: &str, data: &Path, args: &[&str]) -> Output {
    media::hidden_command(executable)
        .arg("--data")
        .arg(data)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_errors_return_nonzero_and_stderr_without_interaction() {
    let temp = tempfile::tempdir().unwrap();
    let blocked_data = temp.path().join("data-is-a-file");
    fs::write(&blocked_data, b"preserve this file").unwrap();
    let commands: &[&[&str]] = &[
        &["serve"],
        &["serve", "--no-open"],
        &["convert", "source.wav", "--output", "out"],
        &[
            "render",
            "source.wav",
            "--output",
            "out",
            "--title",
            "sample",
            "--lyrics",
            "lyrics.lrc",
        ],
        &["fetch-model"],
        &["setup-runtime"],
        &["check"],
        &[
            "align-english",
            "source.wav",
            "--lyrics",
            "reference.txt",
            "--output",
            "out.json",
        ],
        &[
            "align-whisper",
            "transcript.json",
            "--lyrics",
            "reference.txt",
            "--output",
            "out.json",
        ],
        &["read-subtitle-frames", "frames", "--output", "out.json"],
        &["measure-audio", "source.wav"],
        &["inspect-delivery", "--source", "source.wav"],
        &["check-runtime"],
    ];
    for executable in [
        env!("CARGO_BIN_EXE_karaoke-maker-cli"),
        env!("CARGO_BIN_EXE_karaoke-maker"),
    ] {
        for args in commands {
            let result = invoke(executable, &blocked_data, args);
            assert_eq!(result.status.code(), Some(1), "{executable} {args:?}");
            assert!(!result.stderr.is_empty(), "{executable} {args:?}");
            assert!(result.stdout.is_empty(), "{executable} {args:?}");
            assert_eq!(fs::read(&blocked_data).unwrap(), b"preserve this file");
        }
    }
    // A CLI invocation without a subcommand also stays noninteractive on error.
    let result = invoke(env!("CARGO_BIN_EXE_karaoke-maker-cli"), &blocked_data, &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(!result.stderr.is_empty());
}

#[test]
fn alignment_error_keeps_inputs_and_does_not_create_output() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("transcript.json");
    let reference = temp.path().join("reference.txt");
    let output = temp.path().join("words.json");
    let transcript_bytes =
        br#"{"transcription":[{"tokens":[{"text":"sample","offsets":{"from":1000,"to":1000}}]}]}"#;
    fs::write(&transcript, transcript_bytes).unwrap();
    fs::write(&reference, "sample").unwrap();
    let result = invoke(
        env!("CARGO_BIN_EXE_karaoke-maker-cli"),
        &temp.path().join("data"),
        &[
            "align-whisper",
            transcript.to_str().unwrap(),
            "--lyrics",
            reference.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(!result.stderr.is_empty());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    assert_eq!(fs::read(&transcript).unwrap(), transcript_bytes);
    assert_eq!(fs::read_to_string(&reference).unwrap(), "sample");
}

#[test]
fn help_and_version_still_exit_successfully() {
    for executable in [
        env!("CARGO_BIN_EXE_karaoke-maker-cli"),
        env!("CARGO_BIN_EXE_karaoke-maker"),
    ] {
        for flag in ["--help", "--version"] {
            let result = media::hidden_command(executable)
                .arg(flag)
                .output()
                .unwrap();
            assert!(result.status.success());
            assert!(!result.stdout.is_empty());
            assert!(result.stderr.is_empty());
        }
        let result = media::hidden_command(executable)
            .arg("--unknown-option")
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(!result.stderr.is_empty());
    }
}

#[test]
fn valid_alignment_still_writes_words_and_preserves_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("transcript.json");
    let reference = temp.path().join("reference.txt");
    let transcript_bytes =
        br#"{"transcription":[{"tokens":[{"text":"sample","offsets":{"from":1000,"to":2000}}]}]}"#;
    fs::write(&transcript, transcript_bytes).unwrap();
    fs::write(&reference, "sample").unwrap();
    for (index, executable) in [
        env!("CARGO_BIN_EXE_karaoke-maker-cli"),
        env!("CARGO_BIN_EXE_karaoke-maker"),
    ]
    .into_iter()
    .enumerate()
    {
        let output = temp.path().join(format!("words-{index}.json"));
        let result = invoke(
            executable,
            &temp.path().join("data"),
            &[
                "align-whisper",
                transcript.to_str().unwrap(),
                "--lyrics",
                reference.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ],
        );
        assert!(result.status.success(), "{:?}", result.stderr);
        let words: Vec<karaoke_maker::alignment::Word> =
            serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
        assert!(!words.is_empty());
        assert_eq!(fs::read(&transcript).unwrap(), transcript_bytes);
        assert_eq!(fs::read_to_string(&reference).unwrap(), "sample");
    }
}
