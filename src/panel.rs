//! The Drop window — everything the tray menu shows, in a real window.
//!
//! Served on `/panel` and refused to anything that is not loopback, so it can
//! drive the same unauthenticated control API the tray uses.

pub const PANEL: &str = r####"<!doctype html>
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
    :root{ --paper:#131416; --ink:#E8E6E1; --rule:#2C2E31;
           --muted:#8D8B85; --signal:#E0654C; --live:#5FB98A; --sunk:#1B1D1F; }
  }
  *{box-sizing:border-box}
  body{
    margin:0; padding:1.5rem 1.25rem 3rem; background:var(--paper); color:var(--ink);
    font:15px/1.55 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif;
    max-width:34rem; margin-inline:auto;
  }
  .mono{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;
        font-variant-numeric:tabular-nums}
  header{border-bottom:2px solid var(--ink); padding-bottom:.55rem; margin-bottom:1.25rem;
         display:flex; align-items:baseline; justify-content:space-between; gap:1rem}
  h1{font-size:1rem; margin:0; letter-spacing:.16em; text-transform:uppercase; font-weight:650}
  header .who{font-size:.8rem; color:var(--muted)}
  h2{font-size:.68rem; letter-spacing:.14em; text-transform:uppercase; color:var(--muted);
     margin:1.6rem 0 .5rem; font-weight:600}

  .hero{display:flex; gap:1.25rem; align-items:center; background:var(--sunk);
        border:1px solid var(--rule); padding:1rem; border-radius:10px}
  .hero img{width:150px; height:auto; flex:none; border-radius:8px; background:#fff}
  .hero .side{min-width:0; flex:1}
  .pinrow{font-size:2rem; letter-spacing:.28em; font-weight:700; cursor:pointer;
          line-height:1.1; word-break:break-all}
  .pinrow:hover{color:var(--signal)}
  .hint{color:var(--muted); font-size:.75rem; margin-top:.15rem}

  .addr{display:block; width:100%; text-align:left; border:1px solid var(--rule);
        background:transparent; color:inherit; padding:.55rem .7rem; margin-top:.5rem;
        cursor:pointer; border-radius:7px; font-size:.82rem}
  .addr:hover{border-color:var(--signal); color:var(--signal)}
  .addr b{display:block; font-size:.62rem; letter-spacing:.12em; text-transform:uppercase;
          color:var(--muted); font-weight:600; margin-bottom:.15rem}
  .addr:hover b{color:inherit}

  /* .set is the same shape, used by the rows under "Send to them". It had
     no rule of its own, which ran each label into its caption. */
  .row, .set{display:flex; align-items:center; justify-content:space-between; gap:1rem;
       padding:.6rem 0; border-bottom:1px solid var(--rule)}
  .row span.label, .set span.label{font-size:.9rem}
  .row small, .set small{display:block; color:var(--muted); font-size:.75rem}

  /* Switch */
  .sw{position:relative; width:42px; height:24px; flex:none}
  .sw input{opacity:0; width:0; height:0; position:absolute}
  .sw i{position:absolute; inset:0; background:var(--rule); border-radius:999px;
        transition:background .15s; cursor:pointer}
  .sw i::after{content:""; position:absolute; width:18px; height:18px; left:3px; top:3px;
        background:var(--paper); border-radius:50%; transition:transform .15s}
  .sw input:checked + i{background:var(--live)}
  .sw input:checked + i::after{transform:translateX(18px)}
  .sw input:focus-visible + i{outline:2px solid var(--signal); outline-offset:2px}

  button.act{font:inherit; font-size:.82rem; padding:.45rem 1rem; border:1px solid var(--ink);
        background:var(--ink); color:var(--paper); border-radius:7px; cursor:pointer}
  button.act:hover:not(:disabled){background:var(--signal); border-color:var(--signal)}
  button.act:disabled{opacity:.35; cursor:default}

  input.act{font:inherit; font-size:.82rem; padding:.4rem .5rem; border:1px solid var(--rule);
        background:transparent; color:inherit; border-radius:7px; width:11rem; text-align:right}
  input.act:focus{outline:none; border-color:var(--signal)}
  select.act{font:inherit; font-size:.82rem; padding:.4rem .5rem; border:1px solid var(--rule);
        background:transparent; color:inherit; border-radius:7px; cursor:pointer}

  /* Radar. omdrop places a device by radio signal; Drop has none, so the
     ring is the round trip to its port, which is the nearest honest thing. */
  .radar{display:flex; gap:1.1rem; align-items:center; margin:.2rem 0 .4rem}
  .radar svg{flex:none; width:190px; height:190px}
  .radar .ring{fill:none; stroke:var(--rule)}
  .radar .sweep{transform-origin:50% 50%; animation:spin 4s linear infinite}
  @media (prefers-reduced-motion:reduce){ .radar .sweep{animation:none; opacity:.35} }
  @keyframes spin{from{transform:rotate(0deg)}to{transform:rotate(360deg)}}
  .radar .blip{cursor:pointer}
  .radar .blip circle{fill:var(--live)}
  .radar .blip:hover circle{fill:var(--signal)}
  .radar .blip text{font-size:9px; fill:var(--muted)}
  .radar .blip:hover text{fill:var(--ink)}
  .radar .none{font-size:.8rem; color:var(--muted)}
  .radar .legend{min-width:0}
  .radar .legend li{list-style:none; font-size:.82rem; padding:.2rem 0; cursor:pointer}
  .radar .legend li:hover .name{color:var(--signal)}
  .radar .legend .meta{display:block; color:var(--muted); font-size:.72rem}

  /* The window, at the top, because nothing below it matters while it is shut. */
  .gate{display:flex; align-items:center; justify-content:space-between; gap:1rem;
        border:1px solid var(--rule); border-radius:10px; padding:.7rem .85rem;
        margin-bottom:1rem}
  .gate .state{font-size:.95rem; font-weight:600}
  .gate small{display:block; color:var(--muted); font-size:.75rem; font-weight:400}
  .gate .dot{display:inline-block; width:.5rem; height:.5rem; border-radius:999px;
             background:var(--rule); margin-right:.45rem; vertical-align:.05rem}
  body.open .gate{border-color:var(--live)}
  body.open .gate .dot{background:var(--live)}
  /* Shut: the code and the addresses are no use to anyone, so say so quietly. */
  body:not(.open) .hero{opacity:.4}

  ul{list-style:none; margin:0; padding:0}
  li{display:flex; align-items:baseline; gap:.7rem; padding:.45rem 0;
     border-bottom:1px solid var(--rule); font-size:.87rem}
  li .name{flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap}
  li .meta{color:var(--muted); font-size:.76rem; white-space:nowrap}
  .dot{width:7px; height:7px; border-radius:50%; background:var(--live); flex:none}
  .empty{color:var(--muted); font-size:.82rem; padding:.45rem 0}
  .note{display:flex; gap:.6rem; align-items:flex-start; padding:.55rem 0;
        border-bottom:1px solid var(--rule)}
  .note textarea{flex:1; min-width:0; padding:.5rem; border:1px solid var(--rule);
        border-radius:7px; background:transparent; color:inherit; font:inherit;
        font-size:.85rem; resize:vertical}
  .note textarea:focus{outline:2px solid var(--signal); outline-offset:1px}
  /* A connected device is the whole point of the section below it. */
  li.live .name{font-weight:600}
  footer{margin-top:2rem; color:var(--muted); font-size:.75rem}
  .toast{position:fixed; left:50%; bottom:1.2rem; transform:translateX(-50%);
    background:var(--ink); color:var(--paper); padding:.5rem 1rem; border-radius:999px;
    font-size:.8rem; opacity:0; transition:opacity .2s; pointer-events:none}
  .toast.on{opacity:1}

  /* Incoming transfer */
  .offer{border:2px solid var(--signal); border-radius:10px; padding:1rem;
         margin-bottom:1.25rem; background:var(--sunk)}
  .offer h3{margin:0 0 .1rem; font-size:1.05rem}
  .offer .from{color:var(--signal); font-weight:700}
  .offer .sub{color:var(--muted); font-size:.8rem; margin-bottom:.6rem}
  .offer ul{max-height:8.5rem; overflow-y:auto; margin-bottom:.8rem}
  .offer li{padding:.3rem 0; font-size:.83rem}
  .offer .btns{display:flex; gap:.6rem}
  .offer button{flex:1; font:inherit; font-size:.9rem; padding:.55rem; cursor:pointer;
                border-radius:7px; border:1px solid var(--rule); background:transparent;
                color:inherit}
  .offer button.yes{background:var(--live); border-color:var(--live); color:#fff;
                    font-weight:600}
  .offer button.yes:hover{filter:brightness(1.08)}
  .offer button.no:hover{border-color:var(--signal); color:var(--signal)}
  @media (prefers-reduced-motion:no-preference){
    .offer{animation:pop .18s ease-out}
  }
  @keyframes pop{from{transform:scale(.97); opacity:0}to{transform:scale(1); opacity:1}}
</style>
</head><body>

<header>
  <h1>Drop</h1>
  <span class="who mono" id="who"></span>
</header>

<div id="offers"></div>

<div class="gate">
  <span class="state"><span class="dot"></span><span id="recv-state">Receiving</span>
    <small id="recv-note"></small></span>
  <label class="sw"><input type="checkbox" id="t-recv"><i></i></label>
</div>

<div class="hero">
  <img id="qr" alt="QR code for pairing a phone">
  <div class="side">
    <div class="pinrow mono" id="pin" title="Click to copy">------</div>
    <div class="hint">Scan the code, then enter this PIN</div>
    <button class="addr" id="a-public" hidden></button>
    <button class="addr" id="a-local"></button>
  </div>
</div>

<h2>Settings</h2>
<div class="row">
  <span class="label">They see you as
    <small>The name on the phone and in other devices' lists</small></span>
  <input class="act" id="t-name" maxlength="32" spellcheck="false">
</div>
<div class="row">
  <span class="label">Save files to
    <small id="dir-pick-note"></small></span>
  <button class="act" id="b-dir">Choose…</button>
</div>
<div class="row">
  <span class="label">Stay visible for
    <small>How long the switch above stays on</small></span>
  <select class="act" id="s-span"></select>
</div>
<div class="row">
  <span class="label">Largest transfer
    <small id="limit-note"></small></span>
  <select class="act" id="s-limit">
    <option value="10">10% of free space</option>
    <option value="30">30% of free space</option>
    <option value="50">50% of free space</option>
    <option value="80">80% of free space</option>
    <option value="0">No cap</option>
  </select>
</div>
<div class="row">
  <span class="label">Reachable from the internet
    <small id="tunnel-note">Off — this network only</small></span>
  <label class="sw"><input type="checkbox" id="t-tunnel"><i></i></label>
</div>
<div class="row">
  <span class="label">Ask before receiving
    <small>Nothing is saved until you accept it</small></span>
  <label class="sw"><input type="checkbox" id="t-approval"><i></i></label>
</div>
<div class="row">
  <span class="label">Always move to Downloads
    <small id="move-note"></small></span>
  <label class="sw"><input type="checkbox" id="t-move"><i></i></label>
</div>
<div class="row">
  <span class="label">Drop folder<small id="dir-note"></small></span>
  <span style="display:flex; gap:.5rem">
    <button class="act" id="b-open">Open</button>
    <button class="act" id="b-move">Move all</button>
  </span>
</div>
<div class="row" style="border-bottom:none">
  <span class="label">PIN<small>Rotating it signs every device out</small></span>
  <button class="act" id="b-pin">New PIN</button>
</div>

<h2>Connected</h2>
<ul id="connected"></ul>

<h2>Send to them</h2>
<p class="hint" id="s-target">Nothing is connected yet.</p>
<div class="set">
  <span class="label">Files<small id="s-count">Nothing on offer</small></span>
  <button class="act" id="b-share">Choose files…</button>
</div>
<div class="set">
  <span class="label">Clipboard<small>Put what you have copied up for collection</small></span>
  <button class="act" id="b-cliptext">Send clipboard</button>
</div>
<div class="note">
  <textarea id="note" rows="2" placeholder="Or type a note here…"></textarea>
  <button class="act" id="b-note">Send note</button>
</div>
<div class="set">
  <span class="label">Stop<small>Empty the Shared folder and drop the text</small></span>
  <button class="act" id="b-unshare">Stop offering</button>
</div>
<ul id="shared"></ul>

<h2>Received</h2>
<ul id="files"></ul>

<h2>Devices nearby</h2>
<div class="radar">
  <svg id="radar" viewBox="0 0 200 200" aria-label="Nearby Drop devices, placed by how quickly they answer">
    <circle class="ring" cx="100" cy="100" r="92"></circle>
    <circle class="ring" cx="100" cy="100" r="62"></circle>
    <circle class="ring" cx="100" cy="100" r="32"></circle>
    <line class="ring" x1="100" y1="8" x2="100" y2="192"></line>
    <line class="ring" x1="8" y1="100" x2="192" y2="100"></line>
    <g class="sweep">
      <line x1="100" y1="100" x2="100" y2="10" stroke="var(--live)" stroke-width="1.5"></line>
    </g>
    <g id="blips"></g>
  </svg>
  <div class="legend">
    <ul id="peers"></ul>
    <p class="hint">Nearer the middle answers faster. Click a device to send it files.</p>
  </div>
</div>

<footer class="mono" id="foot"></footer>
<div class="toast" id="toast"></div>

<script>
const $ = s => document.querySelector(s);
const esc = t => String(t).replace(/[&<>"]/g, c =>
  ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));

let last = null, busy = false;

function toast(msg){
  const t = $('#toast'); t.textContent = msg; t.classList.add('on');
  setTimeout(() => t.classList.remove('on'), 1400);
}
async function copy(text){
  try { await navigator.clipboard.writeText(text); toast('Copied'); }
  catch { toast(text); }
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

$('#pin').onclick = () => last && copy(last.pin);
$('#b-open').onclick = () => post('/api/control/open');
$('#b-dir').onclick  = () => post('/api/control/dir-picker');
// On blur, not on every keystroke: renaming re-advertises over mDNS.
$('#t-name').onchange = e => {
  const name = e.target.value.trim();
  if (name) post('/api/control/name', {name}); else refresh();
};
$('#b-move').onclick = () => post('/api/control/move-all').then(() => toast('Moved'));
$('#b-pin').onclick  = () => { if (confirm('Generate a new PIN? Every signed-in device is logged out.')) post('/api/control/pin'); };
$('#t-recv').onchange     = e => post('/api/control/receiving', {enabled: e.target.checked});
$('#s-span').onchange     = e => post('/api/control/visible-for', {spec: e.target.value});
$('#s-limit').onchange    = e => post('/api/control/limit', {pct: Number(e.target.value)});
$('#t-tunnel').onchange   = e => post('/api/control/tunnel', {enabled: e.target.checked});
$('#t-move').onchange     = e => post('/api/control/auto-move', {enabled: e.target.checked});
$('#t-approval').onchange = e => post('/api/control/approval', {enabled: e.target.checked});

// Minutes and seconds, as the tray and the CLI show it.
function countdown(sec){
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  const pad = n => String(n).padStart(2, '0');
  return h ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

// What the switch is doing, in the words the tray uses.
function windowNote(s){
  if (s.receiving && s.remaining != null) return countdown(s.remaining) + ' left';
  if (s.receiving) return s.once
    ? 'Until one file arrives' : 'Until you turn it off';
  return s.off_because && s.off_because !== 'off'
    ? 'Off, ' + s.off_because : 'Nothing can reach this computer';
}

// A device keeps the same bearing for as long as it keeps its name, so dots
// do not shuffle between refreshes. omdrop does this off the MAC; Drop has a
// name and an address, which is as stable here.
function bearing(key){
  let h = 0;
  for (const ch of key) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return h;
}

// Round trip to radius: 0 ms sits just off centre, 400 ms or worse is the rim,
// and a device that did not answer at all waits on the rim itself.
function reach(ms){
  if (ms == null) return 1;
  return Math.max(0.12, Math.min(1, Math.sqrt(ms / 400)));
}

// Two machines on the same desk answer in the same millisecond and would be
// drawn on the same spot, one label over the other. Walk a dot outwards along
// its own bearing until it has room, which keeps the ring it sits on roughly
// honest while making both readable.
function place(peers){
  const put = [];
  for (const p of peers){
    let angle = bearing(p.label + p.host) * Math.PI / 180;
    let r = 8 + reach(p.ms) * 84;
    let x = 0, y = 0;
    for (let tries = 0; tries < 14; tries++){
      x = 100 + Math.sin(angle) * r;
      y = 100 - Math.cos(angle) * r;
      // A dot carries its name beside it, so the space it needs is a wide,
      // short box rather than a circle: keep them further apart across than
      // down, or two names sit on one line.
      if (!put.some(q => Math.hypot((q.x - x) / 46, (q.y - y) / 13) < 1)) break;
      angle += 0.5;
      r = Math.min(90, r + 9);
    }
    put.push({x, y, peer: p});
  }
  return put;
}

function drawRadar(peers){
  const blips = $('#blips');
  blips.innerHTML = place(peers).map((d, i) => {
    const flip = d.x > 100 ? 'end' : 'start';
    const tx = d.x + (d.x > 100 ? -7 : 7);
    return `<g class="blip" data-peer="${i}">`
      + `<circle cx="${d.x.toFixed(1)}" cy="${d.y.toFixed(1)}" r="4.5"></circle>`
      + `<text x="${tx.toFixed(1)}" y="${(d.y + 3).toFixed(1)}" text-anchor="${flip}">`
      + `${esc(d.peer.label)}</text></g>`;
  }).join('');
  blips.querySelectorAll('.blip').forEach(g => {
    g.onclick = () => sendTo(peers[Number(g.dataset.peer)]);
  });
}

// Push files straight to another Drop machine, as `drop send` does.
async function sendTo(peer){
  if (!peer) return;
  toast('Choose files for ' + peer.label);
  const r = await fetch('/api/control/send-to', {
    method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({host: peer.host, port: peer.port, label: peer.label}),
  });
  const out = await r.json().catch(() => ({}));
  if (out.cancelled) return;
  toast(r.ok ? 'Sent to ' + peer.label : (out.error || 'Could not send'));
  refresh();
}

function bytes(n){
  if (n < 1024) return n + ' B';
  const u = ['KB','MB','GB']; let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return n.toFixed(1) + ' ' + u[i];
}

async function decide(id, accept){
  await post('/api/control/offer/' + encodeURIComponent(id), {accept});
  toast(accept ? 'Accepted' : 'Declined');
}

function renderOffers(list){
  const box = $('#offers');
  if (!list || !list.length){ box.innerHTML = ''; box.dataset.ids = ''; return; }

  // Only repaint when the set changes, so the buttons stay clickable
  // through the two-second refresh.
  const ids = list.map(o => o.id).join(',');
  if (box.dataset.ids === ids) return;
  box.dataset.ids = ids;

  box.innerHTML = list.map(o => {
    const n = o.files.length;
    const size = o.total ? ' · ' + bytes(o.total) : '';
    return `<div class="offer">
      <h3><span class="from">${esc(o.device)}</span> wants to send you
        ${n} ${n === 1 ? 'file' : 'files'}</h3>
      <div class="sub">${esc(String(n))} ${n === 1 ? 'item' : 'items'}${size}</div>
      <ul>${o.files.map(f =>
        `<li><span class="name">${esc(f.name)}</span>`
        + `<span class="meta mono">${f.size ? bytes(f.size) : ''}</span></li>`).join('')}</ul>
      <div class="btns">
        <button class="no"  data-no="${esc(o.id)}">Decline</button>
        <button class="yes" data-yes="${esc(o.id)}">Accept</button>
      </div>
    </div>`;
  }).join('');

  box.querySelectorAll('[data-yes]').forEach(b =>
    b.onclick = () => decide(b.dataset.yes, true));
  box.querySelectorAll('[data-no]').forEach(b =>
    b.onclick = () => decide(b.dataset.no, false));
}

async function refresh(){
  if (busy) return;
  let s;
  try { s = await (await fetch('/api/status')).json(); } catch { return; }
  last = s;

  $('#who').textContent = s.name;
  $('#pin').textContent = s.pin;
  $('#foot').textContent = s.dir;
  $('#dir-note').textContent = s.files_waiting
    ? s.files_waiting + ' waiting in ' + s.dir : 'Empty';

  document.body.classList.toggle('open', !!s.receiving);
  $('#recv-state').textContent = s.receiving ? 'Receiving' : 'Not receiving';
  $('#recv-note').textContent = windowNote(s);
  if (document.activeElement !== $('#t-recv')) $('#t-recv').checked = !!s.receiving;

  // The choices come from the daemon, so there is one list of them.
  const sel = $('#s-span');
  const specs = (s.spans || []).map(o => o.spec).join(',');
  if (sel.dataset.specs !== specs){
    sel.dataset.specs = specs;
    sel.innerHTML = (s.spans || []).map(o =>
      `<option value="${esc(o.spec)}">${esc(o.label)}</option>`).join('');
  }
  if (document.activeElement !== sel) sel.value = s.visible_for || 'always';

  if (document.activeElement !== $('#t-name')) $('#t-name').value = s.name || '';
  $('#dir-pick-note').textContent = s.dir || '';

  const lim = $('#s-limit');
  if (document.activeElement !== lim) lim.value = String(s.limit_pct ?? 30);
  $('#limit-note').textContent = s.limit_bytes != null
    ? bytes(s.limit_bytes) + ' right now, of ' + bytes(s.free_bytes) + ' free'
    : (s.free_bytes != null ? bytes(s.free_bytes) + ' free' : '');

  const pub = s.tunnel_url;
  const ap = $('#a-public');
  ap.hidden = !pub;
  if (pub){
    ap.innerHTML = '<b>Anywhere — works on mobile data</b>' + esc(pub);
    ap.onclick = () => copy(pub);
  }
  $('#a-local').innerHTML = '<b>This network only</b>' + esc(s.local_url);
  $('#a-local').onclick = () => copy(s.local_url);

  // Cache-bust: the QR must follow whichever URL is current.
  const target = pub || s.local_url;
  if ($('#qr').dataset.for !== target){
    $('#qr').dataset.for = target;
    $('#qr').src = '/panel/qr.svg?v=' + encodeURIComponent(target);
  }

  renderOffers(s.offers);

  if (document.activeElement !== $('#t-tunnel'))   $('#t-tunnel').checked   = s.tunnel_enabled;
  if (document.activeElement !== $('#t-move'))     $('#t-move').checked     = s.auto_move;
  if (document.activeElement !== $('#t-approval')) $('#t-approval').checked = s.require_approval;
  $('#tunnel-note').textContent = s.tunnel_enabled
    ? (pub ? 'On — ' + pub.replace('https://','') : 'Starting…')
    : 'Off — this network only';
  $('#move-note').textContent = 'Into ' + s.move_target;

  $('#files').innerHTML = s.files.length
    ? s.files.map(f => `<li><span class="name">${esc(f.name)}</span>`
        + `<span class="meta mono">${esc(f.size)} · ${esc(f.when)}</span></li>`).join('')
    : '<li class="empty">Nothing yet.</li>';

  const nearby = s.peers_list || [];
  drawRadar(nearby);
  $('#peers').innerHTML = nearby.length
    ? nearby.map((p, i) => `<li data-peer="${i}"><span class="name">${esc(p.label)}</span>`
        + `<span class="meta mono">${esc(p.host)}:${p.port}`
        + `${p.ms != null ? ' · ' + p.ms + ' ms' : ' · no answer'}</span></li>`).join('')
    : '<li class="empty">No other devices advertising.</li>';
  $('#peers').querySelectorAll('li[data-peer]').forEach(li => {
    li.onclick = () => sendTo(nearby[Number(li.dataset.peer)]);
  });

  drawConnected(s.connected || [], Math.floor(Date.now() / 1000));

  const shared = s.shared_files || [];
  const hasText = !!(s.shared_text && s.shared_text.length);
  const offering = shared.length + (hasText ? 1 : 0);
  $('#s-count').textContent = offering
    ? offering + (offering === 1 ? ' item waiting to be collected' : ' items waiting to be collected')
    : 'Nothing on offer';
  $('#b-unshare').disabled = !offering;

  const rows = shared.map(f =>
    `<li><span class="name">${esc(f.name)}</span>`
    + `<span class="meta mono">${esc(f.size)}</span></li>`);
  if (hasText){
    const one = s.shared_text.split('\n')[0].slice(0, 60);
    rows.unshift(`<li><span class="name">“${esc(one)}”</span>`
      + `<span class="meta mono">text</span></li>`);
  }
  $('#shared').innerHTML = rows.length ? rows.join('')
    : '<li class="empty">Choose files, or drop them in the Shared folder.</li>';
}

function ago(secs){
  if (secs < 60) return 'just now';
  const m = Math.floor(secs / 60);
  if (m < 60) return m + (m === 1 ? ' min ago' : ' mins ago');
  const h = Math.floor(m / 60);
  return h + (h === 1 ? ' hour ago' : ' hours ago');
}

function drawConnected(list, now){
  const box = $('#connected');
  box.innerHTML = list.length
    ? list.map(c =>
        `<li class="live"><span class="dot"></span>`
        + `<span class="name">${esc(c.device)}</span>`
        + `<span class="meta mono">${esc(c.ip)} · since ${esc(ago(now - c.since))}</span></li>`).join('')
    : '<li class="empty">Nothing connected. Scan the QR and enter the PIN on the phone.</li>';

  // Name who is going to collect whatever gets shared.
  const target = $('#s-target');
  if (!list.length){
    target.textContent = 'Nothing is connected yet — anything you share here waits until something is.';
  } else if (list.length === 1){
    target.textContent = list[0].device + ' is connected and will see anything you put up here.';
  } else {
    target.textContent = list.map(c => c.device).join(', ') + ' are connected.';
  }
}

$('#b-share').onclick    = () => post('/api/control/share-files-picker');
$('#b-cliptext').onclick = () => post('/api/control/share-clipboard');
$('#b-unshare').onclick  = () => post('/api/control/unshare');
$('#b-note').onclick     = async () => {
  const text = $('#note').value;
  if (!text.trim()) return;
  await post('/api/control/share-text', {text});
  $('#note').value = '';
  toast('Note shared');
};

refresh(); setInterval(refresh, 2000);
</script>
</body></html>
"####;

use anyhow::Result;
use std::process::Command;

/// Open the Drop window.
///
/// Chromium-family browsers have `--app=`, which gives a plain window with no
/// tabs or address bar — near enough to a native app. Everything else falls
/// back to a normal window.
pub fn open(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/panel");

    for (bin, args) in [
        ("chromium", vec![format!("--app={url}")]),
        ("chromium-browser", vec![format!("--app={url}")]),
        ("google-chrome", vec![format!("--app={url}")]),
        ("brave-browser", vec![format!("--app={url}")]),
        ("microsoft-edge", vec![format!("--app={url}")]),
    ] {
        if which(bin).is_some() {
            Command::new(bin).args(args).spawn()?;
            return Ok(());
        }
    }

    Command::new("xdg-open").arg(&url).spawn()?;
    Ok(())
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}
