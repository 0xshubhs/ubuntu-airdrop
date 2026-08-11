# ubuntu-airdrop

AirDrop-ish file transfer for your own network. Push files from an iPhone, a Mac, or
another Linux box to Ubuntu — with a desktop indicator that shows the PIN, a QR code,
and what just arrived.

A single 3 MB Rust binary. One dependency: `libc6`.

```
  advertise  ──▶  _mydrop._tcp.local.      (mDNS / Avahi / Bonjour)
  discover   ──▶  browse the same type, collect host:port
  transfer   ──▶  POST /api/upload, multipart, streamed to disk
  reach      ──▶  optional Cloudflare tunnel for off-LAN
```

---

## Install

```bash
sudo dpkg -i drop_0.1.0-1_amd64.deb
```

Then, without logging out:

```bash
systemctl --user start drop
/usr/bin/drop tray &
```

The package enables the service for every user (`systemctl --global enable`) and drops a
desktop-autostart entry for the tray, so from your next login both come up on their own.

To keep receiving while logged out and from boot:

```bash
sudo loginctl enable-linger $USER
```

### Build it yourself

```bash
cargo build --release          # binary at target/release/drop
cargo deb                      # package at target/debian/
```

## The indicator

Click the Drop icon in the top-right:

```
  Receiving as "keshava"
  ─────────────────────────
  PIN  481902                   ← click to copy
  http://192.168.0.113:8420     ← click to copy
  Internet: xyz.trycloudflare.com
  Show QR code…                 ← scan from the phone
  ─────────────────────────
  3 files waiting
  1 other device nearby
  Open Drop folder
  Move all to Downloads
  ☐ Always move to Downloads
  ─────────────────────────
  ☐ Reachable from the internet
  New PIN
  Quit
```

**Always move to Downloads** is the auto-file option: each finished transfer is moved out
of `~/Drop` into `~/Downloads` the moment it lands, so the Drop folder stays empty.
**Move all to Downloads** does the same on demand. Moves fall back to copy-then-delete
across filesystems.

## CLI

```bash
drop status                     # PIN, address, current settings
drop peers                      # other devices advertising
drop send thinkpad ~/photo.jpg  # push to a peer by name, IP, or host:port
drop pin                        # new PIN, invalidates live sessions
drop serve                      # run the daemon in the foreground
drop tray                       # run the indicator

systemctl --user status drop
journalctl --user -u drop -f    # watch files land
```

## iPhone: the Share Sheet

1. **Shortcuts** → **+** → name it `Drop to Ubuntu`.
2. ⓘ → **Show in Share Sheet**. Under *Share Sheet Types* keep only **Files, Images,
   Media, URLs**.
3. Add **Get Contents of URL**:
   - URL: `http://keshava.local:8420/api/upload` *(your hostname)*
   - Method **POST**
   - Header `X-Drop-Pin` = your PIN
   - Request Body **Form** → add field, type **File**, key `files`, value **Shortcut Input**

Any photo, file, or webpage → Share → *Drop to Ubuntu*.

Two things that bite:

- On first run iOS asks for **Local Network** permission. Deny it and the request fails
  with nothing useful in the log. Settings → Shortcuts → Local Network.
- Prefer `http://<hostname>.local:8420` over a hardcoded IP — it survives the laptop
  moving between Wi-Fi and Ethernet, or getting a new DHCP lease.

This will never appear in the iPhone's **AirDrop** list. AirDrop is AWDL, a proprietary
Apple radio protocol; the only open implementation needs a Wi-Fi card with raw frame
injection (Broadcom/nexmon, some Atheros) and is broken against current iOS. The Share
Sheet entry sits one row below the AirDrop row, and that is the ceiling on Linux.

## macOS: a Finder Quick Action

**Automator** → **Quick Action** → *receives files or folders in Finder* → **Run Shell
Script**, *Pass input* as **arguments**:

```bash
for f in "$@"; do
  /usr/bin/curl -s -F "files=@$f" -H "X-Drop-Pin: YOUR-PIN" \
    http://keshava.local:8420/api/upload
done
```

Bind a hotkey in System Settings → Keyboard → Shortcuts.

## Reaching it from outside the LAN

Tick **Reachable from the internet** and the daemon starts a Cloudflare quick tunnel
(`cloudflared` must be installed; no Cloudflare account needed) and shows the public
`https://…trycloudflare.com` URL in the menu. Quick-tunnel hostnames are random and
change on every restart, which is why the menu offers a QR code instead of asking you to
type one.

**This puts your receiver on the public internet.** The PIN is then the only thing in
front of your Drop folder, so:

- every route is gated — page, listing, uploads, downloads
- a wrong PIN costs the caller an exponential lockout, from 30 s up to 15 min, per IP
- sessions are HMAC-signed cookies; `New PIN` rotates the signing key and logs everyone out

Off by default. Leave it off unless you need it.

## Security notes

Auth is a six-digit PIN, sent either as `X-Drop-Pin` (Shortcuts, `curl`) or exchanged for
a signed session cookie (browser). Every route except the PIN prompt itself requires it.
Control endpoints (`/api/status`, `/api/control/*`) additionally refuse anything that is
not loopback, so the tray can drive them but the network cannot.

Uploaded filenames are stripped to their final path component and sanitised, and
downloads are canonicalised and confirmed to be inside the Drop folder before opening.

**Transfers are plaintext HTTP on the LAN.** Anyone on the same network can read them.
Through the tunnel they are HTTPS to Cloudflare, which terminates TLS and can see them.

## Where to take it next

**Sender identity, not just a PIN.** A keypair per device, fingerprint in the mDNS TXT
record, signed uploads — then the receiver can ask *"Accept 3 files from iPhone?"* and
remember the answer. That is the real AirDrop model.

**TLS on the LAN.** Self-signed cert on first run, pinned by fingerprint on the sender.
Ugly in Safari, correct.

**Resume.** Content-hash the file, `HEAD /api/upload/<hash>` for how many bytes landed,
then send a `Content-Range`. Matters as soon as you move video.

**Skip the router.** Both paths go through the access point. Wi-Fi Direct via
`wpa_supplicant` P2P gets you a direct link between two Linux boxes; iOS won't join it.

**Native iOS client.** `NWBrowser` to find `_mydrop._tcp` and `NWListener` to receive
would close the loop — discovery, progress, and receiving, none of which a Shortcut can
do. Needs a Mac and Xcode; free provisioning expires every 7 days.

---

`legacy/` holds the original Python prototype (FastAPI + zeroconf) this replaced. It
still works — `legacy/install.sh --pin 123456` — but needs four Python packages whose
versions drift between Ubuntu releases, which is exactly why the shipping version is a
single static binary.
