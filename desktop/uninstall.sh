#!/usr/bin/env bash
# Removes Ferry. Your settings and pairings in ~/.config/ferry are kept (delete manually if wanted).
set -u
systemctl --user disable --now ferry.service 2>/dev/null
gnome-extensions disable ferry@ferry.local 2>/dev/null
rm -f  "$HOME/.local/bin/ferry" "$HOME/.local/bin/ferry-send" \
       "$HOME/.config/systemd/user/ferry.service" \
       "$HOME/.local/share/applications/ferry-send.desktop" \
       "$HOME/.local/share/nautilus/scripts/Send to phone"
rm -rf "$HOME/.local/share/gnome-shell/extensions/ferry@ferry.local"
systemctl --user daemon-reload
echo "Ferry removed. Settings remain in ~/.config/ferry"
