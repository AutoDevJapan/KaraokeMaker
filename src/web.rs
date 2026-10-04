use crate::{
    Progress,
    media::{self, Options},
};
use anyhow::{Context, Result};
use axum::{
    Form, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as WebPath, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
    thread::JoinHandle,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Notify, Semaphore},
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

const MAX_UPLOAD: usize = 1024 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    title: String,
    status: String,
    error: Option<String>,
}
#[derive(Clone)]
struct Job {
    record: Record,
    progress: Progress,
}
type Jobs = Arc<Mutex<BTreeMap<String, Job>>>;

#[derive(Clone)]
pub struct App {
    root: PathBuf,
    cache: PathBuf,
    pub(crate) csrf: String,
    pub(crate) music: Arc<crate::music::Workspace>,
    origin: String,
    jobs: Jobs,
    busy: Arc<Semaphore>,
    shutdown: Arc<Notify>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
    revision_sources: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl App {
    pub fn new(data: &Path, port: u16) -> Result<Self> {
        let root = data.join("jobs");
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let mut jobs = BTreeMap::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if Uuid::parse_str(&name).is_err() || !entry.file_type()?.is_dir() {
                continue;
            }
            let saved = fs::read(entry.path().join("status.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                .filter(|record| record.id == name);
            let mut record = match Self::recover(&entry.path(), &name).or(saved) {
                Some(record) => record,
                None => continue,
            };
            if record.status == "running" {
                record.status = "failed".into();
                record.error =
                    Some("前回の処理が途中で終了しました。もう一度読み込んでください。".into());
            }
            jobs.insert(
                name,
                Job {
                    record,
                    progress: Progress::default(),
                },
            );
        }
        Ok(Self {
            music: Arc::new(crate::music::Workspace::new(data.join("music"))?),
            root,
            cache: data.join("models"),
            csrf: Uuid::new_v4().to_string(),
            origin: format!("http://127.0.0.1:{port}"),
            jobs: Arc::new(Mutex::new(jobs)),
            busy: Arc::new(Semaphore::new(1)),
            shutdown: Arc::new(Notify::new()),
            worker: Arc::new(Mutex::new(None)),
            revision_sources: Arc::new(Mutex::new(std::collections::HashSet::new())),
        })
    }
    fn directory(&self, id: &str) -> Result<PathBuf> {
        let uuid = Uuid::parse_str(id).context("処理が見つかりません。")?;
        anyhow::ensure!(uuid.to_string() == id, "処理が見つかりません。");
        Ok(self.root.join(id))
    }
    fn save(&self, record: &Record) -> Result<()> {
        use std::io::Write;
        let dir = self.directory(&record.id)?;
        let temp = dir.join(format!("status-{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(record)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, dir.join("status.json"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result?;
        Ok(())
    }
    fn recover(dir: &Path, id: &str) -> Option<Record> {
        let options: Options =
            serde_json::from_slice(&fs::read(dir.join("settings.json")).ok()?).ok()?;
        options.validate().ok()?;
        for name in ["karaoke.wav", "karaoke.mp3", "karaoke.mp4", "vocals.wav"] {
            if fs::metadata(dir.join(name)).ok()?.len() <= 44 {
                return None;
            }
        }
        let tools = media::Tools::discover();
        let vocal = media::probe(&tools, &dir.join("vocals.wav")).ok()?;
        let audio = media::probe(&tools, &dir.join("karaoke.wav")).ok()?;
        let video = media::probe(&tools, &dir.join("karaoke.mp4")).ok()?;
        let expected = vocal.duration / options.tempo;
        if !video.video
            || (video.duration - expected).abs() >= 0.25
            || (audio.duration - expected).abs() >= 0.25
        {
            return None;
        }
        Some(Record {
            id: id.into(),
            title: options.title,
            status: "done".into(),
            error: None,
        })
    }
    fn finish(&self, id: &str, result: Result<()>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = jobs.get_mut(id) {
            job.record.status = if result.is_ok() { "done" } else { "failed" }.into();
            job.record.error = result.err().map(|e| e.to_string());
            if let Err(error) = self.save(&job.record) {
                eprintln!("結果の記録に失敗しました: {error}");
            }
        }
    }
    pub async fn stop_workers(&self) {
        self.cancel_workers();
        let worker = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(worker) = worker {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
    pub fn cancel_workers(&self) {
        self.music.session.close();
        for job in self.jobs.lock().unwrap_or_else(|e| e.into_inner()).values() {
            job.progress.cancelled.store(true, Ordering::Relaxed);
        }
    }
    pub fn shutdown(&self) {
        self.cancel_workers();
        self.shutdown.notify_one();
    }
    pub async fn stopped(&self) {
        self.shutdown.notified().await;
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .merge(crate::music::routes())
        .route(
            "/api/health",
            get(|| async {
                axum::Json(
                    serde_json::json!({"app":"KaraokeMaker","version":env!("CARGO_PKG_VERSION")}),
                )
            }),
        )
        .route("/", get(home))
        .route("/jobs", post(upload))
        .route("/jobs/{id}", get(job_page))
        .route("/jobs/{id}/{name}", get(artifact))
        .route("/cancel/{id}", post(cancel))
        .route(
            "/revise/{id}",
            post(revise).layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route("/delete/{id}", post(delete))
        .route("/shutdown", post(shutdown))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD + 128 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), local_only))
        .with_state(app)
}

async fn local_only(
    State(app): State<App>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let expected_host = app.origin.trim_start_matches("http://");
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(expected_host)
    {
        return (
            StatusCode::FORBIDDEN,
            "このPCのアプリ画面から開いてください。",
        )
            .into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && origin.to_str().ok() != Some(app.origin.as_str())
    {
        return (
            StatusCode::FORBIDDEN,
            "このPCのアプリ画面から操作してください。",
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("content-security-policy","default-src 'self'; style-src 'self' 'unsafe-inline'; media-src 'self' https://*.suno.com https://*.suno.ai; form-action 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub(crate) fn shell(title: &str, body: &str, refresh: bool) -> Html<String> {
    Html(format!(
        r#"<!doctype html><html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">{}<title>{} · KaraokeMaker</title><style>
    :root{{color-scheme:light;font-family:'Yu Gothic',system-ui,sans-serif;background:#f3f1e9;color:#172d24}}*{{box-sizing:border-box}}body{{margin:0}}main{{max-width:900px;margin:auto;padding:44px 24px 64px}}header{{display:flex;justify-content:space-between;align-items:center;border-bottom:1px solid #c9cdc2;padding-bottom:20px;margin-bottom:34px}}.brand{{font-size:15px;font-weight:700;letter-spacing:1px}}.tag{{font-size:12px;color:#617266}}h1{{font-size:clamp(28px,5vw,48px);line-height:1.3;letter-spacing:-1px;margin:0 0 16px}}h2{{font-size:20px;margin:32px 0 14px}}p{{line-height:1.85}}.lead{{max-width:640px;color:#506355}}.panel{{border:1px solid #c9cdc2;background:#fffdf7;padding:26px;margin-top:28px;border-radius:8px}}label{{display:block;font-weight:700;margin:18px 0 8px}}label:first-child{{margin-top:0}}input,select,textarea{{font:inherit;max-width:100%;border:1px solid #aab7aa;border-radius:4px;padding:10px;background:#fff;width:100%;color:#172d24}}input[type=file]{{border:1px dashed #91a997;padding:24px}}textarea{{resize:vertical;min-height:130px}}.row{{display:grid;grid-template-columns:1fr 1fr;gap:20px}}.hint{{color:#617266;font-size:13px;margin:8px 0 16px}}button,.button{{display:inline-block;background:#214e37;color:#fff;padding:13px 22px;border:0;border-radius:4px;font:inherit;font-weight:700;cursor:pointer;text-decoration:none}}.secondary{{background:#e5eade;color:#214e37}}.actions{{display:flex;gap:12px;flex-wrap:wrap;margin:22px 0}}a{{color:#214e37}}video,audio{{width:100%;margin:12px 0}}video{{background:#142e27;border-radius:4px}}.error{{background:#fff0df;padding:18px;border-left:4px solid #a46226;white-space:pre-wrap}}.status{{font-size:20px;font-weight:700}}.history{{padding:14px 0;border-bottom:1px solid #d5d9cf;display:flex;justify-content:space-between;gap:14px}}.history span{{font-size:13px;color:#617266}}details{{margin:22px 0}}summary{{cursor:pointer;font-weight:700}}footer{{margin-top:38px;color:#617266;font-size:12px}}@media(max-width:560px){{.row{{grid-template-columns:1fr}}.panel{{padding:18px}}main{{padding:24px 16px}}.history{{display:block}}}}
    </style></head><body><main><header><a class="brand" href="/">KaraokeMaker</a><span class="tag">自分の曲で、歌おう。</span></header>{body}<footer>カラオケ変換はこのPCで処理します。Sunoでの生成時は、歌詞と曲の雰囲気をSunoへ送ります。<br>曲によって歌声やコーラスが残ることがあります。</footer></main></body></html>"#,
        if refresh {
            "<meta http-equiv=\"refresh\" content=\"3\">"
        } else {
            ""
        },
        escape(title)
    ))
}
fn error_page(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (status,shell("読み込めませんでした",&format!("<h1>読み込めませんでした</h1><p class=\"error\">{}</p><a class=\"button secondary\" href=\"/\">もう一度選ぶ</a>",escape(&error.to_string())),false)).into_response()
}
async fn home(State(app): State<App>) -> Html<String> {
    let mut history = String::new();
    for job in app
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .rev()
    {
        history.push_str(&format!(
            "<div class=\"history\"><a href=\"/jobs/{}\">{}</a><span>{}</span></div>",
            job.record.id,
            escape(&job.record.title),
            match job.record.status.as_str() {
                "done" => "完成",
                "failed" => "確認が必要です",
                _ => "処理中",
            }
        ));
    }
    let body = format!(
        r#"<h1>まだカラオケにない曲を、<br>自分の一曲に。</h1><p class="lead">音楽・動画を選ぶと、歌声を除いた音源とカラオケ映像を作ります。原本はそのまま残ります。</p>
    <form class="panel" action="/jobs" method="post" enctype="multipart/form-data"><input type="hidden" name="csrf" value="{}">
    <label for="media">音楽・動画</label><input id="media" name="media" type="file" accept="audio/*,video/*,.mkv,.flac" required><p class="hint">MP3・WAV・FLAC・M4A・MP4など。15分・1GB以内。</p>
    <label for="title">曲名（任意）</label><input id="title" name="title" type="text" maxlength="120" placeholder="空欄ならファイル名を使います">
    <div class="row"><div><label for="key">キー</label><input id="key" name="key" type="number" min="-6" max="6" value="0"><p class="hint">半音単位。0で元のキー。</p></div><div><label for="tempo">速度</label><input id="tempo" name="tempo" type="number" min="0.5" max="1.5" step="0.05" value="1"><p class="hint">1で元の速度。0.8でゆっくり。</p></div></div>
    <label for="video">映像</label><select id="video" name="video"><option value="original">元の動画を使う（音楽だけなら背景を付ける）</option><option value="background">落ち着いた背景にする</option></select>
    <label for="preset">出力解像度</label><select id="preset" name="preset"><option value="hd" selected>HD（1280×720）</option><option value="1080p">フルHD（1920×1080）</option><option value="1440p">1440p（2560×1440）</option><option value="4k">4K（3840×2160）</option><option value="vertical">縦長フルHD（1080×1920）</option><option value="vertical-4k">縦長4K（2160×3840）</option></select><p class="hint">4Kは処理時間と容量が増えます。原本の細部は拡大しても増えません。</p>
    <details><summary>時刻付きの歌詞を載せる（任意）</summary><label for="lyrics">歌詞</label><textarea id="lyrics" name="lyrics" maxlength="65536" placeholder="[00:12.34]最初の歌詞&#10;[00:18.00]次の歌詞"></textarea><p class="hint">LRC形式で貼り付けます。表示中の行を色で追い、速度を変えると歌詞の時刻も合わせます。歌詞の自動聞き取りはありません。</p></details>
    <button type="submit">カラオケを作る</button><p class="hint">曲の長さとPCの性能によって、数分以上かかります。</p></form>
    <h2>作ったカラオケ</h2>{}<form action="/shutdown" method="post" style="margin-top:32px"><input type="hidden" name="csrf" value="{}"><button class="secondary" type="submit">アプリを終了する</button></form>"#,
        app.csrf,
        if history.is_empty() {
            "<p class=\"hint\">完成した音源と映像がここに並びます。</p>".into()
        } else {
            history
        },
        app.csrf
    );
    shell("自分の曲で歌おう", &body, false)
}

async fn upload(State(app): State<App>, mut multipart: Multipart) -> Response {
    let Ok(permit) = app.busy.clone().try_acquire_owned() else {
        return error_page(
            StatusCode::CONFLICT,
            "別の曲を処理しています。完成してから次の曲を選んでください。",
        );
    };
    let id = Uuid::new_v4().to_string();
    let dir = app.root.join(&id);
    if let Err(error) = tokio::fs::create_dir(&dir).await {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    let result = receive(&app, &dir, &mut multipart).await;
    let options = match result {
        Ok(options) => options,
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return error_page(StatusCode::BAD_REQUEST, error);
        }
    };
    let progress = Progress::default();
    progress.set("処理を始めています。");
    let record = Record {
        id: id.clone(),
        title: options.title.clone(),
        status: "running".into(),
        error: None,
    };
    if let Err(error) = app.save(&record) {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    app.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(
        id.clone(),
        Job {
            record,
            progress: progress.clone(),
        },
    );
    let worker_app = app.clone();
    let worker_id = id.clone();
    let worker = std::thread::Builder::new().name("karaoke-convert".into()).stack_size(8*1024*1024).spawn(move || {
        let _permit = permit;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||media::convert(&dir.join("input.bin"),&dir,&worker_app.cache,&options,&progress)))
            .unwrap_or_else(|_|Err(anyhow::anyhow!("音源の分離中にエラーが起きました。GPUのドライバーを確認して、アプリを起動し直してください。")));
        let _=fs::remove_file(dir.join("input.bin"));
        worker_app.finish(&worker_id,result);
    });
    match worker {
        Ok(worker) => {
            *app.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
            Redirect::to(&format!("/jobs/{id}")).into_response()
        }
        Err(error) => {
            app.finish(&id, Err(error.into()));
            error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "処理を開始できませんでした。",
            )
        }
    }
}

async fn receive(app: &App, dir: &Path, multipart: &mut Multipart) -> Result<Options> {
    let mut options = Options::default();
    let mut authorized = false;
    let mut has_file = false;
    let mut fallback_title = String::new();
    let mut seen = std::collections::HashSet::new();
    while let Some(mut field) = multipart.next_field().await? {
        let name = field.name().unwrap_or("").to_string();
        anyhow::ensure!(seen.insert(name.clone()), "同じ項目が複数届きました。");
        if name == "media" {
            anyhow::ensure!(authorized, "アプリの画面からファイルを選んでください。");
            let filename = field
                .file_name()
                .unwrap_or("個人用カラオケ")
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or("個人用カラオケ");
            fallback_title = Path::new(filename)
                .file_stem()
                .unwrap_or_else(|| std::ffi::OsStr::new("個人用カラオケ"))
                .to_string_lossy()
                .chars()
                .take(120)
                .collect();
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join("input.bin"))
                .await?;
            let mut size = 0usize;
            while let Some(chunk) = field.chunk().await? {
                size = size
                    .checked_add(chunk.len())
                    .context("ファイルが大きすぎます。")?;
                anyhow::ensure!(size <= MAX_UPLOAD, "ファイルは1GB以内にしてください。");
                file.write_all(&chunk).await?;
            }
            anyhow::ensure!(size > 0, "ファイルが空です。");
            file.flush().await?;
            has_file = true;
        } else {
            let mut bytes = Vec::new();
            while let Some(chunk) = field.chunk().await? {
                anyhow::ensure!(bytes.len() + chunk.len() <= 64 * 1024, "入力が長すぎます。");
                bytes.extend_from_slice(&chunk);
            }
            let value = String::from_utf8(bytes).context("入力を読み取れませんでした。")?;
            match name.as_str() {
                "csrf" => {
                    anyhow::ensure!(value == app.csrf, "アプリを開き直してください。");
                    authorized = true;
                }
                "title" => options.title = value.trim().into(),
                "key" => options.key = value.parse().context("キーは整数で入力してください。")?,
                "tempo" => {
                    options.tempo = value.parse().context("速度は数値で入力してください。")?
                }
                "video" => {
                    anyhow::ensure!(
                        ["original", "background"].contains(&value.as_str()),
                        "映像の選び方が不正です。"
                    );
                    options.original_video = value == "original";
                }
                "lyrics" => options.lyrics = value,
                "separation" => {
                    options.separation =
                        <crate::model::SeparationModel as clap::ValueEnum>::from_str(&value, false)
                            .map_err(|_| anyhow::anyhow!("分離方法を選んでください。"))?
                }
                "automatic_subtitles" => options.automatic_subtitles = value == "true",
                "preset" => {
                    options.preset =
                        <crate::presentation::VideoPreset as clap::ValueEnum>::from_str(
                            &value, false,
                        )
                        .map_err(|_| anyhow::anyhow!("出力解像度を選んでください。"))?
                }
                _ => anyhow::bail!("読み取れない項目が届きました。"),
            }
        }
    }
    anyhow::ensure!(authorized && has_file, "音楽・動画を選んでください。");
    if options.title.is_empty() || !seen.contains("title") {
        options.title = fallback_title;
    }
    options.validate()?;
    Ok(options)
}

async fn job_page(State(app): State<App>, WebPath(id): WebPath<String>) -> Response {
    let job = app
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned();
    let Some(job) = job else {
        return error_page(StatusCode::NOT_FOUND, "処理が見つかりません。");
    };
    let mut body = format!("<h1>{}</h1>", escape(&job.record.title));
    let running = job.record.status == "running";
    if job.record.status == "done" {
        body.push_str(&format!(r#"<p class="status">音源と映像ができました。</p><video controls preload="metadata" src="/jobs/{id}/karaoke.mp4"></video><div class="actions"><a class="button" download href="/jobs/{id}/karaoke.mp4">映像を保存（MP4）</a><a class="button secondary" download href="/jobs/{id}/karaoke.mp3">音源を保存（MP3）</a><a class="button secondary" download href="/jobs/{id}/karaoke.wav">音源を保存（WAV）</a></div><h2>伴奏を聴く</h2><audio controls preload="none" src="/jobs/{id}/karaoke.mp3"></audio><details><summary>分離した歌声を確認する</summary><audio controls preload="none" src="/jobs/{id}/vocals.wav"></audio><a download href="/jobs/{id}/vocals.wav">歌声だけを保存</a></details>"#));
        if let Ok(options) =
            fs::read(app.directory(&id).unwrap().join("settings.json")).and_then(|bytes| {
                serde_json::from_slice::<Options>(&bytes).map_err(std::io::Error::other)
            })
            && let Ok(lrc) = crate::revision::editable_lrc(&options)
        {
            let preset = <crate::presentation::VideoPreset as clap::ValueEnum>::to_possible_value(
                &options.preset,
            )
            .unwrap();
            body.push_str(&format!(r#"<section class="panel"><h2>歌詞と時刻を直す</h2><p>下の時刻は、いま再生している伴奏に合わせています。背景色付きの字幕動画を新しく作ります。</p><form method="post" action="/revise/{id}"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="preset" value="{}"><label for="revision-lyrics">時刻付きの歌詞</label><textarea id="revision-lyrics" name="lyrics" maxlength="65536">{}</textarea><p class="hint">歌い終わりに [00:16.00] のような空の行を入れると、間奏の歌詞を消せます。</p><button type="submit">歌詞だけ直して動画を作る</button></form></section>"#, app.csrf, preset.get_name(), escape(&lrc)));
        }
    } else if running {
        body.push_str(&format!("<div class=\"panel\"><p class=\"status\">{}</p><p class=\"hint\">この画面は3秒ごとに更新します。</p></div>",escape(&job.progress.get())));
    } else {
        body.push_str(&format!(
            "<p class=\"error\">{}</p>",
            escape(
                job.record
                    .error
                    .as_deref()
                    .unwrap_or("処理できませんでした。")
            )
        ));
    }
    body.push_str(&format!("<div class=\"actions\"><a class=\"button secondary\" href=\"/\">曲を選ぶ画面へ</a><form method=\"post\" action=\"/{}/{id}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"secondary\" type=\"submit\">{}</button></form></div>",if running {"cancel"} else {"delete"},app.csrf,if running {"処理を取り消す"} else {"このアプリ内の結果を削除する"}));
    shell(&job.record.title, &body, running).into_response()
}

#[derive(Deserialize)]
struct Token {
    csrf: String,
}

#[derive(Deserialize)]
struct RevisionForm {
    csrf: String,
    lyrics: String,
    preset: String,
}

struct SourceLease {
    sources: Arc<Mutex<std::collections::HashSet<String>>>,
    id: String,
}
impl Drop for SourceLease {
    fn drop(&mut self) {
        self.sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

async fn revise(
    State(app): State<App>,
    WebPath(source_id): WebPath<String>,
    Form(form): Form<RevisionForm>,
) -> Response {
    if form.csrf != app.csrf {
        return error_page(StatusCode::FORBIDDEN, "アプリを開き直してください。");
    }
    let Some(source) = app
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&source_id)
        .cloned()
    else {
        return error_page(StatusCode::NOT_FOUND, "結果が見つかりません。");
    };
    if source.record.status != "done" {
        return error_page(StatusCode::CONFLICT, "完成した曲の歌詞を直してください。");
    }
    let preset = match <crate::presentation::VideoPreset as clap::ValueEnum>::from_str(
        &form.preset,
        false,
    ) {
        Ok(preset) => preset,
        Err(_) => return error_page(StatusCode::BAD_REQUEST, "出力解像度を選んでください。"),
    };
    if let Err(error) = crate::lyrics::parse_lrc(&form.lyrics) {
        return error_page(StatusCode::BAD_REQUEST, error);
    }
    let Ok(permit) = app.busy.clone().try_acquire_owned() else {
        return error_page(
            StatusCode::CONFLICT,
            "別の曲を処理しています。完成してから歌詞を直してください。",
        );
    };
    let source_dir = match app.directory(&source_id) {
        Ok(dir) => dir,
        Err(error) => return error_page(StatusCode::BAD_REQUEST, error),
    };
    let lease = {
        // Deletion uses the same jobs lock, so a source cannot disappear between
        // checking its state and pinning it for the background copy.
        let jobs = app.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if jobs
            .get(&source_id)
            .is_none_or(|job| job.record.status != "done")
        {
            return error_page(StatusCode::NOT_FOUND, "修正元の結果が見つかりません。");
        }
        app.revision_sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source_id.clone());
        SourceLease {
            sources: app.revision_sources.clone(),
            id: source_id,
        }
    };
    let id = Uuid::new_v4().to_string();
    let dir = app.root.join(&id);
    if let Err(error) = fs::create_dir(&dir) {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    let progress = Progress::default();
    progress.set("歌詞を直して動画を作っています。音源の分離は繰り返しません。");
    let record = Record {
        id: id.clone(),
        title: source.record.title,
        status: "running".into(),
        error: None,
    };
    if let Err(error) = app.save(&record) {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    app.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(
        id.clone(),
        Job {
            record,
            progress: progress.clone(),
        },
    );
    let worker_app = app.clone();
    let worker_id = id.clone();
    let worker = std::thread::Builder::new()
        .name("karaoke-revise".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _permit = permit;
            let _lease = lease;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::revision::render_revision(&source_dir, &dir, &form.lyrics, preset, &progress)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("字幕の作成に失敗しました。")));
            worker_app.finish(&worker_id, result);
        });
    match worker {
        Ok(worker) => {
            *app.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
            Redirect::to(&format!("/jobs/{id}")).into_response()
        }
        Err(error) => {
            app.finish(&id, Err(error.into()));
            error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "字幕の作成を始められませんでした。",
            )
        }
    }
}
async fn cancel(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return error_page(StatusCode::FORBIDDEN, "アプリを開き直してください。");
    }
    if let Some(job) = app.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
        job.progress.cancelled.store(true, Ordering::Relaxed);
        job.progress
            .set("取り消しています。分離中の区間が終わるまでお待ちください。");
    }
    Redirect::to(&format!("/jobs/{id}")).into_response()
}
async fn delete(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return error_page(StatusCode::FORBIDDEN, "アプリを開き直してください。");
    }
    let mut jobs = app.jobs.lock().unwrap_or_else(|e| e.into_inner());
    let Some(job) = jobs.get(&id) else {
        return error_page(StatusCode::NOT_FOUND, "結果が見つかりません。");
    };
    if app
        .revision_sources
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&id)
    {
        return error_page(
            StatusCode::CONFLICT,
            "この曲の歌詞を修正しています。動画ができてから削除してください。",
        );
    }
    if job.record.status == "running" {
        return error_page(StatusCode::CONFLICT, "処理が終わってから削除してください。");
    }
    let result = app.directory(&id).and_then(|path| {
        fs::remove_dir_all(path)?;
        Ok(())
    });
    if let Err(error) = result {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    jobs.remove(&id);
    Redirect::to("/").into_response()
}
async fn shutdown(State(app): State<App>, Form(token): Form<Token>) -> Response {
    if token.csrf != app.csrf {
        return error_page(StatusCode::FORBIDDEN, "アプリを開き直してください。");
    }
    app.shutdown();
    shell("終了しました","<h1>アプリを終了しました。</h1><p>この画面を閉じてください。作った結果は次回の起動時に開けます。</p>",false).into_response()
}

fn byte_range(range: &str, length: u64) -> Option<(u64, u64)> {
    let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
    if length == 0 {
        return None;
    }
    if start.is_empty() {
        let tail = end.parse::<u64>().ok()?;
        if tail == 0 {
            return None;
        }
        return Some((length.saturating_sub(tail), length - 1));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end && start < length).then_some((start, end))
}

async fn artifact(
    State(app): State<App>,
    WebPath((id, name)): WebPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mime = match name.as_str() {
        "karaoke.mp4" => "video/mp4",
        "karaoke.mp3" => "audio/mpeg",
        "karaoke.wav" | "vocals.wav" => "audio/wav",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let done = app
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .is_some_and(|j| j.record.status == "done");
    if !done {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(dir) = app.directory(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(mut file) = tokio::fs::File::open(dir.join(&name)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(metadata) = file.metadata().await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let length = metadata.len();
    let mut status = StatusCode::OK;
    let mut start = 0;
    let mut end = length.saturating_sub(1);
    if let Some(range) = headers.get(header::RANGE) {
        let Some((from, to)) = range.to_str().ok().and_then(|r| byte_range(r, length)) else {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{length}"))],
            )
                .into_response();
        };
        start = from;
        end = to;
        status = StatusCode::PARTIAL_CONTENT;
    }
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let count = if length == 0 { 0 } else { end - start + 1 };
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file.take(count))));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, mime.parse().unwrap());
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, count.to_string().parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("inline; filename=\"{name}\"").parse().unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}").parse().unwrap(),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn lyrics_can_be_revised_through_the_page_without_loading_a_model() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let source_id = Uuid::new_v4().to_string();
        let source_dir = app.directory(&source_id).unwrap();
        fs::create_dir(&source_dir).unwrap();
        let samples: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
        media::write_stereo(&source_dir.join("karaoke.wav"), &samples, &samples).unwrap();
        media::write_stereo(&source_dir.join("vocals.wav"), &samples, &samples).unwrap();
        let options = Options {
            lyrics: "[00:00.10]before\n[00:00.80]\n".into(),
            ..Options::default()
        };
        fs::write(
            source_dir.join("settings.json"),
            serde_json::to_vec(&options).unwrap(),
        )
        .unwrap();
        app.jobs.lock().unwrap().insert(
            source_id.clone(),
            Job {
                record: Record {
                    id: source_id.clone(),
                    title: "試験".into(),
                    status: "done".into(),
                    error: None,
                },
                progress: Progress::default(),
            },
        );
        let service = router(app.clone());
        let mut form = reqwest::Url::parse("http://127.0.0.1/").unwrap();
        form.query_pairs_mut()
            .append_pair("csrf", &app.csrf)
            .append_pair("lyrics", "[00:00.20]corrected\n[00:00.75]\n")
            .append_pair("preset", "1080p");
        let response = service
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/revise/{source_id}"))
                    .header("host", "127.0.0.1:19531")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(form.query().unwrap().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string();
        let revised_id = location.strip_prefix("/jobs/").unwrap().to_string();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let status = app.jobs.lock().unwrap()[&revised_id].record.status.clone();
            if status != "running" {
                assert_eq!(status, "done");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "字幕修正の処理が完了しません"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!temp.path().join("models").exists());
        let revised_dir = app.directory(&revised_id).unwrap();
        assert_eq!(
            fs::read_to_string(revised_dir.join("lyrics.srt")).unwrap(),
            "1\n00:00:00,200 --> 00:00:00,750\ncorrected\n\n"
        );
        assert_eq!(
            fs::read_to_string(source_dir.join("settings.json")).unwrap(),
            serde_json::to_string(&options).unwrap()
        );
        assert!(App::recover(&revised_dir, &revised_id).is_some());
        let page = service
            .oneshot(
                Request::builder()
                    .uri(location)
                    .header("host", "127.0.0.1:19531")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = String::from_utf8(
            page.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("歌詞だけ直して動画を作る"));
        assert!(html.contains("[00:00.20]corrected"));
        app.stop_workers().await;
    }

    #[tokio::test]
    async fn revision_form_accepts_encoded_japanese_and_leased_sources_cannot_be_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let id = Uuid::new_v4().to_string();
        fs::create_dir(app.directory(&id).unwrap()).unwrap();
        app.jobs.lock().unwrap().insert(
            id.clone(),
            Job {
                record: Record {
                    id: id.clone(),
                    title: "曲".into(),
                    status: "done".into(),
                    error: None,
                },
                progress: Progress::default(),
            },
        );
        let service = router(app.clone());
        let japanese = (0..300)
            .map(|n| format!("[{:02}:{:02}.00]{}", n / 60, n % 60, "歌".repeat(50)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(japanese.len() < 64 * 1024);
        let mut encoded = reqwest::Url::parse("http://127.0.0.1/").unwrap();
        encoded
            .query_pairs_mut()
            .append_pair("csrf", &app.csrf)
            .append_pair("lyrics", &japanese)
            .append_pair("preset", "invalid");
        assert!(encoded.query().unwrap().len() > 128 * 1024);
        let response = service
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/revise/{id}"))
                    .header("host", "127.0.0.1:19531")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(encoded.query().unwrap().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST); // Parsed form, invalid preset, not 413.
        encoded
            .query_pairs_mut()
            .clear()
            .append_pair("csrf", &app.csrf)
            .append_pair("lyrics", &"a".repeat(65537))
            .append_pair("preset", "1080p");
        let response = service
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/revise/{id}"))
                    .header("host", "127.0.0.1:19531")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(encoded.query().unwrap().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        app.revision_sources.lock().unwrap().insert(id.clone());
        let lease = SourceLease {
            sources: app.revision_sources.clone(),
            id: id.clone(),
        };
        let response = service
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/delete/{id}"))
                    .header("host", "127.0.0.1:19531")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={}", app.csrf)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(app.directory(&id).unwrap().is_dir());
        drop(lease);
        let response = service
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/delete/{id}"))
                    .header("host", "127.0.0.1:19531")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={}", app.csrf)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(!app.directory(&id).unwrap().exists());
    }
    #[tokio::test]
    async fn web_resolution_select_is_received_and_invalid_values_are_rejected() {
        use axum::extract::FromRequest;
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let page = home(State(app.clone())).await;
        let html = page.0;
        assert!(html.contains("value=\"4k\""));
        assert!(html.contains("value=\"vertical-4k\""));
        for (index, preset, expected) in [
            (0, "4k", Some((3840, 2160))),
            (1, "vertical-4k", Some((2160, 3840))),
            (2, "8k", None),
        ] {
            let dir = temp.path().join(format!("parse{index}"));
            fs::create_dir(&dir).unwrap();
            let body = format!(
                "--testboundary\r\nContent-Disposition: form-data; name=\"csrf\"\r\n\r\n{}\r\n--testboundary\r\nContent-Disposition: form-data; name=\"preset\"\r\n\r\n{preset}\r\n--testboundary\r\nContent-Disposition: form-data; name=\"media\"; filename=\"demo.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFFtestdata\r\n--testboundary--\r\n",
                app.csrf
            );
            let req = Request::builder()
                .method("POST")
                .header("content-type", "multipart/form-data; boundary=testboundary")
                .body(Body::from(body))
                .unwrap();
            let mut multipart = Multipart::from_request(req, &()).await.unwrap();
            let parsed = receive(&app, &dir, &mut multipart).await;
            match expected {
                Some(d) => assert_eq!(parsed.unwrap().preset.dimensions(), d),
                None => assert!(parsed.is_err()),
            }
        }
    }
    #[tokio::test]
    async fn malicious_host_or_origin_and_missing_token_cannot_modify_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let router = router(app.clone());
        let bad_host = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("host", "attacker.test:19531")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_host.status(), StatusCode::FORBIDDEN);
        let bad_origin = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/shutdown")
                    .header("host", "127.0.0.1:19531")
                    .header("origin", "https://attacker.test")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={}", app.csrf)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);
        let missing_token = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/shutdown")
                    .header("host", "127.0.0.1:19531")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("csrf=wrong"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_token.status(), StatusCode::FORBIDDEN);
        assert!(app.jobs.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn saved_results_support_seek_and_cannot_expose_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let id = Uuid::new_v4().to_string();
        let dir = app.directory(&id).unwrap();
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("karaoke.mp4"), b"0123456789").unwrap();
        fs::write(dir.join("input.bin"), b"private").unwrap();
        app.jobs.lock().unwrap().insert(
            id.clone(),
            Job {
                record: Record {
                    id: id.clone(),
                    title: "<script>".into(),
                    status: "done".into(),
                    error: None,
                },
                progress: Progress::default(),
            },
        );
        let router = router(app);
        let result = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/jobs/{id}/karaoke.mp4"))
                    .header("host", "127.0.0.1:19531")
                    .header("range", "bytes=2-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(result.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(
            result.into_body().collect().await.unwrap().to_bytes(),
            "2345"
        );
        let private = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/jobs/{id}/input.bin"))
                    .header("host", "127.0.0.1:19531")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(private.status(), StatusCode::NOT_FOUND);
        let page = router
            .oneshot(
                Request::builder()
                    .uri(format!("/jobs/{id}"))
                    .header("host", "127.0.0.1:19531")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = String::from_utf8(
            page.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains("&lt;script&gt;"));
        assert!(!text.contains("<script>"));
    }
    #[test]
    fn range_bounds_and_suffix_are_exact() {
        assert_eq!(byte_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(byte_range("bytes=5-", 10), Some((5, 9)));
        assert_eq!(byte_range("bytes=0-999", 10), Some((0, 9)));
        for range in [
            "bytes=10-20",
            "bytes=5-2",
            "bytes=-0",
            "bytes=0-1,3-4",
            "other=1-2",
        ] {
            assert_eq!(byte_range(range, 10), None);
        }
    }
}
