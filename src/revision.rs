use crate::{
    Progress, lyrics,
    media::{self, Options},
    presentation::VideoPreset,
};
use anyhow::{Result, ensure};
use std::{fs, path::Path};

/// A visible lyric interval on the already adjusted playback timeline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TimedLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Edits are validated before replacing the current draft. Invalid edits do not
/// consume undo history or replace the last valid lyrics.
pub struct TimingDraft {
    lines: Vec<TimedLine>,
    duration: f64,
    undo: Vec<Vec<TimedLine>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedTimingDraft {
    version: u32,
    duration: f64,
    lines: Vec<TimedLine>,
    undo: Vec<Vec<TimedLine>>,
}

impl TimingDraft {
    /// Persist current lyrics and undo history without embedding media or paths.
    pub fn to_json(&self) -> Result<String> {
        let text = serde_json::to_string(&SavedTimingDraft {
            version: 1,
            duration: self.duration,
            lines: self.lines.clone(),
            undo: self.undo.clone(),
        })?;
        ensure!(text.len() <= 16 * 1024 * 1024, "編集履歴が大きすぎます。");
        Ok(text)
    }

    pub fn from_json(text: &str, expected_duration: f64) -> Result<Self> {
        ensure!(text.len() <= 16 * 1024 * 1024, "編集履歴が大きすぎます。");
        let saved: SavedTimingDraft = serde_json::from_str(text)?;
        ensure!(
            saved.version == 1 && saved.duration == expected_duration,
            "編集履歴の版か曲の長さが一致しません。"
        );
        ensure!(saved.undo.len() <= 100, "編集履歴は100段までです。");
        timed_lines_lrc(&saved.lines, saved.duration)?;
        for lines in &saved.undo {
            timed_lines_lrc(lines, saved.duration)?;
        }
        Ok(Self {
            lines: saved.lines,
            duration: saved.duration,
            undo: saved.undo,
        })
    }
    pub fn lines(&self) -> &[TimedLine] {
        &self.lines
    }

    pub fn from_lrc(text: &str, duration: f64) -> Result<Self> {
        Ok(Self {
            lines: timed_lines_from_lrc(text, duration)?,
            duration,
            undo: Vec::new(),
        })
    }

    pub fn replace(&mut self, lines: Vec<TimedLine>) -> Result<()> {
        timed_lines_lrc(&lines, self.duration)?;
        if lines != self.lines {
            self.undo.push(std::mem::replace(&mut self.lines, lines));
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        if let Some(lines) = self.undo.pop() {
            self.lines = lines;
            true
        } else {
            false
        }
    }

    pub fn lrc(&self) -> Result<String> {
        timed_lines_lrc(&self.lines, self.duration)
    }
}

/// Blank cues end an interval; they are never displayed as editable lyric rows.
pub fn timed_lines_from_lrc(text: &str, duration: f64) -> Result<Vec<TimedLine>> {
    ensure!(
        duration.is_finite() && duration > 0.0,
        "曲の長さが不正です。"
    );
    let cues = lyrics::parse_lrc(text)?;
    let end_centiseconds = (duration * 100.0).round() as u64;
    ensure!(
        cues.iter().all(|cue| cue.seconds <= duration
            || (cue.text.trim().is_empty()
                && (cue.seconds * 100.0).round() as u64 == end_centiseconds)),
        "曲の長さを超える歌詞の時刻があります。"
    );
    let mut lines = Vec::new();
    for (index, cue) in cues.iter().enumerate() {
        if cue.text.trim().is_empty() {
            continue;
        }
        let end = cues
            .get(index + 1)
            .map_or(duration, |next| next.seconds.min(duration));
        lines.push(TimedLine {
            start: cue.seconds,
            end,
            text: cue.text.clone(),
        });
    }
    timed_lines_lrc(&lines, duration)?;
    Ok(lines)
}

/// Convert explicit editor intervals to LRC, including blank cues for rests.
pub fn timed_lines_lrc(lines: &[TimedLine], duration: f64) -> Result<String> {
    ensure!(
        duration.is_finite() && duration > 0.0,
        "曲の長さが不正です。"
    );
    let mut cues = Vec::new();
    let mut previous_end = 0.0;
    for line in lines {
        ensure!(
            line.start.is_finite()
                && line.end.is_finite()
                && line.start >= 0.0
                && line.start < line.end
                && line.end <= duration,
            "開始・終了は曲の範囲内で、開始より終了を後にしてください。"
        );
        ensure!(line.start >= previous_end, "歌詞の時刻が重複しています。");
        ensure!(!line.text.trim().is_empty(), "歌詞を入力してください。");
        ensure!(
            line.text
                .split('\n')
                .all(|part| !part.trim_start().starts_with('[')),
            "歌詞行の先頭の [ はLRCの時刻と区別できません。全角の［を使用してください。"
        );
        // Validate on the persisted centisecond timeline as well as raw input.
        ensure!(
            (line.start * 100.0).round() < (line.end * 100.0).round(),
            "開始と終了は0.01秒以上離してください。"
        );
        let start = (line.start * 100.0).round() / 100.0;
        let end = (line.end * 100.0).round() / 100.0;
        if cues
            .last()
            .is_some_and(|cue: &lyrics::Cue| cue.seconds == start)
        {
            cues.pop();
        }
        cues.push(lyrics::Cue {
            seconds: start,
            text: line.text.clone(),
        });
        cues.push(lyrics::Cue {
            seconds: end,
            text: String::new(),
        });
        previous_end = line.end;
    }
    ensure!(!cues.is_empty(), "歌詞を1行以上入力してください。");
    let lrc = lyrics::to_lrc(&cues);
    lyrics::parse_lrc(&lrc)?;
    Ok(lrc)
}

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

#[cfg(test)]
mod timing_tests {
    use super::*;
    fn line(start: f64, end: f64, text: &str) -> TimedLine {
        TimedLine {
            start,
            end,
            text: text.into(),
        }
    }
    #[test]
    fn intervals_preserve_rests_multiline_and_adjacent_lyrics() {
        let text = timed_lines_lrc(
            &[
                line(1.0, 2.0, "lead\nharmony"),
                line(3.0, 4.0, "next"),
                line(4.0, 5.0, "last"),
            ],
            5.0,
        )
        .unwrap();
        let cues = lyrics::parse_lrc(&text).unwrap();
        assert_eq!(
            cues.iter().map(|c| c.seconds).collect::<Vec<_>>(),
            [1.0, 2.0, 3.0, 4.0, 5.0]
        );
        assert_eq!(cues[0].text, "lead\nharmony");
        assert_eq!(cues[1].text, "");
        assert_eq!(cues[3].text, "last");
        assert_eq!(cues[4].text, "");
    }
    #[test]
    fn saved_history_restores_undo_and_rejects_other_song_duration() {
        let mut draft = TimingDraft::from_lrc("[00:01.00]original\n[00:02.00]\n", 5.0).unwrap();
        draft.replace(vec![line(2.0, 3.0, "changed")]).unwrap();
        let json = draft.to_json().unwrap();
        let mut reopened = TimingDraft::from_json(&json, 5.0).unwrap();
        assert_eq!(reopened.lines(), draft.lines());
        assert!(reopened.undo());
        assert_eq!(reopened.lines()[0].text, "original");
        assert!(TimingDraft::from_json(&json, 6.0).is_err());
        let invalid = json.replace("changed", "[00:04.00]injected");
        assert!(TimingDraft::from_json(&invalid, 5.0).is_err());
    }
    #[test]
    fn history_preserves_tempo_adjusted_duration_exactly() {
        for tempo in [0.5, 0.8, 1.0, 1.2, 1.5] {
            for micros in (1_000_001..300_000_000).step_by(7919) {
                let duration = micros as f64 / 1_000_000.0 / tempo;
                let draft =
                    TimingDraft::from_lrc("[00:00.00]line\n[00:00.50]\n", duration).unwrap();
                let json = draft.to_json().unwrap();
                let reopened = TimingDraft::from_json(&json, duration).unwrap();
                assert_eq!(reopened.duration.to_bits(), duration.to_bits());
                assert_eq!(reopened.lrc().unwrap(), draft.lrc().unwrap());
            }
        }
    }
    #[test]
    fn history_retains_exactly_one_hundred_edits_in_original_order() {
        let mut draft = TimingDraft::from_lrc("[00:01.00]state-0\n[00:02.00]\n", 5.0).unwrap();
        for index in 1..=150 {
            draft
                .replace(vec![line(1.0, 2.0, &format!("state-{index}"))])
                .unwrap();
        }
        let json = draft.to_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["undo"].as_array().unwrap().len(), 100);
        let mut restored = TimingDraft::from_json(&json, 5.0).unwrap();
        for index in (50..150).rev() {
            assert!(restored.undo());
            assert_eq!(restored.lines()[0].text, format!("state-{index}"));
        }
        assert!(!restored.undo());
        let mut unknown = value;
        unknown["unexpected"] = serde_json::json!(true);
        assert!(TimingDraft::from_json(&unknown.to_string(), 5.0).is_err());
    }
    #[test]
    fn history_size_limit_accepts_exact_boundary_and_rejects_one_more_byte() {
        const LIMIT: usize = 16 * 1024 * 1024;
        let draft = TimingDraft::from_lrc("[00:01.00]line\n[00:02.00]\n", 5.0).unwrap();
        let mut input = draft.to_json().unwrap();
        input.extend(std::iter::repeat_n(' ', LIMIT - input.len()));
        assert!(TimingDraft::from_json(&input, 5.0).is_ok());
        input.push(' ');
        assert!(TimingDraft::from_json(&input, 5.0).is_err());

        // JSON escapes expand one valid LRC byte into six persisted bytes.
        // Build a valid bounded history, then fill its JSON to the exact limit.
        let mut large = draft;
        let state = vec![line(1.0, 2.0, &"\u{0001}".repeat(60_000))];
        large.lines = state.clone();
        large.undo = vec![state; 45];
        let size = large.to_json().unwrap().len();
        assert!(size < LIMIT);
        let remaining = LIMIT - size;
        let mut remaining = remaining;
        for lines in std::iter::once(&mut large.lines).chain(large.undo.iter_mut()) {
            let count = remaining.min(5_000);
            lines[0].text.push_str(&"x".repeat(count));
            remaining -= count;
        }
        assert_eq!(remaining, 0);
        let json = large.to_json().unwrap();
        assert_eq!(json.len(), LIMIT);
        assert!(TimingDraft::from_json(&json, 5.0).is_ok());
        large.lines[0].text.push('x');
        assert!(large.to_json().is_err());
    }
    #[test]
    fn corrupt_unsupported_or_excessive_history_cannot_be_restored() {
        let mut draft = TimingDraft::from_lrc("[00:01.00]original\n[00:02.00]\n", 5.0).unwrap();
        draft.replace(vec![line(2.0, 3.0, "changed")]).unwrap();
        let json = draft.to_json().unwrap();
        assert!(TimingDraft::from_json("{", 5.0).is_err());
        assert!(
            TimingDraft::from_json(&json.replace("\"version\":1", "\"version\":2"), 5.0).is_err()
        );
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["undo"] = serde_json::json!(vec![vec![line(1.0, 2.0, "old")]; 101]);
        assert!(TimingDraft::from_json(&value.to_string(), 5.0).is_err());
        value["undo"] = serde_json::json!([vec![line(4.0, 3.0, "invalid")]]);
        assert!(TimingDraft::from_json(&value.to_string(), 5.0).is_err());
        assert_eq!(draft.lines()[0].text, "changed");
        assert!(draft.undo());
        assert_eq!(draft.lines()[0].text, "original");
    }
    #[test]
    fn accepted_edits_remain_readable_at_rounded_song_end() {
        for duration in [5.006, 59.996, 60.006, 68.036, 599.996, 899.996] {
            let saved = timed_lines_lrc(&[line(1.0, duration, "end")], duration).unwrap();
            let reopened = timed_lines_from_lrc(&saved, duration).unwrap();
            assert!((reopened[0].end - duration).abs() <= 0.005);
            assert_eq!(timed_lines_lrc(&reopened, duration).unwrap(), saved);
            let beyond = (duration * 100.0).round() as u64 + 1;
            let invalid = format!(
                "[00:01.00]end\n[{:02}:{:02}.{:02}]\n",
                beyond / 6000,
                beyond / 100 % 60,
                beyond % 100
            );
            assert!(
                timed_lines_from_lrc(&invalid, duration).is_err(),
                "{duration}"
            );
        }
        for ticks in 6000..90_000 {
            let duration = ticks as f64 / 100.0 + 0.006;
            let saved = timed_lines_lrc(&[line(1.0, duration, "end")], duration).unwrap();
            let reopened = timed_lines_from_lrc(&saved, duration).unwrap();
            assert_eq!(timed_lines_lrc(&reopened, duration).unwrap(), saved);
        }
    }
    #[test]
    fn ambiguous_or_oversized_lyrics_do_not_replace_valid_draft() {
        let mut draft = TimingDraft::from_lrc("[00:01.00]before\n[00:02.00]\n", 10.0).unwrap();
        let saved = draft.lrc().unwrap();
        for text in [
            "[chorus]".into(),
            "[00:04.00]extra".into(),
            "line\n [00:04.00]extra".into(),
            "x".repeat(65536),
        ] {
            assert!(draft.replace(vec![line(1.0, 2.0, &text)]).is_err());
            assert_eq!(draft.lrc().unwrap(), saved);
        }
        assert!(!draft.undo());
    }
    #[test]
    fn failed_edit_preserves_valid_draft_and_undo_history() {
        let original = "[00:01.00]first\n[00:02.00]\n";
        let mut draft = TimingDraft::from_lrc(original, 5.0).unwrap();
        assert!(!draft.undo());
        draft.replace(vec![line(2.0, 3.0, "changed")]).unwrap();
        let valid = draft.lrc().unwrap();
        assert!(draft.replace(vec![line(4.0, 3.0, "invalid")]).is_err());
        assert_eq!(draft.lrc().unwrap(), valid);
        assert!(draft.undo());
        assert_eq!(draft.lrc().unwrap(), original);
        assert!(!draft.undo());
    }
    #[test]
    fn opening_and_saving_editor_preserves_explicit_rests() {
        let input = "[00:01.00]first\n[00:02.00]\n[00:03.00]last\n[00:05.00]\n";
        let lines = timed_lines_from_lrc(input, 5.0).unwrap();
        assert_eq!(lines, vec![line(1.0, 2.0, "first"), line(3.0, 5.0, "last")]);
        assert_eq!(timed_lines_lrc(&lines, 5.0).unwrap(), input);
        assert!(timed_lines_from_lrc("[00:05.00]outside", 5.0).is_err());
        assert!(timed_lines_from_lrc("[00:01.00]line\n[00:08.00]\n", 5.0).is_err());
        assert!(timed_lines_from_lrc("[00:01.00]line\n[00:05.02]\n", 5.006).is_err());
    }
    #[test]
    fn rounded_adjacent_boundaries_do_not_merge_a_blank_into_lyrics() {
        let text =
            timed_lines_lrc(&[line(0.0, 1.001, "first"), line(1.002, 2.0, "next")], 2.0).unwrap();
        let cues = lyrics::parse_lrc(&text).unwrap();
        assert_eq!(cues[1].text, "next");
        assert_eq!(cues.len(), 3);
    }
    #[test]
    fn invalid_or_collapsed_intervals_are_rejected() {
        for lines in [
            vec![],
            vec![line(0.0, 0.004, "tiny")],
            vec![line(f64::NAN, 1.0, "bad")],
            vec![line(-1.0, 1.0, "bad")],
            vec![line(2.0, 1.0, "bad")],
            vec![line(0.0, 6.0, "bad")],
            vec![line(0.0, 1.0, " ")],
            vec![line(0.0, 2.0, "one"), line(1.0, 3.0, "two")],
        ] {
            assert!(timed_lines_lrc(&lines, 5.0).is_err(), "{lines:?}");
        }
    }
}
