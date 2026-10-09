# Ferry

Send files and share the clipboard between your Android phone and your computers (Linux/GNOME or
Windows 10/11), and send files to any other Ferry device on the network.

## Two kinds of devices

| | **My devices** | **Nearby** |
|---|---|---|
| What | Your phone and your computers, **paired once with a code** | Any other Ferry device on the same network |
| Files they send you | Saved directly (`Downloads` / `Download/Ferry`) | Wait in **Incoming** until you tap **Accept**. Deleted after 24 h if you don't. |
| Clipboard | **Synced automatically** between all of them | Never. Text they send waits in Incoming too |
| Sending to them | One tap. The ★ main device is the default | Pick them from the list; they have to accept |

* **Not reachable?** The send waits in a queue and goes out by itself as soon as the device appears
  again (up to 24 h). Nothing is retried after the sending device is switched off.
* **No spam:** Incoming has size and count limits, rate limits per device, and **Block**.
  Turn off **Visible to nearby devices** and only your own devices can see you.
* **No pop-ups:** a waiting transfer is one quiet notification with Accept / Decline. It is also
  listed in the menu or app, so you can decide later or simply ignore it.

## What it is

* **Desktop:** a small Rust program (≈600 KB, **≈2.5 MB RAM, 0 % CPU while idle**, no external
  crates) with a GNOME panel menu or a Windows tray icon.
* **Android:** a Java app with **no library dependencies** (Android 10+), in the share sheet as
  **Ferry**, with a Quick Settings tile.
* **Encrypted:** X25519 + ChaCha20-Poly1305. Paired devices are authenticated by the pairing key
  (see [PROTOCOL.md](PROTOCOL.md)).

```
ferry/
├── desktop/          Rust daemon + CLI, GNOME extension, install.sh, install.cmd (Windows)
├── android/          Android app, build.sh
├── tests/            live tests: real desktop daemons <-> Android core on a JVM
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
icon in the top bar), and adds **Send to phone** and **Send to device…** under right-click →
*Scripts* in Files.

On Wayland, **log out and back in once** so GNOME loads the extension.

If you use a firewall, open port 47800 TCP+UDP (the script prints the exact command for
firewalld/ufw). Ubuntu’s firewall is off by default.

## 1b. Desktop (Windows 10/11)

Double-click **`desktop\install.cmd`**. The script:

* installs Rust if needed. It uses the GNU toolchain, so you don't need Visual Studio.
* builds Ferry and installs it to `%LOCALAPPDATA%\Programs\Ferry`.
* starts it at login. A tray icon appears; it may be hidden under **^** at first.
* adds **Send to → Phone (Ferry)** (your main device) and **Send to → Ferry (choose device)** to
  Explorer's right-click menu, plus a Start menu entry and the `ferry` command for new terminals.
* asks once for admin rights to allow Ferry through the firewall on private networks.

**Or build a real installer on Linux** (`FerrySetup-<version>.exe`; the Windows PC then needs no Rust):

```bash
sudo apt install mingw-w64 nsis        # Fedora: sudo dnf install mingw64-gcc mingw32-nsis
cd desktop && ./package-windows.sh     # -> desktop/dist/FerrySetup-0.2.2.exe
```

The installer does the same things as `install.cmd` and adds an uninstaller under Settings → Apps.
It needs no admin rights apart from the one firewall prompt. The GitHub workflow in `.github/`
builds the installer too.

Unsigned, it triggers Windows' "Windows protected your PC" prompt (*More info → Run anyway*). It may
also trigger a heuristic Defender false positive. **[WINDOWS-DEFENDER.md](WINDOWS-DEFENDER.md)**
explains what was changed in v0.2 to avoid that and how to sign the build.

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

## 3. Pair your own devices (once)

On the computer, click the Ferry icon → **Pair a new device…** (or run `ferry pair`). A dialog shows
the address and a code like `7KQ2M-X9PRT`.
On the phone, tap **Pair with a computer (enter its code)**, type the address and code, then tap **Pair**.

Pair the phone with each of your computers. Clipboard sync then covers all of them, because each
device passes new clipboard text on to its other devices. You can also pair two computers directly:
run `ferry pair` on one, then `ferry pair <ip> <code>` on the other.

You can also pair the other way round: tap **Pair: show a code on this phone**, then run
`ferry pair <phone-ip> <code>` on the computer.

Nearby devices need no setup: every Ferry device on the network shows up by itself.

## Daily use

| What | How |
|---|---|
| Phone → computer files | Share from any app → **Ferry** → pick the device. Your paired computers also appear directly in the share sheet (Direct Share). |
| Computer → phone / other computer | Linux: Ferry menu → **Send files to ▸**, right-click → *Scripts* → **Send to phone** / **Send to device…**, or `ferry send --to NAME FILE…`. Windows: tray → **Send files to ▸**, or right-click → **Send to**. |
| Clipboard computer → phone | **Automatic**: copy on any paired computer and it reaches all your devices. Password-manager copies (KeePassXC, 1Password, Bitwarden…) are never synced. |
| Clipboard phone → computers | The **Clipboard → PC** Quick Settings tile (add it with the button in the app), the button on the Ferry notification, or **Send clipboard to my devices** in the app |
| Something arrives from a nearby device | One quiet notification with **Accept** / **Decline**, also listed under **Incoming** in the menu / app. Accepted files go to Downloads, text to the clipboard. |

Android doesn't let background apps read the clipboard, so phone → computer takes one tap.
Computer → phone is fully automatic.

### CLI

```
ferry status                   this device, my devices, waiting items
ferry devices                  my devices + nearby devices
ferry send [--to NAME] FILE…   send files (default: main device)   --pick = file dialog
ferry text --to NAME TEXT      send a text
ferry clip [TEXT]              text / clipboard to all my devices
ferry incoming                 what nearby devices sent you
ferry accept ID|all            ferry decline ID|all      ferry block ID|NAME
ferry queue                    sends waiting for a device   ferry cancel ID|all
ferry pair  /  ferry pair <ip> <code>  /  ferry unpair NAME
ferry set visible off          hide from nearby devices (only my devices can send)
ferry set incoming_limit_mb 2048 / incoming_hours 24 / auto_clipboard on|off / name …
journalctl --user -u ferry -f  logs (Windows: %APPDATA%\Ferry\ferry.log)
```

You can bind `ferry clip` to a keyboard shortcut (Settings → Keyboard → Custom Shortcuts) if you want it.

## Outside the local network

Ferry talks plain TCP to an IP address or hostname, so it works across the internet through any VPN
that gives both devices reachable addresses. The easiest is **[Tailscale](https://tailscale.com)**
(free, peer-to-peer WireGuard): install it on both, then pair using the desktop’s Tailscale address
(`100.x.y.z` or its MagicDNS name). After that, Ferry works at home and on the go. Ferry has no NAT
hole-punching of its own; that would need a relay server.

## How it stays light

* Desktop: a few threads, all blocked in `accept()`, `recv()` or a condition wait. GNOME clipboard
  changes arrive as shell events through a Unix socket, so nothing polls.
* Nearby devices are only looked for when you open a send menu or list (one UDP broadcast, ~0.7 s).
  Each device also announces itself once when it starts. The queue and Incoming expiry sleep until
  their next deadline.
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
python3 tests/core_loopback.py                   # two Android cores, platform and built-in cipher mixed
python3 tests/interop.py                         # paired basics: desktop daemon <-> Android core
python3 tests/desktop_v2.py                      # 4 desktops: nearby, accept/decline/block, queue, relay
python3 tests/interop_v2.py                      # 3 desktops + phone core: relay, nearby both ways, queue
```

The crypto primitives are implemented in-tree, so both apps build with zero third-party
dependencies. They are checked against the RFC 7748/8439/5869/4231 test vectors and against
the JDK’s X25519 and ChaCha20-Poly1305. The code has not had an independent security audit.
