//! Delegate official-UI operations to the existing OAuth browser worker queue.
//! This module never owns Chrome, reads a browser session, or contacts Suno.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BrowserTask {
    pub id: String,
    pub todo_id: Option<u64>,
    pub phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Claimed,
    Unknown,
    Cancelled,
    Results,
}

pub fn worker_prompt(project: &str, task: &str) -> Result<String> {
    crate::music_import::canonical_id(project)?;
    crate::music_import::canonical_id(task)?;
    Ok(format!(
        r#"Read C:/Users/makhmeto/.codex/AGENTS.md first and the KaraokeMaker project instructions. This is one owner-requested music generation using existing Suno subscription credits, not implementation work. Use existing OAuth tools and supported browser tooling against the owner's ordinary signed-in Chrome. No Python, API keys, authentication files, cookies, browser databases, bearer/session values, separate profiles, raw private endpoints or custom browser protocol clients. Do not access client-projects.txt resources or protected OpenMausBot. No Discord reports, payment/upgrade/terms/security changes or publication.
Open a task-created ordinary-Chrome tab at http://127.0.0.1:10012/music/{project}/worker/{task}. If Console is blocked, do not bypass the block: report the visible symptom to the intake, leave this request unfinished, and close your tabs. Do not use a new direct backend route as an end-to-end success substitute.
The page has an immutable draft and a unique request. Only a successful press of 担当を取得する which visibly shows 送信操作の担当を取得済み gives this worker permission to submit. A second claim is always rejected. If the request is cancelled, unknown, already claimed, absent or completed, never press Suno Create. Never change the draft, reuse another request or edit project JSON.
After the successful claim, use the official Suno Create page and exactly the displayed title, lyrics, style and instrumental setting. Verify these visible values before pressing official Create ONCE. Consume only existing subscription credits. Do not accept new terms, purchase, retry, solve CAPTCHA or request a fresh login if ordinary Chrome is signed in. If any barrier occurs before submission, mark the request uncertain using the worker page and record the actual barrier; do not automatically retry.
After the single Create action, any interruption or ambiguity means the request remains uncertain. Do not submit again, including after a crash or restart. Recover only by observing official history and exact song URLs. One request may yield two candidates. Use the worker page's result form to record the real canonical song IDs; do not invent IDs or claim completion from source/mocks. Save only completed official MP3 downloads, then import via the candidate's native MP3 form with the exact ID. Never read arbitrary source paths or manually alter project JSON. Keep streaming/incomplete/unimported candidates unselectable. Stop playback, preserve downloads, and close only your task-created tabs.
Update the linked shared ToDo with confirmed versus untested results through supported CLI operations. No additional real generation is authorized by recovery. If interrupted, leave an accepted continuation whose sole job is history/result recovery. The final result starts 状態: 完了 only after real IDs, import and persisted state are verified; otherwise 状態: 継続中. Deliver results to the intake, never direct Discord.
"#
    ))
}

pub struct Broker {
    client: reqwest::Client,
    base: String,
}
impl Broker {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()?,
            base: "http://127.0.0.1:10012/api/agent".into(),
        })
    }
    async fn post(&self, operation: &str, body: Value) -> Result<Value> {
        let mut response = self.client.post(format!("{}/{operation}", self.base)).json(&body).send().await
            .map_err(|_| anyhow::anyhow!("担当の受付結果を確認できませんでした。曲は再生成せず、保存した要求の状態を確認してください。"))?;
        ensure!(
            response.status().is_success(),
            "担当の受付が完了していません。曲は再生成せず、保存した要求の状態を確認してください。"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= 128 * 1024,
                "担当の受付応答を確認できませんでした。"
            );
            bytes.extend_from_slice(&chunk);
        }
        let data: Value =
            serde_json::from_slice(&bytes).context("担当の受付応答を確認できませんでした。")?;
        ensure!(
            data["ok"] == true,
            "担当の受付が完了していません。曲は再生成せず、保存した要求の状態を確認してください。"
        );
        Ok(data["data"].clone())
    }
    pub async fn add_task(&self, project: &str, task: &str) -> Result<u64> {
        crate::music_import::canonical_id(project)?;
        crate::music_import::canonical_id(task)?;
        let data = self.post("todo/add", json!({"project":"KaraokeMaker", "title":"保存した歌詞から通常ChromeのSunoで曲候補を1回生成する", "note":format!("音楽制作画面で明示された要求です。担当取得後に1回だけ送信します。結果が不明なら履歴の確認だけを行います。下書き {project}、要求 {task}。操作画面 http://127.0.0.1:10012/music/{project}/worker/{task}")})).await?;
        data["item"]["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .context("担当の作業番号を確認できませんでした。")
    }
    pub async fn dispatch(&self, project: &str, task: &BrowserTask) -> Result<()> {
        ensure!(
            task.phase == Phase::Queued,
            "この要求をもう一度送信できません。"
        );
        let todo = task.todo_id.context(
            "担当の作業番号がありません。新しい曲は生成せず、受付の履歴を確認してください。",
        )?;
        let data = self.post("delegate", json!({"id":todo, "prompt":worker_prompt(project, &task.id)?, "request_id":format!("suno-browser:{project}:{}", task.id)})).await?;
        ensure!(
            data["accepted"] == true && data.get("suppressed_reason").is_none_or(|v| v.is_null()),
            "担当の受付を完了できませんでした。保存した要求の状態を確認してください。"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_is_bound_to_one_request_and_forbids_resubmission() {
        let project = "00000000-0000-4000-8000-000000000001";
        let task = "00000000-0000-4000-8000-000000000002";
        let prompt = worker_prompt(project, task).unwrap();
        assert!(prompt.contains(&format!("/music/{project}/worker/{task}")));
        assert!(prompt.contains("official Create ONCE"));
        assert!(prompt.contains("A second claim is always rejected"));
        assert!(worker_prompt("../outside", task).is_err());
    }
    #[tokio::test]
    async fn delegates_with_one_stable_identity_without_external_generation() {
        use axum::{Json, Router, routing::post};
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/agent", listener.local_addr().unwrap());
        let router = Router::new()
            .route(
                "/api/agent/todo/add",
                post(|| async { Json(json!({"ok":true,"data":{"item":{"id":123}}})) }),
            )
            .route(
                "/api/agent/delegate",
                post(move |Json(body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        seen.lock().unwrap().push(body);
                        Json(json!({"ok":true,"data":{"accepted":true}}))
                    }
                }),
            );
        let worker = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut broker = Broker::new().unwrap();
        broker.base = base;
        let project = "00000000-0000-4000-8000-000000000001";
        let mut task = BrowserTask {
            id: "00000000-0000-4000-8000-000000000002".into(),
            todo_id: Some(
                broker
                    .add_task(project, "00000000-0000-4000-8000-000000000002")
                    .await
                    .unwrap(),
            ),
            phase: Phase::Queued,
        };
        broker.dispatch(project, &task).await.unwrap();
        broker.dispatch(project, &task).await.unwrap();
        task.phase = Phase::Unknown;
        assert!(broker.dispatch(project, &task).await.is_err());
        let bodies = calls.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(bodies[0]["id"], 123);
        assert_eq!(
            bodies[0]["request_id"],
            "suno-browser:00000000-0000-4000-8000-000000000001:00000000-0000-4000-8000-000000000002"
        );
        drop(bodies);
        worker.abort();
        let _ = worker.await;
    }
}
