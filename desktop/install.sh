#!/usr/bin/env bash
# Builds and installs Ferry for the current user (no root needed).
set -euo pipefail
cd "$(dirname "$0")"

BIN="$HOME/.local/bin"
EXT_UUID="ferry@ferry.local"
EXT_DIR="$HOME/.local/share/gnome-shell/extensions/$EXT_UUID"

if ! command -v cargo >/dev/null 2>&1; then
    [ -x "$HOME/.cargo/bin/cargo" ] && export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust is not installed. Install it with:"
    echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo "(or: sudo apt install cargo / sudo dnf install cargo) and run ./install.sh again."
    exit 1
fi

if [ -f src/sys.rs ] && [ -d src/sys ]; then
    echo "==> Removing src/sys.rs left over from an older Ferry version"
    rm -f src/sys.rs
fi

echo "==> Building (no external crates, works offline)"
cargo build --release

echo "==> Installing to $BIN"
install -Dm755 target/release/ferry "$BIN/ferry"
install -Dm755 packaging/ferry-send "$BIN/ferry-send"

install -Dm644 packaging/ferry.service "$HOME/.config/systemd/user/ferry.service"

mkdir -p "$HOME/.local/share/applications"
sed "s|^Exec=ferry-send|Exec=$BIN/ferry-send|; s|^Exec=ferry clip|Exec=$BIN/ferry clip|" \
    packaging/ferry-send.desktop > "$HOME/.local/share/applications/ferry-send.desktop"

# Files (Nautilus): right-click -> Scripts -> Send to phone
install -Dm755 packaging/ferry-send "$HOME/.local/share/nautilus/scripts/Send to phone"
install -Dm755 packaging/ferry-send-choose "$HOME/.local/share/nautilus/scripts/Send to device…"

echo "==> Installing GNOME Shell extension"
mkdir -p "$EXT_DIR"
cp gnome-extension/$EXT_UUID/* "$EXT_DIR/"
if command -v gsettings >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    cur=$(gsettings get org.gnome.shell enabled-extensions 2>/dev/null || echo "@as []")
    new=$(python3 - "$cur" "$EXT_UUID" <<'PY'
import ast, sys
cur, uuid = sys.argv[1], sys.argv[2]
cur = cur.replace("@as ", "")
lst = ast.literal_eval(cur) if cur.strip() else []
if uuid not in lst:
    lst.append(uuid)
print(str(lst))
PY
)
    gsettings set org.gnome.shell enabled-extensions "$new" || true
    if [ "$(gsettings get org.gnome.shell disable-user-extensions 2>/dev/null)" = "true" ]; then
        echo "    NOTE: user extensions are globally disabled; enable them in the Extensions app."
    fi
fi
gnome-extensions enable "$EXT_UUID" >/dev/null 2>&1 || true

echo "==> Starting background service"
systemctl --user daemon-reload
systemctl --user enable ferry.service >/dev/null 2>&1
systemctl --user restart ferry.service

sleep 0.5
PORT=$(sed -n 's/^port *= *//p' "$HOME/.config/ferry/config" 2>/dev/null || echo 47800)
PORT=${PORT:-47800}

echo
echo "Ferry is installed and running."
echo
if [ "${XDG_SESSION_TYPE:-}" = "wayland" ] && ! gnome-extensions info "$EXT_UUID" 2>/dev/null | grep -q "ACTIVE"; then
    echo " * Log out and back in once so GNOME loads the panel icon (Wayland limitation)."
fi
if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld; then
    echo " * firewalld is active. Allow Ferry with:"
    echo "     sudo firewall-cmd --permanent --add-port=$PORT/tcp --add-port=$PORT/udp && sudo firewall-cmd --reload"
elif command -v ufw >/dev/null 2>&1 && LANG=C sudo -n ufw status 2>/dev/null | grep -q "Status: active"; then
    echo " * ufw is active. Allow Ferry with:  sudo ufw allow $PORT"
fi
case ":$PATH:" in *":$BIN:"*) ;; *) echo " * Add $BIN to your PATH to use the 'ferry' command in a terminal.";; esac
echo " * Pair your phone: click the phone icon in the top bar -> 'Pair new phone…'  (or run: ferry pair)"
