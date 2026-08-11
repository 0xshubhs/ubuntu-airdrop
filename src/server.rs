//! The receiving daemon: HTTP, auth, mDNS, tunnel supervision.

use crate::auth::{self, Throttle};
use crate::config::{Config, COOKIE, SESSION_SECS};
use crate::discovery::{self, Peers};
use crate::net::{human_size, is_loopback, safe_name};
use crate::page;
use anyhow::Result;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path as AxPath, State};
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

async fn index(State(app): State<Arc<App>>) -> Html<String> {
    let cfg = app.cfg.read().await;
    Html(page::render(
        &cfg.name,
        &format!("{}:{}", crate::net::lan_ip(), cfg.port),
        &cfg.dir.display().to_string(),
    ))
}

async fn state(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }

    let dir = app.dir().await;
    let mut files = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
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
            files.push((mtime, Received {
                name,
                size: human_size(meta.len()),
                when: clock(mtime),
            }));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let files: Vec<Received> = files.into_iter().map(|(_, f)| f).take(60).collect();

    let peers: Vec<_> = app.peers.read().await.values().cloned().collect();
    Json(json!({
        "device": app.cfg.read().await.name,
        "files": files,
        "peers": peers,
    }))
    .into_response()
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

async fn upload(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if let Err(r) = guard(&app, &headers).await {
        return r;
    }

    let (dir, auto_move, target) = {
        let cfg = app.cfg.read().await;
        (cfg.dir.clone(), cfg.auto_move, cfg.move_target.clone())
    };
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let mut saved = Vec::new();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        };
        let Some(raw) = field.file_name().map(|s| s.to_string()) else {
            continue;
        };

        let mut path = safe_name(&raw, &dir);
        let mut written: u64 = 0;
        let mut field = field;

        let mut file = match tokio::fs::File::create(&path).await {
            Ok(f) => f,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        loop {
            match field.chunk().await {
                Ok(Some(bytes)) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                    }
                    written += bytes.len() as u64;
                }
                Ok(None) => break,
                Err(e) => {
                    // Half a file is worse than none.
                    let _ = tokio::fs::remove_file(&path).await;
                    return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
                }
            }
        }
        if let Err(e) = file.flush().await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        drop(file);

        if auto_move {
            if let Ok(moved) = relocate(&path, &target).await {
                path = moved;
            }
        }

        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        println!("  <- {name}  ({})", human_size(written));

        let mut recent = app.recent.write().await;
        recent.push_front(Received {
            name: name.clone(),
            size: human_size(written),
            when: clock(auth::now()),
        });
        recent.truncate(RECENT_MAX);
        drop(recent);

        app.counter.fetch_add(1, Ordering::Relaxed);
        saved.push(name);
    }

    Json(json!({"saved": saved})).into_response()
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

    let dir = app.dir().await;
    // Take only the final component, then confirm the result really is inside
    // the Drop folder before opening it.
    let Some(base) = std::path::Path::new(&fname).file_name() else {
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
    let files_waiting = count_files(&cfg.dir).await;

    Json(json!({
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
        "peers": app.peers.read().await.len(),
        "files_waiting": files_waiting,
    }))
    .into_response()
}

async fn count_files(dir: &std::path::Path) -> usize {
    let mut n = 0;
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if e.metadata().await.map(|m| m.is_file()).unwrap_or(false) {
                n += 1;
            }
        }
    }
    n
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
        .route("/files/{fname}", get(download))
        .route("/api/status", get(status))
        .route("/api/control/auto-move", post(set_auto_move))
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
        cfg: RwLock::new(cfg.clone()),
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
