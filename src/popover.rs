//! The top-right popover: incoming transfers to accept, and the pairing QR.
//!
//! gnome-shell draws the tray menu itself over D-Bus, which carries labels and
//! checkmarks but not images or buttons — so this cannot live *inside* the
//! dropdown. What it can do is be a small window placed directly under the
//! icon. Wayland forbids a client positioning itself, but XWayland does not,
//! and Chromium-family browsers expose `--window-position`, so an `--app`
//! window lands exactly where the dropdown would be.

use anyhow::{bail, Result};
use std::process::Command;

pub const WIDTH: u32 = 380;
pub const HEIGHT: u32 = 560;
/// Clear of the GNOME top bar.
const TOP_MARGIN: u32 = 42;
const RIGHT_MARGIN: u32 = 14;

pub const POPOVER: &str = r####"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Drop</title>
<style>
  :root{
    --paper:#FAFAF7; --ink:#16181A; --rule:#D9D6CD;
    --muted:#6E6C66; --signal:#B3402C; --live:#2F6F4F; --sunk:#F0EEE8;
  }
  @media (prefers-color-scheme: dark){
    :root{ --paper:#1D1F21; --ink:#E8E6E1; --rule:#34373A;
           --muted:#9A9891; --signal:#E0654C; --live:#5FB98A; --sunk:#26292B; }
  }
  *{box-sizing:border-box}
  html,body{height:100%}
  body{
    margin:0; padding:.9rem; background:var(--paper); color:var(--ink);
    font:14px/1.5 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif;
    overflow-x:hidden;
  }
  .mono{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;
        font-variant-numeric:tabular-nums}
  h1{font-size:.7rem; margin:0 0 .7rem; letter-spacing:.16em; text-transform:uppercase;
     color:var(--muted); font-weight:650; display:flex; justify-content:space-between}

  .offer{border:2px solid var(--signal); border-radius:10px; padding:.8rem;
         margin-bottom:.8rem; background:var(--sunk)}
  .offer .from{font-weight:700; color:var(--signal)}
  .offer .sub{color:var(--muted); font-size:.78rem; margin:.15rem 0 .5rem}
  .offer ul{list-style:none; margin:0 0 .7rem; padding:0; max-height:6.5rem; overflow-y:auto}
  .offer li{display:flex; gap:.5rem; padding:.18rem 0; font-size:.8rem}
  .offer li .n{flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap}
  .offer li .s{color:var(--muted); font-size:.74rem}
  .btns{display:flex; gap:.5rem}
  .btns button{flex:1; font:inherit; font-size:.88rem; padding:.5rem; cursor:pointer;
               border-radius:7px; border:1px solid var(--rule); background:transparent;
               color:inherit}
  .btns .yes{background:var(--live); border-color:var(--live); color:#fff; font-weight:650}
  .btns .yes:hover{filter:brightness(1.08)}
  .btns .no:hover{border-color:var(--signal); color:var(--signal)}

  .qrwrap{text-align:center; background:var(--sunk); border:1px solid var(--rule);
          border-radius:10px; padding:.8rem}
  .qrwrap img{width:100%; max-width:210px; height:auto; border-radius:6px; background:#fff}
  .pin{font-size:1.6rem; letter-spacing:.3em; font-weight:700; margin-top:.4rem;
       cursor:pointer}
  .pin:hover{color:var(--signal)}
  .hint{color:var(--muted); font-size:.72rem}
  .addr{display:block; width:100%; text-align:left; margin-top:.5rem; padding:.45rem .55rem;
        border:1px solid var(--rule); border-radius:7px; background:transparent;
        color:inherit; cursor:pointer; font-size:.74rem; overflow:hidden;
        text-overflow:ellipsis; white-space:nowrap}
  .addr:hover{border-color:var(--signal); color:var(--signal)}
  .addr b{display:block; font-size:.58rem; letter-spacing:.1em; text-transform:uppercase;
          color:var(--muted); font-weight:650}
  .addr:hover b{color:inherit}
  .foot{margin-top:.7rem; display:flex; gap:.5rem}
  .foot button{flex:1; font:inherit; font-size:.78rem; padding:.4rem; cursor:pointer;
               border:1px solid var(--rule); border-radius:7px; background:transparent;
               color:var(--muted)}
  .foot button:hover{color:inherit; border-color:var(--ink)}
  .toast{position:fixed; left:50%; bottom:.7rem; transform:translateX(-50%);
    background:var(--ink); color:var(--paper); padding:.35rem .9rem; border-radius:999px;
    font-size:.75rem; opacity:0; transition:opacity .2s; pointer-events:none}
  .toast.on{opacity:1}
  [hidden]{display:none !important}
</style>
</head><body>

<h1><span>Drop</span><span class="mono" id="who"></span></h1>
<div id="offers"></div>

<div class="qrwrap" id="pair">
  <img id="qr" alt="QR code for pairing a phone">
  <div class="pin mono" id="pin" title="Click to copy">------</div>
  <div class="hint">Scan, then enter this PIN</div>
  <button class="addr" id="a-public" hidden></button>
  <button class="addr" id="a-local"></button>
</div>

<div class="foot">
  <button id="b-window">Open full window</button>
  <button id="b-close">Close</button>
</div>
<div class="toast" id="toast"></div>

<script>
const $ = s => document.querySelector(s);
const esc = t => String(t).replace(/[&<>"]/g, c =>
  ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));

let last = null, busy = false;

function toast(m){
  const t = $('#toast'); t.textContent = m; t.classList.add('on');
  setTimeout(() => t.classList.remove('on'), 1300);
}
async function copy(t){
  try { await navigator.clipboard.writeText(t); toast('Copied'); } catch { toast(t); }
}
function bytes(n){
  if (!n) return '';
  if (n < 1024) return n + ' B';
  const u = ['KB','MB','GB']; let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return n.toFixed(1) + ' ' + u[i];
}
async function post(path, body){
  busy = true;
  try {
    await fetch(path, {
      method:'POST',
      headers: body ? {'Content-Type':'application/json'} : {},
      body: body ? JSON.stringify(body) : undefined,
    });
  } finally { busy = false; }
  refresh();
}

$('#pin').onclick    = () => last && copy(last.pin);
$('#b-close').onclick = () => window.close();
$('#b-window').onclick = async () => { await post('/api/control/window'); window.close(); };

// Opened for a transfer: once it is dealt with, get out of the way.
const forOffer = new URLSearchParams(location.search).get('offer') === '1';

async function decide(id, accept){
  await post('/api/control/offer/' + encodeURIComponent(id), {accept});
  toast(accept ? 'Accepted' : 'Declined');
  if (forOffer) setTimeout(() => window.close(), 700);
}

function renderOffers(list){
  const box = $('#offers');
  const ids = (list || []).map(o => o.id).join(',');
  if (box.dataset.ids === ids) return;
  box.dataset.ids = ids;

  if (!list || !list.length){
    box.innerHTML = '';
    $('#pair').hidden = false;
    return;
  }
  // While something needs a decision, that is all this window is for.
  $('#pair').hidden = true;

  box.innerHTML = list.map(o => {
    const n = o.files.length;
    return `<div class="offer">
      <div><span class="from">${esc(o.device)}</span> wants to send</div>
      <div class="sub">${n} ${n === 1 ? 'file' : 'files'}${o.total ? ' · ' + bytes(o.total) : ''}</div>
      <ul>${o.files.map(f =>
        `<li><span class="n">${esc(f.name)}</span><span class="s mono">${bytes(f.size)}</span></li>`
      ).join('')}</ul>
      <div class="btns">
        <button class="no"  data-no="${esc(o.id)}">Decline</button>
        <button class="yes" data-yes="${esc(o.id)}">Accept</button>
      </div>
    </div>`;
  }).join('');

  box.querySelectorAll('[data-yes]').forEach(b => b.onclick = () => decide(b.dataset.yes, true));
  box.querySelectorAll('[data-no]').forEach(b => b.onclick = () => decide(b.dataset.no, false));
}

async function refresh(){
  if (busy) return;
  let s;
  try { s = await (await fetch('/api/status')).json(); } catch { return; }
  last = s;

  $('#who').textContent = s.name;
  $('#pin').textContent = s.pin;
  renderOffers(s.offers);

  const pub = s.tunnel_url;
  const ap = $('#a-public');
  ap.hidden = !pub;
  if (pub){
    ap.innerHTML = '<b>Anywhere</b>' + esc(pub);
    ap.onclick = () => copy(pub);
  }
  $('#a-local').innerHTML = '<b>This network only</b>' + esc(s.local_url);
  $('#a-local').onclick = () => copy(s.local_url);

  const target = pub || s.local_url;
  if ($('#qr').dataset.for !== target){
    $('#qr').dataset.for = target;
    $('#qr').src = '/panel/qr.svg?v=' + encodeURIComponent(target);
  }
}

refresh(); setInterval(refresh, 1500);
window.addEventListener('blur', () => { if (forOffer) return; });
</script>
</body></html>
"####;

/// Top-right of the primary monitor, just under the panel.
fn corner() -> (i32, i32) {
    let out = Command::new("xrandr").output().ok();
    let text = out
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // "eDP-2 connected primary 1920x1200+0+0 (normal ..." — fall back to the
    // first connected output when nothing is flagged primary.
    let line = text
        .lines()
        .find(|l| l.contains(" connected primary"))
        .or_else(|| text.lines().find(|l| l.contains(" connected")));

    let geom = line.and_then(|l| {
        l.split_whitespace()
            .find(|w| w.contains('x') && w.contains('+'))
            .and_then(parse_geometry)
    });

    match geom {
        Some((w, _h, x, y)) => (
            x + w as i32 - WIDTH as i32 - RIGHT_MARGIN as i32,
            y + TOP_MARGIN as i32,
        ),
        None => (900, TOP_MARGIN as i32),
    }
}

/// `1920x1200+0+0` -> (w, h, x, y)
fn parse_geometry(raw: &str) -> Option<(u32, u32, i32, i32)> {
    let (size, rest) = raw.split_once('+')?;
    let (w, h) = size.split_once('x')?;
    let (x, y) = rest.split_once('+')?;
    Some((
        w.parse().ok()?,
        h.parse().ok()?,
        x.parse().ok()?,
        y.parse().ok()?,
    ))
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Open the popover under the tray icon. `for_offer` closes it once the
/// transfer has been dealt with.
pub fn open(port: u16, for_offer: bool) -> Result<()> {
    let url = format!(
        "http://127.0.0.1:{port}/popover{}",
        if for_offer { "?offer=1" } else { "" }
    );
    let (x, y) = corner();

    for bin in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "brave-browser",
        "microsoft-edge",
    ] {
        if which(bin).is_none() {
            continue;
        }
        Command::new(bin)
            .arg(format!("--app={url}"))
            .arg(format!("--window-size={WIDTH},{HEIGHT}"))
            .arg(format!("--window-position={x},{y}"))
            // A separate profile dir keeps this out of the user's browsing
            // session and stops it reusing an existing window.
            .arg("--class=drop-popover")
            .spawn()?;
        return Ok(());
    }

    bail!("no Chromium-family browser for a positioned window")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_xrandr_geometry() {
        assert_eq!(parse_geometry("1920x1200+0+0"), Some((1920, 1200, 0, 0)));
        assert_eq!(parse_geometry("2560x1440+1920+0"), Some((2560, 1440, 1920, 0)));
        assert_eq!(parse_geometry("nonsense"), None);
    }
}
