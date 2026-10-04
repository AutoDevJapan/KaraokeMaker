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
            let from = token["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
            let to = token["offsets"]["to"].as_f64().unwrap_or(from * 1000.0) / 1000.0;
            let dtw = token["t_dtw"]
                .as_f64()
                .filter(|&v| v >= 0.0)
                .map(|v| v / 100.0);
            let center = dtw.unwrap_or((from + to) / 2.0);
            for c in text
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
            {
                timed.push((c, center, (to - from).max(0.02)));
            }
        }
    }
    ensure!(
        !chars.is_empty() && !timed.is_empty() && chars.len() * timed.len() <= 5_000_000,
        "歌詞と認識結果が空か長すぎます。"
    );
    let columns = timed.len() + 1;
    let mut score = vec![0u32; (chars.len() + 1) * columns];
    for i in 1..=chars.len() {
        for j in 1..=timed.len() {
            score[i * columns + j] = if chars[i - 1]
                .to_lowercase()
                .eq(timed[j - 1].0.to_lowercase())
            {
                score[(i - 1) * columns + j - 1] + 1
            } else {
                score[(i - 1) * columns + j].max(score[i * columns + j - 1])
            };
        }
    }
    let mut mapped = vec![None; chars.len()];
    let (mut i, mut j) = (chars.len(), timed.len());
    while i > 0 && j > 0 {
        if chars[i - 1]
            .to_lowercase()
            .eq(timed[j - 1].0.to_lowercase())
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
    let mut centers = vec![0.0f64; chars.len()];
    for n in 0..chars.len() {
        centers[n] = if let Some(k) = mapped[n] {
            timed[k].1
        } else {
            let previous = (0..n)
                .rev()
                .find_map(|p| mapped[p].map(|k| (p, timed[k].1)));
            let following = (n + 1..chars.len()).find_map(|p| mapped[p].map(|k| (p, timed[k].1)));
            match (previous, following) {
                (Some((a, t0)), Some((b, t1))) => t0 + (t1 - t0) * (n - a) as f64 / (b - a) as f64,
                (Some((_, t)), None) | (None, Some((_, t))) => t,
                _ => 0.0,
            }
        };
        if n > 0 {
            centers[n] = centers[n].max(centers[n - 1]);
        }
    }
    Ok(chars
        .into_iter()
        .enumerate()
        .map(|(n, c)| Word {
            text: c.to_string(),
            start: if n == 0 {
                (centers[n] - 0.04).max(0.0)
            } else {
                (centers[n - 1] + centers[n]) / 2.0
            },
            end: centers
                .get(n + 1)
                .map(|v| (centers[n] + v) / 2.0)
                .unwrap_or(centers[n] + 0.1),
            confidence: if mapped[n].is_some() { 1.0 } else { 0.0 },
        })
        .collect())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f64,
}

pub fn prepare(
    audio: &Path,
    dir: &Path,
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
        force_english(
            &wave,
            reference,
            Path::new("D:/CC/Tools/forced-align/models/model_quantized.onnx"),
            Path::new("D:/CC/Tools/forced-align/models/vocab.json"),
        )?
    } else {
        let executable = Path::new("D:/CC/Tools/whisper-cpp/v1.9.2/Release/whisper-cli.exe");
        let model = Path::new("D:/CC/Tools/whisper-cpp/models/ggml-large-v3-turbo-q5_0.bin");
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
                    })
                })
                .collect()
        }
    };
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

#[cfg(feature = "ort")]
pub fn force_english(
    audio: &Path,
    reference: &str,
    model: &Path,
    vocab: &Path,
) -> Result<Vec<Word>> {
    use ort::{session::Session, value::Tensor};
    let runtime = std::env::current_exe()?
        .parent()
        .context("実行先がありません。")?
        .join("onnxruntime.dll");
    let runtime = if runtime.is_file() {
        runtime
    } else {
        "D:/CC/Tools/onnxruntime-directml/1.22.1/package/runtimes/win-x64/native/onnxruntime.dll"
            .into()
    };
    ort::init_from(runtime.display().to_string()).commit()?;
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
    let words: Vec<String> = reference
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_ascii_alphabetic() || *c == '\'')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let mut target = Vec::new();
    let mut word_indices = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if !target.is_empty() {
            target.push(dictionary["|"]);
            word_indices.push(index);
        }
        for c in word.to_ascii_uppercase().chars() {
            target.push(
                *dictionary
                    .get(&c.to_string())
                    .context("正本の歌詞に対応しない文字があります。")?,
            );
            word_indices.push(index);
        }
    }
    let path = ctc_path(&probabilities, &target, dictionary["<pad>"])?;
    let mut result: Vec<Word> = words
        .into_iter()
        .map(|text| Word {
            text,
            start: f64::INFINITY,
            end: 0.0,
            confidence: 0.0,
        })
        .collect();
    let mut counts = vec![0usize; result.len()];
    for (t, &state) in path.iter().enumerate() {
        if state % 2 == 0 {
            continue;
        }
        let token = state / 2;
        let word = word_indices[token];
        result[word].start = result[word].start.min(times[t]);
        result[word].end = times
            .get(t + 1)
            .copied()
            .unwrap_or(audio.len() as f64 / 16000.0);
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

#[cfg(not(feature = "ort"))]
pub fn force_english(
    _audio: &Path,
    _reference: &str,
    _model: &Path,
    _vocab: &Path,
) -> Result<Vec<Word>> {
    anyhow::bail!("このビルドには時刻合わせが含まれていません。")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_characters_require_a_blank_between_them() {
        let p = vec![vec![-5.0, -0.1], vec![-0.1, -5.0], vec![-5.0, -0.1]];
        assert_eq!(ctc_path(&p, &[1, 1], 0).unwrap(), vec![1, 2, 3]);
        assert!(ctc_path(&p[..2], &[1, 1], 0).is_err());
    }
}
