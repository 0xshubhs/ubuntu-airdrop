# ubuntu-airdrop

AirDrop for the rest of us. Push files from an iPhone or a Mac to an Ubuntu box —
straight from the iOS Share Sheet and the Finder right-click menu, no cloud in between.

Three moving parts:

```
  advertise  ──▶  _mydrop._tcp.local.   (zeroconf / Avahi / Bonjour)
  discover   ──▶  browse the same service type, collect host:port
  transfer   ──▶  POST /api/upload, multipart, streamed to disk
```

---

## Install

```bash
git clone https://github.com/0xshubhs/ubuntu-airdrop.git
cd ubuntu-airdrop
./install.sh --pin 314159
```

That one script:

- builds a venv at `~/.venvs/drop` with `fastapi uvicorn python-multipart zeroconf httpx`
- drops a `drop` launcher into `~/.local/bin`
- installs a **systemd user service**, enables it, and turns on lingering — so the
  receiver comes up at boot, before you log in, and stays up after you log out

Pass a fixed `--pin` so the iPhone shortcut keeps working across restarts. Leave it off
and you get a random one each install.

If `ufw` is running, open the ports:

```bash
sudo ufw allow 8420/tcp
sudo ufw allow 5353/udp     # mDNS
```

Uninstall with `./install.sh --uninstall`. Your files in `~/Drop` are left alone.

## Use it

```bash
systemctl --user status drop     # is the receiver up?
journalctl --user -u drop -f     # watch files land
drop peers                       # who else is advertising
drop send thinkpad ~/Pictures/*.jpg
drop send 192.168.1.31 report.pdf
```

The receiver prints its address on start:

```
  Drop  —  keshava
  http://192.168.0.113:8420
  saving to /home/you/Drop
  PIN 314159
```

Open that URL on any device on the network and you get a drag-and-drop page. That alone
already solves iPhone → Ubuntu.

**Prefer `http://<hostname>.local:8420` over the raw IP.** Avahi publishes it, iOS and
macOS resolve it natively, and it survives your laptop moving between Wi-Fi and Ethernet
or getting a new DHCP lease. The hardcoded IP does not.

## iPhone: put it in the Share Sheet

This is the piece that makes it feel like AirDrop instead of like a website.

1. Open **Shortcuts** → **+** → rename it `Drop to Ubuntu`.
2. Tap the ⓘ info button → turn on **Show in Share Sheet**.
3. Under *Share Sheet Types*, deselect everything except **Files, Images, Media, URLs**.
4. Add action **Get Contents of URL**:
   - URL: `http://keshava.local:8420/api/upload`  *(your hostname)*
   - Method: **POST**
   - Headers: add `X-Drop-Pin` = your PIN
   - Request Body: **Form**
   - Add field → type **File** → key `files` → value **Shortcut Input**
5. Done.

Now: any photo, any file, any webpage → Share → *Drop to Ubuntu*. Two taps.

First run, iOS asks for **Local Network** permission — allow it, or the request fails
with nothing useful in the log. Settings → Shortcuts → Local Network if you missed it.

## macOS: a Quick Action in Finder

**Automator** → new **Quick Action** → *Workflow receives files or folders in Finder* →
add **Run Shell Script**, set *Pass input* to **as arguments**:

```bash
for f in "$@"; do
  /usr/bin/curl -s -F "files=@$f" -H "X-Drop-Pin: YOUR-PIN" \
    http://keshava.local:8420/api/upload
done
```

Save as `Drop to Ubuntu`. It now appears in Finder's right-click menu and in Services.
Bind a hotkey to it in System Settings → Keyboard → Shortcuts.

---

## Known gaps

**The PIN only guards uploads.** `GET /api/state` and `GET /files/<name>` are wide open
to anyone on the LAN — they can list and download everything already in `~/Drop`. Fine
on your own network, not fine on café Wi-Fi.

**No TLS.** Transfers are plaintext HTTP. Anyone on the same network can read them.

## Where to take it next

Roughly in order of payoff:

**Sender identity, not just a PIN.** Right now anyone with the PIN can push. Replace it
with a keypair per device: each device generates one on first run, publishes the
fingerprint in the mDNS TXT record, and signs uploads. Then the receiver shows
*"Accept 3 files from iPhone?"* and remembers the answer. That's the real AirDrop model.

**TLS.** Self-signed cert generated on first run, pinned by fingerprint on the sender.
Annoying in Safari (cert warnings) but correct.

**Resume.** Content-hash the file, `HEAD /api/upload/<hash>` to ask how many bytes the
receiver already has, then send a `Content-Range`. Matters as soon as you move video.

**Skip the router.** Both current paths go through the access point. Real AirDrop uses
AWDL, a peer-to-peer link. The open equivalent is Wi-Fi Direct on Linux via
`wpa_supplicant` P2P — you can get a direct link between two Linux boxes, though iOS
won't join it. This is where the project gets genuinely hard, and also where it gets
interesting.

**Native iOS client.** The Shortcut has real limits: no discovery, no progress, no
receiving. A small SwiftUI app using `NWBrowser` to find `_mydrop._tcp` and `NWListener`
to receive would close the loop. Needs a Mac and Xcode; free provisioning works but the
build expires every 7 days.
