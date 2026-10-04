use crate::{
    suno::{BrowserSession, Clip},
    web::{self, App},
};
use anyhow::{Result, ensure};
use axum::{
    Form, Router,
    extract::{Path as WebPath, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tokio::sync::Semaphore;
use uuid::Uuid;

pub struct Workspace {
    root: PathBuf,
    pub session: BrowserSession,
    busy: std::sync::Arc<Semaphore>,
    records: std::sync::Mutex<()>,
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

impl Workspace {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("projects"))?;
        let workspace = Self {
            session: BrowserSession::new(root.join("suno-browser")),
            root,
            busy: std::sync::Arc::new(Semaphore::new(1)),
            records: std::sync::Mutex::new(()),
        };
        for mut song in workspace.list() {
            if song.stage == "starting" {
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
        let uuid = Uuid::parse_str(id)?;
        ensure!(uuid.to_string() == id, "曲が見つかりません。");
        Ok(self.root.join("projects").join(format!("{id}.json")))
    }
    fn load(&self, id: &str) -> Result<Song> {
        let song: Song = serde_json::from_slice(&fs::read(self.path(id)?)?)?;
        ensure!(song.id == id, "曲が見つかりません。");
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
    <form method="post" action="/music/connect"><input type="hidden" name="csrf" value="{}"><button type="submit">Sunoに接続</button></form><p class="hint">初回は専用のChrome画面でSunoにログインします。パスワードや認証情報のコピーは不要です。本人確認が出たら、その画面で操作します。</p>
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
        "ready" => "候補が完成",
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
    match app.music.session.open() {
        Ok(()) => Redirect::to("/music").into_response(),
        Err(error) => fail(error),
    }
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
        if !clip.audio_url.is_empty() {
            body.push_str(&format!(
                "<audio controls preload=\"none\" src=\"{}\"></audio>",
                web::escape(&clip.audio_url)
            ));
        }
        body.push_str(&format!("<a href=\"https://suno.com/song/{}\" target=\"_blank\" rel=\"noopener\">Sunoで開く・ダウンロードする</a>",clip.id));
        if clip.status == "complete" {
            body.push_str(&format!("<form method=\"post\" action=\"/music/{id}/select\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"clip\" value=\"{}\"><button class=\"secondary\" type=\"submit\">{}</button></form>",app.csrf,clip.id,if song.selected.as_deref()==Some(&clip.id){"この曲を採用中"}else{"この曲を採用する"}));
        }
        body.push_str("</div>");
    }
    if ["draft", "blocked"].contains(&song.stage.as_str()) {
        body.push_str(&format!("<form method=\"post\" action=\"/music/{id}/generate\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button type=\"submit\">Sunoで曲を生成する</button><p class=\"hint\">この歌詞と曲の雰囲気をSunoへ送り、契約の生成枠を使います。</p></form>",app.csrf));
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
        return fail("別の曲を生成中です。少し待ってから操作してください。");
    };
    let mut song = match app.music.load(&id) {
        Ok(song) => song,
        Err(error) => return fail(error),
    };
    if !["draft", "blocked"].contains(&song.stage.as_str()) {
        return fail(
            "この曲は生成要求を送っています。結果を更新するか、Sunoの履歴を確認してください。",
        );
    }
    song.stage = "starting".into();
    song.error = None;
    if let Err(error) = app.music.save(&song) {
        return fail(error);
    }
    let mut requested = song.clone();
    requested.stage = "requested".into();
    let result = app
        .music
        .session
        .generate(
            &song.title,
            &song.lyrics,
            &song.style,
            song.instrumental,
            || app.music.save(&requested),
        )
        .await;
    match result {
        Ok(clips) => {
            song.clips = clips;
            song.stage = if song.clips.iter().all(|clip| clip.status == "complete") {
                "ready"
            } else {
                "requested"
            }
            .into();
        }
        Err(error) => {
            let sent = app
                .music
                .load(&id)
                .is_ok_and(|saved| saved.stage == "requested");
            song.stage = if sent && !error.is::<crate::suno::Rejected>() {
                "unknown"
            } else {
                "blocked"
            }
            .into();
            song.error = Some(error.to_string());
        }
    }
    if let Err(error) = app.music.save(&song) {
        return fail(error);
    }
    Redirect::to(&format!("/music/{id}")).into_response()
}
async fn refresh(
    State(app): State<App>,
    WebPath(id): WebPath<String>,
    Form(token): Form<Token>,
) -> Response {
    if token.csrf != app.csrf {
        return fail("音楽制作の画面を開き直してください。");
    }
    let song = match app.music.load(&id) {
        Ok(song) => song,
        Err(error) => return fail(error),
    };
    let ids = song
        .clips
        .iter()
        .map(|clip| clip.id.clone())
        .collect::<Vec<_>>();
    let result = app.music.session.refresh(&ids).await;
    let saved = app.music.update(&id, |song| {
        match result {
            Ok(clips) => {
                if ensure_same_ids(&ids, &clips).is_err() {
                    anyhow::bail!("Sunoから別の曲の応答を受信しました。");
                }
                song.clips = clips;
                song.error = None;
                if song.selected.is_none() {
                    song.stage = if song.clips.iter().all(|clip| clip.status == "complete") {
                        "ready"
                    } else {
                        "requested"
                    }
                    .into();
                }
            }
            Err(error) => {
                song.error = Some(error.to_string());
            }
        }
        Ok(())
    });
    if let Err(error) = saved {
        return fail(error);
    }
    Redirect::to(&format!("/music/{id}")).into_response()
}
fn ensure_same_ids(ids: &[String], clips: &[Clip]) -> Result<()> {
    ensure!(
        ids.len() == clips.len() && clips.iter().all(|clip| ids.contains(&clip.id)),
        "別の曲の応答を受信しました。"
    );
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
    let saved = app.music.update(&id, |song| {
        if !song
            .clips
            .iter()
            .any(|clip| clip.id == token.clip && clip.status == "complete")
        {
            anyhow::bail!("完成した曲の候補を選んでください。");
        }
        song.selected = Some(token.clip);
        song.stage = "selected".into();
        Ok(())
    });
    if let Err(error) = saved {
        return fail(error);
    }
    Redirect::to(&format!("/music/{id}")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
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
