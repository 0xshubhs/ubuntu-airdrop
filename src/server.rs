//! The receiving daemon: HTTP, auth, mDNS, tunnel supervision.

use crate::auth::{self, Throttle};
use crate::config::{Config, COOKIE, SESSION_SECS};
use crate::discovery::{self, Peers};
use crate::net::{human_size, is_loopback, safe_name};
use crate::page;
use crate::window::{Shut, Span, Window};
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
use std::time::Duration;
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

/// What finished arriving most recently.
#[derive(Debug, Clone, Serialize)]
pub struct Arrival {
    pub path: PathBuf,
    /// An http(s) address, when what arrived was a shared link.
    pub link: Option<String>,
    /// How many files came in that transfer.
    pub count: usize,
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
    /// The last thing to arrive, so the notification can offer to open it
    /// rather than only the folder it went into.
    pub last_arrival: RwLock<Option<Arrival>>,
    /// Whether this machine is taking arrivals, and for how much longer.
    pub window: RwLock<Window>,
    /// The mDNS handle, kept so the advertisement can be withdrawn as well as
    /// published, and the advertisement itself while one is up.
    pub mdns: Option<mdns_sd::ServiceDaemon>,
    pub advert: RwLock<Option<mdns_sd::ServiceInfo>>,
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

    async fn receiving(&self) -> bool {
        self.window.read().await.receiving(auth::now())
    }

    /// Opening a window because this machine is putting something up for the
    /// other device to collect: it has to be able to reach us to take it.
    /// omdrop does the same when you send with receiving off.
    async fn open_for_sharing(&self) {
        let now = auth::now();
        let mut window = self.window.write().await;
        if !window.receiving(now) {
            window.open(None, now);
            println!("  receiving on, to hand something over");
        }
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

/// The cap on one transfer: a share of what is free where files land.
async fn transfer_budget(app: &App) -> Option<u64> {
    let (dir, pct) = {
        let cfg = app.cfg.read().await;
        (cfg.dir.clone(), cfg.limit_pct)
    };
    crate::disk::budget_for(&dir, pct)
}

/// 507, with both numbers, because "too big" without them is no help.
fn too_big(cap: u64, asked: Option<u64>) -> Response {
    let mut say = format!(
        "This computer will only take {} at a time",
        human_size(cap)
    );
    if let Some(asked) = asked {
        say = format!("{say}, and that is {}", human_size(asked));
    }
    (
        StatusCode::INSUFFICIENT_STORAGE,
        Json(json!({"error": format!("{say}."), "limit": cap})),
    )
        .into_response()
}

/// Arrivals only happen inside a window: signing in, offering, uploading and
/// pasting all go through here.
///
/// 503 rather than 403, because nothing is wrong with the request. This
/// machine is not listening at the moment, and will be again when someone
/// says so. A device already past the PIN can still collect what this side
/// put up for it: that is this machine's own doing, not an arrival.
async fn window_open(app: &App) -> Result<(), Response> {
    if app.receiving().await {
        return Ok(());
    }
    Err((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "not accepting files right now", "receiving": false})),
    )
        .into_response())
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

    // Before the PIN: a machine that is not receiving has nothing to say
    // about whether the PIN was right.
    if let Err(r) = window_open(&app).await {
        return r;
    }

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
    //
    // Loopback is this machine talking to itself: the Drop window, the
    // popover, a curl from a terminal here. None of those are a device that
    // arrived, and listing them would claim a phone is on the page when none
    // is. Only something that came over the network counts.
    if !crate::net::is_loopback(&ip) {
        let fresh = app
            .sessions
            .open(&ip.to_string(), req.device.clone(), auth::now())
            .await;
        if fresh {
            let who = req.device.as_deref().unwrap_or("A device");
            println!("  + {who} connected from {ip}");
        }
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
    // desktop's "connected" list honest without a websocket. Loopback is
    // skipped here for the same reason it is skipped at sign-in.
    let now = auth::now();
    if !crate::net::is_loopback(&addr.ip()) {
        app.sessions
            .touch(&addr.ip().to_string(), device_header(&headers), now)
            .await;
    }
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
    if let Err(r) = window_open(&app).await {
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
        let saved_at = relocate(&stage.join(&name), &dest).await;
        let _ = tokio::fs::remove_dir_all(&stage).await;
        let Ok(saved_at) = saved_at else {
            return (StatusCode::INTERNAL_SERVER_ERROR, "cannot save").into_response();
        };
        note_received(&app, &name, body.len() as u64).await;
        landed(&app, std::slice::from_ref(&saved_at)).await;
        // The snippet itself beats the file it was written to, and a snippet
        // that is only a link is the link.
        match crate::links::in_text(&body) {
            Some(url) => {
                to_clipboard(&url);
                *app.last_arrival.write().await = Some(Arrival {
                    path: saved_at,
                    link: Some(url),
                    count: 1,
                });
            }
            None => to_clipboard(&body),
        }
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
    app.open_for_sharing().await;
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
    app.open_for_sharing().await;
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
/// Put the files themselves on the clipboard, as `text/uri-list`, which is
/// what a file manager, a browser or a chat window pastes as an attachment.
fn files_to_clipboard(paths: &[PathBuf]) {
    let list: String = paths
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .map(|p| format!("file://{}\r\n", p.display()))
        .collect();
    if list.is_empty() {
        return;
    }
    clipboard_write(&list, Some("text/uri-list"));
}

fn to_clipboard(text: &str) {
    clipboard_write(text, None);
}

fn clipboard_write(body: &str, mime: Option<&str>) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("wl-copy");
    if let Some(mime) = mime {
        cmd.args(["--type", mime]);
    }
    let Ok(mut child) = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
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
    if let Err(r) = window_open(&app).await {
        return r;
    }
    if req.files.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "no files"}))).into_response();
    }
    // The sender's own figures, so this is a courtesy: it saves uploading a
    // video that was never going to fit. The stream is what is enforced.
    let asked: u64 = req.files.iter().map(|f| f.size).sum();
    if let Some(cap) = transfer_budget(&app).await {
        if asked > cap {
            return too_big(cap, Some(asked));
        }
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
    if let Err(r) = window_open(&app).await {
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

    let staged = match stream_to(multipart, &stage, transfer_budget(&app).await).await {
        Ok(v) => v,
        Err(spill) => {
            let _ = tokio::fs::remove_dir_all(&stage).await;
            return match spill {
                Spill::TooBig(cap) => too_big(cap, None),
                Spill::Failed(e) => (StatusCode::BAD_REQUEST, e).into_response(),
            };
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
/// Why a stream stopped. The two cases answer differently, so they are not
/// one string: a transfer that outgrew the disk is not a bad request.
enum Spill {
    Failed(String),
    TooBig(u64),
}

/// Stream a multipart body into `into`, stopping if it outgrows `budget`.
///
/// The declared sizes in an offer are checked before a byte moves, but they
/// are the sender's word. This is the number that counts.
async fn stream_to(
    mut multipart: Multipart,
    into: &std::path::Path,
    budget: Option<u64>,
) -> Result<Vec<crate::offers::OfferFile>, Spill> {
    let mut out: Vec<crate::offers::OfferFile> = Vec::new();
    let mut total: u64 = 0;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return Err(Spill::Failed(e.to_string())),
        };
        let Some(raw) = field.file_name().map(|s| s.to_string()) else {
            continue;
        };

        let path = safe_name(&raw, into);
        let mut written: u64 = 0;
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| Spill::Failed(e.to_string()))?;
        loop {
            match field.chunk().await {
                Ok(Some(bytes)) => {
                    total += bytes.len() as u64;
                    if budget.is_some_and(|cap| total > cap) {
                        // Stop at the cap rather than at the end of the
                        // disk, and leave nothing behind.
                        let _ = tokio::fs::remove_file(&path).await;
                        for done in &out {
                            let _ = tokio::fs::remove_file(into.join(&done.name)).await;
                        }
                        return Err(Spill::TooBig(budget.unwrap_or(0)));
                    }
                    if let Err(e) = file.write_all(&bytes).await {
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(Spill::Failed(e.to_string()));
                    }
                    written += bytes.len() as u64;
                }
                Ok(None) => break,
                Err(e) => {
                    // Half a file is worse than none.
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(Spill::Failed(e.to_string()));
                }
            }
        }
        file.flush()
            .await
            .map_err(|e| Spill::Failed(e.to_string()))?;
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
    let written = match stream_to(multipart, dir, transfer_budget(app).await).await {
        Ok(v) => v,
        Err(Spill::TooBig(cap)) => return too_big(cap, None),
        Err(Spill::Failed(e)) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let mut saved = Vec::new();
    let mut landed_at = Vec::new();
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
        landed_at.push(path);
        saved.push(name);
    }
    landed(app, &landed_at).await;
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

/// Everything that happens once a batch has finished arriving.
///
/// One place rather than four, because a transfer is one event however many
/// files it carried: the clipboard is set once, a "one file" window closes
/// once, and the notification has one thing to offer opening.
async fn landed(app: &Arc<App>, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }

    // A link shared from a phone arrives as a one-line file. Read it now,
    // while we know which file it was.
    let mut link = None;
    if paths.len() == 1
        && let Some(name) = paths[0].file_name().map(|n| n.to_string_lossy().to_string())
        && let Ok(body) = tokio::fs::read_to_string(&paths[0]).await
    {
        link = crate::links::in_file(&name, &body);
    }

    // What the clipboard gets: the link itself when that is what arrived,
    // otherwise the files, so a paste into a chat window sends them on.
    match &link {
        Some(url) => to_clipboard(url),
        None => files_to_clipboard(paths),
    }

    *app.last_arrival.write().await = Some(Arrival {
        path: paths[0].clone(),
        link,
        count: paths.len(),
    });

    // "One file" means this one: the window closes behind it.
    if app.window.write().await.arrived() {
        println!("  receiving off ({})", Shut::OneFile.say());
    }
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
    let (receiving, remaining, once, span, shut) = {
        let window = app.window.read().await;
        let now = auth::now();
        (
            window.receiving(now),
            window.remaining(now),
            window.once(),
            window.span(),
            window.shut(),
        )
    };

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
        "limit_pct": cfg.limit_pct,
        "limit_bytes": crate::disk::budget_for(&cfg.dir, cfg.limit_pct),
        "free_bytes": crate::disk::free_bytes(&cfg.dir),
        "last_arrival": app.last_arrival.read().await.clone(),
        "receiving": receiving,
        "remaining": remaining,
        "once": once,
        "visible_for": span.spec(),
        "visible_label": span.to_string(),
        "off_because": shut.say(),
        "spans": crate::window::CHOICES
            .iter()
            .map(|s| json!({"spec": s.spec(), "label": s.to_string()}))
            .collect::<Vec<_>>(),
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
    let mut landed_at = Vec::new();
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
                landed_at.push(moved);
                saved.push(name);
            }
        }
    }
    let _ = tokio::fs::remove_dir_all(&stage).await;
    landed(&app, &landed_at).await;

    // A snippet is saved like a file, but the point of sending one is usually
    // to paste it, so put it on the clipboard too. A snippet that is nothing
    // but a link is treated as the link it is.
    if let Some(text) = &offer.text {
        match crate::links::in_text(text) {
            Some(url) => {
                to_clipboard(&url);
                if let Some(path) = landed_at.first() {
                    *app.last_arrival.write().await = Some(Arrival {
                        path: path.clone(),
                        link: Some(url),
                        count: 1,
                    });
                }
            }
            None => to_clipboard(text),
        }
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

#[derive(Deserialize)]
struct SendToReq {
    host: String,
    port: u16,
    #[serde(default)]
    label: String,
}

/// Push files to another Drop machine, which is what clicking a device on
/// the radar does. The same path `drop send` takes, picker and all.
async fn send_to_peer(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<SendToReq>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let Some(paths) = tokio::task::spawn_blocking(crate::tray::pick_files)
        .await
        .ok()
        .flatten()
        .filter(|p: &Vec<String>| !p.is_empty())
    else {
        return Json(json!({"cancelled": true})).into_response();
    };

    // The other end is almost always this person's own machine, so its PIN
    // is ours. If it is not, the receiver says so and that is the error.
    let pin = app.pin().await;
    let target = format!("{}:{}", req.host, req.port);
    let files = crate::send::expand(&paths);
    let who = if req.label.is_empty() {
        target.clone()
    } else {
        req.label.clone()
    };
    match crate::send::send(&target, &files, &pin, Duration::from_secs(3)).await {
        Ok(()) => {
            println!("  -> sent {} file(s) to {who}", files.len());
            Json(json!({"sent": files.len(), "to": who})).into_response()
        }
        Err(e) => {
            println!("  -> could not send to {who}: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("{who} did not take it: {e}")})),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
struct NameReq {
    name: String,
}

/// "They see you as": the name in the advertisement and on the phone's page.
async fn set_name(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<NameReq>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "a name, up to 32 characters"})),
        )
            .into_response();
    }

    {
        let mut cfg = app.cfg.write().await;
        cfg.name = name.to_string();
        let _ = cfg.save();
    }
    // Drop the advertisement rather than renaming it: the reconciler puts a
    // fresh one up within the second, under the new name.
    if let (Some(mdns), Some(info)) = (app.mdns.as_ref(), app.advert.write().await.take()) {
        let _ = discovery::unadvertise(mdns, &info);
    }
    println!("  known as {name}");
    Json(json!({"name": name})).into_response()
}

#[derive(Deserialize)]
struct DirReq {
    /// Where files should land. `~` is expanded, and the folder is created.
    path: String,
}

/// "Save files to".
async fn set_dir(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<DirReq>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let raw = req.path.trim();
    if raw.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "no folder"}))).into_response();
    }
    let path = crate::net::expand_home(raw);
    if let Err(e) = tokio::fs::create_dir_all(&path).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("cannot use {}: {e}", path.display())})),
        )
            .into_response();
    }

    {
        let mut cfg = app.cfg.write().await;
        cfg.dir = path.clone();
        let _ = cfg.save();
    }
    println!("  saving to {}", path.display());
    Json(json!({"dir": path})).into_response()
}

/// The folder chooser, run for the panel the way the file picker is.
async fn pick_dir(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let Some(chosen) = tokio::task::spawn_blocking(crate::tray::pick_folder)
        .await
        .ok()
        .flatten()
    else {
        return Json(json!({"cancelled": true})).into_response();
    };

    let path = crate::net::expand_home(&chosen);
    if tokio::fs::create_dir_all(&path).await.is_err() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "cannot use that folder"})))
            .into_response();
    }
    {
        let mut cfg = app.cfg.write().await;
        cfg.dir = path.clone();
        let _ = cfg.save();
    }
    println!("  saving to {}", path.display());
    Json(json!({"dir": path})).into_response()
}

#[derive(Deserialize)]
struct LimitReq {
    /// Percent of the free space one transfer may use. 0 lifts the cap.
    pct: u8,
}

/// The largest transfer, as omdrop's `limit` sets it.
async fn set_limit(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<LimitReq>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    if req.pct > 100 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "a percentage, so 0 to 100"})),
        )
            .into_response();
    }
    let (dir, pct) = {
        let mut cfg = app.cfg.write().await;
        cfg.limit_pct = req.pct;
        let _ = cfg.save();
        (cfg.dir.clone(), cfg.limit_pct)
    };
    let cap = crate::disk::budget_for(&dir, pct);
    match cap {
        Some(bytes) => println!("  largest transfer {pct}% of free space ({})", human_size(bytes)),
        None => println!("  largest transfer: no cap"),
    }
    Json(json!({"limit_pct": pct, "limit_bytes": cap})).into_response()
}

#[derive(Deserialize)]
struct ReceivingReq {
    enabled: bool,
    /// A span for this window alone, leaving the stored preference as it is.
    #[serde(default)]
    span: Option<String>,
}

/// The switch: turn receiving on, with an optional one-off span, or off.
async fn set_receiving(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<ReceivingReq>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let span = match req.span.as_deref() {
        Some(raw) => match Span::parse(raw) {
            Some(span) => Some(span),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("{raw:?} is not a length of time")})),
                )
                    .into_response()
            }
        },
        None => None,
    };

    let now = auth::now();
    let (receiving, remaining, label) = {
        let mut window = app.window.write().await;
        if req.enabled {
            window.open(span, now);
        } else {
            window.close(Shut::Off);
        }
        let shown = span.unwrap_or_else(|| window.span());
        (
            window.receiving(now),
            window.remaining(now),
            shown.to_string(),
        )
    };
    println!(
        "  receiving {}",
        if receiving {
            format!("on, for {label}")
        } else {
            "off".to_string()
        }
    );
    Json(json!({"receiving": receiving, "remaining": remaining})).into_response()
}

#[derive(Deserialize)]
struct SpanReq {
    spec: String,
}

/// "Stay visible for": the remembered choice, which a live window adopts at
/// once so picking a shorter one shortens the window in front of you.
async fn set_visible_for(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<SpanReq>,
) -> Response {
    if let Err(r) = local_only(&addr) {
        return r;
    }
    let Some(span) = Span::parse(&req.spec) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{:?} is not a length of time", req.spec)})),
        )
            .into_response();
    };

    {
        let mut cfg = app.cfg.write().await;
        cfg.visible_for = span.spec();
        let _ = cfg.save();
    }
    let now = auth::now();
    let (receiving, remaining) = {
        let mut window = app.window.write().await;
        window.set_span(span, now);
        (window.receiving(now), window.remaining(now))
    };
    println!("  stay visible for {span}");
    Json(json!({"visible_for": span.spec(), "receiving": receiving, "remaining": remaining}))
        .into_response()
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
        .route("/api/control/receiving", post(set_receiving))
        .route("/api/control/limit", post(set_limit))
        .route("/api/control/name", post(set_name))
        .route("/api/control/dir", post(set_dir))
        .route("/api/control/dir-picker", post(pick_dir))
        .route("/api/control/send-to", post(send_to_peer))
        .route("/api/control/visible-for", post(set_visible_for))
        .route("/api/control/move-all", post(move_all))
        .route("/api/control/pin", post(new_pin))
        // Photos and video are far past axum's 2 MB default.
        .layer(DefaultBodyLimit::disable())
        .with_state(app)
}

/// Keep the mDNS advertisement in step with the window.
///
/// Reconciling rather than registering at each change keeps one place in
/// charge of it, the way the tunnel supervisor owns cloudflared: whatever
/// opened or closed the window, the advertisement follows within a second.
async fn sync_advert(app: &Arc<App>) {
    let Some(mdns) = app.mdns.as_ref() else {
        return;
    };
    let want = app.receiving().await;
    if want == app.advert.read().await.is_some() {
        return;
    }
    if want {
        let (name, port) = {
            let cfg = app.cfg.read().await;
            (cfg.name.clone(), cfg.port)
        };
        match discovery::advertise(mdns, &name, port) {
            Ok(info) => *app.advert.write().await = Some(info),
            Err(e) => eprintln!("  mdns: could not advertise ({e})"),
        }
    } else if let Some(info) = app.advert.write().await.take() {
        if let Err(e) = discovery::unadvertise(mdns, &info) {
            eprintln!("  mdns: could not withdraw the advertisement ({e})");
        }
    }
}

pub async fn serve(cfg: Config) -> Result<()> {
    tokio::fs::create_dir_all(&cfg.dir).await?;

    let peers: Peers = Default::default();
    let tunnel_url = Arc::new(RwLock::new(None));
    let (tunnel_tx, tunnel_rx) = mpsc::channel::<bool>(4);

    let mdns = match discovery::daemon() {
        Ok(mdns) => Some(mdns),
        Err(e) => {
            eprintln!("  mdns: unavailable ({e})");
            None
        }
    };

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
        last_arrival: RwLock::new(None),
        window: RwLock::new(Window::new(cfg.span())),
        mdns: mdns.clone(),
        advert: RwLock::new(None),
    });

    // mDNS: keep an eye on who else is out there. Advertising ourselves is
    // the visibility task's job, since it only happens inside a window.
    if let Some(mdns) = mdns {
        tokio::spawn(discovery::browse(mdns, cfg.name.clone(), peers.clone()));
    }

    // Visibility: close a window that has run out, and keep the
    // advertisement in step with it.
    {
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                if app.window.write().await.sweep(auth::now()) {
                    println!("  receiving off ({})", Shut::Expired.say());
                }
                sync_advert(&app).await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    // Tunnel supervisor: owns the cloudflared child, toggled over a channel.
    tokio::spawn(crate::tunnel::supervise(
        cfg.port,
        tunnel_url.clone(),
        cfg.tunnel,
        tunnel_rx,
    ));

    // How far away each device is, as near as Drop can tell. Every few
    // seconds, because a laptop that has been shut moves to the rim.
    {
        let peers = peers.clone();
        tokio::spawn(async move {
            loop {
                discovery::measure(&peers).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
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
    match cfg.span() {
        Span::Always => println!("  receiving on, until you turn it off"),
        span => println!("  receiving off; turn it on for {span} with: drop on"),
    }
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
