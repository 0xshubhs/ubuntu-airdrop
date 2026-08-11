#!/usr/bin/env python3
"""
drop.py - a small AirDrop-alike for your own LAN.

  ./drop.py serve            start receiving (and show the web page)
  ./drop.py peers            list devices currently advertising
  ./drop.py send <who> FILE  push files to a peer

Discovery is mDNS (_mydrop._tcp.local.), transport is HTTP multipart.
"""

import argparse
import os
import random
import re
import socket
import sys
import threading
import time
from pathlib import Path

SERVICE_TYPE = "_mydrop._tcp.local."
DEFAULT_PORT = 8420
CHUNK = 1024 * 256


# --------------------------------------------------------------------------
# small helpers
# --------------------------------------------------------------------------

def lan_ip() -> str:
    """Best-guess outward-facing IP. Opens a UDP socket but sends nothing."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("192.0.2.1", 53))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def safe_name(raw: str, folder: Path) -> Path:
    """Strip paths, keep it boring, never overwrite."""
    base = Path(raw or "untitled").name
    base = re.sub(r"[^\w.\- ()\[\]]", "_", base).strip(". ") or "untitled"
    target = folder / base
    if not target.exists():
        return target
    stem, suffix = target.stem, target.suffix
    for n in range(2, 10_000):
        candidate = folder / f"{stem} ({n}){suffix}"
        if not candidate.exists():
            return candidate
    return folder / f"{stem}-{int(time.time())}{suffix}"


def human_size(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024.0
    return f"{n:.1f} GB"


# --------------------------------------------------------------------------
# mDNS
# --------------------------------------------------------------------------

class PeerBook:
    """Thread-safe registry of peers seen on the network."""

    def __init__(self, exclude: str = ""):
        self._peers: dict[str, dict] = {}
        self._lock = threading.Lock()
        self.exclude = exclude

    # zeroconf calls these from its own thread
    def add_service(self, zc, type_, name):
        info = zc.get_service_info(type_, name, timeout=2000)
        if not info or not info.addresses:
            return
        props = {
            k.decode(): v.decode()
            for k, v in (info.properties or {}).items()
            if k and v
        }
        label = props.get("name") or name.split(".")[0]
        if label == self.exclude:
            return
        with self._lock:
            self._peers[name] = {
                "label": label,
                "host": socket.inet_ntoa(info.addresses[0]),
                "port": info.port,
                "os": props.get("os", "?"),
            }

    def update_service(self, zc, type_, name):
        self.add_service(zc, type_, name)

    def remove_service(self, zc, type_, name):
        with self._lock:
            self._peers.pop(name, None)

    def all(self) -> list[dict]:
        with self._lock:
            return sorted(self._peers.values(), key=lambda p: p["label"].lower())

    def find(self, needle: str) -> dict | None:
        needle = needle.lower()
        for p in self.all():
            if p["label"].lower() == needle:
                return p
        for p in self.all():
            if needle in p["label"].lower():
                return p
        return None


def browse(exclude: str = "") -> tuple["PeerBook", object, object]:
    from zeroconf import ServiceBrowser, Zeroconf

    zc = Zeroconf()
    book = PeerBook(exclude=exclude)
    browser = ServiceBrowser(zc, SERVICE_TYPE, book)
    return book, zc, browser


def advertise(zc, name: str, port: int):
    from zeroconf import ServiceInfo

    info = ServiceInfo(
        SERVICE_TYPE,
        f"{name}.{SERVICE_TYPE}",
        addresses=[socket.inet_aton(lan_ip())],
        port=port,
        properties={
            "name": name,
            "os": sys.platform,
            "v": "1",
        },
        # NOT "<name>.local." — Avahi/mDNSResponder already owns the plain
        # hostname on most machines and we'd lose the conflict check.
        server=f"{re.sub(r'[^a-zA-Z0-9-]', '-', name)}-drop.local.",
    )
    zc.register_service(info, allow_name_change=True)
    return info


# --------------------------------------------------------------------------
# the web page
# --------------------------------------------------------------------------

PAGE = r"""<!doctype html>
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
  #zone:hover,#zone.hot{border-color:var(--signal); background:color-mix(in srgb,var(--signal) 6%,transparent)}
  #zone strong{display:block; font-size:1rem; margin-bottom:.2rem}
  #zone span{color:var(--muted); font-size:.85rem}
  #pin{
    font-size:1rem; letter-spacing:.35em; width:7.5em; padding:.5rem .6rem; text-align:center;
    border:1px solid var(--rule); background:transparent; color:inherit; margin-top:1rem;
  }
  #pin:focus{outline:2px solid var(--signal); outline-offset:1px}
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
  footer{margin-top:2.5rem; color:var(--muted); font-size:.78rem}
</style>
</head><body>

<header>
  <h1>Drop</h1>
  <span class="who mono">__NAME__ &middot; __ADDR__</span>
</header>

<div id="zone" tabindex="0" role="button">
  <strong>Choose files to send here</strong>
  <span>or drag them onto this box</span>
</div>
<input id="picker" type="file" multiple hidden>
<div id="bar"></div>
<div id="pinwrap"><input id="pin" class="mono" inputmode="numeric" maxlength="6"
   placeholder="PIN" aria-label="Six digit PIN shown in the terminal"></div>

<h2>Received</h2>
<ul id="files"></ul>

<h2>Devices on this network</h2>
<ul id="peers"></ul>

<footer class="mono">Saving to __DIR__</footer>

<script>
const $ = s => document.querySelector(s);
const zone = $('#zone'), picker = $('#picker'), bar = $('#bar');

if (__NOPIN__) $('#pinwrap').remove();
else picker.addEventListener('click', e => { if (!$('#pin').value) { e.preventDefault();
       $('#pin').focus(); } });

zone.addEventListener('click', () => picker.click());
zone.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') picker.click(); });
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
  const pinEl = $('#pin');
  if (pinEl) xhr.setRequestHeader('X-Drop-Pin', pinEl.value.trim());
  xhr.upload.onprogress = e => {
    if (e.lengthComputable) bar.style.width = (e.loaded / e.total * 100) + '%';
  };
  xhr.onload = () => {
    bar.style.width = '0';
    picker.value = '';
    if (xhr.status === 401) { alert('Wrong PIN. Check the terminal on the receiving device.'); return; }
    if (xhr.status !== 200) { alert('Transfer failed: ' + xhr.status); return; }
    refresh();
  };
  xhr.onerror = () => { bar.style.width = '0'; alert('Lost connection during transfer.'); };
  xhr.send(form);
}

async function refresh(){
  let s;
  try { s = await (await fetch('/api/state')).json(); } catch { return; }

  $('#files').innerHTML = s.files.length
    ? s.files.map(f => `<li><a href="/files/${encodeURIComponent(f.name)}" download>${esc(f.name)}</a>`
        + `<span class="meta mono">${f.size} &middot; ${f.when}</span></li>`).join('')
    : '<li class="empty">Nothing yet.</li>';

  $('#peers').innerHTML = s.peers.length
    ? s.peers.map(p => `<li><span class="dot"></span><span>${esc(p.label)}</span>`
        + `<span class="meta mono">${esc(p.host)}:${p.port}</span></li>`).join('')
    : '<li class="empty">No other devices advertising right now.</li>';
}

const esc = t => t.replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
refresh(); setInterval(refresh, 2500);
</script>
</body></html>
"""


# --------------------------------------------------------------------------
# serve
# --------------------------------------------------------------------------

def cmd_serve(args):
    from fastapi import FastAPI, File, Header, HTTPException, UploadFile
    from fastapi.responses import FileResponse, HTMLResponse, JSONResponse
    import uvicorn

    folder = Path(args.dir).expanduser()
    folder.mkdir(parents=True, exist_ok=True)

    name = args.name or socket.gethostname().split(".")[0]
    pin = None if args.no_pin else (args.pin or f"{random.randrange(10**6):06d}")

    book, zc, _browser = browse(exclude=name)
    info = advertise(zc, name, args.port)

    app = FastAPI(docs_url=None, redoc_url=None)

    def check(supplied: str | None):
        if pin and (supplied or "").strip() != pin:
            raise HTTPException(401, "bad pin")

    @app.get("/", response_class=HTMLResponse)
    def index():
        return (PAGE
                .replace("__NAME__", name)
                .replace("__ADDR__", f"{lan_ip()}:{args.port}")
                .replace("__DIR__", str(folder))
                .replace("__NOPIN__", "true" if pin is None else "false"))

    @app.get("/api/state")
    def state():
        files = []
        for p in sorted(folder.iterdir(), key=lambda p: -p.stat().st_mtime):
            if p.is_file() and not p.name.startswith("."):
                st = p.stat()
                files.append({
                    "name": p.name,
                    "size": human_size(st.st_size),
                    "when": time.strftime("%H:%M", time.localtime(st.st_mtime)),
                })
        return {"device": name, "peers": book.all(), "files": files[:60]}

    @app.post("/api/upload")
    async def upload(
        files: list[UploadFile] = File(...),
        x_drop_pin: str | None = Header(default=None),
    ):
        check(x_drop_pin)
        saved = []
        for up in files:
            target = safe_name(up.filename, folder)
            total = 0
            with open(target, "wb") as out:
                while chunk := await up.read(CHUNK):
                    out.write(chunk)
                    total += len(chunk)
            saved.append(target.name)
            print(f"  <- {target.name}  ({human_size(total)})", flush=True)
        return JSONResponse({"saved": saved})

    @app.get("/files/{fname}")
    def download(fname: str):
        target = (folder / Path(fname).name).resolve()
        if target.parent != folder.resolve() or not target.is_file():
            raise HTTPException(404, "no such file")
        return FileResponse(target, filename=target.name)

    url = f"http://{lan_ip()}:{args.port}"
    print(f"\n  Drop  —  {name}")
    print(f"  {url}")
    print(f"  saving to {folder}")
    print(f"  PIN {pin}" if pin else "  no PIN (open to your LAN)")
    print("  Ctrl-C to stop\n", flush=True)

    try:
        uvicorn.run(app, host="0.0.0.0", port=args.port, log_level="warning")
    except KeyboardInterrupt:
        pass
    finally:
        zc.unregister_service(info)
        zc.close()


# --------------------------------------------------------------------------
# peers / send
# --------------------------------------------------------------------------

def cmd_peers(args):
    me = socket.gethostname().split(".")[0]
    book, zc, _b = browse(exclude=me)
    print(f"listening for {args.wait:.0f}s ...")
    time.sleep(args.wait)
    peers = book.all()
    zc.close()
    if not peers:
        print("nothing found. is drop.py serve running on the other machine?")
        return
    for p in peers:
        print(f"  {p['label']:<22} {p['host']}:{p['port']}  ({p['os']})")


def cmd_send(args):
    import httpx

    paths = []
    for raw in args.files:
        p = Path(raw).expanduser()
        if not p.is_file():
            sys.exit(f"not a file: {p}")
        paths.append(p)

    # target may be host:port, a bare IP, or a peer name to resolve via mDNS
    target = args.to
    if re.match(r"^\d{1,3}(\.\d{1,3}){3}(:\d+)?$", target) or target.startswith("http"):
        host_port = target.split("//")[-1]
        if ":" not in host_port:
            host_port += f":{DEFAULT_PORT}"
    else:
        book, zc, _b = browse()
        time.sleep(args.wait)
        peer = book.find(target)
        zc.close()
        if not peer:
            sys.exit(f"no peer matching '{target}'. try: drop.py peers")
        host_port = f"{peer['host']}:{peer['port']}"
        print(f"-> {peer['label']} at {host_port}")

    pin = args.pin
    if pin is None:
        pin = input("PIN (blank if disabled): ").strip()

    files, handles = [], []
    try:
        for p in paths:
            fh = open(p, "rb")
            handles.append(fh)
            files.append(("files", (p.name, fh, "application/octet-stream")))
        r = httpx.post(
            f"http://{host_port}/api/upload",
            files=files,
            headers={"X-Drop-Pin": pin},
            timeout=None,
        )
    finally:
        for fh in handles:
            fh.close()

    if r.status_code == 401:
        sys.exit("rejected: wrong PIN")
    r.raise_for_status()
    for n in r.json().get("saved", []):
        print(f"   sent {n}")


# --------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="AirDrop-ish file transfer over your LAN")
    sub = ap.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("serve", help="receive files")
    s.add_argument("--port", type=int, default=DEFAULT_PORT)
    s.add_argument("--name", help="device name (default: hostname)")
    s.add_argument("--dir", default=str(Path.home() / "Drop"))
    s.add_argument("--pin", help="fixed PIN instead of a random one")
    s.add_argument("--no-pin", action="store_true", help="accept anything on the LAN")
    s.set_defaults(func=cmd_serve)

    p = sub.add_parser("peers", help="list devices advertising right now")
    p.add_argument("--wait", type=float, default=2.0)
    p.set_defaults(func=cmd_peers)

    d = sub.add_parser("send", help="push files to a peer")
    d.add_argument("to", help="peer name, IP, or host:port")
    d.add_argument("files", nargs="+")
    d.add_argument("--pin")
    d.add_argument("--wait", type=float, default=2.0)
    d.set_defaults(func=cmd_send)

    args = ap.parse_args()
    try:
        args.func(args)
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
