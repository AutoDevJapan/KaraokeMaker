use anyhow::{Context, Result, ensure};

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub seconds: f64,
    pub text: String,
}

/// Standard line-timed LRC. Multiple timestamps and global offsets are supported.
pub fn parse_lrc(text: &str) -> Result<Vec<Cue>> {
    ensure!(text.len() <= 64 * 1024, "歌詞は64KB以内にしてください。");
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut offset = 0.0;
    let mut cues = Vec::new();
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let mut rest = line.trim();
        if rest.is_empty() {
            continue;
        }
        if let Some(value) = rest
            .strip_prefix("[offset:")
            .and_then(|s| s.strip_suffix(']'))
        {
            offset = value
                .parse::<i64>()
                .context("歌詞のoffsetはミリ秒の整数にしてください。")? as f64
                / 1000.0;
            ensure!(offset.abs() <= 3600.0, "歌詞のoffsetが大きすぎます。");
            continue;
        }
        if ["[ar:", "[ti:", "[al:", "[by:", "[length:", "[re:", "[ve:"]
            .iter()
            .any(|s| rest.starts_with(s))
        {
            continue;
        }
        let mut stamps = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let (stamp, tail) = after
                .split_once(']')
                .with_context(|| format!("歌詞の{}行目で ] が見つかりません。", index + 1))?;
            let (minutes, seconds) = stamp.split_once(':').with_context(|| {
                format!(
                    "歌詞の{}行目は [00:12.34]歌詞 の形にしてください。",
                    index + 1
                )
            })?;
            let minutes = minutes
                .parse::<u32>()
                .context("歌詞の分は整数にしてください。")?;
            let seconds = seconds
                .parse::<f64>()
                .context("歌詞の秒を読み取れません。")?;
            ensure!(
                seconds.is_finite() && (0.0..60.0).contains(&seconds) && minutes <= 60,
                "歌詞の時刻が範囲外です。"
            );
            stamps.push(minutes as f64 * 60.0 + seconds);
            rest = tail;
        }
        ensure!(
            !stamps.is_empty(),
            "歌詞の{}行目に時刻がありません。[00:12.34]歌詞 の形にしてください。",
            index + 1
        );
        for seconds in stamps {
            cues.push(Cue {
                seconds,
                text: rest.to_string(),
            });
        }
    }
    ensure!(
        !cues.is_empty(),
        "時刻付きの歌詞を1行以上入力してください。"
    );
    for cue in &mut cues {
        cue.seconds = (cue.seconds - offset).max(0.0);
    }
    cues.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    // Duplicate cues at a timestamp become a single two-line subtitle.
    let mut merged: Vec<Cue> = Vec::new();
    for cue in cues {
        if let Some(last) = merged
            .last_mut()
            .filter(|c| (c.seconds - cue.seconds).abs() < 0.00001)
        {
            last.text.push('\n');
            last.text.push_str(&cue.text);
        } else {
            merged.push(cue);
        }
    }
    Ok(merged)
}

fn ass_text(text: &str) -> String {
    text.replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace('\r', "")
        .replace('\n', "\\N")
}

fn stamp(seconds: f64) -> String {
    let cs = (seconds.max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        cs / 6000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}

pub fn make_ass(title: &str, cues: &[Cue], duration: f64, tempo: f64) -> Result<String> {
    ensure!(
        duration.is_finite() && duration > 0.0 && tempo.is_finite() && (0.5..=1.5).contains(&tempo),
        "映像の長さか速度が不正です。"
    );
    ensure!(
        cues.iter()
            .all(|c| c.seconds.is_finite() && c.seconds >= 0.0 && c.seconds < duration),
        "曲の長さを超える歌詞の時刻があります。"
    );
    let mut ass = String::from(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1280\nPlayResY: 720\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Lyrics,Yu Gothic,44,&H00A4FFCE,&H00FFFFFF,&H00202020,&H80000000,-1,0,0,0,100,100,0,0,1,3,1,2,60,60,80,1\nStyle: Title,Yu Gothic,28,&H00FFFFFF,&H00FFFFFF,&H00202020,&H80000000,0,0,0,0,100,100,0,0,1,2,0,8,60,60,32,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );
    ass.push_str(&format!(
        "Dialogue: 0,0:00:00.00,{},Title,,0,0,0,,{}\n",
        stamp(duration / tempo),
        ass_text(title)
    ));
    for (index, cue) in cues.iter().enumerate() {
        let end = cues
            .get(index + 1)
            .map_or(duration, |c| c.seconds)
            .min(duration);
        if end <= cue.seconds || cue.text.trim().is_empty() {
            continue;
        }
        let fill = ((end - cue.seconds) / tempo * 100.0).round().max(1.0) as u64;
        ass.push_str(&format!(
            "Dialogue: 1,{},{},Lyrics,,0,0,0,,{{\\kf{fill}}}{}\n",
            stamp(cue.seconds / tempo),
            stamp(end / tempo),
            ass_text(&cue.text)
        ));
    }
    Ok(ass)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lrc_multiple_stamps_offsets_and_duplicates() {
        let cues =
            parse_lrc("[ti:試験]\n[00:10.50][00:20.50]声\n[00:10.50]合唱\n[offset:-500]").unwrap();
        assert_eq!(
            cues,
            vec![
                Cue {
                    seconds: 11.0,
                    text: "声\n合唱".into()
                },
                Cue {
                    seconds: 21.0,
                    text: "声".into()
                }
            ]
        );
    }
    #[test]
    fn lyrics_errors_are_visible() {
        for invalid in [
            "歌詞だけ",
            "[00:NaN]危険",
            "[00:60]範囲外",
            "[00:02歌詞",
            "[offset:one]",
            "[ti:曲名だけ]",
        ] {
            assert!(parse_lrc(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn ass_uses_output_timeline_and_neutralizes_override_tags() {
        let ass = make_ass(
            "曲名{\\pos(1,1)}",
            &[
                Cue {
                    seconds: 10.0,
                    text: "私{\\p1}".into(),
                },
                Cue {
                    seconds: 20.0,
                    text: "歌う".into(),
                },
            ],
            30.0,
            1.25,
        )
        .unwrap();
        assert!(
            ass.contains("Dialogue: 1,0:00:08.00,0:00:16.00,Lyrics,,0,0,0,,{\\kf800}私｛＼p1｝")
        );
        assert!(!ass.contains("{\\pos(1,1)}"));
        assert!(
            make_ass(
                "曲",
                &[Cue {
                    seconds: 31.0,
                    text: "超過".into()
                }],
                30.0,
                1.0
            )
            .is_err()
        );
    }
}
