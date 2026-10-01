// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::{Command, Session};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use tokio::sync::Mutex;
pub type Shared = Arc<Mutex<Session>>;
pub fn router(shared: Shared) -> Router {
    Router::new()
        .route(
            "/autoscaler",
            get(|| async { Html(include_str!("index.html")) }),
        )
        .route(
            "/autoscaler.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("app.css"),
                )
            }),
        )
        .route(
            "/autoscaler.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("app.js"),
                )
            }),
        )
        .route(
            "/autoscaler-ui.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../playground/autoscaler-ui.js"),
                )
            }),
        )
        .route(
            "/autoscaler-ui.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../playground/autoscaler-ui.css"),
                )
            }),
        )
        .route(
            "/api/autoscaler/state",
            get(|State(s): State<Shared>| async move { Json(s.lock().await.view()) }),
        )
        .route(
            "/api/autoscaler/export",
            get(|State(s): State<Shared>| async move {
                (
                    [(
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=reflex-autoscaler.json",
                    )],
                    Json(s.lock().await.export()),
                )
            }),
        )
        .route("/api/autoscaler/command", post(command))
        .with_state(shared)
}
async fn command(State(s): State<Shared>, Json(command): Json<Command>) -> Response {
    let mut session = s.lock().await;
    match session.command(command).await {
        Ok(()) => Json(session.view()).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}
/// The session's 50 ms wall clock. An engine error pauses the run and is shown on the page.
pub fn start_clock(shared: Shared) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut previous = std::time::Instant::now();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut session = shared.lock().await;
            let elapsed = previous.elapsed().as_millis() as u64;
            previous += std::time::Duration::from_millis(elapsed);
            // Datadog observes this run in wall-clock time, so simulated time must match it.
            let ms = if session.uses_datadog() { elapsed } else { 50 };
            if let Err(e) = session.tick(ms).await {
                let _ = session.command(Command::Pause).await;
                session.error = Some(e.to_string());
            }
        }
    })
}
