use crate::{
    music_import::{self, ImportedAudio},
    suno::{Broker, BrowserTask, Clip, Phase},
    web::{self, App},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Form, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as WebPath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Semaphore;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

pub struct Workspace {
    root: PathBuf,
    broker: Broker,
    busy: std::sync::Arc<Semaphore>,
    records: std::sync::Mutex<()>,
    validated_audio: std::sync::Mutex<BTreeSet<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Song {
    id: String,
    title: String,
    lyrics: String,
    style: String,
    instrumental: bool,
    main_vocal: String,
    karaoke_delay_hours: u16,
    stage: String,
    error: Option<String>,
    clips: Vec<Clip>,
    selected: Option<String>,
    #[serde(default)]
    audio: BTreeMap<String, ImportedAudio>,
    #[serde(default)]
    browser_task: Option<BrowserTask>,
}
#[derive(Deserialize)]
struct Draft {
    csrf: String,
    title: String,
    lyrics: String,
    style: String,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    main_vocal: String,
    karaoke_delay_hours: u16,
}
#[derive(Deserialize)]
struct Token {
    csrf: String,
    #[serde(default)]
    clip: String,
}
#[derive(Deserialize)]
struct ResultIds {
    csrf: String,
    ids: String,
}

impl Workspace {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("projects"))?;
        let root = root.canonicalize()?;
        ensure!(
            root.join("projects").canonicalize()? == root.join("projects"),
            "曲の保存先を確認できませんでした。"
        );
        let workspace = Self {
            broker: Broker::new()?,
            root,
            busy: std::sync::Arc::new(Semaphore::new(1)),
            records: std::sync::Mutex::new(()),
            validated_audio: std::sync::Mutex::new(BTreeSet::new()),
        };
        for mut song in workspace.list() {
            // Previously selected samples are retained, but cannot carry an
            // active completed-song selection into the new acceptance policy.
            if let Some(clip) = song.selected.as_deref() {
                let checked = song
                    .audio
                    .get(clip)
                    .context("採用曲の音源を確認できませんでした。")
                    .and_then(|audio| {
                        let path =
                            music_import::verified_path(&workspace.root, &song.id, clip, audio)?;
                        music_import::require_finished_song(music_import::validate_saved(
                            &path,
                            &audio.sha256,
                        )?)
                    });
                if let Err(error) = checked {
                    song.selected = None;
                    song.stage = "results".into();
                    song.error = Some(error.to_string());
                    workspace.save(&song)?;
                }
            }
            if song
                .browser_task
                .as_ref()
                .is_some_and(|t| t.phase == Phase::Claimed)
            {
                song.browser_task.as_mut().unwrap().phase = Phase::Unknown;
                song.stage = "unknown".into();
                song.error = Some("送信担当の操作中にアプリが終了しました。再送せず、Sunoの履歴から結果だけを確認してください。".into());
                workspace.save(&song)?;
            } else if song.stage == "starting" {
                song.stage = "blocked".into();
                song.error=Some("接続確認中にアプリが終了しました。生成要求は送っていないため、もう一度操作できます。".into());
                workspace.save(&song)?;
            } else if song.stage == "requested" && song.clips.is_empty() {
                song.stage = "unknown".into();
                song.error = Some(
                    "生成要求の結果を確認できませんでした。Sunoの履歴を確認してください。".into(),
                );
                workspace.save(&song)?;
            }
        }
        Ok(workspace)
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        let id = music_import::storage_id(id)?;
        Ok(self.root.join("projects").join(format!("{id}.json")))
    }
    fn load(&self, id: &str) -> Result<Song> {
        let requested = self.path(id)?;
        let path = requested.canonicalize()?;
        if !path.starts_with(self.root.join("projects")) {
            anyhow::bail!("曲の保存先を確認できませんでした。");
        }
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink() && path == requested,
            "曲の保存先を確認できませんでした。"
        );
        let song: Song = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(song.id == id, "曲が見つかりません。");
        let mut ids = std::collections::BTreeSet::new();
        for clip in &song.clips {
            music_import::canonical_id(&clip.id)?;
            ensure!(
                ids.insert(clip.id.as_str()),
                "保存した候補のIDが重複しています。"
            );
        }
        ensure!(
            song.audio.keys().all(|clip| ids.contains(clip.as_str())),
            "保存した音源と候補が一致しません。"
        );
        if let Some(task) = &song.browser_task {
            music_import::canonical_id(&task.id)?;
        }
        Ok(song)
    }
    fn save(&self, song: &Song) -> Result<()> {
        let _lock = self.records.lock().unwrap_or_else(|e| e.into_inner());
        self.save_unlocked(song)
    }
    fn update(&self, id: &str, change: impl FnOnce(&mut Song) -> Result<()>) -> Result<()> {
        let _lock = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let mut latest = self.load(id)?;
        change(&mut latest)?;
        self.save_unlocked(&latest)
    }
    fn save_unlocked(&self, song: &Song) -> Result<()> {
        use std::io::Write;
        let target = self.path(&song.id)?;
        let temp = target.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(song)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target)?;
        Ok(())
    }
    fn verified_audio(&self, project: &str, clip: &str, audio: &ImportedAudio) -> Result<PathBuf> {
        // Always check the current bytes/hash. Cache only successful content
        // validation, so range playback does not launch decoders repeatedly.
        let path = music_import::verified_path(&self.root, project, clip, audio)?;
        let mut validated = self
            .validated_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !validated.contains(&audio.sha256) {
            music_import::validate_saved(&path, &audio.sha256)?;
            validated.insert(audio.sha256.clone());
        }
        Ok(path)
    }

    fn list(&self) -> Vec<Song> {
        let Ok(entries) = fs::read_dir(self.root.join("projects")) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension()?.to_str()? != "json" {
                    return None;
                }
                let id = path.file_stem()?.to_str()?;
                self.load(id).ok()
            })
            .collect()
    }
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/music", get(home))
        .route("/music/drafts", post(create))
        .route("/music/connect", post(connect))
        .route("/music/{id}", get(detail))
        .route("/music/{id}/generate", post(generate))
        .route("/music/{id}/refresh", post(refresh))
        .route("/music/{id}/select", post(select))
        .route("/music/{id}/results", post(register_results))
        .route(
            "/music/{id}/import",
            post(import_audio).layer(DefaultBodyLimit::max(music_import::MAX_FORM)),
        )
        .route("/music/{id}/audio/{clip}", get(audio))
        .route("/music/{id}/dispatch", post(dispatch_again))
        .route("/music/{id}/worker/{task}", get(worker_page))
        .route("/music/{id}/worker/{task}/claim", post(claim))
        .route("/music/{id}/worker/{task}/uncertain", post(uncertain))
        .route("/music/{id}/worker/{task}/cancel", post(cancel))
        .route("/music/{id}/worker/{task}/results", post(worker_results))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}
fn fail(error: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST,web::shell("音楽制作の確認",&format!("<h1>音楽制作の確認</h1><p class=\"error\">{}</p><a class=\"button secondary\" href=\"/music\">音楽制作に戻る</a>",web::escape(&error.to_string())),false)).into_response()
}
async fn characters() -> Vec<(String, String)> {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return Vec::new();
    };
    let Ok(response) = client
        .get("http://127.0.0.1:10013/api/vtuber-characters")
        .send()
        .await
    else {
        return Vec::new();
    };
    let Ok(data) = response.json::<serde_json::Value>().await else {
        return Vec::new();
    };
    data.as_array()
        .or_else(|| data["characters"].as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    Some((
                        value["id"].as_str()?.to_string(),
                        value["name"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}
async fn home(State(app): State<App>) -> Response {
    let mut cast = String::from("<option value=\"\">後で決める</option>");
    for (id, name) in characters().await {
        cast.push_str(&format!(
            "<option value=\"{}\">{}</option>",
            web::escape(&id),
            web::escape(&name)
        ));
    }
    let mut history = String::new();
    for song in app.music.list() {
        history.push_str(&format!(
            "<div class=\"history\"><a href=\"/music/{}\">{}</a><span>{}</span></div>",
            song.id,
            web::escape(&song.title),
            stage_name(&song.stage)
        ));
    }
    let body = format!(
        r#"<h1>歌詞から、一曲へ。</h1><p class="lead">曲の候補を聴いて選び、担当キャラと映像につなげます。公開する曲と投稿先は、選んでから決めます。</p>
    <div class="actions"><a class="button secondary" href="/">Consoleに戻る</a><a class="button secondary" href="http://127.0.0.1:19531/">KaraokeMakerを開く</a></div>
    <a class="button secondary" href="https://suno.com/create" target="_blank" rel="noopener">Sunoの公式画面を開く</a><p class="hint">ログイン済みの通常Chromeを使います。生成操作は、保存した下書きの要求を担当者へ渡して行います。</p>
    <form class="panel" method="post" action="/music/drafts"><input type="hidden" name="csrf" value="{}">
    <label for="song-title">曲名</label><input id="song-title" name="title" maxlength="120" required>
    <label for="song-lyrics">歌詞</label><textarea id="song-lyrics" name="lyrics" maxlength="5000" placeholder="[Verse]&#10;歌詞を入力&#10;&#10;[Chorus]&#10;サビを入力"></textarea>
    <label for="song-style">曲の雰囲気</label><textarea id="song-style" name="style" maxlength="1000" required placeholder="例：力強いロック、疾走感、日本語ボーカル"></textarea>
    <label for="song-kind">音源</label><select id="song-kind" name="instrumental"><option value="false">歌入り</option><option value="true">インストゥルメンタル</option></select>
    <label for="main-vocal">映像でメインボーカルを担当するキャラ</label><select id="main-vocal" name="main_vocal">{cast}</select><p class="hint">登録済みのAI-VTuberから選びます。歌声の指定は今後、Sunoの声設定と別に連携します。</p>
    <label for="karaoke-delay">MV公開からカラオケ版を公開するまでの時間</label><input id="karaoke-delay" type="number" name="karaoke_delay_hours" min="0" max="720" value="24"><p class="hint">24で1日後。予定を保存する項目です。現在の版ではYouTubeへの投稿や予約は実行しません。</p>
    <button type="submit">曲の下書きを保存</button><p class="hint">保存後に生成を始めます。Sunoの契約と利用枠を使い、外部の有料APIやCAPTCHAサービスは使いません。</p></form>
    <h2>制作中の曲</h2>{}<div class="panel"><strong>制作の流れ</strong><p>作詞 → Sunoで曲候補を生成 → 曲を選ぶ → JIZURAでMV → チャンネルを選んで投稿 → カラオケ版を予約</p><p class="hint">この版はSunoの生成と曲候補の選択までを扱います。Gemini・Claudeでの作詞、キャラ出演、MVとYouTubeの連携は次の段階です。</p></div>"#,
        app.csrf,
        if history.is_empty() {
            "<p class=\"hint\">保存した曲がここに並びます。</p>".into()
        } else {
            history
        }
    );
    web::shell("音楽制作", &body, false).into_response()
}
fn stage_name(stage: &str) -> &str {
    match stage {
        "draft" => "下書き",
        "starting" => "接続を確認中",
        "requested" => "Sunoの結果待ち",
        "ready" => "保存した曲候補があります",
        "results" => "曲候補のMP3を保存してください",
        "queued" => "通常Chromeで操作する担当を受付中",
        "cancelled" => "生成要求を取り消しました",
        "selected" => "採用する曲を選択済み",
        "unknown" => "Sunoの履歴を確認",
        _ => "接続を確認してください",
    }
}
async fn create(State(app): State<App>, Form(draft): Form<Draft>) -> Response {
    let result = (|| -> Result<Song> {
        ensure!(
            draft.csrf == app.csrf,
            "音楽制作の画面を開き直してください。"
        );
        ensure!(
            !draft.title.trim().is_empty() && draft.title.chars().count() <= 120,
            "曲名を120文字以内で入力してください。"
        );
        ensure!(
            draft.lyrics.chars().count() <= 5000
                && draft.style.chars().count() <= 1000
                && !draft.style.trim().is_empty(),
            "歌詞は5000文字、曲の雰囲気は1000文字以内にしてください。"
        );
        ensure!(
            draft.instrumental || !draft.lyrics.trim().is_empty(),
            "歌入りの曲には歌詞を入力してください。"
        );
        ensure!(
            draft.karaoke_delay_hours <= 720 && draft.main_vocal.len() <= 200,
            "担当キャラか公開予定を確認してください。"
        );
        let song = Song {
            id: Uuid::new_v4().to_string(),
            title: draft.title.trim().into(),
            lyrics: draft.lyrics,
            style: draft.style,
            instrumental: draft.instrumental,
            main_vocal: draft.main_vocal,
            karaoke_delay_hours: draft.karaoke_delay_hours,
            stage: "draft".into(),
            error: None,
            clips: Vec::new(),
            selected: None,
            audio: BTreeMap::new(),
            browser_task: None,
        };
        app.music.save(&song)?;
        Ok(song)
    })();
    match result {
        Ok(song) => Redirect::to(&format!("/music/{}", song.id)).into_response(),
        Err(error) => fail(error),
    }
}
async fn connect(State(app): State<App>, Form(token): Form<Token>) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    Redirect::to("https://suno.com/create").into_response()
}
async fn detail(State(app): State<App>, WebPath(id): WebPath<String>) -> Response {
    let song = match app.music.load(&id) {
        Ok(song) => song,
        Err(_) => return fail("曲が見つかりません。"),
    };
    let mut body = format!(
        "<h1>{}</h1><p class=\"status\">{}</p><p>カラオケ版の公開予定：MVの公開から{}時間後</p>",
        web::escape(&song.title),
        stage_name(&song.stage),
        song.karaoke_delay_hours
    );
    if let Some(error) = &song.error {
        body.push_str(&format!("<p class=\"error\">{}</p>", web::escape(error)));
    }
    body.push_str(&format!("<details><summary>歌詞と曲の雰囲気</summary><p style=\"white-space:pre-wrap\">{}</p><p>{}</p></details>",web::escape(&song.lyrics),web::escape(&song.style)));
    for clip in &song.clips {
        body.push_str(&format!(
            "<div class=\"panel\"><h2>{}</h2><p>{}</p>",
            web::escape(if clip.title.is_empty() {
                &song.title
            } else {
                &clip.title
            }),
            web::escape(&clip.status)
        ));
        body.push_str(&format!(
            "<p class=\"hint\">Sunoの曲ID：<code>{}</code></p>",
            web::escape(&clip.id)
        ));
        if song.audio.contains_key(&clip.id) {
            body.push_str(&format!(
                "<audio controls preload=\"none\" src=\"/music/{id}/audio/{}\"></audio>",
                clip.id
            ));
        }
        body.push_str(&format!("<a href=\"https://suno.com/song/{}\" target=\"_blank\" rel=\"noopener\">Sunoで開く・ダウンロードする</a>",clip.id));
        if clip.status == "complete" && song.audio.contains_key(&clip.id) {
            body.push_str(&format!("<form method=\"post\" action=\"/music/{id}/select\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"clip\" value=\"{}\"><button class=\"secondary\" type=\"submit\">{}</button></form>",app.csrf,clip.id,if song.selected.as_deref()==Some(&clip.id){"この曲を採用中"}else{"この曲を採用する"}));
        } else {
            body.push_str(&format!(r#"<form method="post" enctype="multipart/form-data" action="/music/{id}/import"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="clip" value="{}"><label><input type="checkbox" name="complete" value="true" required>このIDの曲を全編聴き、歌詞・導入・展開・終結と演奏がつながり、無音や断片の反復で尺を増やしていないことを確認しました</label><label>この候補の公式MP3（16MB以内）<input type="file" name="media" accept="audio/mpeg,.mp3" required></label><button class="secondary" type="submit">MP3を取り込む</button><p class="hint">この曲IDのSuno公式画面から保存したファイルを選びます。原本を変更せず、このアプリへ保存します。保存を検査できた候補だけを採用できます。</p></form>"#, app.csrf, clip.id));
        }
        body.push_str("</div>");
    }
    if song.browser_task.is_none() {
        body.push_str(&format!(r#"<details><summary>公式画面で生成済みの曲を関連付ける</summary><form method="post" action="/music/{id}/results"><input type="hidden" name="csrf" value="{}"><label>この下書きから生成した曲のID（1行に1件、8件まで）<textarea name="ids" maxlength="400" required></textarea></label><button class="secondary" type="submit">生成済みの曲を関連付ける</button><p class="hint">Sunoの曲URLの末尾にあるIDを使います。新しい曲は生成しません。登録後、候補ごとに公式MP3を取り込みます。</p></form></details>"#, app.csrf));
    }
    if song.browser_task.is_none()
        && song.clips.is_empty()
        && ["draft", "blocked"].contains(&song.stage.as_str())
    {
        body.push_str(&format!("<form method=\"post\" action=\"/music/{id}/generate\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button type=\"submit\">Sunoで曲を生成する</button><p class=\"hint\">この歌詞と曲の雰囲気をSunoへ送り、契約の生成枠を使います。</p></form>",app.csrf));
    }
    if let Some(task) = &song.browser_task {
        body.push_str(&format!(
            "<a href=\"/music/{id}/worker/{}\">生成を担当する操作画面と状態</a>",
            task.id
        ));
        if task.phase == Phase::Queued {
            body.push_str(&format!(r#"<form method="post" action="/music/{id}/worker/{}/cancel"><input type="hidden" name="csrf" value="{}"><button class="secondary" type="submit">未送信の要求を取り消す</button></form>"#, task.id, app.csrf));
            body.push_str(&format!(r#"<form method="post" action="/music/{id}/dispatch"><input type="hidden" name="csrf" value="{}"><button class="secondary" type="submit">同じ担当要求の受付を確認する</button><p class="hint">同じ要求番号で担当の受付を確認します。新しい曲の生成要求は作りません。</p></form>"#, app.csrf));
        }
    }
    if !song.clips.is_empty() {
        body.push_str(&format!("<form method=\"post\" action=\"/music/{id}/refresh\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"secondary\" type=\"submit\">生成結果を更新する</button></form>",app.csrf));
    }
    body.push_str("<div class=\"actions\"><a href=\"/music\">音楽制作に戻る</a><a href=\"https://suno.com/create\" target=\"_blank\" rel=\"noopener\">Sunoの履歴を確認する</a><a href=\"https://852wa.github.io/JIZURA/\" target=\"_blank\" rel=\"noopener\">JIZURAでMVを作る</a></div>");
    web::shell(&song.title, &body, false).into_response()
}
async fn generate(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    let Ok(_permit) = app.music.busy.clone().try_acquire_owned() else {
        return fail("別の音楽処理が進んでいます。状態を確認してから操作してください。");
    };
    let task = BrowserTask {
        id: Uuid::new_v4().to_string(),
        todo_id: None,
        phase: Phase::Queued,
    };
    if let Err(error) = app.music.update(&id, |song| {
        ensure!(song.browser_task.is_none() && song.clips.is_empty() && ["draft", "blocked"].contains(&song.stage.as_str()),
            "この曲には保存した生成要求があります。再送せず、担当の状態かSunoの履歴を確認してください。");
        song.browser_task = Some(task.clone());
        song.stage = "queued".into();
        song.error = None;
        Ok(())
    }) { return fail(error); }
    let result = dispatch_saved(&app.music, &id).await;
    if let Err(error) = result
        && let Err(save_error) = app.music.update(&id, |song| {
            song.error = Some(error.to_string());
            Ok(())
        })
    {
        return fail(save_error);
    }
    Redirect::to(&format!("/music/{id}")).into_response()
}

async fn dispatch_saved(workspace: &Workspace, id: &str) -> Result<()> {
    let song = workspace.load(id)?;
    let task = song
        .browser_task
        .as_ref()
        .context("保存した要求がありません。")?;
    ensure!(
        task.phase == Phase::Queued,
        "この要求をもう一度送信できません。"
    );
    if task.todo_id.is_none() {
        let todo = workspace.broker.add_task(id, &task.id).await?;
        workspace.update(id, |song| {
            let current = song
                .browser_task
                .as_mut()
                .context("生成要求がありません。")?;
            ensure!(
                current.id == task.id && current.phase == Phase::Queued,
                "生成要求を取り消しています。"
            );
            ensure!(
                current.todo_id.is_none_or(|saved| saved == todo),
                "受付した担当番号が一致しません。"
            );
            current.todo_id = Some(todo);
            Ok(())
        })?;
    }
    let saved = workspace.load(id)?;
    workspace
        .broker
        .dispatch(id, saved.browser_task.as_ref().unwrap())
        .await?;
    workspace.update(id, |song| {
        if song
            .browser_task
            .as_ref()
            .is_some_and(|current| current.id == task.id && current.phase == Phase::Queued)
        {
            song.error = None;
        }
        Ok(())
    })
}
async fn dispatch_again(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    let result = dispatch_saved(&app.music, &id).await;
    match result {
        Ok(()) => Redirect::to(&format!("/music/{id}")).into_response(),
        Err(error) => fail(error),
    }
}
async fn refresh(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    if app.music.load(&id).is_err() {
        return fail("曲が見つかりません。");
    }
    Redirect::to(&format!("/music/{id}")).into_response()
}
async fn worker_page(
    State(app): State<App>,
    WebPath((id, request)): WebPath<(String, String)>,
) -> Response {
    let Ok(song) = app.music.load(&id) else {
        return fail("曲が見つかりません。");
    };
    let Some(task) = &song.browser_task else {
        return fail("この曲の担当要求がありません。");
    };
    if task.id != request {
        return fail("別の生成要求です。");
    }
    let mut body = format!(
        "<h1>通常Chromeで曲を生成する担当</h1><p>{}</p><p>曲名：{}</p><p>音源：{}</p><pre>{}</pre><p>曲の雰囲気：{}</p>",
        stage_name(&song.stage),
        web::escape(&song.title),
        if song.instrumental {
            "インストゥルメンタル"
        } else {
            "歌入り"
        },
        web::escape(&song.lyrics),
        web::escape(&song.style)
    );
    if task.phase == Phase::Queued && task.todo_id.is_some() {
        body.push_str(&format!(r#"<form method="post" action="/music/{id}/worker/{request}/claim"><input type="hidden" name="csrf" value="{}"><button type="submit">担当を取得する</button><p>取得成功後に公式Createを1回だけ押せます。他の担当や再開した担当は送信できません。</p></form>"#, app.csrf));
    } else if task.phase == Phase::Claimed {
        body.push_str("<p>送信操作の担当を取得済み</p><p>取得を成功させた担当だけが公式Createを1回操作できます。再開した担当は、履歴から結果を回収するだけにしてください。</p>");
    } else {
        body.push_str("<p>生成の再送は禁止です。公式履歴から結果だけを確認してください。</p>");
    }
    if [Phase::Claimed, Phase::Unknown, Phase::Results].contains(&task.phase) {
        body.push_str(&format!(r#"<form method="post" action="/music/{id}/worker/{request}/results"><input type="hidden" name="csrf" value="{}"><label>公式履歴の実曲ID（1行に1件）<textarea name="ids" maxlength="400" required></textarea></label><button type="submit">実際の生成結果を記録する</button></form>"#, app.csrf));
    }
    if task.phase == Phase::Claimed {
        body.push_str(&format!(r#"<form method="post" action="/music/{id}/worker/{request}/uncertain"><input type="hidden" name="csrf" value="{}"><button class="secondary" type="submit">結果が不明な要求として保存する</button></form>"#, app.csrf));
    }
    body.push_str(&format!("<a href=\"/music/{id}\">曲の保存と選択へ</a>"));
    web::shell("生成操作の担当", &body, false).into_response()
}
fn transition(workspace: &Workspace, id: &str, request: &str, to: Phase) -> Result<()> {
    workspace.update(id, |song| {
        let task = song
            .browser_task
            .as_mut()
            .context("生成要求がありません。")?;
        ensure!(task.id == request, "別の生成要求です。");
        match to {
            Phase::Claimed => ensure!(
                task.phase == Phase::Queued && task.todo_id.is_some(),
                "担当は取得済みか、要求が取り消されています。新しく送信できません。"
            ),
            Phase::Cancelled => ensure!(
                task.phase == Phase::Queued,
                "送信担当が操作した可能性があります。取消ではなく履歴の確認が必要です。"
            ),
            Phase::Unknown => ensure!(
                task.phase == Phase::Claimed,
                "この要求は送信担当の操作中ではありません。"
            ),
            _ => anyhow::bail!("生成要求の状態を変更できません。"),
        }
        task.phase = to;
        song.stage = match to {
            Phase::Claimed => "requested",
            Phase::Cancelled => "cancelled",
            _ => "unknown",
        }
        .into();
        song.error = None;
        Ok(())
    })
}
async fn claim(
    State(app): State<App>,
    WebPath((id, request)): WebPath<(String, String)>,
    Form(token): Form<Token>,
) -> Response {
    task_transition(app, id, request, token, Phase::Claimed)
}
async fn uncertain(
    State(app): State<App>,
    WebPath((id, request)): WebPath<(String, String)>,
    Form(token): Form<Token>,
) -> Response {
    task_transition(app, id, request, token, Phase::Unknown)
}
async fn cancel(
    State(app): State<App>,
    WebPath((id, request)): WebPath<(String, String)>,
    Form(token): Form<Token>,
) -> Response {
    task_transition(app, id, request, token, Phase::Cancelled)
}
fn task_transition(app: App, id: String, request: String, token: Token, to: Phase) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    match transition(&app.music, &id, &request, to) {
        Ok(()) => Redirect::to(&format!("/music/{id}/worker/{request}")).into_response(),
        Err(error) => fail(error),
    }
}
async fn worker_results(
    State(app): State<App>,
    WebPath((id, request)): WebPath<(String, String)>,
    Form(form): Form<ResultIds>,
) -> Response {
    if form.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    let result = app.music.update(&id, |song| {
        let task = song
            .browser_task
            .as_ref()
            .context("生成要求がありません。")?;
        ensure!(
            task.id == request
                && [Phase::Claimed, Phase::Unknown, Phase::Results].contains(&task.phase),
            "この生成要求の結果を記録できません。"
        );
        attach_ids(song, &form.ids)?;
        song.browser_task.as_mut().unwrap().phase = Phase::Results;
        Ok(())
    });
    match result {
        Ok(()) => Redirect::to(&format!("/music/{id}")).into_response(),
        Err(error) => fail(error),
    }
}
fn attach_ids(song: &mut Song, input: &str) -> Result<()> {
    ensure!(input.len() <= 400, "曲のIDは8件まで入力してください。");
    let ids: Vec<_> = input
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    ensure!(
        !ids.is_empty() && ids.len() <= 8,
        "曲のIDは1件から8件まで入力してください。"
    );
    let mut unique = std::collections::BTreeSet::new();
    for clip in &ids {
        music_import::canonical_id(clip)?;
        ensure!(unique.insert(*clip), "同じ曲のIDを2回入力しています。");
    }
    if !song.clips.is_empty() {
        ensure!(
            song.clips.len() == ids.len()
                && song.clips.iter().all(|c| unique.contains(c.id.as_str())),
            "関連付け済みの曲IDと違います。別の曲を上書きできません。"
        );
        return Ok(());
    }
    song.clips = ids
        .into_iter()
        .map(|clip| Clip {
            id: clip.into(),
            title: song.title.clone(),
            status: "awaiting_import".into(),
            audio_url: String::new(),
        })
        .collect();
    song.stage = "results".into();
    song.error = None;
    Ok(())
}

async fn select(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    let music = app.music.clone();
    let project = id.clone();
    let saved = tokio::task::spawn_blocking(move || {
        music.update(&project, |song| {
            if !song
                .clips
                .iter()
                .any(|clip| clip.id == token.clip && clip.status == "complete")
            {
                anyhow::bail!("完成した曲の候補を選んでください。");
            }
            let imported = song
                .audio
                .get(&token.clip)
                .ok_or_else(|| anyhow::anyhow!("先に候補の公式MP3を取り込んでください。"))?;
            let path = music.verified_audio(&project, &token.clip, imported)?;
            // Completion is measured from the original, not editable metadata
            // or the playback validation cache. Short imports remain drafts.
            music_import::require_finished_song(music_import::validate_saved(
                &path,
                &imported.sha256,
            )?)?;
            song.selected = Some(token.clip);
            song.stage = "selected".into();
            Ok(())
        })
    })
    .await;
    match saved {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return fail(error),
        Err(error) => return fail(error),
    }
    Redirect::to(&format!("/music/{id}")).into_response()
}

async fn register_results(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(form): Form<ResultIds>,
) -> Response {
    if form.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    let Ok(_permit) = app.music.busy.clone().try_acquire_owned() else {
        return fail("別の音楽処理が進んでいます。状態を確認してから操作してください。");
    };
    let result = app.music.update(&id, |song| {
        ensure!(
            song.browser_task.is_none(),
            "担当要求があります。担当の結果画面で実際の曲IDを記録してください。"
        );
        ensure!(
            song.stage != "starting",
            "担当者の結果がまだ不明です。履歴の確認が終わるまでお待ちください。"
        );
        attach_ids(song, &form.ids)
    });
    match result {
        Ok(()) => Redirect::to(&format!("/music/{id}")).into_response(),
        Err(error) => fail(error),
    }
}

async fn import_audio(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    mut multipart: Multipart,
) -> Response {
    let Ok(_permit) = app.music.busy.clone().try_acquire_owned() else {
        return fail("別の音楽処理が進んでいます。状態を確認してから操作してください。");
    };
    let result = async {
        let mut csrf = None;
        let mut clip_id = None;
        let mut complete = false;
        let mut data = None;
        let mut replaceable_hash = None;
        while let Some(field) = multipart.next_field().await? {
            match field.name() {
                Some("csrf") if csrf.is_none() => {
                    let value = field.text().await?;
                    ensure!(value == app.csrf, "音楽制作の画面を開き直してください。");
                    csrf = Some(value);
                }
                Some("clip") if clip_id.is_none() => {
                    let value = field.text().await?;
                    music_import::canonical_id(&value)?;
                    clip_id = Some(value);
                }
                Some("complete") if !complete => {
                    ensure!(
                        field.text().await? == "true",
                        "公式画面で完成した候補を確認してください。"
                    );
                    complete = true;
                }
                Some("media") if data.is_none() => {
                    ensure!(
                        csrf.is_some() && clip_id.is_some() && complete,
                        "取り込み画面からMP3を選んでください。"
                    );
                    let song = app.music.load(&id)?;
                    ensure!(
                        song.clips.iter().any(|c| Some(&c.id) == clip_id.as_ref()),
                        "この下書きに関連付けた候補を選んでください。"
                    );
                    let bytes = field.bytes().await?;
                    ensure!(
                        !bytes.is_empty() && bytes.len() <= music_import::MAX_MP3,
                        "MP3は16MB以内で選んでください。"
                    );
                    if let Some(existing) = song.audio.get(clip_id.as_ref().unwrap()) {
                        use sha2::{Digest, Sha256};
                        if existing.sha256 != format!("{:x}", Sha256::digest(&bytes)) {
                            let music = app.music.clone();
                            let project = id.clone();
                            let clip = clip_id.as_ref().unwrap().clone();
                            let existing = existing.clone();
                            let hash = existing.sha256.clone();
                            let checked = tokio::task::spawn_blocking(move || {
                                music.verified_audio(&project, &clip, &existing)
                            })
                            .await?;
                            match checked {
                                Err(error) if music_import::is_incomplete(&error) => {
                                    replaceable_hash = Some(hash)
                                }
                                Err(error) => return Err(error),
                                Ok(_) => anyhow::bail!(
                                    "保存済みの候補と内容が違います。別の音源で上書きできません。"
                                ),
                            }
                        }
                    }
                    data = Some(bytes);
                }
                _ => anyhow::bail!("取り込み画面からMP3を選んでください。"),
            }
        }
        let clip = clip_id.context("曲のIDがありません。")?;
        ensure!(csrf.is_some(), "音楽制作の画面を開き直してください。");
        let bytes = data.context("MP3を選んでください。")?;
        let root = app.music.root.clone();
        let project = id.clone();
        let imported_clip = clip.clone();
        let imported = tokio::task::spawn_blocking(move || {
            music_import::import_mp3(&root, &project, &imported_clip, &bytes)
        })
        .await??;
        app.music.update(&id, |song| {
            let candidate = song
                .clips
                .iter_mut()
                .find(|c| c.id == clip)
                .context("この下書きの候補が見つかりません。")?;
            if let Some(existing) = song.audio.get(&clip) {
                ensure!(
                    existing.sha256 == imported.sha256
                        || replaceable_hash.as_ref() == Some(&existing.sha256),
                    "保存済みの候補と内容が違います。別の音源で上書きできません。"
                );
            }
            candidate.status = "complete".into();
            candidate.audio_url.clear();
            song.audio.insert(clip, imported);
            if song.selected.is_none() {
                song.stage = "ready".into();
            }
            song.error = None;
            Ok(())
        })
    }
    .await;
    match result {
        Ok(()) => Redirect::to(&format!("/music/{id}")).into_response(),
        Err(error) => fail(error),
    }
}

async fn audio(
    State(app): State<App>,
    WebPath((id, clip)): WebPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let music = app.music.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let song = music.load(&id)?;
        ensure!(
            song.clips
                .iter()
                .any(|c| c.id == clip && c.status == "complete"),
            "保存した候補がありません。"
        );
        music.verified_audio(
            &id,
            &clip,
            song.audio.get(&clip).context("音源がありません。")?,
        )
    })
    .await;
    let Ok(Ok(requested)) = result else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(path) = requested.canonicalize() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !path.starts_with(app.music.root.join("audio")) || path != requested {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(metadata) = file.metadata().await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let length = metadata.len();
    if length == 0 || length > music_import::MAX_MP3 as u64 {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut status = StatusCode::OK;
    let (start, end) = if let Some(range) = headers.get(header::RANGE) {
        let Some(range) = range.to_str().ok().and_then(|r| web::byte_range(r, length)) else {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{length}"))],
            )
                .into_response();
        };
        status = StatusCode::PARTIAL_CONTENT;
        range
    } else {
        (0, length - 1)
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let count = end - start + 1;
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file.take(count))));
    *response.status_mut() = status;
    for (name, value) in [
        (header::CONTENT_TYPE, "audio/mpeg".into()),
        (header::ACCEPT_RANGES, "bytes".into()),
        (header::CONTENT_LENGTH, count.to_string()),
        (header::CACHE_CONTROL, "no-store".into()),
    ] {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    if status == StatusCode::PARTIAL_CONTENT {
        response.headers_mut().insert(
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

    fn fixture() -> Song {
        Song {
            id: Uuid::new_v4().to_string(),
            title: "合成試験".into(),
            lyrics: "歌詞".into(),
            style: "rock".into(),
            instrumental: false,
            main_vocal: String::new(),
            karaoke_delay_hours: 24,
            stage: "draft".into(),
            error: None,
            clips: Vec::new(),
            selected: None,
            audio: BTreeMap::new(),
            browser_task: None,
        }
    }

    #[test]
    fn claim_is_once_cancellation_prevents_claim_and_restart_requires_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(temp.path().into()).unwrap();
        let mut song = fixture();
        let request = Uuid::new_v4().to_string();
        song.browser_task = Some(BrowserTask {
            id: request.clone(),
            todo_id: Some(123),
            phase: Phase::Queued,
        });
        song.stage = "queued".into();
        workspace.save(&song).unwrap();
        assert!(
            transition(
                &workspace,
                &song.id,
                &Uuid::new_v4().to_string(),
                Phase::Claimed
            )
            .is_err()
        );
        transition(&workspace, &song.id, &request, Phase::Claimed).unwrap();
        assert!(transition(&workspace, &song.id, &request, Phase::Claimed).is_err());
        assert!(transition(&workspace, &song.id, &request, Phase::Cancelled).is_err());
        let restarted = Workspace::new(temp.path().into()).unwrap();
        let recovered = restarted.load(&song.id).unwrap();
        assert_eq!(recovered.stage, "unknown");
        assert_eq!(recovered.browser_task.unwrap().phase, Phase::Unknown);
        assert!(transition(&restarted, &song.id, &request, Phase::Claimed).is_err());
        song.id = Uuid::new_v4().to_string();
        workspace.save(&song).unwrap();
        transition(&workspace, &song.id, &request, Phase::Cancelled).unwrap();
        assert!(transition(&workspace, &song.id, &request, Phase::Claimed).is_err());
        let restarted = Workspace::new(temp.path().into()).unwrap();
        assert_eq!(
            restarted
                .load(&song.id)
                .unwrap()
                .browser_task
                .unwrap()
                .phase,
            Phase::Cancelled
        );
    }

    #[tokio::test]
    async fn queued_request_recovers_lost_receipt_after_restart_without_new_request() {
        use axum::Json;
        use serde_json::{Value, json};
        use std::sync::{Arc, Mutex};
        let state = Arc::new(Mutex::new((
            0_usize,
            0_usize,
            BTreeMap::<String, u64>::new(),
        )));
        let seen = state.clone();
        let delegated = state.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/agent", listener.local_addr().unwrap());
        let stub = Router::new()
            .route(
                "/api/agent/todo/ensure",
                post(move |Json(body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        let key = body["request_id"].as_str().unwrap().to_owned();
                        let mut state = seen.lock().unwrap();
                        state.0 += 1;
                        let todo = *state.2.entry(key.clone()).or_insert(123);
                        if state.0 == 1 {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        Json(json!({"ok":true,"data":{"request_id":key,"item":{"id":todo}}}))
                            .into_response()
                    }
                }),
            )
            .route(
                "/api/agent/delegate",
                post(move || {
                    let delegated = delegated.clone();
                    async move {
                        delegated.lock().unwrap().1 += 1;
                        Json(json!({"ok":true,"data":{"accepted":true}}))
                    }
                }),
            );
        let worker = tokio::spawn(async move {
            axum::serve(listener, stub).await.unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path(), 19531).unwrap();
        Arc::get_mut(&mut app.music).unwrap().broker = Broker::for_test(base.clone());
        let song = fixture();
        app.music.save(&song).unwrap();
        let response = web::router(app.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/music/{}/generate", song.id))
                    .header(header::HOST, "127.0.0.1:19531")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={}", app.csrf)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let interrupted = app.music.load(&song.id).unwrap().browser_task.unwrap();
        assert_eq!(interrupted.phase, Phase::Queued);
        assert_eq!(interrupted.todo_id, None);
        assert!(app.music.load(&song.id).unwrap().error.is_some());
        drop(app);
        let mut restarted = App::new(temp.path(), 19531).unwrap();
        Arc::get_mut(&mut restarted.music).unwrap().broker = Broker::for_test(base);
        let router = web::router(restarted.clone());
        let page = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/music/{}", song.id))
                    .header(header::HOST, "127.0.0.1:19531")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let page = page.into_body().collect().await.unwrap().to_bytes();
        assert!(
            std::str::from_utf8(&page)
                .unwrap()
                .contains("同じ担当要求の受付を確認する")
        );
        let recovered = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/music/{}/dispatch", song.id))
                    .header(header::HOST, "127.0.0.1:19531")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={}", restarted.csrf)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(recovered.status(), StatusCode::SEE_OTHER);
        let task = restarted
            .music
            .load(&song.id)
            .unwrap()
            .browser_task
            .unwrap();
        assert_eq!(task.id, interrupted.id);
        assert_eq!(task.todo_id, Some(123));
        assert!(restarted.music.load(&song.id).unwrap().error.is_none());
        transition(&restarted.music, &song.id, &task.id, Phase::Claimed).unwrap();
        assert!(transition(&restarted.music, &song.id, &task.id, Phase::Claimed).is_err());
        assert!(dispatch_saved(&restarted.music, &song.id).await.is_err());
        {
            let counts = state.lock().unwrap();
            assert_eq!(counts.0, 2);
            assert_eq!(counts.1, 1);
            assert_eq!(counts.2.len(), 1);
        }
        worker.abort();
        let _ = worker.await;
    }

    #[test]
    fn attaches_exact_distinct_ids_idempotently_and_never_reports_unimported_complete() {
        let mut song = fixture();
        let first = "00000000-0000-4000-8000-000000000001";
        let second = "00000000-0000-4000-8000-000000000002";
        assert!(attach_ids(&mut song, &format!("{first}\n{first}")).is_err());
        assert!(song.clips.is_empty());
        let ids = format!("{first}\n{second}");
        attach_ids(&mut song, &ids).unwrap();
        attach_ids(&mut song, &format!("{second}\n{first}")).unwrap();
        assert_eq!(song.clips.len(), 2);
        assert!(
            song.clips
                .iter()
                .all(|clip| clip.status == "awaiting_import" && clip.audio_url.is_empty())
        );
        assert_eq!(song.stage, "results");
        assert!(attach_ids(&mut song, first).is_err());
        assert!(attach_ids(&mut song, "../outside").is_err());
        assert_eq!(song.clips[0].id, first);
    }

    #[tokio::test]
    async fn incomplete_import_cannot_be_selected_and_legacy_audio_can_be_repaired() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let mut song = fixture();
        let clip = "00000000-0000-4000-8000-000000000001";
        attach_ids(&mut song, clip).unwrap();
        app.music.save(&song).unwrap();
        let (normal, variants) = music_import::tests::synthetic_variants(temp.path());
        let router = web::router(app.clone());
        let import = |bytes: &[u8]| {
            let mut body = format!("--demo\r\nContent-Disposition: form-data; name=\"csrf\"\r\n\r\n{}\r\n--demo\r\nContent-Disposition: form-data; name=\"clip\"\r\n\r\n{clip}\r\n--demo\r\nContent-Disposition: form-data; name=\"complete\"\r\n\r\ntrue\r\n--demo\r\nContent-Disposition: form-data; name=\"media\"; filename=\"official.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\n", app.csrf).into_bytes();
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n--demo--\r\n");
            Request::builder()
                .method("POST")
                .uri(format!("/music/{}/import", song.id))
                .header(header::HOST, "127.0.0.1:19531")
                .header(header::CONTENT_TYPE, "multipart/form-data; boundary=demo")
                .body(Body::from(body))
                .unwrap()
        };
        let select = || {
            Request::builder()
                .method("POST")
                .uri(format!("/music/{}/select", song.id))
                .header(header::HOST, "127.0.0.1:19531")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf={}&clip={clip}", app.csrf)))
                .unwrap()
        };
        for bytes in &variants {
            assert_eq!(
                router
                    .clone()
                    .oneshot(import(bytes))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
            let saved = app.music.load(&song.id).unwrap();
            assert!(saved.audio.is_empty() && saved.selected.is_none());
            assert_eq!(saved.clips[0].status, "awaiting_import");
            assert_eq!(
                router.clone().oneshot(select()).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
        // Seed synthetic storage exactly as the old importer did; no real
        // project data is read or modified by this compatibility regression.
        let broken = &variants[1];
        let legacy = ImportedAudio {
            sha256: format!("{:x}", Sha256::digest(broken)),
            bytes: broken.len() as u64,
            duration_seconds: 5.0,
        };
        let directory = app.music.root.join("audio").join(&song.id).join(clip);
        let old_path = directory.join(format!("{}.mp3", legacy.sha256));
        fs::write(&old_path, broken).unwrap();
        song.audio.insert(clip.into(), legacy);
        song.clips[0].status = "complete".into();
        app.music.save(&song).unwrap();
        assert_eq!(
            router.clone().oneshot(select()).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/music/{}/audio/{clip}", song.id))
                        .header(header::HOST, "127.0.0.1:19531")
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            router
                .clone()
                .oneshot(import(&normal))
                .await
                .unwrap()
                .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            router.clone().oneshot(select()).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        // A valid short import can be played, but editing its duration cannot
        // promote it to a completed song (even after playback validation).
        let mut spoofed = app.music.load(&song.id).unwrap();
        spoofed.audio.get_mut(clip).unwrap().duration_seconds = 200.0;
        app.music.save(&spoofed).unwrap();
        assert_eq!(
            router.clone().oneshot(select()).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        let mut other = normal.clone();
        other.extend_from_slice(&[0; 128]);
        assert_eq!(
            router
                .clone()
                .oneshot(import(&other))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(fs::read(old_path).unwrap(), *broken);
        assert_eq!(fs::read(temp.path().join("synthetic.mp3")).unwrap(), normal);
        // Old versions could persist a short selected candidate. Reopening
        // retains its original and ID but returns it to unfinished results.
        let mut legacy_selected = app.music.load(&song.id).unwrap();
        legacy_selected.selected = Some(clip.into());
        legacy_selected.stage = "selected".into();
        app.music.save(&legacy_selected).unwrap();
        let restarted = App::new(temp.path(), 19531).unwrap();
        let saved = restarted.music.load(&song.id).unwrap();
        assert!(saved.selected.is_none());
        assert_eq!(saved.stage, "results");
        assert_eq!(
            fs::read(
                restarted
                    .music
                    .verified_audio(&song.id, clip, &saved.audio[clip])
                    .unwrap()
            )
            .unwrap(),
            normal
        );
    }

    #[tokio::test]
    async fn http_import_selection_range_and_restart_keep_exact_bytes_and_unselected_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(temp.path(), 19531).unwrap();
        let mut song = fixture();
        let first = "00000000-0000-4000-8000-000000000001";
        let second = "00000000-0000-4000-8000-000000000002";
        attach_ids(&mut song, &format!("{first}\n{second}")).unwrap();
        app.music.save(&song).unwrap();
        let original = temp.path().join("original.mp3");
        let tools = crate::media::Tools::discover();
        let status = crate::media::hidden_command(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=181",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "320k",
            ])
            .arg(&original)
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = fs::read(&original).unwrap();
        assert!(bytes.len() > 128 * 1024);
        let form = |clip: &str| {
            let mut form = format!("--demo\r\nContent-Disposition: form-data; name=\"csrf\"\r\n\r\n{}\r\n--demo\r\nContent-Disposition: form-data; name=\"clip\"\r\n\r\n{clip}\r\n--demo\r\nContent-Disposition: form-data; name=\"complete\"\r\n\r\ntrue\r\n--demo\r\nContent-Disposition: form-data; name=\"media\"; filename=\"../../../outside.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\n", app.csrf).into_bytes();
            form.extend_from_slice(&bytes);
            form.extend_from_slice(b"\r\n--demo--\r\n");
            form
        };
        let router = web::router(app.clone());
        let post_selection = |clip: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/music/{}/select", song.id))
                .header(header::HOST, "127.0.0.1:19531")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf={}&clip={clip}", app.csrf)))
                .unwrap()
        };
        assert_eq!(
            router
                .clone()
                .oneshot(post_selection(first))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        for _ in 0..2 {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/music/{}/import", song.id))
                        .header(header::HOST, "127.0.0.1:19531")
                        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=demo")
                        .body(Body::from(form(first)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
        }
        assert_eq!(
            router
                .clone()
                .oneshot(post_selection(second))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            router
                .clone()
                .oneshot(post_selection(first))
                .await
                .unwrap()
                .status(),
            StatusCode::SEE_OTHER
        );
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/music/{}/audio/{first}", song.id))
                    .header(header::HOST, "127.0.0.1:19531")
                    .header(header::RANGE, "bytes=1-4")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/mpeg");
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes 1-4/{}", bytes.len())
        );
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            &bytes[1..5]
        );
        let restarted = App::new(temp.path(), 19531).unwrap();
        let saved = restarted.music.load(&song.id).unwrap();
        assert_eq!(saved.selected.as_deref(), Some(first));
        assert_eq!(saved.stage, "selected");
        assert_eq!(
            saved.clips.iter().find(|c| c.id == second).unwrap().status,
            "awaiting_import"
        );
        assert_eq!(saved.audio.len(), 1);
        let path = music_import::verified_path(
            &restarted.music.root,
            &song.id,
            first,
            &saved.audio[first],
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(fs::read(original).unwrap(), bytes);
    }
    #[test]
    fn saved_draft_survives_restart_and_cannot_escape_project_directory() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(temp.path().into()).unwrap();
        let song = Song {
            id: Uuid::new_v4().to_string(),
            title: "私の一曲".into(),
            lyrics: "歌詞".into(),
            style: "rock".into(),
            instrumental: false,
            main_vocal: "character-one".into(),
            karaoke_delay_hours: 24,
            stage: "draft".into(),
            error: None,
            clips: Vec::new(),
            selected: None,
            audio: BTreeMap::new(),
            browser_task: None,
        };
        workspace.save(&song).unwrap();
        let restarted = Workspace::new(temp.path().into()).unwrap();
        let restored = restarted.load(&song.id).unwrap();
        assert_eq!(restored.lyrics, "歌詞");
        assert_eq!(restored.karaoke_delay_hours, 24);
        assert_eq!(restored.main_vocal, "character-one");
        assert!(restarted.path("../outside").is_err());
    }

    #[test]
    fn interrupted_preflight_can_retry_but_sent_request_stays_uncertain() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(temp.path().into()).unwrap();
        let mut song = Song {
            id: Uuid::new_v4().to_string(),
            title: "試験".into(),
            lyrics: "歌詞".into(),
            style: "rock".into(),
            instrumental: false,
            main_vocal: String::new(),
            karaoke_delay_hours: 24,
            stage: "starting".into(),
            error: None,
            clips: Vec::new(),
            selected: None,
            audio: BTreeMap::new(),
            browser_task: None,
        };
        workspace.save(&song).unwrap();
        let first_id = song.id.clone();
        song.id = Uuid::new_v4().to_string();
        song.stage = "requested".into();
        workspace.save(&song).unwrap();
        let restarted = Workspace::new(temp.path().into()).unwrap();
        assert_eq!(restarted.load(&first_id).unwrap().stage, "blocked");
        assert_eq!(restarted.load(&song.id).unwrap().stage, "unknown");
    }

    #[test]
    fn refresh_update_preserves_selection_made_after_network_read_started() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(temp.path().into()).unwrap();
        let clip = Clip {
            id: "00000000-0000-4000-8000-000000000001".into(),
            title: "候補".into(),
            status: "complete".into(),
            audio_url: String::new(),
        };
        let song = Song {
            id: Uuid::new_v4().to_string(),
            title: "試験".into(),
            lyrics: "歌詞".into(),
            style: "rock".into(),
            instrumental: false,
            main_vocal: String::new(),
            karaoke_delay_hours: 24,
            stage: "ready".into(),
            error: None,
            clips: vec![clip.clone()],
            selected: None,
            audio: BTreeMap::new(),
            browser_task: None,
        };
        workspace.save(&song).unwrap();
        let old = workspace.load(&song.id).unwrap();
        workspace
            .update(&song.id, |current| {
                current.selected = Some(clip.id.clone());
                current.stage = "selected".into();
                Ok(())
            })
            .unwrap();
        workspace
            .update(&old.id, |current| {
                current.clips = old.clips;
                current.error = Some("接続の確認が必要です。".into());
                Ok(())
            })
            .unwrap();
        let saved = workspace.load(&song.id).unwrap();
        assert_eq!(saved.selected, Some(clip.id));
        assert_eq!(saved.stage, "selected");
    }
}
