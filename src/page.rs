//! The receiving page. Served at `/`, gated behind the PIN.

pub const PAGE: &str = r####"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Drop &middot; __NAME__</title>
<style>
  :root{
    --paper:#FAFAF7; --ink:#16181A; --rule:#D9D6CD;
    --muted:#6E6C66; --signal:#B3402C; --live:#2F6F4F;
  }
  @media (prefers-color-scheme: dark){
    :root{ --paper:#131416; --ink:#E8E6E1; --rule:#2C2E31;
           --muted:#8D8B85; --signal:#E0654C; --live:#5FB98A; }
  }
  *{box-sizing:border-box}
  body{
    margin:0; padding:2rem 1.25rem 4rem; background:var(--paper); color:var(--ink);
    font:15px/1.55 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif;
    max-width:44rem; margin-inline:auto; -webkit-text-size-adjust:100%;
  }
  .mono{font-family:ui-monospace,SFMono-Regular,"SF Mono",Menlo,monospace;
        font-variant-numeric:tabular-nums}
  header{border-bottom:2px solid var(--ink); padding-bottom:.6rem; margin-bottom:1.5rem;
         display:flex; align-items:baseline; justify-content:space-between; gap:1rem}
  h1{font-size:1.05rem; margin:0; letter-spacing:.16em; text-transform:uppercase; font-weight:650}
  header .who{font-size:.8rem; color:var(--muted)}
  h2{font-size:.7rem; letter-spacing:.14em; text-transform:uppercase; color:var(--muted);
     margin:2rem 0 .6rem; font-weight:600}
  #zone{
    border:1.5px dashed var(--rule); padding:2.2rem 1rem; text-align:center;
    cursor:pointer; transition:border-color .15s, background .15s;
  }
  #zone:hover,#zone.hot{border-color:var(--signal);
    background:color-mix(in srgb,var(--signal) 6%,transparent)}
  #zone strong{display:block; font-size:1rem; margin-bottom:.2rem}
  #zone span{color:var(--muted); font-size:.85rem}
  input[type=password],input[type=text]{
    font-size:1.15rem; letter-spacing:.35em; width:8em; padding:.6rem; text-align:center;
    border:1px solid var(--rule); background:transparent; color:inherit;
    font-family:ui-monospace,monospace;
  }
  input:focus{outline:2px solid var(--signal); outline-offset:1px}
  button{
    font:inherit; padding:.6rem 1.4rem; border:1px solid var(--ink); cursor:pointer;
    background:var(--ink); color:var(--paper); margin-left:.5rem;
  }
  button:hover{background:var(--signal); border-color:var(--signal)}
  ul{list-style:none; margin:0; padding:0}
  li{display:flex; align-items:baseline; gap:.75rem; padding:.55rem 0;
     border-bottom:1px solid var(--rule)}
  li a{color:inherit; text-decoration:none; flex:1; min-width:0;
       overflow:hidden; text-overflow:ellipsis; white-space:nowrap}
  li a:hover{color:var(--signal); text-decoration:underline}
  li .meta{color:var(--muted); font-size:.8rem; white-space:nowrap}
  .dot{width:7px; height:7px; border-radius:50%; background:var(--live); flex:none}
  @media (prefers-reduced-motion:no-preference){
    .dot{animation:pulse 2.4s ease-in-out infinite}
  }
  @keyframes pulse{0%,100%{opacity:1}50%{opacity:.25}}
  .empty{color:var(--muted); font-size:.85rem; padding:.55rem 0}
  #bar{height:2px; background:var(--signal); width:0; transition:width .1s; margin-top:1rem}
  #held{border:1.5px solid var(--signal); padding:1.1rem; text-align:center;
        margin-bottom:1rem; transition:border-color .2s}
  #held strong{display:block; font-size:.95rem; margin-bottom:.15rem}
  #held span{color:var(--muted); font-size:.85rem}
  #held.yes{border-color:var(--live)} #held.yes strong{color:var(--live)}
  #held.no strong{color:var(--signal)}
  @media (prefers-reduced-motion:no-preference){
    #held.wait strong::after{content:""; display:inline-block; width:.5em;
      animation:dots 1.2s steps(4,end) infinite}
  }
  @keyframes dots{0%{content:""}25%{content:"."}50%{content:".."}75%{content:"..."}}
  footer{margin-top:2.5rem; color:var(--muted); font-size:.78rem}
  #gate{text-align:center; padding:3rem 0}
  #gate p{color:var(--muted); font-size:.85rem; margin:.4rem 0 1.6rem}
  .err{color:var(--signal); font-size:.85rem; min-height:1.4em; margin-top:1rem}
  [hidden]{display:none !important}
</style>
</head><body>

<header>
  <h1>Drop</h1>
  <span class="who mono">__NAME__ &middot; __ADDR__</span>
</header>

<section id="gate" hidden>
  <strong>Enter the PIN</strong>
  <p>Shown in the Drop menu, top-right of the desktop.</p>
  <form id="gateform">
    <input id="pin" type="password" inputmode="numeric" maxlength="6"
           autocomplete="one-time-code" placeholder="000000" aria-label="Six digit PIN">
    <button type="submit">Unlock</button>
  </form>
  <div class="err" id="gateerr"></div>
</section>

<main id="app" hidden>
  <div id="held" hidden>
    <strong id="held-title">Waiting to be accepted…</strong>
    <span id="held-sub">The other device has to approve this transfer.</span>
  </div>

  <div id="zone" tabindex="0" role="button">
    <strong>Choose files to send here</strong>
    <span>or drag them onto this box</span>
  </div>
  <input id="picker" type="file" multiple hidden>
  <div id="bar"></div>

  <h2>Received</h2>
  <ul id="files"></ul>

  <h2>Devices on this network</h2>
  <ul id="peers"></ul>

  <footer class="mono">Saving to __DIR__</footer>
</main>

<script>
const $ = s => document.querySelector(s);
const esc = t => String(t).replace(/[&<>"]/g, c =>
  ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));

let timer = null;

function showGate(msg){
  $('#gate').hidden = false; $('#app').hidden = true;
  $('#gateerr').textContent = msg || '';
  if (timer) { clearInterval(timer); timer = null; }
  $('#pin').focus();
}
function showApp(){
  $('#gate').hidden = true; $('#app').hidden = false;
  if (!timer) timer = setInterval(refresh, 2500);
}

$('#gateform').addEventListener('submit', async e => {
  e.preventDefault();
  const pin = $('#pin').value.trim();
  if (!pin) return;
  const r = await fetch('/api/auth', {
    method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({pin})
  });
  if (r.ok) { $('#pin').value=''; showApp(); refresh(); return; }
  const body = await r.json().catch(() => ({}));
  showGate(body.error || 'Wrong PIN.');
});

const zone = $('#zone'), picker = $('#picker'), bar = $('#bar');
zone.addEventListener('click', () => picker.click());
zone.addEventListener('keydown', e => {
  if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); picker.click(); }
});
picker.addEventListener('change', () => upload(picker.files));

['dragenter','dragover'].forEach(t => zone.addEventListener(t, e => {
  e.preventDefault(); zone.classList.add('hot');
}));
['dragleave','drop'].forEach(t => zone.addEventListener(t, e => {
  e.preventDefault(); zone.classList.remove('hot');
}));
zone.addEventListener('drop', e => upload(e.dataTransfer.files));

function upload(list){
  if (!list || !list.length) return;
  const form = new FormData();
  for (const f of list) form.append('files', f, f.name);
  const xhr = new XMLHttpRequest();
  xhr.open('POST', '/api/upload');
  xhr.upload.onprogress = e => {
    if (e.lengthComputable) bar.style.width = (e.loaded / e.total * 100) + '%';
  };
  xhr.onload = () => {
    bar.style.width = '0'; picker.value = '';
    if (xhr.status === 401) { showGate('Session expired. Enter the PIN again.'); return; }
    // 202: the receiver has the bytes but has not accepted them yet.
    if (xhr.status === 202) {
      let id = null;
      try { id = JSON.parse(xhr.responseText).offer; } catch {}
      waitForApproval(id);
      return;
    }
    if (xhr.status !== 200) { alert('Transfer failed: ' + xhr.status); return; }
    refresh();
  };
  xhr.onerror = () => { bar.style.width = '0'; alert('Lost connection during transfer.'); };
  xhr.send(form);
}

// Follow one offer to its verdict, so the sender sees what actually happened
// rather than a status code.
function waitForApproval(id){
  const box = $('#held'), title = $('#held-title'), sub = $('#held-sub');
  box.hidden = false;
  box.className = 'wait';
  title.textContent = 'Waiting to be accepted';
  sub.textContent = 'The other device has to approve this transfer.';

  const settle = (cls, head, tail, keep) => {
    box.className = cls;
    title.textContent = head;
    sub.textContent = tail;
    refresh();
    setTimeout(() => { box.hidden = true; box.className = ''; }, keep);
  };

  // No id means an older receiver: fall back to a plain notice.
  if (!id){
    setTimeout(() => { box.hidden = true; refresh(); }, 4000);
    return;
  }

  let ticks = 0;
  const timer = setInterval(async () => {
    if (++ticks > 200){                       // ~5 min, past the offer TTL
      clearInterval(timer);
      settle('no', 'No response', 'The transfer expired without an answer.', 6000);
      return;
    }
    let s;
    try { s = await (await fetch('/api/offer/' + encodeURIComponent(id))).json(); }
    catch { return; }

    if (s.status === 'accepted' || s.status === 'complete'){
      clearInterval(timer);
      settle('yes', 'Accepted', 'Your files are on the other device.', 4000);
    } else if (s.status === 'declined'){
      clearInterval(timer);
      settle('no', 'Declined', 'The other device turned this transfer down.', 6000);
    } else if (s.status === 'expired'){
      clearInterval(timer);
      settle('no', 'No response', 'The transfer expired without an answer.', 6000);
    }
  }, 1500);
}

async function refresh(){
  let r;
  try { r = await fetch('/api/state'); } catch { return; }
  if (r.status === 401) { showGate(); return; }
  if (!r.ok) return;
  const s = await r.json();
  showApp();

  $('#files').innerHTML = s.files.length
    ? s.files.map(f =>
        `<li><a href="/files/${encodeURIComponent(f.name)}" download>${esc(f.name)}</a>`
        + `<span class="meta mono">${esc(f.size)} &middot; ${esc(f.when)}</span></li>`).join('')
    : '<li class="empty">Nothing yet.</li>';

  $('#peers').innerHTML = s.peers.length
    ? s.peers.map(p =>
        `<li><span class="dot"></span><span>${esc(p.label)}</span>`
        + `<span class="meta mono">${esc(p.host)}:${p.port}</span></li>`).join('')
    : '<li class="empty">No other devices advertising right now.</li>';
}

refresh();
</script>
</body></html>
"####;

pub fn render(name: &str, addr: &str, dir: &str) -> String {
    PAGE.replace("__NAME__", &escape(name))
        .replace("__ADDR__", &escape(addr))
        .replace("__DIR__", &escape(dir))
}

/// The device name comes from config, but it still ends up inside HTML.
fn escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
