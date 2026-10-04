use crate::{Progress, model::Stems};
use anyhow::Result;
#[cfg(feature = "ort")]
use anyhow::{Context, ensure};
use std::path::Path;

#[cfg(all(windows, feature = "ort"))]
fn gpu_device() -> Result<i32> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut fallback = 0;
    for index in 0..16 {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        let desc = unsafe { adapter.GetDesc1()? };
        let name = String::from_utf16_lossy(&desc.Description)
            .trim_end_matches('\0')
            .to_string();
        eprintln!(
            "GPU候補{index}: {name}（専用メモリ{} MiB）",
            desc.DedicatedVideoMemory / 1024 / 1024
        );
        if desc.VendorId == 0x10de {
            eprintln!("RoFormerで使うGPU: {name}");
            return Ok(index as i32);
        }
        if desc.DedicatedVideoMemory > 0 {
            fallback = index as i32;
        }
    }
    Ok(fallback)
}
#[cfg(all(not(windows), feature = "ort"))]
fn gpu_device() -> Result<i32> {
    anyhow::bail!("DirectMLの分離はWindowsで使用してください。")
}

#[cfg(feature = "ort")]
pub fn separate(left: &[f32], right: &[f32], model: &Path, progress: &Progress) -> Result<Stems> {
    use ort::{execution_providers::DirectMLExecutionProvider, session::Session, value::Tensor};
    ensure!(
        left.len() == right.len() && !left.is_empty(),
        "音源の左右の長さが不正です。"
    );
    let adjacent = std::env::current_exe()?
        .parent()
        .context("実行先を確認できません。")?
        .join("onnxruntime.dll");
    let runtime = if adjacent.is_file() {
        adjacent
    } else {
        "D:/CC/Tools/onnxruntime-directml/1.22.1/package/runtimes/win-x64/native/onnxruntime.dll"
            .into()
    };
    ensure!(runtime.is_file(), "ONNX Runtimeの導入が必要です。");
    ort::init_from(runtime.display().to_string()).commit()?;
    progress.set("Mel-Band RoFormerをGPUへ読み込んでいます。");
    let mut session = Session::builder()?
        .with_parallel_execution(false)?
        .with_memory_pattern(false)?
        .with_execution_providers([DirectMLExecutionProvider::default()
            .with_device_id(gpu_device()?)
            .build()
            .error_on_failure()])?
        .commit_from_file(model)
        .context("Mel-Band RoFormerを読み込めませんでした。")?;
    let split_spectra = session.inputs.iter().any(|input| input.name == "spec_real");
    eprintln!(
        "モデル入力: {:?}",
        session
            .inputs
            .iter()
            .map(|input| (&input.name, &input.input_type))
            .collect::<Vec<_>>()
    );
    const CHUNK: usize = 352800;
    const STEP: usize = CHUNK / 2;
    let border = if left.len() > CHUNK { STEP } else { 0 };
    let padded_length = left.len() + 2 * border;
    let mut vocals_left = vec![0.0_f32; padded_length];
    let mut vocals_right = vec![0.0_f32; padded_length];
    let mut weights = vec![0.0_f32; padded_length];
    for (index, start) in (0..padded_length).step_by(STEP).enumerate() {
        progress.check()?;
        progress.set(format!(
            "Mel-Band RoFormerで歌声を分離しています（{} / {}）。",
            index + 1,
            padded_length.div_ceil(STEP)
        ));
        let count = (padded_length - start).min(CHUNK);
        let mut chunk_left = vec![0.0_f32; CHUNK];
        let mut chunk_right = vec![0.0_f32; CHUNK];
        for i in 0..CHUNK {
            let local = if i < count {
                i
            } else if count > STEP {
                crate::spectral::reflect_index(i as isize, count)
            } else {
                continue;
            };
            let source = crate::spectral::reflect_index(
                (start + local) as isize - border as isize,
                left.len(),
            );
            chunk_left[i] = left[source];
            chunk_right[i] = right[source];
        }
        let spec = crate::spectral::spectrum(&chunk_left, &chunk_right)?;
        let started = std::time::Instant::now();
        let (separated_left, separated_right) = if split_spectra {
            let mut real = vec![0f32; 2 * 1025 * 801];
            let mut imaginary = real.clone();
            for c in 0..2 {
                for f in 0..1025 {
                    for t in 0..801 {
                        let from = ((f * 2 + c) * 801 + t) * 2;
                        let to = (c * 1025 + f) * 801 + t;
                        real[to] = spec[from];
                        imaginary[to] = spec[from + 1];
                    }
                }
            }
            let outputs=session.run(ort::inputs!["spec_real"=>Tensor::from_array(([1,2,1025,801],real))?,"spec_imag"=>Tensor::from_array(([1,2,1025,801],imaginary))?])?;
            let (shape, a) = outputs[0].try_extract_tensor::<f32>()?;
            let (_, b) = outputs[1].try_extract_tensor::<f32>()?;
            ensure!(
                shape.as_ref() == [1, 1, 2, 1025, 801] && a.len() == b.len(),
                "RoFormer出力スペクトルが不正です。"
            );
            let mut combined = spec.clone();
            for c in 0..2 {
                for f in 0..1025 {
                    for t in 0..801 {
                        let to = ((f * 2 + c) * 801 + t) * 2;
                        let from = (c * 1025 + f) * 801 + t;
                        combined[to] = a[from];
                        combined[to + 1] = b[from];
                    }
                }
            }
            let mut identity = vec![0f32; spec.len()];
            for i in (0..identity.len()).step_by(2) {
                identity[i] = 1.0;
            }
            crate::spectral::synthesize(&combined, &identity, true)?
        } else {
            let tensor = Tensor::from_array(([1, 2050, 801, 2], spec.clone()))?;
            let output = session.run(ort::inputs!["spec" => tensor])?;
            let (shape, values) = output[0].try_extract_tensor::<f32>()?;
            ensure!(
                shape.as_ref() == [1, 1, 2050, 801, 2] && values.len() == spec.len(),
                "分離モデルの出力が一致しません。"
            );
            crate::spectral::synthesize(&spec, values, true)?
        };
        eprintln!(
            "RoFormer区間{}の推論: {:.2}秒",
            index + 1,
            started.elapsed().as_secs_f64()
        );
        for i in 0..count {
            let fade = CHUNK / 10;
            let weight = if i < fade && start != 0 {
                i as f32 / (fade - 1) as f32
            } else if i >= CHUNK - fade && start + STEP < padded_length {
                (CHUNK - 1 - i) as f32 / (fade - 1) as f32
            } else {
                1.0
            };
            vocals_left[start + i] += separated_left[i] * weight;
            vocals_right[start + i] += separated_right[i] * weight;
            weights[start + i] += weight;
        }
    }
    for i in 0..padded_length {
        if weights[i] > 0.0 {
            vocals_left[i] /= weights[i];
            vocals_right[i] /= weights[i];
        }
    }
    let vocals_left = vocals_left[border..border + left.len()].to_vec();
    let vocals_right = vocals_right[border..border + right.len()].to_vec();
    Ok(Stems {
        instrumental_left: left.iter().zip(&vocals_left).map(|(x, v)| x - v).collect(),
        instrumental_right: right
            .iter()
            .zip(&vocals_right)
            .map(|(x, v)| x - v)
            .collect(),
        vocals_left,
        vocals_right,
    })
}

#[cfg(not(feature = "ort"))]
pub fn separate(
    _left: &[f32],
    _right: &[f32],
    _model: &Path,
    _progress: &Progress,
) -> Result<Stems> {
    anyhow::bail!("このビルドにはRoFormer分離が含まれていません。")
}
