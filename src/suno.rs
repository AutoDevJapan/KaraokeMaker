//! A native adapter for the unofficial Suno endpoints. Authentication stays in
//! an app-owned Chrome profile; no keys, credential files, or browser database
//! are read by this program. Session values never leave the Suno request path.
use anyhow::{Context, Result, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Child, sync::Mutex, time::Duration};

const BASE: &str = "https://studio-api.prod.suno.com";

pub struct BrowserSession {
    profile: PathBuf,
    running: Mutex<Option<(Child, u16)>>,
}
struct Session {
    bearer: String,
    cookie: String,
}

#[derive(Debug)]
pub struct Rejected;
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sunoが生成要求を拒否しました。契約・利用枠・モデルの対応を確認してから再操作してください。")
    }
}
impl std::error::Error for Rejected {}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Clip {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub audio_url: String,
}

impl BrowserSession {
    pub fn new(profile: PathBuf) -> Self {
        Self {
            profile,
            running: Mutex::new(None),
        }
    }
    /// The user opens this isolated browser and signs in directly with Suno.
    /// Existing personal Chrome profiles are never attached or inspected.
    pub fn open(&self) -> Result<()> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((child, _)) = running.as_mut()
            && child.try_wait()?.is_none()
        {
            return Ok(());
        }
        let mut locations = Vec::new();
        for name in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(name) {
                locations.push(PathBuf::from(base).join("Google/Chrome/Application/chrome.exe"));
            }
        }
        let executable = locations
            .into_iter()
            .find(|path| path.is_file())
            .context("Google Chromeが見つかりませんでした。")?;
        std::fs::create_dir_all(&self.profile)?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let child = crate::media::hidden_command(executable)
            .arg(format!("--user-data-dir={}", self.profile.display()))
            .arg(format!("--remote-debugging-port={port}"))
            .args([
                "--remote-debugging-address=127.0.0.1",
                "--disable-background-mode",
                "--no-first-run",
                "--no-default-browser-check",
                "--new-window",
                "https://suno.com/create",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Sunoのログイン画面を開けませんでした。")?;
        *running = Some((child, port));
        Ok(())
    }
    async fn session(&self) -> Result<Session> {
        let port = {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            let (child, port) = running
                .as_mut()
                .context("「Sunoに接続」を押し、開いた画面でログインしてください。")?;
            ensure!(
                child.try_wait()?.is_none(),
                "Sunoの接続画面が閉じています。「Sunoに接続」を押してください。"
            );
            *port
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?;
        let tabs: Value = client
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Sunoの接続画面を準備しています。少し待ってもう一度操作してください。"
                )
            })?
            .json()
            .await?;
        let target = tabs
            .as_array()
            .context("Sunoの画面が見つかりません。")?
            .iter()
            .find(|tab| {
                tab["type"] == "page"
                    && tab["url"].as_str().is_some_and(|url| {
                        url == "https://suno.com/" || url.starts_with("https://suno.com/")
                    })
            })
            .context("Sunoへのログインを終えて、生成画面を開いてください。")?;
        let url = target["webSocketDebuggerUrl"]
            .as_str()
            .context("Sunoの接続画面を開き直してください。")?;
        let parsed = reqwest::Url::parse(url)?;
        ensure!(
            parsed.scheme() == "ws"
                && parsed.host_str() == Some("127.0.0.1")
                && parsed.port() == Some(port),
            "Sunoの接続先を確認できませんでした。"
        );
        let cookies=tokio::time::timeout(Duration::from_secs(5),async {
            let (mut socket,_)=tokio_tungstenite::connect_async(url).await?;
            socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"id":1,"method":"Network.getCookies","params":{"urls":["https://suno.com/","https://studio-api.prod.suno.com/"]}}).to_string().into())).await?;
            while let Some(message)=socket.next().await {
                let message=message?;
                if let Ok(text)=message.to_text() {
                    let response:Value=serde_json::from_str(text)?;
                    if response["id"]==1 {return Ok::<Value,anyhow::Error>(response["result"]["cookies"].clone());}
                }
            }
            anyhow::bail!("session unavailable")
        }).await.map_err(|_|anyhow::anyhow!("Sunoの接続確認に時間がかかっています。"))?
            .map_err(|_|anyhow::anyhow!("Sunoの接続を確認できませんでした。接続画面を開き直してください。"))?;
        let cookies = cookies.as_array().context("Sunoにログインしてください。")?;
        let bearer = cookies
            .iter()
            .find(|cookie| cookie["name"] == "__session")
            .and_then(|c| c["value"].as_str())
            .filter(|value| !value.is_empty())
            .context("Sunoへのログインが必要です。")?
            .to_string();
        let cookie = cookies
            .iter()
            .filter_map(|entry| {
                let name = entry["name"].as_str()?;
                let value = entry["value"].as_str()?;
                if name.contains([';', '\r', '\n']) || value.contains([';', '\r', '\n']) {
                    return None;
                }
                Some(format!("{name}={value}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        Ok(Session { bearer, cookie })
    }
    fn client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?)
    }
    fn authenticated(
        client: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
        session: &Session,
    ) -> reqwest::RequestBuilder {
        client
            .request(method, format!("{BASE}{path}"))
            .bearer_auth(&session.bearer)
            .header(reqwest::header::COOKIE, &session.cookie)
            .header(reqwest::header::ORIGIN, "https://suno.com")
            .header(reqwest::header::REFERER, "https://suno.com/create")
    }
    pub async fn generate(
        &self,
        title: &str,
        lyrics: &str,
        style: &str,
        instrumental: bool,
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<Clip>> {
        let session = self.session().await?;
        let client = Self::client()?;
        let check = Self::authenticated(&client, reqwest::Method::POST, "/api/c/check", &session)
            .json(&json!({"ctype":"generation"}))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Sunoに接続できませんでした。生成は開始していません。"))?;
        ensure!(
            check.status().is_success(),
            "Sunoのログイン・契約・利用枠を、接続画面で確認してください。生成は開始していません。"
        );
        let check: Value = check.json().await?;
        ensure!(
            check["required"] == false,
            "Sunoが本人確認を求めています。接続画面で操作してください。自動の生成は開始していません。"
        );
        let payload = generation_payload(title, lyrics, style, instrumental);
        before_submit()?;
        let response=Self::authenticated(&client,reqwest::Method::POST,"/api/generate/v2/",&session).json(&payload).send().await
            .map_err(|_|anyhow::anyhow!("生成要求の結果を確認できませんでした。重複生成を避けるため、Sunoの履歴を確認してください。"))?;
        if response.status().is_client_error()
            && response.status() != reqwest::StatusCode::REQUEST_TIMEOUT
        {
            return Err(Rejected.into());
        }
        ensure!(
            response.status().is_success(),
            "Sunoの生成要求の結果を確認できませんでした。Sunoの履歴を確認してください。"
        );
        let data: Value = response.json().await.map_err(|_| {
            anyhow::anyhow!("生成要求の結果を確認できませんでした。Sunoの履歴を確認してください。")
        })?;
        parse_clips(&data)
    }
    pub async fn refresh(&self, ids: &[String]) -> Result<Vec<Clip>> {
        ensure!(
            !ids.is_empty() && ids.iter().all(|id| uuid::Uuid::parse_str(id).is_ok()),
            "曲の識別子が不正です。"
        );
        let session = self.session().await?;
        let client = Self::client()?;
        let response = Self::authenticated(&client, reqwest::Method::GET, "/api/feed/", &session)
            .query(&[("ids", ids.join(","))])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Sunoの結果を取得できませんでした。"))?;
        ensure!(
            response.status().is_success(),
            "Sunoの結果を取得できませんでした。接続画面のログイン状態を確認してください。"
        );
        parse_clips(&response.json::<Value>().await?)
    }
    pub fn close(&self) {
        if let Some((mut child, _)) = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn generation_payload(title: &str, lyrics: &str, style: &str, instrumental: bool) -> Value {
    json!({"make_instrumental":instrumental,"mv":"chirp-crow","prompt":lyrics,"tags":style,"title":title,"negative_tags":"","generation_type":"TEXT","task":null,"token":null})
}
pub fn parse_clips(data: &Value) -> Result<Vec<Clip>> {
    let clips = data
        .as_array()
        .or_else(|| data["clips"].as_array())
        .context("Sunoの応答が変わりました。Sunoの履歴を確認してください。")?;
    ensure!(
        !clips.is_empty(),
        "Sunoの応答に曲がありません。Sunoの履歴を確認してください。"
    );
    clips
        .iter()
        .map(|clip| {
            let clip: Clip = serde_json::from_value(clip.clone())?;
            ensure!(
                uuid::Uuid::parse_str(&clip.id).is_ok(),
                "Sunoの曲を読み取れませんでした。"
            );
            if !clip.audio_url.is_empty() {
                let url = reqwest::Url::parse(&clip.audio_url)?;
                ensure!(
                    url.scheme() == "https"
                        && url.host_str().is_some_and(|host| host == "suno.com"
                            || host.ends_with(".suno.com")
                            || host.ends_with(".suno.ai")),
                    "Suno以外の音源URLを受信しました。"
                );
            }
            Ok(clip)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_preserves_lyrics_and_does_not_contain_authentication() {
        let payload = generation_payload("私の曲", "[Verse]\nあいうえお", "rock", false);
        assert_eq!(payload["prompt"], "[Verse]\nあいうえお");
        assert_eq!(payload["title"], "私の曲");
        assert_eq!(payload["make_instrumental"], false);
        assert!(payload.get("cookie").is_none());
        assert!(payload.get("Authorization").is_none());
    }
    #[test]
    fn clips_validate_ids_and_playback_urls() {
        let id = "00000000-0000-4000-8000-000000000001";
        assert_eq!(
            parse_clips(&json!({"clips":[{"id":id,"title":"曲","status":"submitted"}]})).unwrap()
                [0]
            .id,
            id
        );
        assert!(parse_clips(&json!([])).is_err());
        assert!(parse_clips(&json!([{"id":"../outside"}])).is_err());
        assert!(
            parse_clips(&json!([{"id":id,"audio_url":"https://attacker.test/track"}])).is_err()
        );
    }
}
