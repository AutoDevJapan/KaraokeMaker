use crate::{
    Progress,
    media::{self, Options, Tools},
    model,
    presentation::{VideoPreset, VisualStyle},
    web::{self, App},
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "音楽・動画から、このPCで個人用カラオケを作ります。")]
struct Cli {
    #[arg(long, global = true)]
    data: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Mode>,
}
#[derive(Subcommand)]
enum Mode {
    Serve {
        #[arg(long, default_value_t = 19531)]
        port: u16,
        #[arg(long)]
        no_open: bool,
    },
    Convert {
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "個人用カラオケ")]
        title: String,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        key: i32,
        #[arg(long, default_value_t = 1.0)]
        tempo: f64,
        #[arg(long)]
        lyrics: Option<PathBuf>,
        #[arg(long)]
        background: bool,
        #[arg(long, value_enum, default_value_t = VideoPreset::Hd)]
        preset: VideoPreset,
        /// JSON with background/accent RGB colors and an installed font name.
        #[arg(long)]
        style: Option<PathBuf>,
        #[arg(long,value_enum,default_value_t=crate::model::SeparationModel::MelBandFp16)]
        separation: crate::model::SeparationModel,
        #[arg(long)]
        words: Option<PathBuf>,
        #[arg(long)]
        no_subtitles: bool,
    },
    /// 指定した音源に歌詞を付けます（音源分離なし）。
    Render {
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        title: String,
        #[arg(long)]
        lyrics: PathBuf,
        #[arg(long, value_enum, default_value_t = VideoPreset::FullHd)]
        preset: VideoPreset,
        #[arg(long)]
        style: Option<PathBuf>,
        #[arg(long)]
        background: bool,
        #[arg(long)]
        words: Option<PathBuf>,
    },
    FetchModel,
    Check,
    AlignEnglish {
        audio: PathBuf,
        #[arg(long)]
        lyrics: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(
            long,
            default_value = "D:/CC/Tools/forced-align/models/model_quantized.onnx"
        )]
        model: PathBuf,
        #[arg(long, default_value = "D:/CC/Tools/forced-align/models/vocab.json")]
        vocab: PathBuf,
    },
    AlignWhisper {
        transcript: PathBuf,
        #[arg(long)]
        lyrics: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    ReadSubtitleFrames {
        folder: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    MeasureAudio {
        input: PathBuf,
    },
    CheckRuntime {
        #[arg(long)]
        gpu_first: bool,
    },
}
pub fn main() {
    let result = std::thread::Builder::new()
        .name("karaoke-main".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(run)
        .expect("thread creation failed")
        .join();
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("{error:#}");
            show_error(&error.to_string());
            std::process::exit(1);
        }
        Err(_) => {
            show_error(
                "アプリを起動できませんでした。GPUドライバーなどの動作環境を確認してください。",
            );
            std::process::exit(1);
        }
    }
}
fn run() -> Result<()> {
    let cli = Cli::parse();
    let data = cli.data.unwrap_or(
        std::env::current_exe()?
            .parent()
            .context("アプリのフォルダが見つかりません。")?
            .join("data"),
    );
    fs::create_dir_all(&data)?;
    let data = data.canonicalize()?;
    match cli.command.unwrap_or(Mode::Serve {
        port: 19531,
        no_open: false,
    }) {
        Mode::CheckRuntime { gpu_first } => {
            #[cfg(feature = "ort")]
            {
                let cpu = || -> Result<()> {
                    use ort::{session::Session, value::Tensor};
                    crate::runtime::initialize(false)?;
                    let mut session = Session::builder()?
                        .commit_from_file("D:/CC/Tools/forced-align/models/model_quantized.onnx")?;
                    let _=session.run(ort::inputs!["input_values"=>Tensor::from_array(([1,16000],vec![0f32;16000]))?])?;
                    Ok(())
                };
                let gpu = || -> Result<()> {
                    let signal: Vec<f32> =
                        (0..44100).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
                    let _ = crate::model::separate_using(
                        &signal,
                        &signal,
                        &data.join("models"),
                        crate::model::SeparationModel::MelBandFp16,
                        &Progress::default(),
                    )?;
                    Ok(())
                };
                if gpu_first {
                    gpu()?;
                    cpu()?;
                } else {
                    cpu()?;
                    gpu()?;
                }
                println!("CPUとCUDAを同じプロセスで利用できました。");
            }
            #[cfg(not(feature = "ort"))]
            {
                let _ = gpu_first;
                anyhow::bail!("このビルドにはONNX接続がありません。");
            }
        }
        Mode::MeasureAudio { input } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::quality::measure(&input)?)?
            );
        }
        Mode::ReadSubtitleFrames { folder, output } => {
            use std::io::Write;
            let result = crate::ocr::read_folder(&folder)?;
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)?
                .write_all(&serde_json::to_vec_pretty(&result)?)?;
        }
        Mode::AlignWhisper {
            transcript,
            lyrics,
            output,
        } => {
            let words = crate::alignment::reference_to_whisper(
                &fs::read_to_string(lyrics)?,
                &serde_json::from_slice(&fs::read(transcript)?)?,
            )?;
            use std::io::Write;
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)?
                .write_all(&serde_json::to_vec_pretty(&words)?)?;
        }
        Mode::AlignEnglish {
            audio,
            lyrics,
            output,
            model,
            vocab,
        } => {
            let words = crate::alignment::force_english(
                &audio,
                &fs::read_to_string(lyrics)?,
                &model,
                &vocab,
                &Progress::default(),
            )?;
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)?;
            file.write_all(&serde_json::to_vec_pretty(&words)?)?;
        }
        Mode::Check => {
            Tools::discover().check()?;
            println!("FFmpegの音源・映像・字幕変換を利用できます。");
        }
        Mode::FetchModel => {
            let _ = model::ensure_weights(&data.join("models"), &Progress::default())?;
            println!("分離モデルの取得と検証が終わりました。");
        }
        Mode::Convert {
            input,
            output,
            title,
            key,
            tempo,
            lyrics,
            background,
            preset,
            style,
            separation,
            words,
            no_subtitles,
        } => {
            fs::create_dir_all(&output)?;
            let dir = output.join(uuid::Uuid::new_v4().to_string());
            fs::create_dir(&dir)?;
            let options = Options {
                title,
                key,
                tempo,
                lyrics: lyrics
                    .map(fs::read_to_string)
                    .transpose()?
                    .unwrap_or_default(),
                original_video: !background,
                preset,
                style: read_style(style)?,
                separation,
                sample_rate: 48000,
                words: words
                    .map(|p| -> Result<_> { Ok(serde_json::from_slice(&fs::read(p)?)?) })
                    .transpose()?
                    .unwrap_or_default(),
                automatic_subtitles: !no_subtitles,
            };
            media::convert(
                &input,
                &dir,
                &data.join("models"),
                &options,
                &Progress::default(),
            )?;
            println!("{}", dir.canonicalize()?.display());
        }
        Mode::Render {
            input,
            output,
            title,
            lyrics,
            preset,
            style,
            background,
            words,
        } => {
            let options = Options {
                title,
                lyrics: fs::read_to_string(lyrics)?,
                original_video: !background,
                preset,
                style: read_style(style)?,
                words: words
                    .map(|p| -> Result<_> { Ok(serde_json::from_slice(&fs::read(p)?)?) })
                    .transpose()?
                    .unwrap_or_default(),
                ..Options::default()
            };
            options.validate()?;
            fs::create_dir_all(&output)?;
            let dir = output.join(uuid::Uuid::new_v4().to_string());
            fs::create_dir(&dir)?;
            media::render(&input, &dir, &options, &Progress::default())?;
            println!("{}", dir.canonicalize()?.display());
        }
        Mode::Serve { port, no_open } => {
            let instance = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(data.join("server.lock"))?;
            if instance.try_lock().is_err() {
                if !no_open {
                    let url=fs::read_to_string(data.join("address.txt")).context("同じ保存フォルダのKaraokeMakerが起動中です。少し待ってから開いてください。")?;
                    let parsed = reqwest::Url::parse(&url)?;
                    anyhow::ensure!(
                        parsed.scheme() == "http"
                            && parsed.host_str() == Some("127.0.0.1")
                            && parsed.path() == "/",
                        "起動中のKaraokeMakerの画面を確認できませんでした。"
                    );
                    open::that(url.trim())?;
                    return Ok(());
                }
                anyhow::bail!("同じ保存フォルダのKaraokeMakerはすでに起動しています。");
            }
            Tools::discover().check()?;
            tokio::runtime::Runtime::new()?.block_on(async {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                    Ok(listener) => listener,
                    Err(_) if port != 0 => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
                    Err(error) => return Err(error.into()),
                };
                let actual_port = listener.local_addr()?.port();
                let url = format!("http://127.0.0.1:{actual_port}");
                let app = App::new(&data, actual_port)?;
                fs::write(data.join("address.txt"), &url)?;
                println!("{url}");
                if !no_open {
                    open::that(&url).context(
                        "ブラウザーを開けませんでした。address.txtのURLを開いてください。",
                    )?;
                }
                let shutdown_app = app.clone();
                let (closing, closed) = tokio::sync::oneshot::channel();
                use std::future::IntoFuture;
                let server = axum::serve(listener, web::router(app.clone()))
                    .with_graceful_shutdown(async move {
                        tokio::select! {_=shutdown_app.stopped()=>{},_=tokio::signal::ctrl_c()=>{}}
                        shutdown_app.cancel_workers();
                        let _ = closing.send(());
                    })
                    .into_future();
                tokio::pin!(server);
                let result = tokio::select! {
                    result = &mut server => result,
                    _ = closed => {
                        tokio::time::timeout(std::time::Duration::from_secs(5), &mut server)
                            .await.unwrap_or(Ok(()))
                    }
                };
                app.stop_workers().await;
                result?;
                Ok::<(), anyhow::Error>(())
            })?;
        }
    }
    Ok(())
}
fn read_style(path: Option<PathBuf>) -> Result<VisualStyle> {
    let style = path
        .map(|p| -> Result<VisualStyle> { Ok(serde_json::from_slice(&fs::read(p)?)?) })
        .transpose()?
        .unwrap_or_default();
    style.validate()?;
    Ok(style)
}
#[cfg(windows)]
fn show_error(message: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    if std::env::args().any(|arg| {
        arg == "--no-open"
            || arg == "convert"
            || arg == "render"
            || arg == "fetch-model"
            || arg == "check"
    }) {
        return;
    }
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "KaraokeMaker".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
    }
}
#[cfg(not(windows))]
fn show_error(_message: &str) {}
