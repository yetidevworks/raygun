use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle, time};
use tracing::{info, warn};

use crate::{protocol::RayRequest, state::AppState};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let bind_addr = std::env::var("RAYGUN_BIND")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 23_517)));

        Self { bind_addr }
    }
}

#[derive(Clone)]
struct HttpState {
    app_state: Arc<AppState>,
}

#[derive(Debug)]
pub struct ServerHandle {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<Result<(), std::io::Error>>>,
}

impl ServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn shutdown(mut self) -> Result<(), ServerError> {
        if let Some(tx) = self.shutdown.take() {
            if tx.send(()).is_err() {
                warn!("server shutdown signal receiver dropped");
            }
        }

        let mut join_handle = match self.join_handle.take() {
            Some(handle) => handle,
            None => return Ok(()),
        };

        tokio::select! {
            join_result = &mut join_handle => match join_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(ServerError::Io(error)),
                Err(error) => Err(ServerError::Join(error)),
            },
            _ = time::sleep(Duration::from_secs(2)) => {
                warn!("HTTP server shutdown timed out; aborting");
                join_handle.abort();
                Ok(())
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("server task failed to join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub async fn spawn(
    state: Arc<AppState>,
    config: ServerConfig,
) -> Result<ServerHandle, ServerError> {
    let listener = TcpListener::bind(config.bind_addr).await?;

    let http_state = HttpState {
        app_state: Arc::clone(&state),
    };

    let router = Router::new()
        .route("/", post(ingest))
        .route("/locks/:name", get(lock_exists))
        .route("/_availability_check", get(availability_check))
        .with_state(http_state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let addr = listener.local_addr()?;

    let server = axum::serve(listener, router.into_make_service()).with_graceful_shutdown(async {
        let _ = shutdown_rx.await;
    });

    let join_handle = tokio::spawn(async move {
        if let Err(error) = server.await {
            warn!(?error, "HTTP server terminated with error");
            Err(error)
        } else {
            Ok(())
        }
    });

    info!(%addr, "HTTP server listening");

    Ok(ServerHandle {
        addr,
        shutdown: Some(shutdown_tx),
        join_handle: Some(join_handle),
    })
}

async fn ingest(
    State(state): State<HttpState>,
    Json(request): Json<RayRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let response = match state.app_state.record_request(request).await {
        Some(event) => json!({
            "recorded": true,
            "event_id": event.id,
        }),
        None => json!({
            "recorded": false,
        }),
    };

    (StatusCode::ACCEPTED, Json(response))
}

#[derive(Debug, Deserialize)]
struct LockQuery {
    hostname: Option<String>,
    project_name: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct LockResponse {
    active: bool,
    #[serde(default)]
    stop_execution: bool,
}

async fn lock_exists(
    State(state): State<HttpState>,
    Path(name): Path<String>,
    Query(query): Query<LockQuery>,
) -> impl IntoResponse {
    let active = state
        .app_state
        .lock_exists(
            name.as_str(),
            query.hostname.as_deref(),
            query.project_name.as_deref(),
        )
        .await;

    (
        StatusCode::OK,
        Json(LockResponse {
            active,
            stop_execution: false,
        }),
    )
}

async fn availability_check() -> impl IntoResponse {
    StatusCode::NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn ingest_handler_records_payload() {
        let app_state = Arc::new(AppState::default());
        let http_state = HttpState {
            app_state: Arc::clone(&app_state),
        };

        let request = RayRequest {
            uuid: "demo".into(),
            payloads: vec![
                serde_json::from_value(json!({
                    "type": "log",
                    "content": { "values": ["hi"], "meta": [] }
                }))
                .unwrap(),
            ],
            meta: Default::default(),
        };

        let (status, Json(body)) = ingest(State(http_state), Json(request)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            body.get("recorded").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(
            body.get("event_id")
                .and_then(|value| value.as_str())
                .is_some()
        );
        assert_eq!(app_state.timeline_len().await, 1);
    }
}
