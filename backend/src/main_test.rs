use super::*;
use std::net::Ipv4Addr;
use std::sync::{Mutex, MutexGuard};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};

// Serialize env-mutating tests (RYU_CORE_PORT / RYU_EXT_TOKEN / GHOST_DATA_DIR
// are process-global).
static ENV_LOCK: Mutex<()> = Mutex::new(());
fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ── CoreCallback env parsing ──────────────────────────────────────────────

#[test]
fn core_callback_new_uses_defaults_without_env() {
    let _g = env_lock();
    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
    let cb = CoreCallback::new();
    assert_eq!(
        cb.core_base,
        format!("http://127.0.0.1:{DEFAULT_CORE_PORT}")
    );
    assert!(cb.ext_token.is_none());
}

#[test]
fn core_callback_new_reads_port_and_token_from_env() {
    let _g = env_lock();
    std::env::set_var("RYU_CORE_PORT", "  9123  ");
    std::env::set_var("RYU_EXT_TOKEN", "  tok123  ");
    let cb = CoreCallback::new();
    assert_eq!(cb.core_base, "http://127.0.0.1:9123");
    assert_eq!(cb.ext_token.as_deref(), Some("tok123"));
    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[test]
fn core_callback_new_ignores_empty_token() {
    let _g = env_lock();
    std::env::set_var("RYU_EXT_TOKEN", "   ");
    std::env::remove_var("RYU_CORE_PORT");
    let cb = CoreCallback::new();
    assert!(cb.ext_token.is_none());
    std::env::remove_var("RYU_EXT_TOKEN");
}

// ── CoreCallback host methods against a mock Core (loopback) ──────────────

#[derive(Clone)]
struct Captured {
    auth: Option<String>,
    plugin_id: Option<String>,
    path: String,
    body: Value,
}

#[derive(Clone)]
struct MockState {
    status: u16,
    body: Value,
    captured: Arc<Mutex<Option<Captured>>>,
}

async fn mock_handler(
    uri: Uri,
    headers: HeaderMap,
    State(state): State<MockState>,
    body: Bytes,
) -> Response {
    let hdr = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    *state.captured.lock().unwrap() = Some(Captured {
        auth: hdr("authorization"),
        plugin_id: hdr(HDR_PLUGIN_ID),
        path: uri.path().to_string(),
        body: parsed,
    });
    let st = StatusCode::from_u16(state.status).unwrap_or(StatusCode::OK);
    (st, Json(state.body)).into_response()
}

/// Spawn a mock Core on a random loopback port that answers every path with
/// `(status, body)` and records the last request. Returns the port + capture.
async fn spawn_mock_core(status: u16, body: Value) -> (u16, Arc<Mutex<Option<Captured>>>) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(Mutex::new(None));
    let state = MockState {
        status,
        body,
        captured: captured.clone(),
    };
    let app = Router::new().fallback(mock_handler).with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, captured)
}

#[tokio::test]
async fn callback_run_returns_raw_envelope_and_stamps_auth() {
    let _g = env_lock();
    let envelope = json!({ "content": [{ "type": "text", "text": "{\"ok\":1}" }] });
    let (port, captured) = spawn_mock_core(200, envelope.clone()).await;
    std::env::set_var("RYU_CORE_PORT", port.to_string());
    std::env::set_var("RYU_EXT_TOKEN", "testtok");

    let cb = CoreCallback::new();
    // call_ghost_run returns the RAW envelope verbatim (crate::run unwraps it).
    let got = cb
        .call_ghost_run("myrecipe", json!({ "n": 2 }))
        .await
        .unwrap();
    assert_eq!(got, envelope);

    let c = captured.lock().unwrap().clone().unwrap();
    // Security: the ext bearer + plugin-id headers are stamped on the hop.
    assert_eq!(c.auth.as_deref(), Some("Bearer testtok"));
    assert_eq!(c.plugin_id.as_deref(), Some(RECIPES_PLUGIN_ID));
    assert_eq!(c.path, "/api/host/capability/ghost.replay");
    assert_eq!(c.body["recipe"], json!("myrecipe"));
    assert_eq!(c.body["params"], json!({ "n": 2 }));

    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[tokio::test]
async fn callback_recorder_start_parses_and_hits_right_path() {
    let _g = env_lock();
    let payload = json!({ "started_at": "t0", "info": { "pid": 7 } });
    let (port, captured) = spawn_mock_core(200, payload).await;
    std::env::set_var("RYU_CORE_PORT", port.to_string());
    std::env::set_var("RYU_EXT_TOKEN", "tok");

    let cb = CoreCallback::new();
    let started = cb.recorder_start("demo").await.unwrap();
    assert_eq!(started.started_at, "t0");
    assert_eq!(started.info, json!({ "pid": 7 }));

    let c = captured.lock().unwrap().clone().unwrap();
    assert_eq!(c.path, "/api/host/capability/ghost.recordStart");
    assert_eq!(c.body["task"], json!("demo"));

    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[tokio::test]
async fn callback_recorder_status_none_deserializes() {
    let _g = env_lock();
    // Core returns `null` for "no active session".
    let (port, _captured) = spawn_mock_core(200, Value::Null).await;
    std::env::set_var("RYU_CORE_PORT", port.to_string());
    std::env::set_var("RYU_EXT_TOKEN", "tok");

    let cb = CoreCallback::new();
    let status = cb.recorder_status().await.unwrap();
    assert!(status.is_none());

    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[tokio::test]
async fn callback_recorder_stop_parses() {
    let _g = env_lock();
    let payload = json!({ "task": "t", "started_at": "t0", "payload": { "events": [] } });
    let (port, _captured) = spawn_mock_core(200, payload).await;
    std::env::set_var("RYU_CORE_PORT", port.to_string());
    std::env::set_var("RYU_EXT_TOKEN", "tok");

    let cb = CoreCallback::new();
    let stopped = cb.recorder_stop().await.unwrap();
    assert_eq!(stopped.task, "t");
    assert_eq!(stopped.started_at, "t0");

    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[tokio::test]
async fn callback_surfaces_core_error_body() {
    let _g = env_lock();
    let (port, _captured) = spawn_mock_core(500, json!({ "error": "kaboom" })).await;
    std::env::set_var("RYU_CORE_PORT", port.to_string());
    std::env::set_var("RYU_EXT_TOKEN", "tok");

    let cb = CoreCallback::new();
    let err = cb
        .call_ghost_run("r", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("kaboom"), "unexpected: {err}");

    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[tokio::test]
async fn callback_malformed_started_payload_errors() {
    let _g = env_lock();
    // Wrong shape for RecorderStarted → deserialize failure surfaced.
    let (port, _captured) = spawn_mock_core(200, json!("not an object")).await;
    std::env::set_var("RYU_CORE_PORT", port.to_string());
    std::env::set_var("RYU_EXT_TOKEN", "tok");

    let cb = CoreCallback::new();
    let err = cb.recorder_start("x").await.unwrap_err().to_string();
    assert!(
        err.contains("malformed RecorderStarted"),
        "unexpected: {err}"
    );

    std::env::remove_var("RYU_CORE_PORT");
    std::env::remove_var("RYU_EXT_TOKEN");
}

#[tokio::test]
async fn callback_post_is_fail_closed_without_token() {
    let _g = env_lock();
    std::env::remove_var("RYU_EXT_TOKEN");
    std::env::remove_var("RYU_CORE_PORT");
    let cb = CoreCallback::new();
    // No token configured → the callback refuses to send.
    let err = cb
        .post("/api/host/capability/ghost.replay", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no RYU_EXT_TOKEN"), "unexpected: {err}");
}

// ── Health probe ──────────────────────────────────────────────────────────

#[tokio::test]
async fn health_reports_ok_and_recipe_count() {
    let _g = env_lock();
    let base = std::env::temp_dir().join(format!("ryu-recipes-health-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("GHOST_DATA_DIR", &base);

    let resp = health().await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["recipeCount"], json!(0));

    std::env::remove_var("GHOST_DATA_DIR");
    let _ = std::fs::remove_dir_all(&base);
}
