#!/usr/bin/env bash
# Installs drop as a CLI tool and a user service that starts at boot.
#
#   ./install.sh              install with a random PIN
#   ./install.sh --pin 481902 install with a fixed PIN (keeps iPhone shortcuts working)
#   ./install.sh --uninstall  remove the service, the launcher and the venv
#
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/drop.py"
VENV="$HOME/.venvs/drop"
BIN="$HOME/.local/bin/drop"
UNIT="$HOME/.config/systemd/user/drop.service"
PORT=8420

if [[ "${1:-}" == "--uninstall" ]]; then
  systemctl --user disable --now drop.service 2>/dev/null || true
  rm -f "$UNIT" "$BIN"
  systemctl --user daemon-reload
  rm -rf "$VENV"
  echo "removed. your files in ~/Drop were left alone."
  exit 0
fi

PIN=""
[[ "${1:-}" == "--pin" ]] && PIN="${2:?--pin needs a value}"
[[ -z "$PIN" ]] && PIN="$(python3 -c 'import random;print(f"{random.randrange(10**6):06d}")')"

echo "==> python venv at $VENV"
python3 -m venv "$VENV"
"$VENV/bin/pip" install --quiet --upgrade pip
"$VENV/bin/pip" install --quiet fastapi uvicorn python-multipart zeroconf httpx

echo "==> launcher at $BIN"
mkdir -p "$(dirname "$BIN")"
cat > "$BIN" <<EOF
#!/usr/bin/env bash
exec "$VENV/bin/python" "$SRC" "\$@"
EOF
chmod +x "$BIN" "$SRC"

echo "==> service at $UNIT"
mkdir -p "$(dirname "$UNIT")"
cat > "$UNIT" <<EOF
[Unit]
Description=Drop - LAN file transfer
After=network-online.target avahi-daemon.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=$VENV/bin/python $SRC serve --pin $PIN --port $PORT
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now drop.service

# survive logout / start at boot without an active session
loginctl enable-linger "$USER" 2>/dev/null \
  || echo "    (could not enable linger - run: sudo loginctl enable-linger $USER)"

echo
echo "    PIN $PIN"
echo "    http://$(hostname -s).local:$PORT"
echo
echo "    drop peers            list devices on the network"
echo "    drop send <who> FILE  push a file to one"
echo "    systemctl --user status drop    check the receiver"
echo
if ! command -v drop >/dev/null 2>&1; then
  echo "NOTE: $HOME/.local/bin is not on your PATH. Add it:"
  echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && exec bash"
fi
if command -v ufw >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
  sudo ufw allow "$PORT/tcp" >/dev/null 2>&1 || true
  sudo ufw allow 5353/udp   >/dev/null 2>&1 || true
else
  echo "If ufw is on, open the ports:"
  echo "  sudo ufw allow $PORT/tcp && sudo ufw allow 5353/udp"
fi
