use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Match a supplied lyric to acoustically timed Whisper tokens; retain match confidence.
pub fn reference_to_whisper(reference: &str, transcript: &serde_json::Value) -> Result<Vec<Word>> {
    let chars: Vec<char> = reference
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
        .collect();
    let mut joins = Vec::new();
    let mut line_indices = Vec::new();
    let mut line_index = 0;
    let mut adjacent = false;
    for c in reference.chars() {
        if c == '\n' {
            line_index += 1;
        }
        if c.is_whitespace() {
            adjacent = false;
        } else if !c.is_ascii_punctuation() {
            joins.push(adjacent);
            line_indices.push(line_index);
            adjacent = true;
        }
    }
    let mut timed = Vec::<(char, f64, f64)>::new();
    let segments = transcript["transcription"]
        .as_array()
        .context("認識した字幕がありません。")?;
    for segment in segments {
        let Some(tokens) = segment["tokens"].as_array() else {
            continue;
        };
        for token in tokens {
            let text = token["text"].as_str().unwrap_or("");
            if text.starts_with("[_") {
                continue;
            }
            let letters: Vec<char> = text
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
                .collect();
            if letters.is_empty() {
                continue;
            }
            let from = token["offsets"]["from"].as_f64().unwrap_or(f64::NAN) / 1000.0;
            let to = token["offsets"]["to"].as_f64().unwrap_or(f64::NAN) / 1000.0;
            if !from.is_finite() || !to.is_finite() || from < 0.0 || to <= from {
                // Keep the text correspondence, but no invented positive timing.
                // Whisper can emit zero-length extra tokens that the reference
                // does not use. Only adopting such a token makes timing invalid.
                timed.extend(letters.into_iter().map(|c| (c, f64::NAN, f64::NAN)));
                continue;
            }
            let dtw = token["t_dtw"]
                .as_f64()
                .filter(|&v| v >= 0.0)
                .map(|v| v / 100.0);
            let center = dtw
                .filter(|&point| point >= from && point <= to)
                .unwrap_or((from + to) / 2.0);
            for (index, &c) in letters.iter().enumerate() {
                let span = (to - from).max(0.02);
                let start = (center - span / 2.0).max(from).max(0.0);
                let end = (center + span / 2.0).min(to).max(start + 0.002);
                let width = (end - start) / letters.len() as f64;
                timed.push((
                    c,
                    start + width * index as f64,
                    start + width * (index + 1) as f64,
                ));
            }
        }
    }
    ensure!(
        !chars.is_empty() && !timed.is_empty() && chars.len() * timed.len() <= 5_000_000,
        "歌詞と認識結果が空か長すぎます。"
    );
    let columns = timed.len() + 1;
    // Maximize text correspondence first, then prefer real timing over an
    // otherwise equivalent match to an extra zero-length token.
    let match_weight = chars.len() as u32 + 1;
    let mut score = vec![0u32; (chars.len() + 1) * columns];
    for i in 1..=chars.len() {
        for j in 1..=timed.len() {
            let matched = if chars[i - 1]
                .to_lowercase()
                .eq(timed[j - 1].0.to_lowercase())
            {
                score[(i - 1) * columns + j - 1]
                    + match_weight
                    + u32::from(timed[j - 1].1.is_finite())
            } else {
                0
            };
            score[i * columns + j] = matched
                .max(score[(i - 1) * columns + j])
                .max(score[i * columns + j - 1]);
        }
    }
    let mut mapped = vec![None; chars.len()];
    let (mut i, mut j) = (chars.len(), timed.len());
    while i > 0 && j > 0 {
        if chars[i - 1]
            .to_lowercase()
            .eq(timed[j - 1].0.to_lowercase())
            && score[i * columns + j]
                == score[(i - 1) * columns + j - 1]
                    + match_weight
                    + u32::from(timed[j - 1].1.is_finite())
        {
            mapped[i - 1] = Some(j - 1);
            i -= 1;
            j -= 1;
        } else if score[(i - 1) * columns + j] >= score[i * columns + j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    let matches = mapped.iter().filter(|v| v.is_some()).count();
    ensure!(
        matches as f64 / chars.len() as f64 > 0.3,
        "歌詞と歌声の対応が少なすぎます。時刻付き歌詞で修正してください。"
    );
    ensure!(
        mapped.iter().flatten().all(|&k| timed[k].1.is_finite()
            && timed[k].2.is_finite()
            && timed[k].1 >= 0.0
            && timed[k].2 > timed[k].1),
        "認識した歌詞の時刻が不正です。時刻付き歌詞で修正してください。"
    );
    let recognized_lines: std::collections::BTreeSet<_> = mapped
        .iter()
        .enumerate()
        .filter_map(|(n, token)| token.map(|_| line_indices[n]))
        .collect();
    ensure!(
        line_indices
            .iter()
            .all(|line| recognized_lines.contains(line)),
        "音声へ合わせられない歌詞の行があります。時刻付き歌詞で修正してください。"
    );
    let mut intervals: Vec<_> = mapped
        .iter()
        .map(|token| token.map_or((f64::NAN, f64::NAN), |k| (timed[k].1, timed[k].2)))
        .collect();
    let mut first = 0;
    while first < chars.len() {
        if mapped[first].is_some() {
            first += 1;
            continue;
        }
        let mut last = first + 1;
        while last < chars.len() && mapped[last].is_none() {
            last += 1;
        }
        let count = last - first;
        let lower = if first == 0 {
            0.0
        } else {
            intervals[first - 1].1
        };
        let upper = (last < chars.len()).then(|| intervals[last].0);
        let width = if let Some(upper) = upper {
            let available = upper - lower;
            ensure!(
                available.is_finite() && available >= count as f64 * 0.002,
                "補間する歌詞の時間が足りません。時刻付き歌詞で修正してください。"
            );
            (available / count as f64).min(0.04)
        } else {
            0.04
        };
        // Keep every adopted recognition time intact. Only confidence-zero
        // estimates fit the known gap; a leading estimate ends at its anchor.
        let leading = first == 0 || line_indices[first - 1] != line_indices[first];
        let base = if leading {
            upper.unwrap() - width * count as f64
        } else {
            lower
        };
        for (index, interval) in intervals[first..last].iter_mut().enumerate() {
            *interval = (
                (base + width * index as f64).max(lower),
                upper.map_or(base + width * (index + 1) as f64, |bound| {
                    (base + width * (index + 1) as f64).min(bound)
                }),
            );
        }
        first = last;
    }
    ensure!(
        intervals.iter().all(|&(start, end)| start.is_finite()
            && end.is_finite()
            && start >= 0.0
            && end > start)
            && intervals.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "認識した歌詞の時刻が不正です。時刻付き歌詞で修正してください。"
    );
    let words: Vec<Word> = chars
        .into_iter()
        .enumerate()
        .map(|(n, c)| Word {
            text: c.to_string(),
            start: intervals[n].0,
            end: intervals[n].1,
            confidence: if mapped[n].is_some() { 1.0 } else { 0.0 },
            join_previous: joins[n],
            line_index: line_indices[n],
        })
        .collect();
    // Use the same display groups as ASS and LRC, including splits inside long
    // reference lines. Every token in the group contributes to its end time.
    let mut previous_end = 0.0;
    for group in crate::lyrics::word_groups(&words) {
        ensure!(
            words[group.start].start >= previous_end,
            "認識した歌詞の表示区間が時間的に重複しています。時刻付き歌詞で修正してください。"
        );
        previous_end = words[group].iter().map(|word| word.end).fold(0.0, f64::max);
    }
    Ok(words)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f64,
    #[serde(default)]
    pub join_previous: bool,
    #[serde(default)]
    pub line_index: u32,
}

pub fn prepare(
    audio: &Path,
    dir: &Path,
    models: &Path,
    reference: &str,
    duration: f64,
    progress: &crate::Progress,
) -> Result<Vec<Word>> {
    use crate::media::{Tools, hidden_command};
    progress.set("歌詞の時刻を合わせています。");
    let wave = dir.join("alignment.wav");
    let status = hidden_command(Tools::discover().ffmpeg)
        .args(["-v", "error", "-nostdin", "-n", "-i"])
        .arg(audio)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&wave)
        .status()?;
    ensure!(status.success(), "時刻合わせ用の音源を作れませんでした。");
    progress.check()?;
    let mut words = if !reference.trim().is_empty() && reference.is_ascii() {
        let assets = crate::subtitle_assets::ctc(models, progress)?;
        #[cfg(feature = "ort")]
        crate::runtime::initialize_for_models(models, progress)?;
        force_english(&wave, reference, &assets.model, &assets.vocab, progress)?
    } else {
        let assets = crate::subtitle_assets::whisper(models, progress)?;
        let executable = &assets.executable;
        let model = &assets.model;
        ensure!(
            executable.is_file() && model.is_file(),
            "歌詞の自動時刻合わせにはwhisper.cppの導入が必要です。時刻付き歌詞でも作成できます。"
        );
        let prefix = dir.join("transcription");
        let log = fs::File::create(dir.join("transcription.log"))?;
        let language = if reference.trim().is_empty() {
            "auto"
        } else {
            "ja"
        };
        let mut child = hidden_command(executable)
            .args(["-m"])
            .arg(model)
            .arg("-f")
            .arg(&wave)
            .args([
                "-l",
                language,
                "-t",
                "4",
                "-nfa",
                "-dtw",
                "large.v3.turbo",
                "-ml",
                "1",
                "-sow",
                "-ojf",
                "-of",
            ])
            .arg(&prefix)
            .args(if reference.trim().is_empty() {
                Vec::<String>::new()
            } else {
                vec!["--prompt".into(), reference.chars().take(700).collect()]
            })
            .stdout(std::process::Stdio::from(log.try_clone()?))
            .stderr(std::process::Stdio::from(log))
            .spawn()?;
        let started = std::time::Instant::now();
        loop {
            if progress.check().is_err() || started.elapsed().as_secs() > 900 {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("歌詞の時刻合わせが取り消されたか、時間を超えました。");
            }
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "歌詞の時刻合わせに失敗しました。");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let transcript: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("transcription.json"))?)?;
        if !reference.trim().is_empty() {
            reference_to_whisper(reference, &transcript)?
        } else {
            transcript["transcription"]
                .as_array()
                .context("認識した歌詞がありません。")?
                .iter()
                .filter_map(|segment| {
                    let text = segment["text"].as_str()?.trim();
                    if text.is_empty() || text.starts_with('*') || text.starts_with('[') {
                        return None;
                    }
                    let start = segment["offsets"]["from"].as_f64()? / 1000.0;
                    let end = segment["offsets"]["to"].as_f64()? / 1000.0;
                    (end > start).then(|| Word {
                        text: text.into(),
                        start,
                        end,
                        confidence: segment["tokens"]
                            .as_array()
                            .and_then(|tokens| {
                                tokens
                                    .iter()
                                    .filter_map(|t| t["p"].as_f64())
                                    .reduce(f64::min)
                            })
                            .unwrap_or(0.0),
                        join_previous: false,
                        line_index: 0,
                    })
                })
                .collect()
        }
    };
    if !reference.trim().is_empty() && reference.is_ascii() {
        let average =
            words.iter().map(|word| word.confidence).sum::<f64>() / words.len().max(1) as f64;
        ensure!(
            average >= 0.35,
            "英語歌詞の時刻合わせの確信度が不足しています。時刻付き歌詞で修正してください。"
        );
    }
    words.retain(|word| word.start < duration && word.end > word.start);
    for word in &mut words {
        word.end = word.end.min(duration);
    }
    ensure!(
        !words.is_empty(),
        "歌詞を認識できませんでした。正本の歌詞か時刻付きの歌詞を入力してください。"
    );
    fs::remove_file(wave)?;
    Ok(words)
}

/// Viterbi over a CTC state graph with blanks and repeated-character constraints.
pub fn ctc_path(log_probs: &[Vec<f64>], target: &[usize], blank: usize) -> Result<Vec<usize>> {
    ctc_path_checked(log_probs, target, blank, &crate::Progress::default())
}

fn ctc_path_checked(
    log_probs: &[Vec<f64>],
    target: &[usize],
    blank: usize,
    progress: &crate::Progress,
) -> Result<Vec<usize>> {
    ensure!(
        !log_probs.is_empty() && !target.is_empty(),
        "音声か正本の歌詞が空です。"
    );
    let states = target.len() * 2 + 1;
    let frames = log_probs.len();
    ensure!(
        states.checked_mul(frames).is_some_and(|n| n <= 100_000_000),
        "歌詞の時刻合わせが大きすぎます。"
    );
    let label = |s: usize| {
        if s.is_multiple_of(2) {
            blank
        } else {
            target[s / 2]
        }
    };
    let mut prev = vec![f64::NEG_INFINITY; states];
    prev[0] = log_probs[0][blank];
    prev[1] = log_probs[0][target[0]];
    let mut back = vec![0u8; states * frames];
    for (t, row) in log_probs.iter().enumerate().skip(1) {
        progress.check()?;
        let mut next = vec![f64::NEG_INFINITY; states];
        for s in 0..states {
            ensure!(label(s) < row.len(), "歌詞の語彙が一致しません。");
            let mut best = prev[s];
            let mut delta = 0;
            if s > 0 && prev[s - 1] > best {
                best = prev[s - 1];
                delta = 1;
            }
            if s > 1 && s % 2 == 1 && label(s) != label(s - 2) && prev[s - 2] > best {
                best = prev[s - 2];
                delta = 2;
            }
            next[s] = best + row[label(s)];
            back[t * states + s] = delta;
        }
        prev = next;
    }
    let mut s = if prev[states - 1] > prev[states - 2] {
        states - 1
    } else {
        states - 2
    };
    ensure!(
        prev[s].is_finite(),
        "歌詞全体を音声へ合わせられませんでした。"
    );
    let mut path = vec![0usize; frames];
    for t in (0..frames).rev() {
        path[t] = s;
        if t > 0 {
            s -= back[t * states + s] as usize;
        }
    }
    Ok(path)
}

#[cfg(any(feature = "ort", test))]
fn ctc_targets(
    words: &[String],
    dictionary: &std::collections::BTreeMap<String, usize>,
) -> Result<(Vec<usize>, Vec<Option<usize>>)> {
    let mut target = Vec::new();
    let mut word_indices = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if !target.is_empty() {
            target.push(dictionary["|"]);
            // This label separates words, and is not part of either sung word.
            word_indices.push(None);
        }
        for c in word.to_ascii_uppercase().chars() {
            target.push(
                *dictionary
                    .get(&c.to_string())
                    .context("正本の歌詞に対応しない文字があります。")?,
            );
            word_indices.push(Some(index));
        }
    }
    Ok((target, word_indices))
}

#[cfg(any(feature = "ort", test))]
fn ctc_word_intervals(
    words: Vec<String>,
    target: &[usize],
    word_indices: &[Option<usize>],
    probabilities: &[Vec<f64>],
    times: &[f64],
    path: &[usize],
    duration: f64,
) -> Result<Vec<Word>> {
    let mut result: Vec<Word> = words
        .into_iter()
        .map(|text| Word {
            text,
            start: f64::INFINITY,
            end: 0.0,
            confidence: 0.0,
            join_previous: false,
            line_index: 0,
        })
        .collect();
    let mut counts = vec![0usize; result.len()];
    for (t, &state) in path.iter().enumerate() {
        if state % 2 == 0 {
            continue;
        }
        let token = state / 2;
        let Some(word) = word_indices[token] else {
            continue;
        };
        result[word].start = result[word].start.min(times[t]);
        result[word].end = times.get(t + 1).copied().unwrap_or(duration);
        result[word].confidence += probabilities[t][target[token]].exp();
        counts[word] += 1;
    }
    for (word, count) in result.iter_mut().zip(counts) {
        ensure!(
            count > 0 && word.start.is_finite(),
            "合わせられない単語がありました。"
        );
        word.confidence /= count as f64;
    }
    Ok(result)
}

#[cfg(any(feature = "ort", test))]
fn ctc_align_reference(
    reference: &str,
    dictionary: &std::collections::BTreeMap<String, usize>,
    probabilities: &[Vec<f64>],
    times: &[f64],
    duration: f64,
    progress: &crate::Progress,
) -> Result<Vec<Word>> {
    let mut words = Vec::new();
    let mut source_lines = Vec::new();
    for (line_index, line) in reference.lines().enumerate() {
        let source_line = u32::try_from(line_index).context("歌詞の行数が多すぎます。")?;
        for word in line.split_whitespace() {
            let word = word
                .chars()
                .filter(|c| c.is_ascii_alphabetic() || *c == '\'')
                .collect::<String>();
            if !word.is_empty() {
                words.push(word);
                source_lines.push(source_line);
            }
        }
    }
    let (target, word_indices) = ctc_targets(&words, dictionary)?;
    let path = ctc_path_checked(probabilities, &target, dictionary["<pad>"], progress)?;
    let mut result = ctc_word_intervals(
        words,
        &target,
        &word_indices,
        probabilities,
        times,
        &path,
        duration,
    )?;
    for (word, source_line) in result.iter_mut().zip(source_lines) {
        word.line_index = source_line;
    }
    Ok(result)
}

#[cfg(feature = "ort")]
pub fn force_english(
    audio: &Path,
    reference: &str,
    model: &Path,
    vocab: &Path,
    progress: &crate::Progress,
) -> Result<Vec<Word>> {
    use ort::{session::Session, value::Tensor};
    crate::runtime::initialize(false)?;
    let mut session = Session::builder()?
        .with_intra_threads(4)?
        .commit_from_file(model)?;
    let dictionary: std::collections::BTreeMap<String, usize> =
        serde_json::from_slice(&fs::read(vocab)?)?;
    let mut reader = hound::WavReader::open(audio)?;
    ensure!(
        reader.spec().sample_rate == 16000
            && reader.spec().channels == 1
            && reader.spec().bits_per_sample == 16,
        "時刻合わせには16kHzモノラルWAVが必要です。"
    );
    let audio: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()?;
    let mut probabilities = Vec::<Vec<f64>>::new();
    let mut times = Vec::new();
    const CHUNK: usize = 16000 * 12;
    for (index, part) in audio.chunks(CHUNK).enumerate() {
        progress.check()?;
        let mean = part.iter().map(|&x| x as f64).sum::<f64>() / part.len() as f64;
        let deviation = (part.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>()
            / part.len() as f64
            + 1e-7)
            .sqrt();
        let normalized: Vec<f32> = part
            .iter()
            .map(|&x| ((x as f64 - mean) / deviation) as f32)
            .collect();
        let input = Tensor::from_array(([1, part.len()], normalized))?;
        let output = session.run(ort::inputs!["input_values"=>input])?;
        let (shape, logits) = output[0].try_extract_tensor::<f32>()?;
        ensure!(
            shape.len() == 3 && shape[0] == 1,
            "認識モデルの出力が不正です。"
        );
        let frames = shape[1] as usize;
        let vocabulary = shape[2] as usize;
        for (frame, row) in logits.chunks(vocabulary).enumerate() {
            let maximum = row.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
            let sum = row
                .iter()
                .map(|&v| (v as f64 - maximum).exp())
                .sum::<f64>()
                .ln();
            probabilities.push(row.iter().map(|&v| v as f64 - maximum - sum).collect());
            times.push(
                index as f64 * 12.0 + frame as f64 * part.len() as f64 / 16000.0 / frames as f64,
            );
        }
    }
    ctc_align_reference(
        reference,
        &dictionary,
        &probabilities,
        &times,
        audio.len() as f64 / 16000.0,
        progress,
    )
}

#[cfg(not(feature = "ort"))]
pub fn force_english(
    _audio: &Path,
    _reference: &str,
    _model: &Path,
    _vocab: &Path,
    _progress: &crate::Progress,
) -> Result<Vec<Word>> {
    anyhow::bail!("このビルドには時刻合わせが含まれていません。")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn english_ctc_preserves_reference_lines_through_lrc_generation() {
        let dictionary = [
            ("<pad>".into(), 0),
            ("|".into(), 1),
            ("A".into(), 2),
            ("B".into(), 3),
        ]
        .into_iter()
        .collect();
        let probabilities = vec![
            vec![-8.0, -8.0, -0.01, -8.0],
            vec![-8.0, -0.01, -8.0, -8.0],
            vec![-8.0, -8.0, -8.0, -0.01],
        ];
        for reference in ["A\n\nB", "A\r\n\r\nB"] {
            let words = ctc_align_reference(
                reference,
                &dictionary,
                &probabilities,
                &[0.5, 0.6, 0.7],
                0.8,
                &crate::Progress::default(),
            )
            .unwrap();
            assert_eq!(
                words.iter().map(|word| word.line_index).collect::<Vec<_>>(),
                [0, 2],
                "CTC must retain original line boundaries, including blank lines"
            );
            assert_eq!((words[0].start, words[0].end), (0.5, 0.6));
            assert_eq!((words[1].start, words[1].end), (0.7, 0.8));
            let lrc = crate::lyrics::to_lrc(&crate::lyrics::word_cues(&words));
            let cues = crate::lyrics::parse_lrc(&lrc).unwrap();
            assert_eq!(
                cues.iter()
                    .filter(|cue| !cue.text.is_empty())
                    .map(|cue| (cue.seconds, cue.text.as_str()))
                    .collect::<Vec<_>>(),
                [(0.5, "A"), (0.7, "B")]
            );
        }
        let one_line = ctc_align_reference(
            "A B",
            &dictionary,
            &probabilities,
            &[0.5, 0.6, 0.7],
            0.8,
            &crate::Progress::default(),
        )
        .unwrap();
        assert_eq!(
            crate::lyrics::word_cues(&one_line)
                .iter()
                .filter(|cue| !cue.text.is_empty())
                .map(|cue| cue.text.as_str())
                .collect::<Vec<_>>(),
            ["A B"]
        );
    }

    #[test]
    fn english_ctc_preserves_all42_reference_lines() {
        let dictionary = [
            ("<pad>".into(), 0),
            ("|".into(), 1),
            ("A".into(), 2),
            ("B".into(), 3),
        ]
        .into_iter()
        .collect();
        let mut reference = Vec::new();
        let mut probabilities = Vec::new();
        for line in 0..42 {
            if line > 0 {
                probabilities.push(vec![-8.0, -0.01, -8.0, -8.0]);
            }
            let label = if line % 2 == 0 { 2 } else { 3 };
            reference.push(if label == 2 { "A" } else { "B" });
            let mut row = vec![-8.0; 4];
            row[label] = -0.01;
            probabilities.push(row);
        }
        let times: Vec<f64> = (0..probabilities.len()).map(|n| n as f64 * 0.1).collect();
        let words = ctc_align_reference(
            &reference.join("\n"),
            &dictionary,
            &probabilities,
            &times,
            probabilities.len() as f64 * 0.1,
            &crate::Progress::default(),
        )
        .unwrap();
        assert_eq!(words.len(), 42);
        for (line, word) in words.iter().enumerate() {
            assert_eq!(word.line_index as usize, line);
        }
        let lrc = crate::lyrics::to_lrc(&crate::lyrics::word_cues(&words));
        assert_eq!(
            crate::lyrics::parse_lrc(&lrc)
                .unwrap()
                .iter()
                .filter(|cue| !cue.text.is_empty())
                .count(),
            42
        );
    }

    #[test]
    fn ctc_word_onset_excludes_the_separator_before_the_sung_letter() {
        // The separator remains a real CTC label, but its frames are not singing.
        let probabilities = vec![
            vec![-8.0, -8.0, -0.01, -8.0],
            vec![-8.0, -0.5, -8.0, -8.0],
            vec![-8.0, -0.5, -8.0, -8.0],
            vec![-8.0, -0.5, -8.0, -8.0],
            vec![-8.0, -0.5, -8.0, -8.0],
            vec![-8.0, -8.0, -8.0, -0.01],
        ];
        let source_words = vec!["A".into(), "B".into()];
        let dictionary = [
            ("<pad>".into(), 0),
            ("|".into(), 1),
            ("A".into(), 2),
            ("B".into(), 3),
        ]
        .into_iter()
        .collect();
        let (target, word_indices) = ctc_targets(&source_words, &dictionary).unwrap();
        assert_eq!(target, [2, 1, 3]);
        let path = ctc_path(&probabilities, &target, 0).unwrap();
        assert_eq!(path, [1, 3, 3, 3, 3, 5]);
        let words = ctc_word_intervals(
            source_words,
            &target,
            &word_indices,
            &probabilities,
            &[0.0, 0.1, 0.5, 0.9, 1.3, 1.7],
            &path,
            1.8,
        )
        .unwrap();
        assert_eq!(
            words[1].start, 1.7,
            "a word must start at its letter, not at the earlier separator"
        );
        assert_eq!((words[0].start, words[0].end), (0.0, 0.1));
        assert_eq!(words[1].end, 1.8);
        assert!((words[1].confidence - (-0.01_f64).exp()).abs() < 1e-12);
    }

    #[test]
    fn ctc_letter_intervals_keep_blank_gaps_and_the_last_frame_end() {
        let source_words = vec!["AB".into()];
        let dictionary = [("<pad>".into(), 0), ("A".into(), 1), ("B".into(), 2)]
            .into_iter()
            .collect();
        let (target, word_indices) = ctc_targets(&source_words, &dictionary).unwrap();
        let probabilities = vec![
            vec![-8.0, -0.01, -8.0],
            vec![-0.01, -8.0, -8.0],
            vec![-8.0, -8.0, -0.03],
        ];
        let path = ctc_path(&probabilities, &target, 0).unwrap();
        assert_eq!(path, [1, 2, 3]);
        let words = ctc_word_intervals(
            source_words,
            &target,
            &word_indices,
            &probabilities,
            &[0.2, 0.3, 0.4],
            &path,
            0.5,
        )
        .unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "AB");
        assert_eq!((words[0].start, words[0].end), (0.2, 0.5));
        let expected = ((-0.01_f64).exp() + (-0.03_f64).exp()) / 2.0;
        assert!((words[0].confidence - expected).abs() < 1e-12);
    }

    #[test]
    fn provided_lyrics_keep_words_and_strict_positive_timing() {
        let data = serde_json::json!({"transcription":[{"tokens":[{"text":"My","offsets":{"from":100,"to":300},"t_dtw":20},{"text":" universe","offsets":{"from":300,"to":1500},"t_dtw":90}]}]});
        let words = reference_to_whisper("My Universe", &data).unwrap();
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<String>(),
            "MyUniverse"
        );
        assert!(!words[2].join_previous);
        assert!(words[3].join_previous);
        assert!(words.iter().all(|w| w.end > w.start));
        assert!(words.windows(2).all(|w| w[0].start <= w[1].start));
        assert!(reference_to_whisper("ほげほげ", &data).is_err());
    }
    #[test]
    fn whisper_reference_rejects_overlapping_lines_and_invalid_token_bounds() {
        let normal = serde_json::json!({"transcription":[{"tokens":[
            {"text":"あ","offsets":{"from":1000,"to":1400},"p":0.9},
            {"text":"い","offsets":{"from":1500,"to":1900},"p":0.9}
        ]}]});
        let words = reference_to_whisper("あ\nい", &normal).unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(
            (words[0].start, words[0].end, words[1].start, words[1].end),
            (1.0, 1.4, 1.5, 1.9)
        );
        let lrc = crate::lyrics::to_lrc(&crate::lyrics::word_cues(&words));
        let cues = crate::lyrics::parse_lrc(&lrc).unwrap();
        assert_eq!(
            cues.iter().map(|cue| cue.seconds).collect::<Vec<_>>(),
            [1.0, 1.4, 1.5, 1.9]
        );
        let mut overlap = normal.clone();
        overlap["transcription"][0]["tokens"][0]["offsets"]["to"] = serde_json::json!(1700);
        assert!(
            reference_to_whisper("あ\nい", &overlap)
                .unwrap_err()
                .to_string()
                .contains("重複"),
            "different lyric lines must not produce simultaneous ASS events while LRC replaces the first line"
        );
        assert!(
            reference_to_whisper("あい", &overlap).is_ok(),
            "overlapping token spans inside one displayed line are retained"
        );
        for bounds in [
            serde_json::json!({"from":-100,"to":100}),
            serde_json::json!({"from":1000,"to":1000}),
            serde_json::json!({"from":1000,"to":500}),
            serde_json::json!({"from":1000}),
            serde_json::json!({"to":1400}),
        ] {
            let mut invalid = normal.clone();
            invalid["transcription"][0]["tokens"][0]["offsets"] = bounds;
            assert!(
                reference_to_whisper("あ\nい", &invalid)
                    .unwrap_err()
                    .to_string()
                    .contains("時刻"),
                "invalid token boundaries must not be invented as positive timing"
            );
        }
    }

    #[test]
    fn unused_zero_length_token_does_not_discard_valid_reference_timing() {
        let normal = serde_json::json!({"transcription":[{"tokens":[
            {"text":"あ","offsets":{"from":1000,"to":1400}},
            {"text":"い","offsets":{"from":1500,"to":1900}}
        ]}]});
        let expected = reference_to_whisper("あ\nい", &normal).unwrap();
        let mut with_extra = normal;
        with_extra["transcription"][0]["tokens"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"text":"ん","offsets":{"from":2000,"to":2000}}));
        let actual = reference_to_whisper("あ\nい", &with_extra).unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        with_extra["transcription"][0]["tokens"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"text":"い","offsets":{"from":2000,"to":2000}}));
        let preferred = reference_to_whisper("あ\nい", &with_extra).unwrap();
        assert_eq!(
            (preferred[1].start, preferred[1].end),
            (1.5, 1.9),
            "valid full correspondence wins over a duplicate zero-length token"
        );
    }

    #[test]
    fn split_reference_line_rejects_overlapping_display_intervals() {
        let data = serde_json::json!({"transcription":[{"tokens":[
            {"text":"あ","offsets":{"from":1000,"to":1500}},
            {"text":"い","offsets":{"from":3000,"to":7000}},
            {"text":"う","offsets":{"from":5000,"to":5500}},
            {"text":"え","offsets":{"from":6000,"to":6500}}
        ]}]});
        assert!(
            reference_to_whisper("あいう\nえ", &data).is_err(),
            "4-second display split must not let the long preceding token overlap a later event"
        );
        let characters: String = (0..37)
            .map(|n| char::from_u32(0x3041 + n).unwrap())
            .collect();
        let char_tokens: Vec<_> = characters.chars().enumerate().map(|(n,c)|{
            let start = 1000+n*10;
            serde_json::json!({"text":c.to_string(),"offsets":{"from":start,"to":if n==0 { 3000 } else { start+5 }}})
        }).collect();
        assert!(
            reference_to_whisper(
                &characters,
                &serde_json::json!({"transcription":[{"tokens":char_tokens}]})
            )
            .is_err(),
            "character-limit split must use the full preceding interval end"
        );
    }

    #[test]
    fn interpolated_line_edges_fit_between_adopted_tokens_without_moving_them() {
        let data = serde_json::json!({"transcription":[{"tokens":[
            {"text":"あ","offsets":{"from":1000,"to":1300}},
            {"text":"い","offsets":{"from":1300,"to":1600}},
            {"text":"え","offsets":{"from":1620,"to":1900}},
            {"text":"お","offsets":{"from":1900,"to":2200}}
        ]}]});
        let recognized = reference_to_whisper("あい\nえお", &data).unwrap();
        for reference in ["あいう\nえお", "あい\nうえお"] {
            let words = reference_to_whisper(reference, &data).unwrap();
            assert_eq!(words.len(), 5);
            assert_eq!(
                (words[1].start, words[1].end),
                (recognized[1].start, recognized[1].end)
            );
            assert_eq!(
                (words[3].start, words[3].end),
                (recognized[2].start, recognized[2].end)
            );
            assert_eq!(words[2].confidence, 0.0);
            assert!(words[2].start >= 1.6 && words[2].end <= 1.62);
            let groups: Vec<_> = crate::lyrics::word_groups(&words).collect();
            assert_eq!(groups.len(), 2);
            let end = words[groups[0].clone()]
                .iter()
                .map(|word| word.end)
                .fold(0.0, f64::max);
            assert!(end <= words[groups[1].start].start);
        }
        let mut overlap = data.clone();
        overlap["transcription"][0]["tokens"][1]["offsets"]["to"] = serde_json::json!(1700);
        assert!(reference_to_whisper("あい\nえお", &overlap).is_err());
        for reference in ["あいうう\nえお", "あい\nううえお"] {
            let words = reference_to_whisper(reference, &data).unwrap();
            assert_eq!(words.len(), 6);
            assert!((words[2].start - 1.6).abs() < 1e-12);
            assert!(words[3].end <= 1.62);
            assert_eq!(
                (words[4].start, words[4].end),
                (recognized[2].start, recognized[2].end)
            );
        }
        for next in [1600, 1601] {
            let mut no_gap = data.clone();
            no_gap["transcription"][0]["tokens"][2]["offsets"]["from"] = serde_json::json!(next);
            assert!(reference_to_whisper("あいう\nえお", &no_gap).is_err());
        }
        let mut leading = data.clone();
        leading["transcription"][0]["tokens"][0]["offsets"]["from"] = serde_json::json!(10);
        assert!(reference_to_whisper("うあい\nえお", &leading).is_ok());
        let mut insufficient_leading = leading.clone();
        insufficient_leading["transcription"][0]["tokens"][0]["offsets"]["from"] =
            serde_json::json!(1);
        assert!(reference_to_whisper("うあい\nえお", &insufficient_leading).is_err());
        let trailing = reference_to_whisper("あい\nえおう", &data).unwrap();
        assert_eq!(trailing.last().unwrap().confidence, 0.0);
        assert!(trailing.last().unwrap().end > recognized.last().unwrap().end);
    }

    #[test]
    fn line_silence_keeps_recognized_start_and_cancel_stops_viterbi() {
        let data = serde_json::json!({"transcription":[{"tokens":[{"text":"あ","offsets":{"from":1000,"to":1200},"t_dtw":110},{"text":"い","offsets":{"from":10000,"to":10200},"t_dtw":1010}]}]});
        let words = reference_to_whisper("あ\nい", &data).unwrap();
        assert!((words[1].start - 10.0).abs() < 0.02);
        assert!(words[0].end < 1.3);
        let progress = crate::Progress::default();
        progress
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            ctc_path_checked(&[vec![-1.0, -0.1], vec![-1.0, -0.1]], &[1], 0, &progress).is_err()
        );
    }
    #[test]
    fn repeated_characters_require_a_blank_between_them() {
        let p = vec![vec![-5.0, -0.1], vec![-0.1, -5.0], vec![-5.0, -0.1]];
        assert_eq!(ctc_path(&p, &[1, 1], 0).unwrap(), vec![1, 2, 3]);
        assert!(ctc_path(&p[..2], &[1, 1], 0).is_err());
    }
}
