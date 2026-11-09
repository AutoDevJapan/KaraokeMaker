use crate::presentation::{VideoPreset, VisualStyle};
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

fn subtitle_text(text: &str) -> String {
    text.replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace('<', "＜")
        .replace('>', "＞")
        .replace('\r', "")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ass_text(text: &str) -> String {
    subtitle_text(text).replace('\n', "\\N")
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

pub fn make_ass_words(
    title: &str,
    words: &[crate::alignment::Word],
    duration: f64,
    tempo: f64,
    preset: VideoPreset,
    style: &VisualStyle,
) -> Result<String> {
    ensure!(
        !words.is_empty() && words.len() <= 10000,
        "単語の時刻が空か多すぎます。"
    );
    let mut previous = 0.0;
    for word in words {
        ensure!(
            word.start.is_finite()
                && word.end.is_finite()
                && word.start >= previous
                && word.end > word.start
                && word.end <= duration + 0.1
                && word.text.len() <= 1024,
            "単語の時刻か文字数が不正です。"
        );
        previous = word.start;
    }
    let mut ass = make_ass_styled(title, &[], duration, tempo, preset, style)?;
    let mut first = 0;
    while first < words.len() {
        let mut last = first + 1;
        let mut count = words[first].text.chars().count();
        while last < words.len()
            && words[last].line_index == words[first].line_index
            && words[last].start - words[first].start < 4.0
            && count + words[last].text.chars().count() < 36
        {
            count += words[last].text.chars().count();
            last += 1;
        }
        let start = words[first].start / tempo;
        let end = words[last - 1].end.min(duration) / tempo;
        let mut text = String::new();
        for (n, word) in words[first..last].iter().enumerate() {
            let next = words[first..last]
                .get(n + 1)
                .map_or(word.end, |w| w.start)
                .max(word.start + 0.01);
            let fill = ((next - word.start) / tempo * 100.0).round().max(1.0) as u64;
            let spacing = if word.text.is_ascii() && n > 0 && !word.join_previous {
                " "
            } else {
                ""
            };
            text.push_str(&format!("{spacing}{{\\kf{fill}}}{}", ass_text(&word.text)));
        }
        ass.push_str(&format!(
            "Dialogue: 1,{},{},Lyrics,,0,0,0,,{text}\n",
            stamp(start),
            stamp(end)
        ));
        first = last;
    }
    Ok(ass)
}

pub fn word_cues(words: &[crate::alignment::Word]) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut first = 0;
    while first < words.len() {
        let mut last = first + 1;
        let mut length = words[first].text.chars().count();
        while last < words.len()
            && words[last].line_index == words[first].line_index
            && words[last].start - words[first].start < 4.0
            && length + words[last].text.chars().count() < 36
        {
            length += words[last].text.chars().count();
            last += 1;
        }
        let text = words[first..last]
            .iter()
            .enumerate()
            .map(|(n, w)| {
                format!(
                    "{}{}",
                    if n > 0 && w.text.is_ascii() && !w.join_previous {
                        " "
                    } else {
                        ""
                    },
                    w.text
                )
            })
            .collect::<String>();
        cues.push(Cue {
            seconds: words[first].start,
            text,
        });
        if words
            .get(last)
            .is_none_or(|w| w.start > words[last - 1].end + 0.01)
        {
            cues.push(Cue {
                seconds: words[last - 1].end,
                text: String::new(),
            });
        }
        first = last;
    }
    cues
}

pub fn to_lrc(cues: &[Cue]) -> String {
    cues.iter()
        .map(|cue| {
            let cs = (cue.seconds * 100.0).round() as u64;
            // Each physical LRC line needs a timestamp, including merged lyrics.
            cue.text
                .split('\n')
                .map(|line| {
                    format!(
                        "[{:02}:{:02}.{:02}]{}\n",
                        cs / 6000,
                        cs / 100 % 60,
                        cs % 100,
                        line
                    )
                })
                .collect::<String>()
        })
        .collect()
}

pub fn make_ass(title: &str, cues: &[Cue], duration: f64, tempo: f64) -> Result<String> {
    make_ass_styled(
        title,
        cues,
        duration,
        tempo,
        VideoPreset::Hd,
        &VisualStyle::default(),
    )
}

fn validate_timeline(cues: &[Cue], duration: f64, tempo: f64) -> Result<()> {
    ensure!(
        duration.is_finite() && duration > 0.0 && tempo.is_finite() && (0.5..=1.5).contains(&tempo),
        "映像の長さか速度が不正です。"
    );
    ensure!(
        cues.iter().all(|c| c.seconds.is_finite()
            && c.seconds >= 0.0
            && (c.seconds < duration || c.text.trim().is_empty() && c.seconds <= duration + 0.02)),
        "曲の長さを超える歌詞の時刻があります。"
    );
    ensure!(
        cues.windows(2)
            .all(|pair| pair[0].seconds < pair[1].seconds),
        "歌詞の時刻は昇順にしてください。"
    );
    Ok(())
}

pub fn make_ass_styled(
    title: &str,
    cues: &[Cue],
    duration: f64,
    tempo: f64,
    preset: VideoPreset,
    style: &VisualStyle,
) -> Result<String> {
    ass_with_layout(title, cues, duration, tempo, preset, style, None)
}

/// Subtitles laid out with a saved project template: margins by orientation and a line limit.
pub fn make_ass_templated(
    title: &str,
    cues: &[Cue],
    duration: f64,
    tempo: f64,
    preset: VideoPreset,
    template: &crate::template::Template,
) -> Result<String> {
    template.validate(&template.project)?;
    ass_with_layout(
        title,
        cues,
        duration,
        tempo,
        preset,
        &template.style,
        Some(template),
    )
}

fn ass_with_layout(
    title: &str,
    cues: &[Cue],
    duration: f64,
    tempo: f64,
    preset: VideoPreset,
    style: &VisualStyle,
    template: Option<&crate::template::Template>,
) -> Result<String> {
    validate_timeline(cues, duration, tempo)?;
    style.validate()?;
    let (width, height) = preset.dimensions();
    let scale = width as f64 / 1280.0;
    let scaled = |value: f64| (value * scale).round() as u32;
    let portrait = matches!(preset, VideoPreset::Vertical | VideoPreset::VerticalUhd);
    let (mut left, mut right, mut bottom, mut top) =
        if matches!(preset, VideoPreset::Vertical | VideoPreset::VerticalUhd) {
            let factor = width as f64 / 1080.0;
            let safe = |value: f64| (value * factor).round() as u32;
            (safe(80.0), safe(160.0), safe(320.0), safe(160.0))
        } else {
            (scaled(60.0), scaled(60.0), scaled(80.0), scaled(32.0))
        };
    if let Some(template) = template {
        // Never smaller than the built-in platform safe area; the template can only widen it.
        let margin = if portrait {
            template.vertical_margin
        } else {
            template.horizontal_margin
        };
        let side = (margin * width as f64).round() as u32;
        let edge = (margin * height as f64).round() as u32;
        left = left.max(side);
        right = right.max(side);
        top = top.max(edge);
        bottom = bottom.max(edge);
    }
    let font = &style.font;
    let accent = style.ass_accent()?;
    let lyric_size = scaled(44.0);
    if let Some(template) = template {
        // CJK glyphs are about one font size wide, so this is a safe upper bound.
        let per_line = (width.saturating_sub(left + right) / lyric_size).max(1) as usize;
        for cue in cues {
            let lines: usize = cue
                .text
                .split('\n')
                .map(|line| line.chars().count().div_ceil(per_line).max(1))
                .sum();
            ensure!(
                lines <= template.max_lines as usize,
                "字幕が{}行を超えます。余白か行数を調整してください。",
                template.max_lines
            );
        }
    }
    let title_size = scaled(28.0);
    let outline = scaled(3.0);
    let title_outline = scaled(2.0);
    let shadow = scaled(1.0);
    // libass rasterizes the vector glyphs after scaling the video to the target size.
    let mut ass = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Lyrics,{font},{lyric_size},{accent},&H00FFFFFF,&H00202020,&H80000000,-1,0,0,0,100,100,0,0,1,{outline},{shadow},2,{left},{right},{bottom},1\nStyle: Title,{font},{title_size},&H00FFFFFF,&H00FFFFFF,&H00202020,&H80000000,0,0,0,0,100,100,0,0,1,{title_outline},0,8,{left},{right},{top},1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
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

/// Editable plain subtitles on the output timeline. Empty LRC cues end a line at rests.
pub fn make_srt(cues: &[Cue], duration: f64, tempo: f64) -> Result<String> {
    validate_timeline(cues, duration, tempo)?;
    fn stamp_srt(seconds: f64) -> String {
        let ms = (seconds * 1000.0).round() as u64;
        format!(
            "{:02}:{:02}:{:02},{:03}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    }
    let mut result = String::new();
    let mut number = 0;
    for (index, cue) in cues.iter().enumerate() {
        let end = cues
            .get(index + 1)
            .map_or(duration, |c| c.seconds)
            .min(duration);
        if cue.text.trim().is_empty() {
            continue;
        }
        number += 1;
        result.push_str(&format!(
            "{number}\n{} --> {}\n{}\n\n",
            stamp_srt(cue.seconds / tempo),
            stamp_srt(end / tempo),
            subtitle_text(&cue.text)
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template(horizontal: f64, vertical: f64, max_lines: u8) -> crate::template::Template {
        crate::template::Template {
            version: 1,
            project: "客先A".into(),
            style: VisualStyle::default(),
            background_image: None,
            logo: None,
            horizontal_margin: horizontal,
            vertical_margin: vertical,
            max_lines,
        }
    }
    fn lyric_margins(ass: &str) -> Vec<u32> {
        let style = ass
            .lines()
            .find(|l| l.starts_with("Style: Lyrics"))
            .unwrap();
        let f: Vec<&str> = style.split(',').collect();
        f[19..22].iter().map(|v| v.parse().unwrap()).collect()
    }
    #[test]
    fn template_margins_apply_per_orientation_and_never_shrink_safe_area() {
        let cues = parse_lrc("[00:01.00]歌\n[00:02.00]\n").unwrap();
        let t = template(0.2, 0.1, 2);
        let wide = make_ass_templated("曲", &cues, 3.0, 1.0, VideoPreset::FullHd, &t).unwrap();
        assert_eq!(lyric_margins(&wide), vec![384, 384, 216]);
        let tall = make_ass_templated("曲", &cues, 3.0, 1.0, VideoPreset::Vertical, &t).unwrap();
        assert_eq!(lyric_margins(&tall), vec![108, 160, 320]);
        let small = template(0.0, 0.0, 2);
        let base =
            make_ass_templated("曲", &cues, 3.0, 1.0, VideoPreset::Vertical, &small).unwrap();
        assert_eq!(lyric_margins(&base), vec![80, 160, 320]);
    }
    #[test]
    fn template_rejects_subtitles_that_would_need_more_lines_than_allowed() {
        let long = "あ".repeat(60);
        let cues = parse_lrc(&format!("[00:01.00]{long}\n[00:02.00]\n")).unwrap();
        assert!(
            make_ass_templated(
                "曲",
                &cues,
                3.0,
                1.0,
                VideoPreset::Hd,
                &template(0.05, 0.05, 1)
            )
            .is_err()
        );
        assert!(
            make_ass_templated(
                "曲",
                &cues,
                3.0,
                1.0,
                VideoPreset::Hd,
                &template(0.05, 0.05, 3)
            )
            .is_ok()
        );
    }
    #[test]
    fn blank_at_song_end_is_editable_and_does_not_extend_srt() {
        let cues = parse_lrc("[00:01.00]end\n[00:10.00]\n").unwrap();
        assert_eq!(
            make_srt(&cues, 10.0, 1.0).unwrap(),
            "1\n00:00:01,000 --> 00:00:10,000\nend\n\n"
        );
        assert!(make_ass("song", &cues, 10.0, 1.0).is_ok());
    }
    #[test]
    fn blank_cues_end_subtitles_and_srt_uses_output_timeline() {
        let cues = parse_lrc("[00:01.25]歌う\n[00:02.50]\n[00:04.00]また歌う\n[00:05.00]").unwrap();
        assert_eq!(
            make_srt(&cues, 6.0, 1.25).unwrap(),
            "1\n00:00:01,000 --> 00:00:02,000\n歌う\n\n2\n00:00:03,200 --> 00:00:04,000\nまた歌う\n\n"
        );
        let ass = make_ass("曲", &cues, 6.0, 1.25).unwrap();
        assert!(ass.contains("Dialogue: 1,0:00:01.00,0:00:02.00,Lyrics"));
        assert_eq!(
            ass.lines()
                .filter(|line| line.starts_with("Dialogue: 1,"))
                .count(),
            2
        );
        assert!(
            make_srt(
                &[Cue {
                    seconds: f64::NAN,
                    text: "不正".into()
                }],
                6.0,
                1.0
            )
            .is_err()
        );
    }
    #[test]
    fn merged_multiline_cues_survive_lrc_export() {
        let cues = parse_lrc("[00:01.25]lead\n[00:01.25]harmony\n[00:03.00]\n").unwrap();
        let exported = to_lrc(&cues);
        assert_eq!(parse_lrc(&exported).unwrap(), cues);
    }

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
