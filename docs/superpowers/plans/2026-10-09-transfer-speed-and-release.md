# Transfer Speed (Android) and Automatic Release — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make phone → PC transfers run at Wi-Fi speed without changing the wire format, make slow transfers diagnosable from a log line, and publish installable files on a GitHub release for every version tag.

**Architecture:** The Android core gets a small `Aead` class that uses the platform's native ChaCha20-Poly1305 only after proving it matches the in-tree cipher, and falls back to the in-tree cipher otherwise. `Proto.Channel` calls `Aead` and counts time spent encrypting and writing; `Node` logs one line per file. The app holds wake and Wi-Fi locks while sending. The desktop only gains log lines. The CI workflow gains a tag-only `release` job.

**Tech Stack:** Java 11 API surface (Android core, tested on a plain JDK), Android SDK 29–35 (app layer), Rust std only (desktop), Python 3 (live tests), GitHub Actions + `gh`.

**Spec:** `docs/superpowers/specs/2026-10-09-transfer-speed-and-release-design.md`

## Global Constraints

* Wire format, frame size (`CHUNK = 64 * 1024`) and protocol version do not change.
* No new dependencies: no crates, no Java libraries, no third-party GitHub actions.
* No user-visible change in the apps: no new toasts, notifications, settings or CLI output. Timing goes to the log only (Android `Host.log`, desktop `eprintln!`).
* Desktop transfer and crypto logic is not changed; only timing statements and log lines are added.
* Android core must compile with `javac --release 11`.
* Target version: desktop `0.2.2`; Android `versionName '0.2.2'`, `versionCode 3`.
* Work on branch `speed-0.2.2`. Stage files by explicit path only — the sandbox shows placeholder dotfiles (`.bashrc`, `.gitconfig`, `.idea`, …) in the repo root that must never be added. Never use `git add -A` or `git add .`.
* In this sandbox: run cargo as `CARGO_HOME="$TMPDIR/cargo-home" cargo … --offline`. `tests/interop.py`, `tests/desktop_v2.py`, `tests/interop_v2.py` and `android/build.sh` cannot run here (Unix sockets and `/home` are blocked); the repository owner runs them.

## Review Focus

1. **Empty file (0 bytes):** no DATA frames and possibly zero elapsed time; the transfer must succeed and the log line must not divide by zero. → Task 3 (`transferLine` test), Task 4 (0-byte file in loopback), Task 5 (Rust test).
2. **Phone without a usable platform cipher:** the app must work exactly as before. → Task 1 (forced in-tree path test), Task 4 (in-tree ↔ platform loopback).
3. **Platform cipher fails in the middle of a session:** the frame must still go out with the correct bytes, and later frames must work. → Task 1 (`testFailPlatform` test).
4. **Corrupted or forged frame while the platform cipher is active:** must be rejected as `authentication failed`, and must not switch the cipher off. → Task 1 (tamper tests, `usingPlatform()` still true afterwards).
5. **File sizes around the frame boundary (65 535, 65 536, 65 537 bytes):** received file must be byte-identical. → Task 4.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `android/app/src/main/java/dev/ferry/core/Aead.java` | create | Choose platform or in-tree frame cipher; self-test; fallback |
| `android/app/src/main/java/dev/ferry/core/Proto.java` | modify `Channel` | Use `Aead`; count seal / write time |
| `android/app/src/main/java/dev/ferry/core/Crypto.java` | modify ChaCha20 | Faster in-tree keystream |
| `android/app/src/main/java/dev/ferry/core/Node.java` | modify | Log one timing line per file |
| `android/core-test/CoreTest.java` | modify | New unit tests; `FERRY_AEAD` switch and `cipher` command for live tests |
| `tests/core_loopback.py` | create | Live transfer between two Android-core processes, mixed ciphers |
| `desktop/src/daemon.rs` | modify | Timing log lines |
| `android/app/src/main/java/dev/ferry/app/FerryApp.java` | modify | Wake + Wi-Fi lock while sending |
| `.github/workflows/build.yml` | modify | Signing-key secrets, loopback test, tag-only release job |
| `desktop/Cargo.toml`, `desktop/Cargo.lock`, `android/app/build.gradle`, `README.md` | modify | Version 0.2.2, new test in the Development list |

---

### Task 0: Branch

- [ ] **Step 1: Create the branch and commit the spec and plan**

```bash
cd /home/luki/code/home/github/ferry
git switch -c speed-0.2.2
git add docs/superpowers/specs/2026-10-09-transfer-speed-and-release-design.md docs/superpowers/plans/2026-10-09-transfer-speed-and-release.md
git commit -m "Spec and plan: Android transfer speed, automatic release"
```

---

### Task 1: `Aead` — platform cipher with verified fallback

**Files:**
- Create: `android/app/src/main/java/dev/ferry/core/Aead.java`
- Modify: `android/app/src/main/java/dev/ferry/core/Proto.java` (`Channel.sendRaw`, `Channel.recv`)
- Modify: `android/app/src/main/java/dev/ferry/core/Node.java` (constructor)
- Test: `android/core-test/CoreTest.java`

**Interfaces:**
- Produces: `Aead.seal(byte[] key, byte[] nonce, byte[] pt) → byte[]` (ciphertext ‖ tag), `Aead.open(byte[] key, byte[] nonce, byte[] ct) → byte[]` or `null` on authentication failure, `Aead.usingPlatform() → boolean`, `public static volatile boolean Aead.forceInTree`, `public static volatile boolean Aead.testFailPlatform`, `public static volatile java.util.function.Consumer<String> Aead.log`.

- [ ] **Step 1: Write the failing tests**

In `android/core-test/CoreTest.java`, add this method above the `// ---- interop "phone"` comment:

```java
    static void frameCipherTests() {
        System.out.println("Frame cipher (platform vs built-in)");
        check(Aead.usingPlatform(), "platform cipher passes the self-test on this JVM");
        boolean same = true, cross = true, tamper = true;
        for (int len : new int[] {0, 1, 15, 16, 17, 63, 64, 65, 65536, 1 << 20}) {
            byte[] k = Crypto.random(32), n = Crypto.random(12), m = Crypto.random(len);
            Aead.forceInTree = false;
            byte[] viaPlatform = Aead.seal(k, n, m);
            Aead.forceInTree = true;
            byte[] viaBuiltIn = Aead.seal(k, n, m);
            same &= Arrays.equals(viaPlatform, viaBuiltIn)
                    && Arrays.equals(viaPlatform, Crypto.seal(k, n, new byte[0], m));
            cross &= Arrays.equals(Aead.open(k, n, viaPlatform), m); // built-in opens the platform's
            Aead.forceInTree = false;
            cross &= Arrays.equals(Aead.open(k, n, viaBuiltIn), m);  // platform opens the built-in's
            for (int pos : new int[] {0, viaPlatform.length - 1}) {  // first byte and last tag byte
                byte[] bad = viaPlatform.clone();
                bad[pos] ^= 1;
                Aead.forceInTree = false;
                tamper &= Aead.open(k, n, bad) == null;
                Aead.forceInTree = true;
                tamper &= Aead.open(k, n, bad) == null;
            }
            Aead.forceInTree = false;
        }
        check(same, "platform and built-in produce identical frames (10 sizes)");
        check(cross, "each opens what the other sealed");
        check(tamper, "both reject a tampered frame");
        check(Aead.open(new byte[32], new byte[12], new byte[5]) == null, "too-short frame is rejected");
        check(Aead.usingPlatform(), "bad frames did not switch the platform cipher off");

        List<String> notes = new ArrayList<>();
        Aead.log = notes::add;
        byte[] k = Crypto.random(32), n = Crypto.random(12), m = Crypto.random(1000);
        Aead.testFailPlatform = true;
        check(Arrays.equals(Aead.seal(k, n, m), Crypto.seal(k, n, new byte[0], m)),
                "a failing platform cipher falls back with identical output");
        check(Arrays.equals(Aead.open(k, n, Aead.seal(k, n, m)), m), "and keeps working afterwards");
        check(!Aead.usingPlatform() && notes.size() == 1, "the fallback is permanent and logged once: " + notes);
        Aead.log = null;
    }
```

In `main`, call it directly before the line `System.out.println(fails == 0 ? "ALL OK" : fails + " FAILURES");`:

```java
        frameCipherTests();
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `android/core-test/run.sh`
Expected: compile error `cannot find symbol … Aead` on stderr, then `java` fails with `ClassNotFoundException: CoreTest` (or a stale result). Either way: not `ALL OK` with the new section.

- [ ] **Step 3: Create `Aead.java`**

```java
package dev.ferry.core;

import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.util.Arrays;
import java.util.Random;
import java.util.function.Consumer;
import javax.crypto.AEADBadTagException;
import javax.crypto.Cipher;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;

/**
 * ChaCha20-Poly1305 for frames (empty AAD). Uses the platform's native cipher when it
 * provably matches the built-in implementation in {@link Crypto}, and the built-in one
 * otherwise. Both produce the same bytes, so the peer cannot tell which is in use.
 */
public final class Aead {
    private Aead() {}

    /** Tests: always use the built-in implementation. */
    public static volatile boolean forceInTree;
    /** Tests: make the next platform call fail, to exercise the fallback. */
    public static volatile boolean testFailPlatform;
    /** Receives the one-time note when the platform cipher is not used. */
    public static volatile Consumer<String> log;

    /** Android's name first, then the JDK's. */
    private static final String[] NAMES = {"ChaCha20/Poly1305/NoPadding", "ChaCha20-Poly1305"};
    private static final byte[] NO_AAD = new byte[0];
    private static final int UNKNOWN = 0, PLATFORM = 1, IN_TREE = 2;
    private static volatile int state = UNKNOWN;
    private static volatile String name;

    public static boolean usingPlatform() {
        return !forceInTree && mode() == PLATFORM;
    }

    /** Returns ciphertext || tag. */
    public static byte[] seal(byte[] key, byte[] nonce, byte[] pt) {
        if (usingPlatform()) {
            try {
                return platform(name, Cipher.ENCRYPT_MODE, key, nonce, NO_AAD, pt);
            } catch (GeneralSecurityException | RuntimeException e) {
                disable(name + ": " + e);
            }
        }
        return Crypto.seal(key, nonce, NO_AAD, pt);
    }

    /** Returns the plaintext, or null if authentication fails. */
    public static byte[] open(byte[] key, byte[] nonce, byte[] ct) {
        if (ct.length < 16) return null;
        if (usingPlatform()) {
            try {
                return platform(name, Cipher.DECRYPT_MODE, key, nonce, NO_AAD, ct);
            } catch (AEADBadTagException e) {
                return null;
            } catch (GeneralSecurityException | RuntimeException e) {
                disable(name + ": " + e);
            }
        }
        return Crypto.open(key, nonce, NO_AAD, ct);
    }

    private static int mode() {
        int s = state;
        if (s != UNKNOWN) return s;
        synchronized (Aead.class) {
            if (state != UNKNOWN) return state;
            String why = "the platform has no ChaCha20-Poly1305";
            for (String n : NAMES) {
                try {
                    if (selfTest(n)) {
                        name = n;
                        state = PLATFORM;
                        return PLATFORM;
                    }
                    why = n + " does not match the built-in cipher";
                } catch (java.security.NoSuchAlgorithmException e) {
                    // try the next name
                } catch (GeneralSecurityException | RuntimeException e) {
                    why = n + ": " + e;
                }
            }
            disable(why);
            return state;
        }
    }

    private static synchronized void disable(String why) {
        if (state == IN_TREE) return;
        state = IN_TREE;
        Consumer<String> l = log;
        if (l != null) l.accept("using built-in encryption (" + why + ")");
    }

    /** A fresh Cipher per call: instances refuse to encrypt twice with the same key and nonce. */
    private static byte[] platform(String n, int op, byte[] key, byte[] nonce, byte[] aad, byte[] in)
            throws GeneralSecurityException {
        if (testFailPlatform) {
            testFailPlatform = false;
            throw new GeneralSecurityException("simulated failure");
        }
        Cipher c = Cipher.getInstance(n);
        c.init(op, new SecretKeySpec(key, "ChaCha20"), new IvParameterSpec(nonce));
        if (aad.length > 0) c.updateAAD(aad);
        return c.doFinal(in);
    }

    /** RFC 8439 section 2.8.2, then random frames compared with the built-in cipher. */
    private static boolean selfTest(String n) throws GeneralSecurityException {
        byte[] key = Crypto.unhex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        byte[] nonce = Crypto.unhex("070000004041424344454647");
        byte[] aad = Crypto.unhex("50515253c0c1c2c3c4c5c6c7");
        byte[] pt = ("Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the "
                + "future, sunscreen would be it.").getBytes(StandardCharsets.US_ASCII);
        byte[] ct = platform(n, Cipher.ENCRYPT_MODE, key, nonce, aad, pt);
        if (ct.length != pt.length + 16) return false;
        if (!Crypto.hex(Arrays.copyOfRange(ct, pt.length, ct.length)).equals("1ae10b594f09e26a7e902ecbd0600691")) {
            return false;
        }
        Random r = new Random(7);
        for (int len : new int[] {0, 1, 15, 16, 17, 63, 64, 65, 1000, 65537}) {
            byte[] k = new byte[32], nn = new byte[12], m = new byte[len];
            r.nextBytes(k);
            r.nextBytes(nn);
            r.nextBytes(m);
            byte[] ref = Crypto.seal(k, nn, NO_AAD, m);
            if (!Arrays.equals(platform(n, Cipher.ENCRYPT_MODE, k, nn, NO_AAD, m), ref)) return false;
            if (!Arrays.equals(platform(n, Cipher.DECRYPT_MODE, k, nn, NO_AAD, ref), m)) return false;
            ref[ref.length - 1] ^= 1;
            try {
                platform(n, Cipher.DECRYPT_MODE, k, nn, NO_AAD, ref);
                return false; // accepted a forged frame
            } catch (AEADBadTagException expected) {
                // correct
            }
        }
        return true;
    }
}
```

- [ ] **Step 4: Use it in `Proto.Channel`**

In `Proto.java`, `Channel.sendRaw`, replace

```java
            byte[] ct = Crypto.seal(sendKey, nonce(sendCtr++), new byte[0], plaintext);
```

with

```java
            byte[] ct = Aead.seal(sendKey, nonce(sendCtr++), plaintext);
```

In `Channel.recv`, replace

```java
            byte[] pt = Crypto.open(recvKey, nonce(recvCtr), new byte[0], ct);
```

with

```java
            byte[] pt = Aead.open(recvKey, nonce(recvCtr), ct);
```

- [ ] **Step 5: Route the fallback note to the app log**

In `Node.java`, constructor:

```java
    public Node(Host host) {
        this.host = host;
        Aead.log = host::log;
    }
```

- [ ] **Step 6: Run the tests**

Run: `android/core-test/run.sh`
Expected: all previous lines `ok`, the new section's nine lines `ok`, last line `ALL OK`, exit code 0.

If `platform cipher passes the self-test on this JVM` fails, stop and investigate before continuing; do not weaken the test.

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/dev/ferry/core/Aead.java android/app/src/main/java/dev/ferry/core/Proto.java android/app/src/main/java/dev/ferry/core/Node.java android/core-test/CoreTest.java
git commit -m "Android core: native ChaCha20-Poly1305 with self-test and built-in fallback"
```

---

### Task 2: Faster built-in ChaCha20

**Files:**
- Modify: `android/app/src/main/java/dev/ferry/core/Crypto.java` (ChaCha20 section)
- Test: existing `android/core-test/CoreTest.java` (RFC vectors, 200 random JDK cross-checks, Task 1's size sweep)

**Interfaces:**
- Consumes / produces: `Crypto.chacha20Xor(byte[] key, int counter, byte[] nonce, byte[] data, int off, int len)` — signature and output unchanged.

- [ ] **Step 1: Record the speed before**

```bash
mkdir -p "$TMPDIR/fb" && cat > "$TMPDIR/fb/B.java" <<'EOF'
import dev.ferry.core.Crypto;
public class B {
    public static void main(String[] a) {
        byte[] key = new byte[32], nonce = new byte[12], pt = new byte[65537];
        int n = (256 << 20) / pt.length;
        for (int round = 0; round < 3; round++) {
            long t = System.nanoTime();
            for (int i = 0; i < n; i++) { nonce[11] = (byte) i; nonce[10] = (byte) (i >> 8); Crypto.seal(key, nonce, new byte[0], pt); }
            System.out.printf("built-in seal: %.0f MB/s%n", 256 * 1.048576 / ((System.nanoTime() - t) / 1e9));
        }
    }
}
EOF
javac -Xlint:-options -d "$TMPDIR/fb/out" android/app/src/main/java/dev/ferry/core/*.java "$TMPDIR/fb/B.java" && java -cp "$TMPDIR/fb/out" B
```

Expected: three lines; note the last one (about 450 MB/s on the development PC).

- [ ] **Step 2: Rewrite the keystream loop**

In `Crypto.java`, replace the whole `block(...)` method and the whole `chacha20Xor(...)` method with:

```java
    private static void rounds(int[] w) {
        for (int i = 0; i < 10; i++) {
            qr(w, 0, 4, 8, 12);
            qr(w, 1, 5, 9, 13);
            qr(w, 2, 6, 10, 14);
            qr(w, 3, 7, 11, 15);
            qr(w, 0, 5, 10, 15);
            qr(w, 1, 6, 11, 12);
            qr(w, 2, 7, 8, 13);
            qr(w, 3, 4, 9, 14);
        }
    }

    private static void initState(int[] s, int[] key, int[] nonce) {
        s[0] = 0x61707865;
        s[1] = 0x3320646e;
        s[2] = 0x79622d32;
        s[3] = 0x6b206574;
        System.arraycopy(key, 0, s, 4, 8);
        s[13] = nonce[0];
        s[14] = nonce[1];
        s[15] = nonce[2];
    }

    private static void block(int[] key, int counter, int[] nonce, byte[] out) {
        int[] s = new int[16];
        initState(s, key, nonce);
        s[12] = counter;
        int[] w = s.clone();
        rounds(w);
        for (int i = 0; i < 16; i++) put32(out, 4 * i, w[i] + s[i]);
    }

    public static void chacha20Xor(byte[] key, int counter, byte[] nonce, byte[] data, int off, int len) {
        int[] s = new int[16], w = new int[16];
        initState(s, words(key, 8), words(nonce, 3));
        for (int p = 0; p < len; p += 64) {
            s[12] = counter++;
            System.arraycopy(s, 0, w, 0, 16);
            rounds(w);
            int o = off + p, m = Math.min(64, len - p);
            if (m == 64) {
                for (int i = 0; i < 16; i++) {
                    int v = w[i] + s[i], q = o + 4 * i;
                    data[q] ^= (byte) v;
                    data[q + 1] ^= (byte) (v >>> 8);
                    data[q + 2] ^= (byte) (v >>> 16);
                    data[q + 3] ^= (byte) (v >>> 24);
                }
            } else {
                for (int i = 0; i < m; i++) data[o + i] ^= (byte) ((w[i >>> 2] + s[i >>> 2]) >>> (8 * (i & 3)));
            }
        }
    }
```

Leave `qr`, `words`, `le32`, `put32` and everything else as they are.

- [ ] **Step 3: Run the tests**

Run: `android/core-test/run.sh`
Expected: `ALL OK`. `aead tag`, `aead open`, `chacha20-poly1305 == JDK (200 random)` and `platform and built-in produce identical frames (10 sizes)` are the lines that prove the keystream is unchanged.

- [ ] **Step 4: Record the speed after**

Run the two commands of Step 1's last line again (`javac … && java -cp "$TMPDIR/fb/out" B`).
Expected: not slower than Step 1. If it is slower, revert this task (`git checkout android/app/src/main/java/dev/ferry/core/Crypto.java`) and skip its commit — the platform cipher of Task 1 already carries the speed-up.

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/dev/ferry/core/Crypto.java
git commit -m "Android core: built-in ChaCha20 without per-block allocations"
```

---

### Task 3: Timing line in the Android core log

**Files:**
- Modify: `android/app/src/main/java/dev/ferry/core/Proto.java` (`Channel`)
- Modify: `android/app/src/main/java/dev/ferry/core/Node.java` (`serve`, `serveGuest`, `sendFile`, new `transferLine`)
- Test: `android/core-test/CoreTest.java`

**Interfaces:**
- Produces: `public long Channel.sealNanos, Channel.writeNanos` (running totals); `public static String Node.transferLine(String verb, String name, long bytes, long nanos, String extra)`.

- [ ] **Step 1: Write the failing test**

At the end of `frameCipherTests()` in `CoreTest.java` add:

```java
        System.out.println("Log line");
        check(Node.transferLine("sent", "a.jpg", 35_000_000L, 2_100_000_000L, null)
                .equals("sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s)"), "transfer line");
        check(Node.transferLine("sent", "a.jpg", 35_000_000L, 2_100_000_000L, "read 0.2 s")
                .equals("sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s)"), "transfer line with details");
        check(Node.transferLine("received", "empty", 0, 0, null)
                .equals("received empty: 0 B in 0.0 s (instant)"), "transfer line for an empty file in no time");
```

- [ ] **Step 2: Run to see it fail**

Run: `android/core-test/run.sh`
Expected: compile error `cannot find symbol … transferLine`.

- [ ] **Step 3: Add `transferLine` to `Node.java`**

Directly below the existing `human(long b)` method:

```java
    /** Debug line for the log, e.g. "sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s)". */
    public static String transferLine(String verb, String name, long bytes, long nanos, String extra) {
        double s = nanos / 1e9;
        String speed = s > 0 ? String.format(java.util.Locale.ROOT, "%.1f MB/s", bytes / 1e6 / s) : "instant";
        return String.format(java.util.Locale.ROOT, "%s %s: %s in %.1f s (%s%s)", verb, name, human(bytes), s,
                speed, extra == null ? "" : "; " + extra);
    }
```

- [ ] **Step 4: Count seal and write time in `Proto.Channel`**

Add a field next to `sendCtr, recvCtr`:

```java
        /** Running totals for the debug log: time spent encrypting and writing to the socket. */
        public long sealNanos, writeNanos;
```

Replace the body of `sendRaw` with:

```java
        public void sendRaw(byte[] plaintext) throws IOException {
            if (plaintext.length > MAX_PLAINTEXT) throw new ProtoException("frame too large");
            long t0 = System.nanoTime();
            byte[] ct = Aead.seal(sendKey, nonce(sendCtr++), plaintext);
            long t1 = System.nanoTime();
            byte[] frame = new byte[4 + ct.length];
            int n = ct.length;
            frame[0] = (byte) (n >>> 24);
            frame[1] = (byte) (n >>> 16);
            frame[2] = (byte) (n >>> 8);
            frame[3] = (byte) n;
            System.arraycopy(ct, 0, frame, 4, ct.length);
            out.write(frame);
            out.flush();
            sealNanos += t1 - t0;
            writeNanos += System.nanoTime() - t1;
        }
```

- [ ] **Step 5: Log from `Node.sendFile`**

Replace the whole `sendFile` method (it becomes an instance method; both callers are already instance methods and need no change):

```java
    private void sendFile(Proto.Channel ch, Outgoing o, long[] done, long total, Progress prog)
            throws IOException {
        byte[] buf = new byte[Proto.CHUNK + 1];
        long t0 = System.nanoTime(), readNanos = 0, seal0 = ch.sealNanos, write0 = ch.writeNanos;
        ch.send(new Proto.Writer(Proto.T_FILE).str(o.name).u64(o.size));
        long left = o.size;
        try (InputStream in = o.source.open()) {
            while (left > 0) {
                int want = (int) Math.min(left, Proto.CHUNK);
                int off = 0;
                long r0 = System.nanoTime();
                while (off < want) {
                    int n = in.read(buf, 1 + off, want - off);
                    if (n < 0) throw new FileNotFoundException(o.name + " is shorter than expected");
                    off += n;
                }
                readNanos += System.nanoTime() - r0;
                buf[0] = Proto.T_DATA;
                ch.sendRaw(Arrays.copyOf(buf, want + 1));
                left -= want;
                done[0] += want;
                if (prog != null) prog.update(done[0], total);
            }
        }
        ch.waitAck();
        host.log(transferLine("sent", o.name, o.size, System.nanoTime() - t0, String.format(java.util.Locale.ROOT,
                "read %.1f s, encrypt %.1f s, network %.1f s%s", readNanos / 1e9, (ch.sealNanos - seal0) / 1e9,
                (ch.writeNanos - write0) / 1e9, Aead.usingPlatform() ? "" : ", built-in cipher")));
    }
```

- [ ] **Step 6: Log from the two receive paths**

In `Node.serve`, in the `T_FILE` branch, replace

```java
                    Incoming inc = host.beginFile(peer, sanitize(name), size);
                    try {
                        receiveInto(ch, inc.stream(), size);
                        inc.commit();
```

with

```java
                    String clean = sanitize(name);
                    long t0 = System.nanoTime();
                    Incoming inc = host.beginFile(peer, clean, size);
                    try {
                        receiveInto(ch, inc.stream(), size);
                        inc.commit();
                        host.log(transferLine("received", clean, size, System.nanoTime() - t0, null));
```

In `Node.serveGuest`, replace

```java
                    receiveInto(ch, t.file(fname, size), size);
                    t.fileDone();
```

with

```java
                    long t0 = System.nanoTime();
                    receiveInto(ch, t.file(fname, size), size);
                    t.fileDone();
                    host.log(transferLine("received", fname, size, System.nanoTime() - t0, null));
```

- [ ] **Step 7: Run the tests**

Run: `android/core-test/run.sh`
Expected: `ALL OK`, including the three `transfer line` checks.

- [ ] **Step 8: Commit**

```bash
git add android/app/src/main/java/dev/ferry/core/Proto.java android/app/src/main/java/dev/ferry/core/Node.java android/core-test/CoreTest.java
git commit -m "Android core: one timing line per transferred file in the log"
```

---

### Task 4: Live loopback test with mixed ciphers

**Files:**
- Modify: `android/core-test/CoreTest.java` (`interop`)
- Create: `tests/core_loopback.py`

**Interfaces:**
- Consumes: `Aead.forceInTree`, `Aead.usingPlatform()`, the `[phone] sent …` log line of Task 3.
- Produces: environment variable `FERRY_AEAD=intree` and stdin command `cipher` for the test "phone".

- [ ] **Step 1: Add the switch and the command to the test phone**

In `CoreTest.interop`, as the first statement of the method:

```java
        if ("intree".equals(System.getenv("FERRY_AEAD"))) Aead.forceInTree = true;
```

In the `switch (f[0])`, before `default:`:

```java
                    case "cipher":
                        System.out.println(Aead.usingPlatform() ? "OK platform" : "OK built-in");
                        break;
```

- [ ] **Step 2: Write the test**

Create `tests/core_loopback.py`:

```python
#!/usr/bin/env python3
"""Live test: two Android cores (Java, on the JVM) send files to each other, with every
combination of platform and built-in frame cipher. Needs only a JDK.

    python3 tests/core_loopback.py
"""
import filecmp, os, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUN = os.path.join(ROOT, "android/core-test/run.sh")
T = tempfile.mkdtemp(prefix="ferry-loopback-")
APORT, BPORT = 47841, 47842
SIZES = [0, 1, 65535, 65536, 65537, 5_000_123]

fails = 0
def check(ok, what):
    global fails
    print(("  ok   " if ok else "  FAIL ") + what)
    fails += 0 if ok else 1

class Phone:
    def __init__(self, tag, port, cipher):
        self.dir = f"{T}/{tag}"; os.makedirs(self.dir)
        self.logs = []
        env = dict(os.environ)
        env.pop("FERRY_AEAD", None)
        if cipher == "built-in": env["FERRY_AEAD"] = "intree"
        self.p = subprocess.Popen([RUN, "interop", self.dir, str(port)], env=env, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        assert self.line() == "READY"
    def line(self):
        while True:
            l = self.p.stdout.readline()
            if not l: return ""
            if l.startswith("  [phone]"): self.logs.append(l.strip()); continue
            return l.strip()
    def cmd(self, c):
        self.p.stdin.write(c + "\n"); self.p.stdin.flush(); return self.line()
    def stop(self):
        self.p.kill(); self.p.wait()

os.makedirs(f"{T}/src")
run = 0
for ca, cb in [("platform", "built-in"), ("built-in", "platform"), ("platform", "platform")]:
    run += 1
    print(f"{run}. A uses the {ca} cipher, B uses the {cb} cipher")
    a, b = Phone(f"a{run}", APORT, ca), Phone(f"b{run}", BPORT, cb)
    try:
        check(a.cmd("cipher") == f"OK {ca}" and b.cmd("cipher") == f"OK {cb}", "ciphers as requested")
        code = a.cmd("listen-pair").split()[1]
        check(b.cmd(f"pair 127.0.0.1:{APORT} {code}") == "OK paired JvmPhone", "paired")
        check(a.cmd("expect paired").startswith("OK"), "A saw the pairing")
        for src, dst, way in [(b, a, "b-to-a"), (a, b, "a-to-b")]:
            ok = True
            for size in SIZES:
                name = f"{way}-{size}.bin"
                with open(f"{T}/src/{name}", "wb") as f: f.write(os.urandom(size))
                sent = src.cmd(f"sendfile {T}/src/{name}")
                got = dst.cmd("expect file")
                same = os.path.exists(f"{dst.dir}/{name}") and filecmp.cmp(f"{T}/src/{name}", f"{dst.dir}/{name}", shallow=False)
                if sent != "OK sent" or not got.startswith("OK") or not same:
                    ok = False
                    print(f"       {name}: sender '{sent}', receiver '{got}', identical {same}")
            check(ok, f"{way}: {len(SIZES)} files identical (sizes {SIZES})")
            big = f"{way}-{SIZES[-1]}.bin"
            check(any(l.startswith(f"[phone] sent {big}: 5.0 MB in ") for l in src.logs), "sender logged a timing line")
            check(any(l.startswith(f"[phone] received {big}: 5.0 MB in ") for l in dst.logs), "receiver logged a timing line")
    finally:
        a.stop(); b.stop()

print("\nALL OK" if fails == 0 else f"\n{fails} FAILURE(S)  (files in {T})")
sys.exit(1 if fails else 0)
```

- [ ] **Step 3: Run it**

Run: `python3 tests/core_loopback.py`
Expected: three numbered sections, every line `ok`, then `ALL OK`, exit code 0.

- [ ] **Step 4: Prove the test can fail**

Temporarily break the built-in cipher: in `Crypto.java` change `chacha20Xor(key, 1, nonce, out, 0, pt.length);` inside `seal` to use counter `2`. Run `python3 tests/core_loopback.py`.
Expected: `ciphers as requested` fails for the platform side (self-test now disagrees) or transfers fail. Then restore the line (`git checkout android/app/src/main/java/dev/ferry/core/Crypto.java`) and rerun to see `ALL OK` again.

- [ ] **Step 5: Mention the test in the README**

In `README.md`, section *Development*, add this line after the `android/core-test/run.sh` line:

```
python3 tests/core_loopback.py                   # two Android cores, platform and built-in cipher mixed
```

- [ ] **Step 6: Commit**

```bash
git add android/core-test/CoreTest.java tests/core_loopback.py README.md
git commit -m "Live test: Android core to Android core with mixed frame ciphers"
```

---

### Task 5: Desktop timing lines

**Files:**
- Modify: `desktop/src/daemon.rs` (`send_file`, `receive_file`, new `transfer_line`, new test module)

**Interfaces:**
- Produces: `fn transfer_line(verb: &str, name: &str, bytes: u64, took: Duration, extra: &str) -> String` (private to `daemon.rs`).

- [ ] **Step 1: Write the failing test**

Append to the end of `desktop/src/daemon.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_line_format() {
        assert_eq!(
            transfer_line("sent", "a.jpg", 35_000_000, Duration::from_millis(2100), ""),
            "sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s)"
        );
        assert_eq!(
            transfer_line("sent", "a.jpg", 35_000_000, Duration::from_millis(2100), "read 0.2 s"),
            "sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s)"
        );
        assert_eq!(
            transfer_line("received", "empty", 0, Duration::ZERO, ""),
            "received empty: 0 B in 0.0 s (instant)"
        );
    }
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cd desktop && CARGO_HOME="$TMPDIR/cargo-home" cargo test --release --offline transfer_line`
Expected: compile error `cannot find function transfer_line`.

- [ ] **Step 3: Add `transfer_line` and use it**

Directly above `fn send_file` add:

```rust
/// Debug line for the log, e.g. "sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s)".
fn transfer_line(verb: &str, name: &str, bytes: u64, took: Duration, extra: &str) -> String {
    let s = took.as_secs_f64();
    let speed = if s > 0.0 { format!("{:.1} MB/s", bytes as f64 / 1e6 / s) } else { "instant".to_string() };
    let extra = if extra.is_empty() { String::new() } else { format!("; {}", extra) };
    format!("{} {}: {} in {:.1} s ({}{})", verb, name, inbox::human(bytes), s, speed, extra)
}
```

Replace the body of `send_file` from `let mut buf = vec![0u8; CHUNK];` to the end of the function with:

```rust
    let mut buf = vec![0u8; CHUNK];
    let mut left = size;
    let (started, mut reading) = (Instant::now(), Duration::ZERO);
    while left > 0 {
        let want = (left as usize).min(CHUNK);
        let t = Instant::now();
        f.read_exact(&mut buf[..want])?;
        reading += t.elapsed();
        let mut msg = Vec::with_capacity(want + 1);
        msg.push(T_DATA);
        msg.extend_from_slice(&buf[..want]);
        ch.send_raw(msg)?;
        left -= want as u64;
    }
    ch.wait_ack()?;
    let took = started.elapsed();
    let detail = format!(
        "read {:.1} s, encrypt+network {:.1} s",
        reading.as_secs_f64(),
        took.saturating_sub(reading).as_secs_f64()
    );
    eprintln!("ferry: {}", transfer_line("sent", &name, size, took, &detail));
    Ok(())
}
```

In `receive_file`, add as the first statement of the function:

```rust
    let started = Instant::now();
```

and replace its last two lines

```rust
    fs::rename(&tmp, &dest)?;
    Ok(dest)
```

with

```rust
    fs::rename(&tmp, &dest)?;
    eprintln!("ferry: {}", transfer_line("received", name, size, started.elapsed(), ""));
    Ok(dest)
```

- [ ] **Step 4: Run all desktop tests and the Windows type-check**

Run: `cd desktop && CARGO_HOME="$TMPDIR/cargo-home" cargo test --release --offline 2>&1 | grep -E "test result|FAILED|error|warning"`
Expected: `test result: ok. 12 passed; 0 failed` for the library, no `error`, no new `warning`.

Run: `cd desktop && FERRY_WINCHECK=1 CARGO_HOME="$TMPDIR/cargo-home" cargo check --offline 2>&1 | tail -3`
Expected: `Finished`. (If this check already fails on `main` for reasons unrelated to this change, note that and move on.)

- [ ] **Step 5: Commit**

```bash
git add desktop/src/daemon.rs
git commit -m "Desktop: one timing line per transferred file in the log"
```

---

### Task 6: Android app stays awake while sending

**Files:**
- Modify: `android/app/src/main/java/dev/ferry/app/FerryApp.java`

This task cannot be compiled in the sandbox (no Android SDK). Keep it exactly as written; the repository owner compiles it with `android/build.sh`.

**Interfaces:**
- Consumes: `Node.send(String, String, List<Node.Outgoing>, String, Node.Progress) → Node.Result` (unchanged).

- [ ] **Step 1: Add the imports**

In `FerryApp.java`, add in alphabetical position among the `android.*` imports:

```java
import android.net.wifi.WifiManager;
import android.os.PowerManager;
```

- [ ] **Step 2: Add the locks**

Below the field `private NotificationManager nm;`:

```java
    private PowerManager.WakeLock sendWake;
    private WifiManager.WifiLock sendWifi;
```

In `onCreate`, directly after `nm = getSystemService(NotificationManager.class);`:

```java
        sendWake = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ferry:send");
        sendWifi = getSystemService(WifiManager.class).createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "ferry:send");
```

- [ ] **Step 3: Add the wrapper**

Directly above the `sendText` method (above its `/** Text to one device …` comment):

```java
    /**
     * Runs a send with the CPU and Wi-Fi kept out of power-save, like FerryService does for
     * receiving. The wake lock times out by itself; the Wi-Fi lock is released in finally.
     */
    private Node.Result sendAwake(String targetHex, String targetName, List<Node.Outgoing> files, String text,
                                  Node.Progress progress) {
        boolean wake = false, wifi = false;
        try {
            try {
                sendWake.acquire(30 * 60 * 1000L);
                wake = true;
                sendWifi.acquire();
                wifi = true;
            } catch (RuntimeException e) {
                log("send locks: " + e);
            }
            return node.send(targetHex, targetName, files, text, progress);
        } finally {
            try {
                if (wifi) sendWifi.release();
            } catch (RuntimeException ignored) {
            }
            try {
                if (wake && sendWake.isHeld()) sendWake.release();
            } catch (RuntimeException ignored) {
            }
        }
    }
```

- [ ] **Step 4: Use it**

In `sendText` replace `node.send(targetHex, targetName, null, text, null)` with `sendAwake(targetHex, targetName, null, text, null)`.

In `sendFiles` replace `Node.Result r = node.send(targetHex, targetName, files, null, (done, total) -> {` with `Node.Result r = sendAwake(targetHex, targetName, files, null, (done, total) -> {`.

- [ ] **Step 5: Check what can be checked here**

Run: `grep -n "node.send(" android/app/src/main/java/dev/ferry/app/FerryApp.java`
Expected: exactly one hit, inside `sendAwake`.

Run: `grep -n "WAKE_LOCK" android/app/src/main/AndroidManifest.xml`
Expected: one hit (the permission is already declared; `WifiLock` needs nothing else).

- [ ] **Step 6: Commit**

```bash
git add android/app/src/main/java/dev/ferry/app/FerryApp.java
git commit -m "Android app: hold wake and Wi-Fi locks while sending"
```

---

### Task 7: Release workflow and version 0.2.2

**Files:**
- Modify: `.github/workflows/build.yml`
- Modify: `desktop/Cargo.toml`, `desktop/Cargo.lock`, `android/app/build.gradle`, `README.md`

- [ ] **Step 1: Replace `.github/workflows/build.yml`**

```yaml
# Builds both apps on GitHub on every push (download the artifacts from the Actions tab).
# Pushing a tag like v0.2.2 also creates a GitHub release with the installable files.
#
# APK signing: Android only installs an update over an existing app if both are signed with the
# same key. Without the two repository secrets below, the APK gets a throw-away key. To use your
# own key from android/build.sh, add (Settings -> Secrets and variables -> Actions):
#   FERRY_KEYSTORE_B64   output of: base64 -w0 ~/.config/ferry/android-release.jks
#   FERRY_KEYSTORE_PASS  content of: ~/.config/ferry/android-release.pass
name: build
on: [push, workflow_dispatch]
permissions:
  contents: read
jobs:
  desktop:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cd desktop && cargo test --release && cargo build --release
      - uses: actions/upload-artifact@v4
        with: { name: ferry-desktop, path: desktop/target/release/ferry }
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - run: cd desktop; cargo build --release
      - uses: actions/upload-artifact@v4
        with: { name: ferry-windows, path: "desktop/target/release/*.exe" }
  windows-installer:
    # Cross-compiles on Linux and builds dist/FerrySetup-<version>.exe with NSIS.
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: sudo apt-get update && sudo apt-get install -y mingw-w64 nsis
      - run: desktop/package-windows.sh
      - uses: actions/upload-artifact@v4
        with: { name: ferry-windows-installer, path: "desktop/dist/*.exe" }
  android:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-java@v4
        with: { distribution: temurin, java-version: 17 }
      - run: android/core-test/run.sh
      - run: python3 tests/core_loopback.py
      - name: Use the repository's signing key, if it has one
        env:
          KEYSTORE_B64: ${{ secrets.FERRY_KEYSTORE_B64 }}
          KEYSTORE_PASS: ${{ secrets.FERRY_KEYSTORE_PASS }}
        run: |
          if [ -n "$KEYSTORE_B64" ] && [ -n "$KEYSTORE_PASS" ]; then
            mkdir -p ~/.config/ferry && chmod 700 ~/.config/ferry
            printf '%s' "$KEYSTORE_B64" | base64 -d > ~/.config/ferry/android-release.jks
            printf '%s' "$KEYSTORE_PASS" > ~/.config/ferry/android-release.pass
            chmod 600 ~/.config/ferry/android-release.*
          fi
      - run: android/build.sh
      - uses: actions/upload-artifact@v4
        with: { name: ferry-apk, path: android/Ferry.apk }
  release:
    # Only for version tags, and only when everything above succeeded.
    if: startsWith(github.ref, 'refs/tags/v')
    needs: [desktop, windows, windows-installer, android]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
        with: { path: dist }
      - env:
          GH_TOKEN: ${{ github.token }}
        run: |
          mv dist/ferry-desktop/ferry dist/ferry-linux-x86_64
          gh release create "$GITHUB_REF_NAME" --repo "$GITHUB_REPOSITORY" --title "$GITHUB_REF_NAME" --notes "" \
            dist/ferry-windows-installer/*.exe dist/ferry-apk/Ferry.apk dist/ferry-linux-x86_64
```

- [ ] **Step 2: Check the workflow file**

Run:

```bash
python3 - <<'EOF'
import yaml
w = yaml.safe_load(open(".github/workflows/build.yml"))
jobs = w["jobs"]
assert set(jobs) == {"desktop", "windows", "windows-installer", "android", "release"}, set(jobs)
assert w["permissions"] == {"contents": "read"}
r = jobs["release"]
assert r["if"] == "startsWith(github.ref, 'refs/tags/v')"
assert sorted(r["needs"]) == ["android", "desktop", "windows", "windows-installer"]
assert r["permissions"] == {"contents": "write"}
uses = [s["uses"] for j in jobs.values() for s in j["steps"] if "uses" in s]
assert all(u.startswith("actions/") for u in uses), uses
print("workflow ok")
EOF
```

Expected: `workflow ok`.

- [ ] **Step 3: Bump the versions**

`desktop/Cargo.toml`: `version = "0.2.1"` → `version = "0.2.2"`.

`android/app/build.gradle`: `versionCode = 2` → `versionCode = 3`, `versionName = '0.2.0'` → `versionName = '0.2.2'`.

`README.md`: `desktop/dist/FerrySetup-0.2.1.exe` → `desktop/dist/FerrySetup-0.2.2.exe`.

Run: `cd desktop && CARGO_HOME="$TMPDIR/cargo-home" cargo build --release --offline 2>&1 | tail -1 && grep -A1 'name = "ferry"' Cargo.lock`
Expected: `Finished …`, and `Cargo.lock` now shows `version = "0.2.2"`.

- [ ] **Step 4: Run everything that runs here**

```bash
cd /home/luki/code/home/github/ferry
(cd desktop && CARGO_HOME="$TMPDIR/cargo-home" cargo test --release --offline 2>&1 | grep -E "test result|FAILED|error")
android/core-test/run.sh | tail -1
python3 tests/core_loopback.py | tail -1
```

Expected: `test result: ok. 12 passed`, `ALL OK`, `ALL OK`.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/build.yml desktop/Cargo.toml desktop/Cargo.lock android/app/build.gradle README.md
git commit -m "v0.2.2: release job for version tags, version bump"
```

---

### Task 8: Hand-over to the repository owner

Not automatable in the sandbox. Report these steps to the owner, in this order:

1. `python3 tests/interop.py && python3 tests/desktop_v2.py && python3 tests/interop_v2.py` — desktop daemon ↔ Android core, expected `ALL OK` three times.
2. `cd android && ./build.sh install` — compiles the app-layer change of Task 6 and installs it.
3. Send the 35 MB photo phone → PC. Then read the two log lines:
   `adb logcat -s Ferry | grep "sent "` on the phone side and `journalctl --user -u ferry -n 20 | grep received` on the PC.
4. Optional: add the two signing secrets named at the top of `.github/workflows/build.yml`.
5. Merge `speed-0.2.2`, then `git tag v0.2.2 && git push origin main v0.2.2` to produce the release.
