#!/usr/bin/env python3
"""Live interop test: real desktop daemon (Rust) <-> Android core (Java, on the JVM).

    python3 tests/interop.py      (needs: cargo build --release in desktop/, a JDK)
"""
import os, socket, subprocess, sys, tempfile, threading, time, filecmp

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FERRY = os.path.join(ROOT, "desktop/target/release/ferry")
T = tempfile.mkdtemp(prefix="ferry-interop-")
DPORT, PPORT = 47830, 47831
env = dict(os.environ, FERRY_HOME=f"{T}/desk", FERRY_SOCKET=f"{T}/desk.sock")
os.makedirs(f"{T}/desk"); os.makedirs(f"{T}/dl"); os.makedirs(f"{T}/phone")
open(f"{T}/desk/config", "w").write(f"name = Desk\nport = {DPORT}\ndownload_dir = {T}/dl\nnotifications = off\n")

fails = 0
def check(ok, what):
    global fails
    print(("  ok   " if ok else "  FAIL ") + what)
    fails += 0 if ok else 1

def ferry(*args, wait=True):
    if wait:
        r = subprocess.run([FERRY, *args], env=env, capture_output=True, text=True, timeout=30)
        return (r.stdout + r.stderr).strip()
    return subprocess.Popen([FERRY, *args], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

daemon = subprocess.Popen([FERRY, "daemon"], env=env, stderr=open(f"{T}/daemon.log", "w"))
time.sleep(0.4)

# fake GNOME extension: receives setclip events
clips = []
def ext():
    s = socket.socket(socket.AF_UNIX); s.connect(f"{T}/desk.sock")
    s.sendall(b"subscribe\tclipboard\n")
    for line in s.makefile():
        if line.startswith("setclip\t"):
            clips.append(line.rstrip("\n").split("\t", 1)[1].replace("\\n", "\n"))
threading.Thread(target=ext, daemon=True).start()

subprocess.run([os.path.join(ROOT, "android/core-test/run.sh"), "--help"], capture_output=True)  # compile once
phone = subprocess.Popen([os.path.join(ROOT, "android/core-test/run.sh"), "interop", f"{T}/phone", str(PPORT)],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
def readline():
    while True:
        l = phone.stdout.readline()
        if not l: return ""
        if l.startswith("  [phone]"): print(l.rstrip()); continue
        return l.strip()
def cmd(c):
    phone.stdin.write(c + "\n"); phone.stdin.flush(); return readline()

assert readline() == "READY"

print("1. phone shows code, desktop enters it")
code = cmd("listen-pair").split()[1]
out = ferry("pair", f"127.0.0.1:{PPORT}", code)
check("paired with JvmPhone" in out, f"desktop: {out}")
check(cmd("expect paired").startswith("OK"), "phone saw pairing")

print("2. desktop -> phone file + clipboard")
big = f"{T}/big.bin"; open(big, "wb").write(os.urandom(5_000_123))
out = ferry("send", big); check(out.startswith("Sent"), f"desktop: {out}")
check(cmd("expect file").startswith("OK"), "phone got file")
check(filecmp.cmp(big, f"{T}/phone/big.bin", shallow=False), "file identical")
out = ferry("clip", "hello from desktop äöü 🚀"); check("clipboard sent" in out, f"desktop: {out}")
r = cmd("expect clip"); check(r == "OK clip hello from desktop äöü 🚀", r)

print("3. auto clipboard sync (extension reports a change)")
s = socket.socket(socket.AF_UNIX); s.connect(f"{T}/desk.sock")
s.sendall(b"clipchanged\tcopied on desktop\n")
r = cmd("expect clip"); check(r == "OK clip copied on desktop", r)

print("4. phone -> desktop file + clipboard")
small = f"{T}/note.txt"; open(small, "w").write("hi\n")
check(cmd(f"sendfile {small}") == "OK sent", "phone sent file")
check(filecmp.cmp(small, f"{T}/dl/note.txt", shallow=False), "desktop has file")
check(cmd("sendclip line1\\nline2") == "OK clip", "phone sent clip")
time.sleep(0.3)
check(clips[-1:] == ["line1\nline2"], f"extension got setclip {clips[-1:]}")

print("5. re-pair the other way: desktop shows code, phone enters it")
out = ferry("unpair", "JvmPhone"); check("removed" in out, out)
p = ferry("pair", wait=False)
code = None
for l in p.stdout:
    if "Code" in l: code = l.split()[-1]; break
r = cmd(f"pair 127.0.0.1:{DPORT} {code}"); check(r == "OK paired Desk", r)
p.wait(timeout=10); check("Paired with JvmPhone" in p.stdout.read(), "desktop CLI saw pairing")
check(cmd(f"sendfile {small}") == "OK sent", "phone sends after re-pair")
check(os.path.exists(f"{T}/dl/note (1).txt"), "duplicate name -> 'note (1).txt'")

print("6. wrong code is rejected")
ferry("pair", wait=False); time.sleep(0.3)
r = cmd(f"pair 127.0.0.1:{DPORT} AAAAA-AAAAA"); check(r.startswith("FAIL"), r)

print("7. stale address -> UDP discovery")
cmd(f"breakaddr {DPORT}")
r = cmd("sendclip found via discovery")
check(r == "OK clip", f"{r} (broadcast may be blocked in containers)")

phone.kill(); daemon.kill()
print("\nALL OK" if fails == 0 else f"\n{fails} FAILURE(S)  (logs in {T})")
sys.exit(1 if fails else 0)
