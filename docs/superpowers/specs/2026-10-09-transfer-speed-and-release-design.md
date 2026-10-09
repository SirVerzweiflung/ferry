# Transfer speed (Android) and automatic GitHub release

Date: 2026-10-09 · Target version: 0.2.2

## Problem

Sending a 35 MB photo from Android to Linux took over a minute (~0.5 MB/s) on a Wi-Fi that
delivers more than 20 MB/s from the internet.

Measured on a desktop PC over loopback, the same code is far faster, so the protocol and the
desktop are not the limit:

| What | Speed |
|---|---|
| Desktop (Rust) encrypt / decrypt | 660–690 MB/s |
| Android core crypto, in-tree Java, on a desktop JVM | 335–460 MB/s |
| Same frames through the platform `ChaCha20-Poly1305` cipher | 1050–1900 MB/s, byte-identical |
| Full transfer Android core → Android core, 400 MB | 270–310 MB/s |
| In-tree Java crypto, interpreter only (`-Xint`) | 7 MB/s |

Two suspects remain, both in the Android app. Neither could be measured, because no phone was
available:

1. Encryption is hand-written Java that allocates per 64-byte block and XORs byte by byte. On a
   phone runtime this can be very slow.
2. Sending holds no wake lock and no Wi-Fi lock (receiving holds a wake lock), so Android may
   put the CPU and radio into power-save during a send.

## Goals

* Phone → PC transfers run at Wi-Fi speed.
* Nothing breaks: every combination of old and new versions keeps working.
* The next slow transfer can be diagnosed from a log line.
* A pushed version tag produces a GitHub release with the installable files.

## Non-goals

* No change to the wire format, frame size or protocol version. Adaptive or larger frames were
  considered and rejected: TCP already ramps its own rate, frame size is not the limit, and
  larger frames risk older installs.
* No parallel connections.
* No change to the desktop's transfer or crypto code.
* No user-visible change in the apps (no new messages, settings or notifications).

## Design

### 1. Timing in the log (both sides, debug only)

After each file, sender and receiver write one log line, for example:

```
sent IMG_1234.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s, encrypt 0.3 s, network 1.6 s)
received IMG_1234.jpg: 35.0 MB in 2.1 s (16.7 MB/s)
```

* Android: through the existing `Host.log` (logcat). Desktop: `eprintln!` (journal / `ferry.log`).
* Toasts, notifications and CLI output stay exactly as they are.
* The sender's split is taken with a monotonic clock around the three steps of each chunk. The
  cost is a few clock reads per 64 KiB.

### 2. Android: platform cipher with a verified fallback

A new small class in `dev.ferry.core` provides `seal` and `open` for `Proto.Channel`:

* At first use it runs a self-test: the RFC 8439 vector, then random inputs of several lengths,
  each sealed and opened by both the platform cipher (`javax.crypto`, `ChaCha20-Poly1305`) and
  the in-tree `Crypto`, and compared. The platform cipher is used only if everything matches.
* If the cipher is missing, the self-test fails, or the platform cipher later throws anything
  other than an authentication failure, the channel uses the in-tree code, as today. The
  fallback is logged once.
* An authentication failure is reported exactly as today (`authentication failed`).
* The choice is per process and can be forced to in-tree by a static flag, for tests.

Both paths produce the same bytes, so the peer cannot tell which one is in use.

### 3. Android: faster in-tree ChaCha20

`Crypto.chacha20Xor` reuses its state arrays instead of allocating two per block, and XORs
whole words where it can. Same output; covered by the RFC vectors and the JDK cross-check.
This matters only when the fallback is active.

### 4. Android: stay awake while sending

`FerryApp.sendFiles` and `sendText` hold a partial wake lock and a Wi-Fi high-performance lock
for the duration of the send, with a 30-minute safety timeout, released in `finally`. This
mirrors what `FerryService` already does for receiving. `WAKE_LOCK` is already required for
that; the manifest is checked and extended only if needed.

Queued sends that are retried later by the outbox thread are not covered in this change.

### 5. Desktop

Only the log lines from step 1.

### 6. Automatic release

`.github/workflows/build.yml` gains a tag trigger and a `release` job:

* Runs only for tags `v*`, after the existing jobs succeed.
* Downloads their artifacts and creates a GitHub release named after the tag with the `gh`
  CLI that is preinstalled on the runner. No third-party action. Empty release notes.
* Attached files: `FerrySetup-<version>.exe`, `Ferry.apk`, and the Linux binary as
  `ferry-linux-x86_64`.
* The job gets `contents: write`; all other jobs keep read-only permissions.

APK signing: Android only installs an update over an existing app when both are signed with the
same key. CI currently uses a throw-away key. The Android job therefore accepts two optional
repository secrets, `FERRY_KEYSTORE_B64` and `FERRY_KEYSTORE_PASS`, and writes them to the place
`android/build.sh` already reads (`~/.config/ferry/`). Without the secrets it behaves as today.
Setting the secrets is a manual step for the repository owner.

### 7. Version

Desktop `0.2.2`; Android `versionName 0.2.2`, `versionCode 3`. README and PROTOCOL.md mention
versions only where they already do.

## Compatibility

The wire bytes do not change. Old phone ↔ new desktop, new phone ↔ old desktop and new ↔ new
all behave as before.

## Testing

Automated, run here:

* Existing Rust tests and Java core tests stay green.
* New Java core tests: platform and in-tree output identical for plaintext sizes 0, 1, 15, 16,
  17, 63, 64, 65, 65 536 and 1 048 576 bytes; each implementation opens what the other sealed;
  tampered ciphertext and tampered tag are rejected by both; the forced in-tree path works.
* Loopback transfer between two Android-core processes, one forced in-tree and one using the
  platform cipher, in both directions, with the received file compared byte for byte.
* Before/after throughput of the Android core on the JVM.

Needs the repository owner, because the sandbox blocks Unix sockets and the Android SDK:

* `tests/interop.py`, `tests/desktop_v2.py`, `tests/interop_v2.py`.
* `android/build.sh`, which also compiles the app-layer changes of steps 1 and 4.
* Resending the 35 MB photo and reading the log line.
* The release job, which can only run on GitHub.

## Risks

| Risk | Handling |
|---|---|
| Platform cipher differs or misbehaves on some phone | Self-test before use; fallback to in-tree code |
| Wake or Wi-Fi lock leaks and drains the battery | Timeout on acquire, release in `finally` |
| App-layer code cannot be compiled in this sandbox | Changes kept small; owner builds before release |
| Neither suspect is the cause | The step 1 log line shows where the time goes |
| Release APK signed with a new key cannot update an installed app | Optional keystore secrets |
