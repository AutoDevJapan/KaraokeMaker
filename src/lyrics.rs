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

// Escaping display fragments must not remove spaces (a fragment can be a space).
fn ass_fragment(text: &str) -> String {
    text.replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace('<', "＜")
        .replace('>', "＞")
        .replace('\r', "")
        .replace('\n', "\\N")
}

fn measured_lines(
    text: &str,
    font: &str,
    size: u32,
    outline: u32,
    width: u32,
) -> Result<Vec<String>> {
    measured_lines_styled(text, font, size, outline, true, width)
}

fn measured_lines_styled(
    text: &str,
    font: &str,
    size: u32,
    outline: u32,
    bold: bool,
    width: u32,
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for line in text.split('\n') {
        let clusters: Vec<_> = glyph_clusters(line).into_iter().map(|c| c.0).collect();
        let mut start = 0;
        if clusters.is_empty() {
            result.push(String::new());
        }
        while start < clusters.len() {
            let (mut low, mut high) = (start + 1, clusters.len() + 1);
            ensure!(
                crate::ass_layout::measure_styled(font, size, outline, bold, &clusters[start])?.0
                    <= width,
                "字幕の1文字が指定幅に収まりません。"
            );
            while low + 1 < high {
                let mid = (low + high) / 2;
                if crate::ass_layout::measure_styled(
                    font,
                    size,
                    outline,
                    bold,
                    &clusters[start..mid].concat(),
                )?
                .0 <= width
                {
                    low = mid
                } else {
                    high = mid
                }
            }
            if low < clusters.len()
                && clusters[low - 1].chars().all(|c| c.is_ascii_alphanumeric())
                && clusters[low].chars().all(|c| c.is_ascii_alphanumeric())
                && let Some(space) = (start..low).rev().find(|&i| clusters[i] == " ")
                && space > start
            {
                low = space + 1;
            }
            result.push(clusters[start..low].concat());
            start = low;
        }
    }
    Ok(result)
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
    template: Option<&crate::template::Template>,
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
    let mut ass = ass_with_layout(title, &[], duration, tempo, preset, style, template)?;
    // With a template, long lines are broken by estimated glyph width so they stay inside
    // the margins (automatic wrapping is off); the line limit is enforced the same way.
    let budget = template.map(|_| {
        let (width, _) = preset.dimensions();
        let (left, right, _, _) = layout_margins(preset, template);
        width.saturating_sub(left + right)
    });
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
        let mut rendered_line = String::new();
        let mut lines = 1;
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
            if let Some(budget) = budget {
                let size = (44.0 * preset.dimensions().0 as f64 / 1280.0).round() as u32;
                let outline = (3.0 * preset.dimensions().0 as f64 / 1280.0).round() as u32;
                let candidate = format!("{rendered_line}{spacing}{}", word.text);
                if !rendered_line.is_empty()
                    && crate::ass_layout::measure(&style.font, size, outline, &candidate)?.0
                        > budget
                {
                    text.push_str("\\N");
                    lines += 1;
                    rendered_line.clear();
                }
                let fragments = measured_lines(&word.text, &style.font, size, outline, budget)?;
                let total = word.text.chars().count().max(1) as u64;
                let mut consumed = 0u64;
                for (i, part) in fragments.iter().enumerate() {
                    if i > 0 {
                        text.push_str("\\N");
                        lines += 1;
                        rendered_line.clear();
                    }
                    let next = consumed + part.chars().count() as u64;
                    let part_fill = fill * next / total - fill * consumed / total;
                    if i == 0 {
                        text.push_str(spacing);
                    }
                    text.push_str(&format!("{{\\kf{part_fill}}}{}", ass_fragment(part)));
                    rendered_line.push_str(part);
                    consumed = next;
                }
                continue;
            }
            text.push_str(&format!("{spacing}{{\\kf{fill}}}{}", ass_text(&word.text)));
        }
        if let Some(template) = template {
            ensure!(
                lines <= template.max_lines as usize,
                "字幕が{}行を超えます。余白か行数を調整してください。",
                template.max_lines
            );
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

/// A glyph cluster that must not be split: a base character with its combining marks,
/// a ZWJ sequence, or a pair of regional indicators. Closing punctuation joins the cluster
/// before it so that a line never starts with it.
fn glyph_clusters(line: &str) -> Vec<(String, f64)> {
    let mut clusters: Vec<(String, f64)> = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        let closing = "。、，．,.）)」』】〕〉》！？!?:;；：…‥";
        if closing.contains(c) && !clusters.is_empty() {
            let last = clusters.last_mut().unwrap();
            last.0.push(c);
            last.1 += if c.is_ascii() { 0.6 } else { 1.0 };
            continue;
        }
        let mut text = c.to_string();
        let regional = ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
        if regional
            && chars
                .peek()
                .is_some_and(|n| ('\u{1F1E6}'..='\u{1F1FF}').contains(n))
        {
            text.push(chars.next().unwrap());
        }
        while let Some(&next) = chars.peek() {
            let mark = matches!(next, '\u{0300}'..='\u{036F}' | '\u{3099}' | '\u{309A}' | '\u{FE00}'..='\u{FE0F}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{200D}');
            if !mark {
                break;
            }
            text.push(next);
            chars.next();
            if next == '\u{200D}'
                && let Some(joined) = chars.next()
            {
                text.push(joined);
            }
        }
        let width = if c.is_ascii() { 0.6 } else { 1.0 };
        clusters.push((text, width));
    }
    clusters
}

/// Greedy line filling: breaks at spaces and between non-ASCII clusters, splitting an ASCII
/// word only when it is wider than a whole line.
#[cfg(test)]
fn fill_lines(line: &str, budget: f64) -> Vec<String> {
    // Units: ASCII words stay whole, every other cluster stands alone, spaces separate.
    let mut units: Vec<(String, f64, bool)> = Vec::new();
    for (text, width) in glyph_clusters(line) {
        let ascii_word = text.chars().all(|c| c.is_ascii() && c != ' ');
        match units.last_mut() {
            Some(last) if ascii_word && last.2 => {
                last.0.push_str(&text);
                last.1 += width;
            }
            _ => units.push((text, width, ascii_word)),
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0.0;
    let mut push_line = |current: &mut String, used: &mut f64| {
        lines.push(current.trim_end().to_string());
        current.clear();
        *used = 0.0;
    };
    for (text, width, ascii_word) in units {
        let space = text == " ";
        if space && current.is_empty() {
            continue;
        }
        if used + width > budget && !current.is_empty() && !space {
            push_line(&mut current, &mut used);
        }
        if ascii_word && width > budget {
            // A word wider than a line: split it by characters.
            for (piece, piece_width) in glyph_clusters(&text) {
                if used + piece_width > budget && !current.is_empty() {
                    push_line(&mut current, &mut used);
                }
                current.push_str(&piece);
                used += piece_width;
            }
            continue;
        }
        current.push_str(&text);
        used += width;
    }
    lines.push(current.trim_end().to_string());
    lines
}

/// Break text into lines whose estimated width (full-width glyph = 1, ASCII = 0.6) fits the
/// budget, then even them out so that no line is left with a stray last character.
#[cfg(test)]
fn wrap_to_width(text: &str, budget: f64) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let greedy = fill_lines(line, budget);
        let count = greedy.len();
        if count < 2 {
            lines.extend(greedy);
            continue;
        }
        // The narrowest width that still gives the same number of lines.
        let (mut low, mut high) = (budget / count as f64, budget);
        for _ in 0..24 {
            let middle = (low + high) / 2.0;
            if fill_lines(line, middle).len() <= count {
                high = middle;
            } else {
                low = middle;
            }
        }
        lines.extend(fill_lines(line, high));
    }
    lines
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

/// Left, right, bottom and top margins in pixels. A template can only widen the built-in
/// platform safe area; its horizontal or vertical value follows the video orientation.
pub(crate) fn layout_margins(
    preset: VideoPreset,
    template: Option<&crate::template::Template>,
) -> (u32, u32, u32, u32) {
    let (width, height) = preset.dimensions();
    let scale = width as f64 / 1280.0;
    let scaled = |value: f64| (value * scale).round() as u32;
    let portrait = matches!(preset, VideoPreset::Vertical | VideoPreset::VerticalUhd);
    let (mut left, mut right, mut bottom, mut top) = if portrait {
        let factor = width as f64 / 1080.0;
        let safe = |value: f64| (value * factor).round() as u32;
        (safe(80.0), safe(160.0), safe(320.0), safe(160.0))
    } else {
        (scaled(60.0), scaled(60.0), scaled(80.0), scaled(32.0))
    };
    if let Some(template) = template {
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
    (left, right, bottom, top)
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
    let (left, right, bottom, top) = layout_margins(preset, template);
    let font = &style.font;
    let accent = style.ass_accent()?;
    let lyric_size = scaled(44.0);
    let title_size = scaled(28.0);
    // With a template, lines are broken here by estimated glyph width (10% headroom) and
    // libass automatic wrapping is turned off, because its Japanese wrapping can exceed
    // the margins. Word-timed subtitles are broken the same way in make_ass_words.
    let fixed_wrap = template.is_some();
    let wrap_style = if fixed_wrap { 2 } else { 0 };
    let available = width.saturating_sub(left + right) as f64;
    let logo_reserve = if template.is_some_and(|t| t.logo.is_some()) {
        // Room kept free on both sides of the title for a logo of up to a quarter of the width.
        (available * 0.25).round() as u32 + scaled(16.0)
    } else {
        0
    };
    let (title_left, title_right) = (left + logo_reserve, right + logo_reserve);
    let title_available = width.saturating_sub(title_left + title_right) as f64;
    let wrapped = |text: &str, size: u32, available: f64| -> Result<Vec<String>> {
        if fixed_wrap {
            measured_lines_styled(
                text,
                font,
                size,
                if size == title_size {
                    scaled(2.0)
                } else {
                    scaled(3.0)
                },
                size != title_size,
                available as u32,
            )
        } else {
            Ok(vec![text.to_string()])
        }
    };
    if let (Some(template), true) = (template, fixed_wrap) {
        for cue in cues {
            ensure!(
                wrapped(&cue.text, lyric_size, available)?.len() <= template.max_lines as usize,
                "字幕が{}行を超えます。余白か行数を調整してください。",
                template.max_lines
            );
        }
    }
    let outline = scaled(3.0);
    let title_outline = scaled(2.0);
    let shadow = scaled(1.0);
    // libass rasterizes the vector glyphs after scaling the video to the target size.
    let mut ass = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nWrapStyle: {wrap_style}\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Lyrics,{font},{lyric_size},{accent},&H00FFFFFF,&H00202020,&H80000000,-1,0,0,0,100,100,0,0,1,{outline},{shadow},2,{left},{right},{bottom},1\nStyle: Title,{font},{title_size},&H00FFFFFF,&H00FFFFFF,&H00202020,&H80000000,0,0,0,0,100,100,0,0,1,{title_outline},0,8,{title_left},{title_right},{top},1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
    );
    ass.push_str(&format!(
        "Dialogue: 0,0:00:00.00,{},Title,,0,0,0,,{}\n",
        stamp(duration / tempo),
        ass_text(&wrapped(title, title_size, title_available)?.join("\n"))
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
            ass_text(&wrapped(&cue.text, lyric_size, available)?.join("\n"))
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
    #[test]
    fn measured_word_layout_accepts_existing_fit_and_preserves_spaces_and_modifiers() {
        let mut t = template(0.1, 0.1, 1);
        let word = |text: String| {
            vec![crate::alignment::Word {
                text,
                start: 0.1,
                end: 0.9,
                confidence: 1.0,
                join_previous: false,
                line_index: 0,
            }]
        };
        let fitting = "あいうえおかきくけこ".repeat(2) + "あいうえお";
        for preset in [VideoPreset::FullHd, VideoPreset::Vertical] {
            assert!(
                make_ass_words(
                    "",
                    &word(fitting.clone()),
                    1.0,
                    1.0,
                    preset,
                    &t.style,
                    Some(&t)
                )
                .is_ok()
            );
        }
        t.max_lines = 4;
        let english = "hello world this is a long synthetic phrase for karaoke";
        let ass = make_ass_words(
            "",
            &word(english.into()),
            1.0,
            1.0,
            VideoPreset::FullHd,
            &t.style,
            Some(&t),
        )
        .unwrap();
        let line = ass.lines().find(|l| l.starts_with("Dialogue: 1")).unwrap();
        let payload = line.splitn(10, ',').nth(9).unwrap();
        let mut plain = String::new();
        let mut tag = false;
        for c in payload.chars() {
            if c == '{' {
                tag = true
            } else if c == '}' {
                tag = false
            } else if !tag {
                plain.push(c)
            }
        }
        assert_eq!(plain.replace("\\N", ""), english);
        let clusters = glyph_clusters(&"👍🏽".repeat(20));
        assert_eq!(clusters.len(), 20);
        assert!(clusters.iter().all(|c| c.0 == "👍🏽"));
    }

    #[test]
    fn long_single_word_wraps_without_changing_its_timing() {
        let mut t = template(0.1, 0.1, 4);
        let words = vec![crate::alignment::Word {
            text: "あいうえおかきくけこ".repeat(4),
            start: 0.1,
            end: 0.9,
            confidence: 1.0,
            join_previous: false,
            line_index: 0,
        }];
        for preset in [VideoPreset::FullHd, VideoPreset::Vertical] {
            let ass = make_ass_words("曲", &words, 1.0, 1.0, preset, &t.style, Some(&t)).unwrap();
            let line = ass.lines().find(|l| l.starts_with("Dialogue: 1")).unwrap();
            assert!(line.contains("\\N"));
            assert!(line.matches("\\kf").count() > 1);
            assert!(line.contains("0:00:00.10,0:00:00.90"));
            t.max_lines = 1;
            assert!(make_ass_words("曲", &words, 1.0, 1.0, preset, &t.style, Some(&t)).is_err());
            t.max_lines = 4;
        }
    }
    #[test]
    fn convert_preflight_checks_actual_words_instead_of_unused_lrc() {
        let mut t = template(0.1, 0.1, 1);
        let mut options = crate::media::Options {
            lyrics: format!("[00:00.10]{}\n", "あ".repeat(40)),
            words: (0..4)
                .map(|i| crate::alignment::Word {
                    text: "あ".repeat(10),
                    start: 0.1 + i as f64 * 0.2,
                    end: 0.3 + i as f64 * 0.2,
                    confidence: 1.0,
                    join_previous: false,
                    line_index: i,
                })
                .collect(),
            project: Some(t.project.clone()),
            template: Some(t.clone()),
            ..Default::default()
        };
        assert!(crate::media::check_lyrics_layout(&options, 1.0).is_ok());
        options.words.truncate(1);
        options.words[0].text = "あ".repeat(40);
        assert!(crate::media::check_lyrics_layout(&options, 1.0).is_err());
        t.max_lines = 4;
        options.template = Some(t);
        assert!(crate::media::check_lyrics_layout(&options, 1.0).is_ok());
    }
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
    fn title_keeps_clear_of_the_logo_corner() {
        let dir = tempfile::tempdir().unwrap();
        let logo = dir.path().join("logo.png");
        std::fs::write(&logo, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let cues = parse_lrc("[00:01.00]歌\n[00:02.00]\n").unwrap();
        let mut t = template(0.1, 0.1, 2);
        let plain = make_ass_templated("曲", &cues, 3.0, 1.0, VideoPreset::Vertical, &t).unwrap();
        t.logo = Some(logo);
        let with_logo =
            make_ass_templated("曲", &cues, 3.0, 1.0, VideoPreset::Vertical, &t).unwrap();
        let title_margins = |ass: &str| -> Vec<u32> {
            let style = ass.lines().find(|l| l.starts_with("Style: Title")).unwrap();
            let f: Vec<&str> = style.split(',').collect();
            f[19..22].iter().map(|v| v.parse().unwrap()).collect()
        };
        // Both sides give up a quarter of the safe width plus a gap, so the centred title
        // can never reach a logo of up to that width in the top-right corner.
        let (a, b) = (title_margins(&plain), title_margins(&with_logo));
        assert!(b[0] >= a[0] + 150 && b[1] >= a[1] + 150, "{a:?} {b:?}");
        assert_eq!(b[2], a[2]);
    }
    #[test]
    fn wrapping_balances_lines_and_never_splits_what_belongs_together() {
        // 20 full-width characters over a budget of 19: two even lines, not 19 + 1.
        let lines = wrap_to_width("あいうえおかきくけこさしすせそたちつてと", 19.0);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.chars().count() >= 9), "{lines:?}");
        // English breaks at spaces only; no word is cut and no space is left at a line end.
        let text = "neon lights tonight and more neon lights tonight";
        let lines = wrap_to_width(text, 14.0);
        assert!(lines.len() > 1);
        assert_eq!(lines.join(" "), text, "{lines:?}");
        assert!(lines.iter().all(|l| l == l.trim()), "{lines:?}");
        // A combining voiced mark never starts a line.
        let lines = wrap_to_width(&"か\u{3099}".repeat(30), 21.0);
        assert!(lines.len() > 1);
        assert!(
            lines.iter().all(|l| !l.starts_with('\u{3099}')),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|l| l.chars().count() % 2 == 0),
            "{lines:?}"
        );
        // ZWJ emoji and flags stay whole.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let lines = wrap_to_width(&family.repeat(12), 5.0);
        assert!(lines.len() > 1);
        assert!(
            lines.iter().all(|l| l.chars().count() % 5 == 0),
            "{lines:?}"
        );
        let lines = wrap_to_width(&"\u{1F1FA}\u{1F1F8}".repeat(12), 5.0);
        assert!(
            lines.iter().all(|l| l.chars().count() % 2 == 0),
            "{lines:?}"
        );
        // Closing punctuation stays with the text before it.
        let lines = wrap_to_width(&format!("{}。", "あ".repeat(10)), 10.0);
        assert!(lines.iter().all(|l| !l.starts_with('。')), "{lines:?}");
        // A single word wider than a line is split instead of overflowing.
        let lines = wrap_to_width("supercalifragilisticexpialidocious", 10.0);
        assert!(lines.len() > 1 && lines.concat() == "supercalifragilisticexpialidocious");
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
