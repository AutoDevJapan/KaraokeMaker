//! Install this module in the existing Console, behind its Host/Origin guard.
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use std::{process::Command, time::Duration};
const BROKER: &str = "http://127.0.0.1:19531";
const EXE: &str = "D:/CC/Apps/KaraokeMaker/KaraokeMaker.exe";
const DATA: &str = "D:/CC/Apps/KaraokeMaker/data";
static PROCESS: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);
pub async fn handle(client: reqwest::Client, request: Request) -> Response {
    proxy(client, request, BROKER).await
}

async fn proxy(client: reqwest::Client, request: Request, broker: &str) -> Response {
    let path = request.uri().path();
    if path != "/music" && !path.starts_with("/music/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let health = client
        .get(format!("{broker}/api/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    match health {
        Ok(response) => {
            let Ok(value) = response.json::<serde_json::Value>().await else {
                return unavailable("音楽制作の接続先を確認できませんでした。");
            };
            if value["app"] != "KaraokeMaker" {
                return unavailable("音楽制作の接続先が別のアプリに使われています。");
            }
        }
        Err(_) => {
            if !std::path::Path::new(EXE).is_file() {
                return unavailable("KaraokeMakerがまだ導入されていません。");
            }
            let mut process = PROCESS.lock().unwrap_or_else(|e| e.into_inner());
            if process
                .as_mut()
                .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_none()))
            {
                return unavailable(
                    "音楽制作を起動しています。数秒後にこの画面を更新してください。",
                );
            }
            {
                let mut command = Command::new(EXE);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x08000000);
                }
                let launched = command
                    .args(["--data", DATA, "serve", "--port", "19531", "--no-open"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                match launched {
                    Ok(child) => *process = Some(child),
                    Err(_) => return unavailable("音楽制作を起動できませんでした。"),
                }
            }
            return unavailable("音楽制作を起動しています。数秒後にこの画面を更新してください。");
        }
    }
    let (parts, body) = request.into_parts();
    let is_head = parts.method == axum::http::Method::HEAD;
    let body = match to_bytes(body, 16 * 1024 * 1024 + 16 * 1024).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let target = format!(
        "{broker}{}",
        parts
            .uri
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/music")
    );
    let mut outbound = client
        .request(parts.method, target)
        .timeout(Duration::from_secs(90))
        .header(header::ORIGIN, broker)
        .body(body);
    if let Some(content_type) = parts.headers.get(header::CONTENT_TYPE) {
        outbound = outbound.header(header::CONTENT_TYPE, content_type.clone());
    }
    if let Some(range) = parts.headers.get(header::RANGE) {
        outbound = outbound.header(header::RANGE, range.clone());
    }
    let result = match outbound.send().await {
        Ok(result) => result,
        Err(_) => {
            return unavailable(
                "音楽制作の応答を確認できませんでした。更新して状態を確認してください。",
            );
        }
    };
    let status = result.status();
    let source_headers = result.headers().clone();
    let mut bytes = Vec::new();
    let mut result = result;
    loop {
        let chunk = match result.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return unavailable("音楽制作の応答を読み取れませんでした。"),
        };
        if bytes.len().saturating_add(chunk.len()) > 16 * 1024 * 1024 + 16 * 1024 {
            return unavailable("音楽制作の応答が大きすぎます。");
        }
        bytes.extend_from_slice(&chunk);
    }
    let bytes = match source_headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
    {
        Some(length) if !is_head && length != bytes.len() => {
            return unavailable("音楽制作の応答を読み取れませんでした。");
        }
        _ => bytes,
    };
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    for name in [
        header::CONTENT_TYPE,
        header::LOCATION,
        header::CONTENT_SECURITY_POLICY,
        header::CACHE_CONTROL,
        header::X_CONTENT_TYPE_OPTIONS,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
    ] {
        if let Some(value) = source_headers.get(&name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}
fn unavailable(message: &str) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE,axum::response::Html(format!("<!doctype html><html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>音楽制作</title><body style=\"font-family:system-ui;padding:32px\"><h1>音楽制作</h1><p>{message}</p><a href=\"/music\">画面を更新する</a><p><a href=\"/\">Consoleに戻る</a></p></body></html>"))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        routing::{get, post},
    };
    #[tokio::test]
    async fn bridge_preserves_large_upload_audio_and_byte_ranges() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let backend = Router::new()
            .route(
                "/api/health",
                get(|| async { axum::Json(serde_json::json!({"app":"KaraokeMaker"})) }),
            )
            .route(
                "/music/project/import",
                post(|request: Request| async move {
                    assert!(request.headers().get(header::COOKIE).is_none());
                    let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                    assert_eq!(bytes.len(), 200 * 1024);
                    assert!(bytes.iter().all(|b| *b == 7));
                    StatusCode::SEE_OTHER
                }),
            )
            .route(
                "/music/project/audio/clip",
                get(|request: Request| async move {
                    if let Some(range) = request.headers().get(header::RANGE) {
                        assert_eq!(range, "bytes=1-3");
                        (
                            StatusCode::PARTIAL_CONTENT,
                            [
                                (header::CONTENT_TYPE, "audio/mpeg"),
                                (header::CONTENT_RANGE, "bytes 1-3/3145728"),
                                (header::ACCEPT_RANGES, "bytes"),
                            ],
                            vec![7_u8; 3],
                        )
                            .into_response()
                    } else {
                        (
                            [
                                (header::CONTENT_TYPE, "audio/mpeg"),
                                (header::ACCEPT_RANGES, "bytes"),
                            ],
                            vec![7_u8; 3 * 1024 * 1024],
                        )
                            .into_response()
                    }
                }),
            );
        let worker = tokio::spawn(async move {
            axum::serve(listener, backend).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let upload = Request::builder()
            .method("POST")
            .uri("/music/project/import")
            .header(
                header::CONTENT_TYPE,
                "multipart/form-data; boundary=fixture",
            )
            .header(header::COOKIE, "dummy=fixture-only")
            .body(Body::from(vec![7_u8; 200 * 1024]))
            .unwrap();
        assert_eq!(
            proxy(client.clone(), upload, &base).await.status(),
            StatusCode::SEE_OTHER
        );
        let response = proxy(
            client.clone(),
            Request::builder()
                .uri("/music/project/audio/clip")
                .body(Body::empty())
                .unwrap(),
            &base,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(bytes.len(), 3 * 1024 * 1024);
        assert!(bytes.iter().all(|b| *b == 7));
        let response = proxy(
            client,
            Request::builder()
                .uri("/music/project/audio/clip")
                .header(header::RANGE, "bytes=1-3")
                .body(Body::empty())
                .unwrap(),
            &base,
        )
        .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            "bytes 1-3/3145728"
        );
        assert_eq!(
            to_bytes(response.into_body(), 4).await.unwrap().as_ref(),
            &[7_u8; 3]
        );
        worker.abort();
        let _ = worker.await;
    }
    #[tokio::test]
    async fn bridge_preserves_form_and_redirect_without_forwarding_credentials() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let backend = Router::new()
            .route(
                "/api/health",
                get(|| async { axum::Json(serde_json::json!({"app":"KaraokeMaker"})) }),
            )
            .route(
                "/music/drafts",
                post(|request: Request| async move {
                    assert!(request.headers().get(header::COOKIE).is_none());
                    assert!(request.headers().get(header::AUTHORIZATION).is_none());
                    assert_eq!(
                        request.headers()[header::CONTENT_TYPE],
                        "application/x-www-form-urlencoded"
                    );
                    let body = to_bytes(request.into_body(), 1024).await.unwrap();
                    assert_eq!(body, "csrf=demo&title=Song");
                    (
                        StatusCode::SEE_OTHER,
                        [(header::LOCATION, "/music/project")],
                        "saved",
                    )
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, backend).await.unwrap();
        });
        let request = Request::builder()
            .method("POST")
            .uri("/music/drafts")
            .header(header::COOKIE, "dummy=local-only")
            .header(header::AUTHORIZATION, "Bearer fixture-only")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("csrf=demo&title=Song"))
            .unwrap();
        let response = proxy(
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            request,
            &base,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/music/project");
        assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "saved");
        let forbidden = Request::builder()
            .uri("/api/agent/delegate")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            handle(reqwest::Client::new(), forbidden).await.status(),
            StatusCode::NOT_FOUND
        );
        task.abort();
        let _ = task.await;
    }
}
