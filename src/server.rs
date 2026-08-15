//! The receiving daemon: HTTP, auth, mDNS, tunnel supervision.

use crate::auth::{self, Throttle};
use crate::config::{Config, COOKIE, SESSION_SECS};
use crate::discovery::{self, Peers};
use crate::net::{human_size, is_loopback, safe_name};
use crate::page;
use anyhow::Result;
use axum::body::Body;
use axum::extract::{
    ConnectInfo, DefaultBodyLimit, Multipart, Path as AxPath, Query as AxQuery, State,
};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::io::ReaderStream;

const RECENT_MAX: usize = 25;

#[derive(Debug, Clone, Serialize)]
pub struct Received {
    pub name: String,
    pub size: String,
    pub when: String,
}

pub struct App {
    pub cfg: RwLock<Config>,
    pub peers: Peers,
    pub tunnel_url: Arc<RwLock<Option<String>>>,
    pub throttle: Mutex<Throttle>,
    pub recent: RwLock<VecDeque<Received>>,
    pub counter: AtomicU64,
    pub tunnel_tx: mpsc::Sender<bool>,
    pub offers: crate::offers::Registry,
    /// Text put up for the other device to collect. Deliberately not saved to
    /// disk — a pasted snippet should not outlive the session.
    pub shared_text: RwLock<String>,
    /// Browsers currently past the PIN.
    pub sessions: crate::sessions::Registry,
}

impl App {
    async fn pin(&self) -> String {
        self.cfg.read().await.pin.clone()
    }
    async fn secret(&self) -> String {
        self.cfg.read().await.secret.clone()
    }
    async fn dir(&self) -> PathBuf {
        self.cfg.read().await.dir.clone()
    }

    /// Files this machine is offering outward. A real folder rather than a
    /// hidden store, so dragging something into it in the file manager is
    /// enough to put it on the phone.
    async fn outbox(&self) -> PathBuf {
        self.dir().await.join("Shared")
    }
}

// ---------------------------------------------------------------------------
// auth
// ---------------------------------------------------------------------------

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == COOKIE)
        .map(|(_, v)| v.trim().to_string())
}

/// A request is authorised by a signed cookie, or by the raw PIN header —
/// the header is what the iOS Shortcut and `curl` use.
async fn authed(app: &App, headers: &HeaderMap) -> bool {
    if let Some(token) = cookie_token(headers) {
        if auth::verify(&app.secret().await, &token) {
            return true;
        }
    }
    if let Some(sent) = headers.get("x-drop-pin").and_then(|v| v.to_str().ok()) {
        return auth::constant_eq(sent.trim(), &app.pin().await);
    }
    false
}

async fn guard(app: &App, headers: &HeaderMap) -> Result<(), Response> {
    if authed(app, headers).await {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorised"}))).into_response())
    }
}

/// Control endpoints are for the tray, which always runs on this machine.
fn local_only(addr: &SocketAddr) -> Result<(), Response> {
    if is_loopback(&addr.ip()) {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "local only").into_response())
    }
}

#[derive(Deserialize)]
struct AuthReq {
    pin: String,
    /// What the browser calls itself, so the desktop can say "iPhone
    /// connected" rather than an IP address.
    #[serde(default)]
    device: Option<String>,
}

async fn do_auth(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<AuthReq>,
) -> Response {
    let ip = addr.ip();

    if let Some(wait) = app.throttle.lock().await.locked_for(&ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": format!("Too many attempts. Wait {}s.", wait.as_secs())})),
        )
            .into_response();
    }

    if !auth::constant_eq(req.pin.trim(), &app.pin().await) {
        app.throttle.lock().await.record_failure(ip);
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Wrong PIN."})),
        )
            .into_response();
    }

    app.throttle.lock().await.record_success(&ip);

    // Connected as of now — the PIN is what counts, not sending anything.
    let fresh = app
        .sessions
        .open(&ip.to_string(), req.device.clone(), auth::now())
        .await;
    if fresh {
        let who = req.device.as_deref().unwrap_or("A device");
        println!("  + {who} connected from {ip}");
    }

    let token = auth::issue(&app.secret().await, SESSION_SECS);
    // No `Secure`: on the LAN this is plain HTTP and the cookie would be dropped.
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={SESSION_SECS}"
    );

    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(json!({"ok": true})),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// pages and transfers
// ---------------------------------------------------------------------------

async fn index(State(app): State<Arc<App>>) -> Response {
    let cfg = app.cfg.read().await;
    let body = page::render(
        &cfg.name,
        &format!("{}:{}", crate::net::lan_ip(), cfg.port),
        &cfg.dir.display().to_string(),
    );
    // The page ships inside the binary, so an upgrade changes it. Safari will
    // otherwise keep running the version it first saw.
    (
        [(header::CACHE_CONTROL, "no-store, must-revalidate")],
        Html(body),
    )
        .into_response()
}

/// Everything in the Drop folder, newest first.
async fn list_files(dir: &std::path::Path) -> Vec<Received> {
    let mut files = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push((
                mtime,
                Received {
                    name,
                    size: human_size(meta.len()),
                    when: clock(mtime),
                },
            ));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().map(|(_, f)| f).take(60).collect()
}

async fn state(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }

    // The page polls this every couple of seconds, which is what keeps the
    // desktop's "connected" list honest without a websocket.
    let now = auth::now();
    app.sessions
        .touch(&addr.ip().to_string(), device_header(&headers), now)
        .await;
    app.sessions.sweep(now).await;

    let files = list_files(&app.dir().await).await;
    let peers: Vec<_> = app.peers.read().await.values().cloned().collect();
    let shared = list_files(&app.outbox().await).await;
    let text = app.shared_text.read().await.clone();
    Json(json!({
        "device": app.cfg.read().await.name,
        "files": files,
        "peers": peers,
        "shared": shared,
        "text": text,
    }))
    .into_response()
}

/// A snippet is meant to be a paste, not a file transfer by another name.
const MAX_TEXT: usize = 64 * 1024;

#[derive(Deserialize)]
struct TextIn {
    text: String,
    #[serde(default)]
    device: Option<String>,
}

#[derive(Deserialize)]
struct ShareIn {
    paths: Vec<String>,
}

/// Collect a file this machine is offering. Same containment check as
/// `download`, against the outbox instead of the Drop folder.
async fn collect(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    AxPath(fname): AxPath<String>,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }
    serve_from(&app.outbox().await, &fname).await
}

/// Take a pasted snippet from the other device. It goes through the same
/// accept prompt as a file, so nothing arrives unannounced.
async fn receive_text(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(req): Json<TextIn>,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }

    let body = req.text.trim_end_matches('\n').to_string();
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "empty"}))).into_response();
    }
    if body.len() > MAX_TEXT {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": "too long"})),
        )
            .into_response();
    }

    let device = req.device.unwrap_or_else(|| "A device".into());
    let dir = app.dir().await;
    let stage = dir.join(".staging").join(crate::config::random_hex(8));
    if tokio::fs::create_dir_all(&stage).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "cannot stage").into_response();
    }

    let name = format!("text-{}.txt", crate::auth::now());
    if tokio::fs::write(stage.join(&name), &body).await.is_err() {
        let _ = tokio::fs::remove_dir_all(&stage).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "cannot stage").into_response();
    }

    let file = crate::offers::OfferFile {
        name: name.clone(),
        size: body.len() as u64,
    };

    // With approval turned off it still has to land, just without the prompt.
    if !app.cfg.read().await.require_approval {
        let dest = {
            let cfg = app.cfg.read().await;
            if cfg.auto_move {
                cfg.move_target.clone()
            } else {
                cfg.dir.clone()
            }
        };
        let landed = relocate(&stage.join(&name), &dest).await.is_ok();
        let _ = tokio::fs::remove_dir_all(&stage).await;
        if !landed {
            return (StatusCode::INTERNAL_SERVER_ERROR, "cannot save").into_response();
        }
        to_clipboard(&body);
        note_received(&app, &name, body.len() as u64).await;
        println!("  <- text ({} bytes) from {device}", body.len());
        return Json(json!({"status": "saved", "name": name})).into_response();
    }

    let offer = app
        .offers
        .create_text(device, file, stage, body.clone())
        .await;
    println!("  ? {} offers a text snippet", offer.device);
    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "pending_approval", "offer": offer.id})),
    )
        .into_response()
}

/// Put a snippet up for the other device to collect.
async fn share_text(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<TextIn>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let body = if req.text.len() > MAX_TEXT {
        req.text[..MAX_TEXT].to_string()
    } else {
        req.text
    };
    *app.shared_text.write().await = body.clone();
    Json(json!({"ok": true, "len": body.len()})).into_response()
}

/// Copy files into the outbox so the other device can collect them.
async fn share_files(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<ShareIn>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    copy_into_outbox(&app, req.paths).await
}

async fn copy_into_outbox(app: &Arc<App>, paths: Vec<String>) -> Response {
    let outbox = app.outbox().await;
    if tokio::fs::create_dir_all(&outbox).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "cannot create outbox").into_response();
    }

    let mut added = Vec::new();
    for raw in paths {
        let from = std::path::PathBuf::from(&raw);
        let Some(base) = from.file_name() else {
            continue;
        };
        if !from.is_file() {
            continue;
        }
        // Copy rather than move: sharing something should not take it out of
        // the folder the user keeps it in.
        let to = crate::net::safe_name(&base.to_string_lossy(), &outbox);
        if tokio::fs::copy(&from, &to).await.is_ok() {
            added.push(to.file_name().unwrap_or_default().to_string_lossy().to_string());
        }
    }
    println!("  -> sharing {} file(s)", added.len());
    Json(json!({"shared": added})).into_response()
}

/// The same two actions as the tray's, for the Drop window — which is a web
/// page and cannot open a file dialog or read the clipboard itself.
async fn share_files_picker(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }

    // zenity blocks until the user is done, so it cannot run on the runtime.
    let Ok(Some(paths)) = tokio::task::spawn_blocking(crate::tray::pick_files).await else {
        return Json(json!({"shared": Vec::<String>::new()})).into_response();
    };
    copy_into_outbox(&app, paths).await
}

async fn share_clipboard(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }

    let Ok(Some(text)) = tokio::task::spawn_blocking(crate::tray::clipboard).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "no clipboard here"})),
        )
            .into_response();
    };
    if text.trim().is_empty() {
        return Json(json!({"ok": false, "len": 0})).into_response();
    }
    let text = if text.len() > MAX_TEXT {
        text[..MAX_TEXT].to_string()
    } else {
        text
    };
    let len = text.len();
    *app.shared_text.write().await = text;
    Json(json!({"ok": true, "len": len})).into_response()
}

/// Stop offering everything: empty the outbox and drop the snippet.
async fn unshare(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let outbox = app.outbox().await;
    let mut removed = 0;
    if let Ok(mut rd) = tokio::fs::read_dir(&outbox).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if entry.metadata().await.map(|m| m.is_file()).unwrap_or(false)
                && tokio::fs::remove_file(entry.path()).await.is_ok()
            {
                removed += 1;
            }
        }
    }
    app.shared_text.write().await.clear();
    Json(json!({"removed": removed})).into_response()
}

/// Put a snippet on the desktop clipboard. Best effort: wl-copy is only a
/// recommended dependency, and a headless session has no clipboard at all.
fn to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    // wl-copy forks a server to own the selection; do not block on it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Seconds-since-epoch to local `HH:MM`, without pulling in a date library.
fn clock(epoch: u64) -> String {
    let offset = local_offset_secs();
    let local = epoch as i64 + offset;
    let day = local.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

/// Read the UTC offset once from /etc/localtime via the TZ-aware `date`
/// fallback: compare `localtime` and `gmtime` is not available in std, so we
/// shell out only if the cheap env var is missing.
fn local_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                let s = s.trim();
                let sign = if s.starts_with('-') { -1 } else { 1 };
                let digits = s.trim_start_matches(['+', '-']);
                let h: i64 = digits.get(0..2)?.parse().ok()?;
                let m: i64 = digits.get(2..4)?.parse().ok()?;
                Some(sign * (h * 3600 + m * 60))
            })
            .unwrap_or(0)
    })
}

/// The name a client claims for itself, if it sets `X-Drop-Device`.
fn device_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-drop-device")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(64).collect())
}

/// Who is sending. Senders that can say so set `X-Drop-Device`.
fn sender_name(headers: &HeaderMap, addr: &SocketAddr) -> String {
    device_header(headers).unwrap_or_else(|| format!("Device at {}", addr.ip()))
}

#[derive(Deserialize)]
struct OfferReq {
    #[serde(default)]
    device: Option<String>,
    files: Vec<crate::offers::OfferFile>,
}

/// Announce an intent to send. Nothing is transferred yet.
async fn create_offer(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<OfferReq>,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }
    if req.files.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "no files"}))).into_response();
    }

    let device = req
        .device
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| sender_name(&headers, &addr));

    let offer = app.offers.create(device, req.files, None).await;
    println!(
        "  ?  {} wants to send {} file(s)  [{}]",
        offer.device,
        offer.files.len(),
        offer.id
    );
    (StatusCode::CREATED, Json(json!(offer))).into_response()
}

/// Sender polls this until the verdict is in.
async fn offer_status(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }
    match app.offers.get(&id).await {
        Some(offer) => Json(json!(offer)).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "no such offer"}))).into_response(),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(default)]
    offer: Option<String>,
}

async fn upload(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxQuery(q): AxQuery<UploadQuery>,
    multipart: Multipart,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }

    let (dir, auto_move, target, require_approval) = {
        let cfg = app.cfg.read().await;
        (
            cfg.dir.clone(),
            cfg.auto_move,
            cfg.move_target.clone(),
            cfg.require_approval,
        )
    };
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Path 1: the sender negotiated first and was accepted.
    if let Some(id) = q.offer {
        return match app.offers.get(&id).await {
            None => (StatusCode::NOT_FOUND, Json(json!({"error": "no such offer"}))).into_response(),
            Some(o) if o.status != crate::offers::Status::Accepted => (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "offer not accepted", "status": o.status})),
            )
                .into_response(),
            Some(_) => {
                let res = drain(&app, multipart, &dir, auto_move, target).await;
                app.offers.set_status(&id, crate::offers::Status::Complete).await;
                res
            }
        };
    }

    // Path 2: approval is off — behave as before.
    if !require_approval {
        return drain(&app, multipart, &dir, auto_move, target).await;
    }

    // Path 3: a one-shot sender (the iOS Shortcut). Take the bytes, but park
    // them out of sight until the verdict.
    let stage = dir.join(".staging").join(crate::config::random_hex(8));
    if let Err(e) = tokio::fs::create_dir_all(&stage).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let staged = match stream_to(multipart, &stage).await {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&stage).await;
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };
    if staged.is_empty() {
        let _ = tokio::fs::remove_dir_all(&stage).await;
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "no files"}))).into_response();
    }

    let device = sender_name(&headers, &addr);
    let offer = app.offers.create(device, staged, Some(stage)).await;
    println!(
        "  ?  {} sent {} file(s), waiting for approval  [{}]",
        offer.device,
        offer.files.len(),
        offer.id
    );

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "pending_approval",
            "offer": offer.id,
            "message": "Waiting for the receiver to accept.",
        })),
    )
        .into_response()
}

/// Write every part into `into`, returning what was written.
async fn stream_to(
    mut multipart: Multipart,
    into: &std::path::Path,
) -> Result<Vec<crate::offers::OfferFile>> {
    let mut out = Vec::new();
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => anyhow::bail!(e.to_string()),
        };
        let Some(raw) = field.file_name().map(|s| s.to_string()) else {
            continue;
        };

        let path = safe_name(&raw, into);
        let mut written: u64 = 0;
        let mut file = tokio::fs::File::create(&path).await?;
        loop {
            match field.chunk().await {
                Ok(Some(bytes)) => {
                    file.write_all(&bytes).await?;
                    written += bytes.len() as u64;
                }
                Ok(None) => break,
                Err(e) => {
                    // Half a file is worse than none.
                    let _ = tokio::fs::remove_file(&path).await;
                    anyhow::bail!(e.to_string());
                }
            }
        }
        file.flush().await?;
        out.push(crate::offers::OfferFile {
            name: path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            size: written,
        });
    }
    Ok(out)
}

/// Accept the parts straight into the Drop folder and record them.
async fn drain(
    app: &Arc<App>,
    multipart: Multipart,
    dir: &std::path::Path,
    auto_move: bool,
    target: PathBuf,
) -> Response {
    let written = match stream_to(multipart, dir).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let mut saved = Vec::new();
    for f in written {
        let mut path = dir.join(&f.name);
        if auto_move {
            if let Ok(moved) = relocate(&path, &target).await {
                path = moved;
            }
        }
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        println!("  <- {name}  ({})", human_size(f.size));
        note_received(app, &name, f.size).await;
        saved.push(name);
    }
    Json(json!({"saved": saved})).into_response()
}

async fn note_received(app: &Arc<App>, name: &str, size: u64) {
    let mut recent = app.recent.write().await;
    recent.push_front(Received {
        name: name.to_string(),
        size: human_size(size),
        when: clock(auth::now()),
    });
    recent.truncate(RECENT_MAX);
    drop(recent);
    app.counter.fetch_add(1, Ordering::Relaxed);
}

/// Move a finished file out of the Drop folder. `rename` fails across
/// filesystems, so fall back to copy-then-delete.
pub async fn relocate(from: &std::path::Path, to_dir: &std::path::Path) -> Result<PathBuf> {
    tokio::fs::create_dir_all(to_dir).await?;
    let name = from
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into());
    let dest = safe_name(&name, to_dir);

    if tokio::fs::rename(from, &dest).await.is_ok() {
        return Ok(dest);
    }
    tokio::fs::copy(from, &dest).await?;
    tokio::fs::remove_file(from).await?;
    Ok(dest)
}

async fn download(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    AxPath(fname): AxPath<String>,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }

    serve_from(&app.dir().await, &fname).await
}

/// Stream one file out of `dir`, and nothing outside it.
async fn serve_from(dir: &std::path::Path, fname: &str) -> Response {
    // Take only the final component, then confirm the result really is inside
    // the folder before opening it.
    let Some(base) = std::path::Path::new(fname).file_name() else {
        return (StatusCode::NOT_FOUND, "no such file").into_response();
    };
    let path = dir.join(base);
    let (Ok(real), Ok(root)) = (path.canonicalize(), dir.canonicalize()) else {
        return (StatusCode::NOT_FOUND, "no such file").into_response();
    };
    if !real.starts_with(&root) || !real.is_file() {
        return (StatusCode::NOT_FOUND, "no such file").into_response();
    }

    let file = match tokio::fs::File::open(&real).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "no such file").into_response(),
    };
    let name = base.to_string_lossy();
    (
        [(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", name.replace('"', "")),
        )],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// control API (loopback only) — this is what the tray talks to
// ---------------------------------------------------------------------------

async fn status(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let cfg = app.cfg.read().await.clone();
    let recent: Vec<_> = app.recent.read().await.iter().cloned().collect();
    let files = list_files(&cfg.dir).await;
    let peers: Vec<_> = app.peers.read().await.values().cloned().collect();
    let shared = list_files(&app.outbox().await).await;
    let shared_text = app.shared_text.read().await.clone();

    Json(json!({
        "shared": shared.len(),
        "shared_files": shared,
        "shared_text": shared_text,
        "outbox": app.outbox().await,
        "connected": app.sessions.live(auth::now()).await,
        "name": cfg.name,
        "pin": cfg.pin,
        "port": cfg.port,
        "dir": cfg.dir,
        "local_url": cfg.local_url(),
        "tunnel_enabled": cfg.tunnel,
        "tunnel_url": app.tunnel_url.read().await.clone(),
        "auto_move": cfg.auto_move,
        "move_target": cfg.move_target,
        "received": app.counter.load(Ordering::Relaxed),
        "recent": recent,
        "peers": peers.len(),
        "peers_list": peers,
        "files": files,
        "files_waiting": files.len(),
        "require_approval": cfg.require_approval,
        "offers": app.offers.pending().await,
    }))
    .into_response()
}

/// The Drop window. Same information as the tray menu, in a real window.
async fn panel(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    Html(crate::panel::PANEL.to_string()).into_response()
}

async fn popover(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(crate::popover::POPOVER.to_string()),
    )
        .into_response()
}

/// The popover's "open full window" button, so it does not need to know how
/// to launch a browser itself.
async fn open_window(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let port = app.cfg.read().await.port;
    let _ = crate::panel::open(port);
    Json(json!({"ok": true})).into_response()
}

async fn panel_qr(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let cfg = app.cfg.read().await.clone();
    let target = app
        .tunnel_url
        .read()
        .await
        .clone()
        .unwrap_or_else(|| cfg.local_url());

    match crate::qr::svg(&target, &cfg.pin, 6) {
        Ok(svg) => (
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            svg,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct Verdict {
    accept: bool,
}

/// Accept or decline a pending transfer. Loopback only — this is the
/// receiver's decision and nobody else's.
async fn decide_offer(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<String>,
    Json(req): Json<Verdict>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }

    let Some(offer) = app.offers.decide(&id, req.accept).await else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no such offer"}))).into_response();
    };

    let Some(stage) = offer.staged.clone() else {
        // Negotiated transfer: the sender is polling and will now upload.
        println!(
            "  {} {}",
            if req.accept { "accepted" } else { "declined" },
            offer.device
        );
        return Json(json!(offer)).into_response();
    };

    // Staged transfer: the bytes are already here, so the verdict decides
    // whether they surface or get shredded.
    if !req.accept {
        let _ = tokio::fs::remove_dir_all(&stage).await;
        println!("  declined {} — staged files deleted", offer.device);
        return Json(json!(offer)).into_response();
    }

    let (dir, auto_move, target) = {
        let cfg = app.cfg.read().await;
        (cfg.dir.clone(), cfg.auto_move, cfg.move_target.clone())
    };
    let dest = if auto_move { target } else { dir };

    let mut saved = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&stage).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let from = entry.path();
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            if let Ok(moved) = relocate(&from, &dest).await {
                let name = moved
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                println!("  <- {name}  ({})", human_size(size));
                note_received(&app, &name, size).await;
                saved.push(name);
            }
        }
    }
    let _ = tokio::fs::remove_dir_all(&stage).await;

    // A snippet is saved like a file, but the point of sending one is usually
    // to paste it, so put it on the clipboard too.
    if let Some(text) = &offer.text {
        to_clipboard(text);
    }

    app.offers
        .set_status(&id, crate::offers::Status::Complete)
        .await;

    Json(json!({"accepted": saved})).into_response()
}

async fn open_folder(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let dir = app.dir().await;
    let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    Json(json!({"opened": dir})).into_response()
}

#[derive(Deserialize)]
struct Toggle {
    enabled: bool,
}

async fn set_auto_move(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<Toggle>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let cfg = {
        let mut cfg = app.cfg.write().await;
        cfg.auto_move = req.enabled;
        cfg.clone()
    };
    let _ = cfg.save();
    Json(json!({"auto_move": req.enabled})).into_response()
}

async fn set_approval(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<Toggle>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let cfg = {
        let mut cfg = app.cfg.write().await;
        cfg.require_approval = req.enabled;
        cfg.clone()
    };
    let _ = cfg.save();
    Json(json!({"require_approval": req.enabled})).into_response()
}

async fn set_tunnel(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<Toggle>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    if req.enabled && !crate::tunnel::available() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "cloudflared is not installed"})),
        )
            .into_response();
    }
    let cfg = {
        let mut cfg = app.cfg.write().await;
        cfg.tunnel = req.enabled;
        cfg.clone()
    };
    let _ = cfg.save();
    let _ = app.tunnel_tx.send(req.enabled).await;
    Json(json!({"tunnel": req.enabled})).into_response()
}

/// Empty the Drop folder into Downloads. This is the "and delete from DROP"
/// half of the feature, run on demand rather than per-file.
async fn move_all(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let (dir, target) = {
        let cfg = app.cfg.read().await;
        (cfg.dir.clone(), cfg.move_target.clone())
    };

    let mut moved = 0usize;
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if !entry.metadata().await.map(|m| m.is_file()).unwrap_or(false) {
                continue;
            }
            if relocate(&path, &target).await.is_ok() {
                moved += 1;
            }
        }
    }
    Json(json!({"moved": moved, "target": target})).into_response()
}

async fn new_pin(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let cfg = {
        let mut cfg = app.cfg.write().await;
        cfg.pin = crate::config::random_pin();
        // Invalidate every live session too, or the old PIN still has reach.
        cfg.secret = crate::config::random_hex(32);
        cfg.clone()
    };
    let _ = cfg.save();
    Json(json!({"pin": cfg.pin})).into_response()
}

// ---------------------------------------------------------------------------
// wiring
// ---------------------------------------------------------------------------

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/auth", post(do_auth))
        .route("/api/state", get(state))
        .route("/api/upload", post(upload))
        .route("/api/offer", post(create_offer))
        .route("/api/offer/{id}", get(offer_status))
        .route("/api/control/offer/{id}", post(decide_offer))
        .route("/files/{fname}", get(download))
        .route("/shared/{fname}", get(collect))
        .route("/api/text", post(receive_text))
        .route("/api/control/share-text", post(share_text))
        .route("/api/control/share-files", post(share_files))
        .route("/api/control/share-files-picker", post(share_files_picker))
        .route("/api/control/share-clipboard", post(share_clipboard))
        .route("/api/control/unshare", post(unshare))
        .route("/api/status", get(status))
        .route("/panel", get(panel))
        .route("/popover", get(popover))
        .route("/api/control/window", post(open_window))
        .route("/panel/qr.svg", get(panel_qr))
        .route("/api/control/open", post(open_folder))
        .route("/api/control/auto-move", post(set_auto_move))
        .route("/api/control/approval", post(set_approval))
        .route("/api/control/tunnel", post(set_tunnel))
        .route("/api/control/move-all", post(move_all))
        .route("/api/control/pin", post(new_pin))
        // Photos and video are far past axum's 2 MB default.
        .layer(DefaultBodyLimit::disable())
        .with_state(app)
}

pub async fn serve(cfg: Config) -> Result<()> {
    tokio::fs::create_dir_all(&cfg.dir).await?;

    let peers: Peers = Default::default();
    let tunnel_url = Arc::new(RwLock::new(None));
    let (tunnel_tx, mut tunnel_rx) = mpsc::channel::<bool>(4);

    let app = Arc::new(App {
        peers: peers.clone(),
        tunnel_url: tunnel_url.clone(),
        throttle: Mutex::new(Throttle::default()),
        recent: RwLock::new(VecDeque::new()),
        counter: AtomicU64::new(0),
        tunnel_tx,
        offers: Default::default(),
        cfg: RwLock::new(cfg.clone()),
        shared_text: RwLock::new(String::new()),
        sessions: Default::default(),
    });

    // mDNS: advertise, and keep an eye on who else is out there.
    match discovery::daemon() {
        Ok(mdns) => {
            if let Err(e) = discovery::advertise(&mdns, &cfg.name, cfg.port) {
                eprintln!("  mdns: could not advertise ({e})");
            }
            tokio::spawn(discovery::browse(mdns, cfg.name.clone(), peers.clone()));
        }
        Err(e) => eprintln!("  mdns: unavailable ({e})"),
    }

    // Tunnel supervisor: owns the cloudflared child, toggled over a channel.
    {
        let url_slot = tunnel_url.clone();
        let port = cfg.port;
        let start_enabled = cfg.tunnel;
        tokio::spawn(async move {
            let mut child: Option<tokio::process::Child> = None;
            let mut enabled = start_enabled;
            loop {
                if enabled && child.is_none() {
                    match crate::tunnel::spawn(port, url_slot.clone()).await {
                        Ok(c) => child = Some(c),
                        Err(e) => eprintln!("  tunnel: {e}"),
                    }
                } else if !enabled {
                    if let Some(mut c) = child.take() {
                        let _ = c.kill().await;
                    }
                    *url_slot.write().await = None;
                }
                match tunnel_rx.recv().await {
                    Some(next) => enabled = next,
                    None => break,
                }
            }
        });
    }

    // Reap offers nobody answered, and the bytes they were holding.
    {
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                for orphan in app.offers.sweep().await {
                    let _ = tokio::fs::remove_dir_all(&orphan).await;
                }
            }
        });
    }

    let addr: SocketAddr = ([0, 0, 0, 0], cfg.port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("\n  Drop  —  {}", cfg.name);
    println!("  {}", cfg.local_url());
    println!("  saving to {}", cfg.dir.display());
    println!("  PIN {}", cfg.pin);
    if cfg.tunnel {
        println!("  tunnel starting…");
    }
    println!();

    axum::serve(
        listener,
        router(app).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
