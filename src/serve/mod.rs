//! serve mode: optional self-hosted webhook service (spec 10)
//!
//! Users get hoverstare[bot] reviews with zero configuration after installing
//! the GitHub App.

pub mod auth;
pub mod job;
pub mod webhook;

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use dashmap::DashMap;
use secrecy::SecretString;

use crate::config::Config;
use crate::serve::auth::AppAuth;
use crate::serve::webhook::HookEvent;

/// serve shared state
pub struct AppState {
    pub cfg: Config,
    pub auth: Arc<AppAuth>,
    pub webhook_secret: String,
    pub job_semaphore: tokio::sync::Semaphore,
    pr_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl AppState {
    /// Serialize executions for the same PR (spec 10)
    pub fn pr_lock(&self, repo: String, pr: u64) -> Arc<tokio::sync::Mutex<()>> {
        self.pr_locks
            .entry(format!("{repo}#{pr}"))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

/// serve configuration (all from env, spec 10)
pub struct ServeConfig {
    pub app_id: String,
    pub private_key_pem: String,
    pub webhook_secret: String,
    pub port: u16,
    pub max_jobs: usize,
}

impl ServeConfig {
    pub fn from_env(port: u16) -> anyhow::Result<ServeConfig> {
        let app_id = std::env::var("HOVERSTARE_APP_ID")
            .map_err(|_| anyhow::anyhow!("missing HOVERSTARE_APP_ID"))?;
        let pem_path = std::env::var("HOVERSTARE_APP_PRIVATE_KEY_PATH")
            .map_err(|_| anyhow::anyhow!("missing HOVERSTARE_APP_PRIVATE_KEY_PATH"))?;
        let private_key_pem = std::fs::read_to_string(&pem_path)
            .map_err(|e| anyhow::anyhow!("failed to read private key ({pem_path}): {e}"))?;
        let webhook_secret = std::env::var("HOVERSTARE_WEBHOOK_SECRET")
            .map_err(|_| anyhow::anyhow!("missing HOVERSTARE_WEBHOOK_SECRET"))?;
        let max_jobs = std::env::var("HOVERSTARE_SERVE_MAX_JOBS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        Ok(ServeConfig {
            app_id,
            private_key_pem,
            webhook_secret,
            port,
            max_jobs,
        })
    }
}

pub async fn run(port: u16) -> anyhow::Result<()> {
    let serve_cfg = ServeConfig::from_env(port)?;
    let cfg = Config::load()?;
    let auth = AppAuth::new(serve_cfg.app_id, &serve_cfg.private_key_pem)?;
    let state = Arc::new(AppState {
        cfg,
        auth,
        webhook_secret: serve_cfg.webhook_secret,
        job_semaphore: tokio::sync::Semaphore::new(serve_cfg.max_jobs),
        pr_locks: DashMap::new(),
    });

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("hoverstare serve listening on :{port} (/webhook, /healthz)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, &'static str) {
    // Verify the signature before parsing (spec 10)
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !webhook::verify_signature(&state.webhook_secret, &body, signature) {
        tracing::warn!("webhook signature verification failed");
        return (StatusCode::UNAUTHORIZED, "bad signature");
    }

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad json"),
    };

    match webhook::parse_event(event_type, &payload) {
        HookEvent::Review(ev) => {
            tracing::info!("webhook review: {}#{}", ev.repo, ev.pr_number);
            tokio::spawn(job::run_review_job(state, ev));
            (StatusCode::OK, "queued")
        }
        HookEvent::Mention(ev) => {
            tracing::info!(
                "webhook mention: {}#{}",
                ev.mention.repo,
                ev.mention.pr_number
            );
            tokio::spawn(job::run_mention_job(state, ev));
            (StatusCode::OK, "queued")
        }
        HookEvent::Ignored => (StatusCode::OK, "ignored"),
    }
}

// Keep a reference to SecretString in case it is injected directly in the future
#[allow(dead_code)]
fn _assert_send(_: SecretString) {}

// Make the HashMap type count as used in this file (can be removed now that
// DashMap replaced it)
#[allow(dead_code)]
type _Unused = HashMap<(), ()>;
