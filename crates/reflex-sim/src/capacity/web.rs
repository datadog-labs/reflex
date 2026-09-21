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
            "/capacity",
            get(|| async { Html(include_str!("index.html")) }),
        )
        .route(
            "/capacity.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("app.css"),
                )
            }),
        )
        .route(
            "/capacity.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("app.js"),
                )
            }),
        )
        .route(
            "/api/capacity/state",
            get(|State(s): State<Shared>| async move { Json(s.lock().await.view()) }),
        )
        .route(
            "/api/capacity/export",
            get(|State(s): State<Shared>| async move {
                (
                    [(
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=reflex-capacity.json",
                    )],
                    Json(s.lock().await.export()),
                )
            }),
        )
        .route("/api/capacity/command", post(command))
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
