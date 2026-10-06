# Ferry

Send files and share the clipboard between your computer (Linux/GNOME or Windows 10/11) and your
Android phone.
Paired once by typing a short code, then it simply works in the background.

* **Desktop:** a small Rust daemon (≈600 KB binary, **≈2.5 MB RAM, 0 % CPU while idle**,
  no external crates) plus a GNOME Shell panel menu.
* **Android:** a Java app with **no library dependencies** (Android 10+). It sits in the
  share sheet as **“Send to desktop”**, has a Quick Settings tile, and waits in a foreground
  service whose threads sleep in `accept()` until a packet arrives.
* **Encrypted:** X25519 + ChaCha20-Poly1305. Pairing uses a 10-character code
  (see [PROTOCOL.md](PROTOCOL.md)).

```
ferry/
├── desktop/          Rust daemon + CLI, GNOME extension, install.sh, install.cmd (Windows)
├── android/          Android app, build.sh
├── tests/interop.py  live test: real desktop daemon <-> Android core on a JVM
└── PROTOCOL.md
```

---

## 1. Desktop (GNOME)

```bash
cd desktop
./install.sh
```

You need Rust (`curl https://sh.rustup.rs -sSf | sh`, or `sudo apt install cargo`). The script builds
the app (offline, no crates to download), installs it to `~/.local/bin`, and sets up a
**systemd user service** (starts with your session). It also adds the **GNOME extension** (the phone
icon in the top bar) and a **“Send to phone”** entry under right-click → *Scripts* in Files.

On Wayland, **log out and back in once** so GNOME loads the extension.

If you use a firewall, open port 47800 TCP+UDP (the script prints the exact command for
firewalld/ufw). Ubuntu’s firewall is off by default.

## 1b. Desktop (Windows 10/11)

Double-click **`desktop\install.cmd`**. The script:

* installs Rust if needed. It uses the GNU toolchain, so you don't need Visual Studio.
* builds Ferry and installs it to `%LOCALAPPDATA%\Programs\Ferry`.
* starts it at login. A tray icon appears; it may be hidden under **^** at first.
* adds **Send to → Phone (Ferry)** to Explorer's right-click menu, a Start menu entry, and the
  `ferry` command for new terminals.
* asks once for admin rights to allow Ferry through the firewall on private networks.

**Or build a real installer on Linux** (`FerrySetup-<version>.exe`; the Windows PC then needs no Rust):

```bash
sudo apt install mingw-w64 nsis        # Fedora: sudo dnf install mingw64-gcc mingw32-nsis
cd desktop && ./package-windows.sh     # -> desktop/dist/FerrySetup-0.1.0.exe
```

The installer does the same things as `install.cmd` and adds an uninstaller under Settings → Apps.
It needs no admin rights apart from the one firewall prompt. It isn't code-signed, so SmartScreen
warns: click *More info → Run anyway*. If you have a certificate, set `FERRY_SIGN_PFX` and
`FERRY_SIGN_PASS` and install `osslsigncode`, and the script signs it. The GitHub workflow in
`.github/` builds the installer too.

Set your Wi-Fi to **Private network** in Windows settings, or the phone can't connect.
Clipboard sync is automatic both ways, as on Linux. Windows reports changes as events, so
nothing polls. Copies that password managers mark as secret are skipped. Received files go to
`Downloads`. Click a notification to show the file in Explorer.

## 2. Android

```bash
cd android
./build.sh            # -> android/Ferry.apk
./build.sh install    # build + install on a USB-connected phone (USB debugging on)
```

Java: the script uses an installed JDK **17–21** (system, SDKMAN or Android Studio's bundled one).
Newer JDKs such as Java 25 are skipped, because the Android build tools can't run on them yet. If there
is no suitable JDK, it downloads Temurin 21 once. If you already have Android Studio, the script uses its
SDK. If you don't, it downloads the Android command-line SDK once (~300 MB, to
`~/.local/share/ferry-android-sdk`). The first run also creates a personal signing key in
`~/.config/ferry/`. Keep it, because Android only installs updates signed with the same key.

Without USB: copy `Ferry.apk` to the phone and open it (allow “install unknown apps”).

When you first open the app:
1. Allow notifications.
2. Tap **“Allow Ferry to run in the background”**. This turns off battery optimisation, which
   matters on Samsung, Xiaomi and similar phones.

## 3. Pair (once)

On the desktop, click the phone icon → **Pair new phone…** (or run `ferry pair`). A dialog shows
the address and a code like `7KQ2M-X9PRT`.
On the phone, tap **Pair with computer (enter code)**, type the address and code, then tap **Pair**.

You can also pair the other way round: tap **Pair: show a code on this phone** on the phone, then
run `ferry pair <phone-ip> <code>` on the desktop.

## Daily use

| What | How |
|---|---|
| Phone → PC files | Share from any app → **Send to desktop**. Files land in `~/Downloads`. |
| Phone → PC clipboard | The **Clipboard → PC** Quick Settings tile (add it with the button in the app, or pull down Quick Settings → ✎ edit → drag it in), the button on the Ferry notification, or share text → *Send to desktop* |
| PC → phone clipboard | **Automatic**: copy on the desktop and it shows up on the phone. Switch it off in the panel menu. Password-manager copies (KeePassXC etc.) are never synced. |
| PC → phone files | Linux: right-click → *Scripts* → **Send to phone**, the panel menu → *Send files…*, or `ferry send FILE…`. Windows: right-click → **Send to → Phone (Ferry)**, or the tray menu. Files land in `Download/Ferry` on the phone. |

Android does not let background apps read the clipboard, so phone → PC needs one tap. PC → phone is fully automatic.

### CLI

```
ferry status                  this device, addresses, paired devices
ferry pair                    show a pairing code
ferry pair <ip> <code>        pair with a code shown on the phone
ferry send [--to NAME] FILE…  send files      ferry send --pick   (file dialog)
ferry clip [TEXT]             send text / the current clipboard
ferry unpair NAME
ferry set name|download_dir|auto_clipboard|notifications VALUE
journalctl --user -u ferry -f   logs
```

You can bind `ferry clip` to a keyboard shortcut (Settings → Keyboard → Custom Shortcuts) if you want it.

## Outside the local network

Ferry talks plain TCP to an IP address or hostname, so it works across the internet through any VPN
that gives both devices reachable addresses. The easiest is **[Tailscale](https://tailscale.com)**
(free, peer-to-peer WireGuard): install it on both, then pair using the desktop’s Tailscale address
(`100.x.y.z` or its MagicDNS name). After that, Ferry works at home and on the go. Ferry has no NAT
hole-punching of its own; that would need a relay server.

## How it stays light

* Desktop: 4 threads, all blocked in `accept()`/`recv()`. GNOME clipboard changes arrive as
  shell events through a Unix socket, so nothing polls.
* Android: a foreground service (Android requires one to keep listening) with two blocked threads.
  It holds no wake lock while idle, only a partial wake lock while a transfer is running.
  Incoming packets wake the phone. On a network change the phone tells the desktop its new address.
  If an address is stale, a single UDP broadcast finds the device again.

## Uninstall

`desktop/uninstall.sh` (Windows: `powershell -ExecutionPolicy Bypass -File desktop\uninstall.ps1`) and uninstall the app on the phone. Your settings are kept in `~/.config/ferry`.

## Development

```bash
cd desktop && cargo test --release              # RFC test vectors + protocol tests
FERRY_WINCHECK=1 cargo check                     # type-check the Windows code from Linux
android/core-test/run.sh                         # Java core vs RFC vectors and the JDK's own crypto
python3 tests/interop.py                         # desktop daemon <-> Android core, live
```

The crypto primitives are implemented in-tree, so both apps build with zero third-party
dependencies. They are checked against the RFC 7748/8439/5869/4231 test vectors and against
the JDK’s X25519 and ChaCha20-Poly1305. The code has not had an independent security audit.
