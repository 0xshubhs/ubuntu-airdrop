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

That is the whole install. The icon appears in the top-right immediately — no logout, no
follow-up commands, and the same on upgrade.

`postinst` runs as root, so it finds logged-in users via `loginctl`, then re-enters each
session with `runuser` and their `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` to restart
the daemon and replace the tray. Without that env a `--user` unit has no instance to talk
to and a tray has no bus to appear on. The service is also `systemctl --global enable`d and
the tray has a desktop-autostart entry, which covers users who are not logged in yet.

To keep receiving while logged out and from boot:

```bash
sudo loginctl enable-linger $USER
```

### Build it yourself

```bash
cargo build --release          # binary at target/release/drop
cargo deb                      # package at target/debian/
```

## Accept before receiving

Nothing lands in your folder unless you say so. A sender announces itself first, you get
a notification and a card in the window —

```
  Shubham's iPhone wants to send you 3 files
  3 items · 4.2 MB
    IMG_2718.jpeg      2.0 MB
    Receipt.pdf        187 KB
    notes.txt          304 B
  [ Decline ]  [ Accept ]
```

— and only then does anything reach `~/Drop`. Decline and it is deleted. The prompt shows
up in three places: a desktop notification with Accept/Decline buttons, the top of the
Drop window, and the top of the tray menu.

There are two ways in, because an iOS Shortcut can only make **one** HTTP request and so
cannot offer, wait, then upload:

- **Negotiated** — `POST /api/offer` with the file list, wait for the verdict, then
  `POST /api/upload?offer=<id>`. Not a byte is transferred before you accept. This is what
  `drop send` and the web page use, so Linux-to-Linux works this way too.
- **Staged** — a one-shot `POST /api/upload`. The bytes stream into a hidden
  `.staging/` folder inside the Drop directory and an offer is raised for them. They only
  become visible on Accept, and are deleted on Decline. The sender gets `202` rather
  than `200`.

Undecided offers expire after five minutes and their staged bytes are swept. Accept and
decline are loopback-only — nothing on the network can answer on your behalf.

Turn it off with **Ask before receiving** in the window if you would rather transfers
just land.

## The window

Press **Super**, type `drop`. You get everything the tray menu has, in a real window:
the QR code and PIN side by side, both addresses, switches for the tunnel and
auto-move, the received-file list, and nearby devices. It refreshes every two seconds.

It is served by the daemon on `/panel` and **refused to anything that is not loopback**,
so it drives the same unauthenticated control API the tray uses without exposing it. On
Chromium-family browsers it opens with `--app=`, giving a window with no tabs or address
bar; elsewhere it falls back to a normal window.

Right-click the icon in the dash for **Show the panel icon**, **Show QR code and PIN**,
and **Open Drop folder**.

## The indicator

Click the Drop icon in the top-right:

```
  Shubham's iPhone wants to send 3 files  (4.2 MB)
      Accept
      Decline
  ─────────────────────────
  Receiving as "keshava"
  Open Drop window
  ─────────────────────────
  PIN  481902                                 ← click to copy
  Anywhere:  xyz.trycloudflare.com            ← only when the tunnel is on
  This network only:  http://192.168.0.113:8420
  Show QR code…                               ← QR + PIN, scan from the phone
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

The two addresses are deliberately labelled. *This network only* is a LAN IP — it works
from the sofa and nowhere else. *Anywhere* appears only once the tunnel is running, and is
the one to use from mobile data. The QR always encodes whichever is the more reachable of
the two, and prints the PIN underneath it.

The QR opens in an image viewer rather than inside the menu. gnome-shell draws the tray
menu over the StatusNotifierItem D-Bus protocol, which carries labels, checkmarks and
small icons but not arbitrary images — and Wayland does not let a client position a window
against the panel. `drop qr` shows the same thing from a terminal.

## CLI

```bash
drop status                     # PIN, address, current settings
drop panel                      # open the Drop window
drop qr                         # QR + PIN for pairing a phone
drop open                       # open the Drop folder
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
   - Header `X-Drop-Device` = `Shubham's iPhone` — this is the name you see on the
     accept prompt; without it you get "Device at 192.168.0.x"
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
    -H "X-Drop-Device: Shubham's MacBook" \
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

Quick tunnels are genuinely free and need no Cloudflare account — `cloudflared` asks for
no credentials and hands out a hostname on first run.

**The free plan caps request bodies at 100 MB.** Photos and documents are fine; a 4K
video is not, and it fails at Cloudflare's edge before it ever reaches your laptop, with
an error that does not point at the real cause. There is no such limit on the LAN — the
daemon disables the body cap entirely — so the practical split is LAN at home, tunnel
when you are out.

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

**Your own subdomain, on a named tunnel.** Quick tunnels hand out a random hostname that
changes on every restart, which is why the tray shows a QR code. A *named* tunnel pins it
to something like `share.yourdomain.com` permanently — so the iOS Shortcut is configured
once and never breaks, at home or on cellular. Cloudflare Tunnel is free on the Free plan;
if you already own the domain there is nothing to buy.

The one requirement is that the domain's nameservers point at Cloudflare — a tunnel
resolves through `*.cfargotunnel.com`, which only exists inside Cloudflare's DNS, so the
zone has to be hosted there. Then, once:

```bash
cloudflared tunnel login          # opens a browser
cloudflared tunnel create drop
cloudflared tunnel route dns drop share.yourdomain.com
```

What the app still needs: a `tunnel_hostname` field in the config, so the daemon runs the
named tunnel instead of a quick one and the tray shows the real URL. Worth pairing with
**Cloudflare Access** in front of the hostname — email OTP or SSO before anything reaches
the laptop, free for up to 50 users, and a great deal stronger than six digits on an
internet-facing endpoint.

Note this does *not* lift the 100 MB body cap; that is the plan, not the tunnel type.

**Why not just point DNS at the machine.** Tempting, but a public A record needs a public
IP, and the laptop only has private ones (`192.168.0.x`). The public address belongs to
the router, so it would mean port-forwarding `8420` inward, a dynamic-DNS updater for when
the ISP rotates the address, and the laptop sitting directly on the internet. It also
breaks the moment you switch between Ethernet and Wi-Fi, since a forward targets one IP
and this machine has two. The tunnel dials *out*, which sidesteps all of it.

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
