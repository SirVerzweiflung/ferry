#!/usr/bin/env python3
"""v0.2 phone <-> desktop scenarios: Rust daemons A, B (paired with the phone), C (stranger)
and the Android core running on the JVM as the phone P.
   python3 tests/interop_v2.py"""
import os, socket, subprocess, sys, tempfile, threading, time, filecmp

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FERRY = os.path.join(ROOT, "desktop/target/release/ferry")
T = tempfile.mkdtemp(prefix="ferry-iv2-")
PORTS = {"A": 47860, "B": 47862, "C": 47864}
PPORT = 47866
DISC = ",".join(str(p) for p in [*PORTS.values(), PPORT])
fails = 0
procs = {}

def check(ok, what):
    global fails
    print(("  ok   " if ok else "  FAIL ") + what)
    fails += 0 if ok else 1

def env(n):
    return dict(os.environ, FERRY_HOME=f"{T}/{n}", FERRY_SOCKET=f"{T}/{n}.sock", FERRY_DISCOVERY_PORTS=DISC)

def ferry(n, *args):
    r = subprocess.run([FERRY, *args], env=env(n), capture_output=True, text=True, timeout=40)
    return (r.stdout + r.stderr).strip()

def start(n):
    procs[n] = subprocess.Popen([FERRY, "daemon"], env=env(n), stderr=open(f"{T}/{n}.log", "a"))
    for _ in range(50):
        try:
            s = socket.socket(socket.AF_UNIX); s.connect(f"{T}/{n}.sock"); s.close(); return
        except OSError:
            time.sleep(0.1)

class Ext:
    def __init__(self, n):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(f"{T}/{n}.sock")
        self.s.sendall(b"subscribe\tclipboard\n"); self.clips = []
        threading.Thread(target=self.loop, daemon=True).start()
    def loop(self):
        for line in self.s.makefile():
            if line.startswith("setclip\t"):
                self.clips.append(line.rstrip("\n").split("\t", 1)[1])
    def copy(self, text):
        self.s.sendall(f"clipchanged\t{text}\n".encode())

def wait_for(pred, secs=10):
    for _ in range(int(secs * 10)):
        if pred(): return True
        time.sleep(0.1)
    return False

for n, port in PORTS.items():
    os.makedirs(f"{T}/{n}"); os.makedirs(f"{T}/{n}-dl")
    open(f"{T}/{n}/config", "w").write(f"name = Desk{n}\nport = {port}\ndownload_dir = {T}/{n}-dl\nnotifications = off\n")
    start(n)
os.makedirs(f"{T}/phone")
subprocess.run([os.path.join(ROOT, "android/core-test/run.sh"), "--help"], capture_output=True)
phone = subprocess.Popen([os.path.join(ROOT, "android/core-test/run.sh"), "interop", f"{T}/phone", str(PPORT)],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                         env=dict(os.environ, FERRY_DISCOVERY_PORTS=DISC))
def readline():
    while True:
        l = phone.stdout.readline()
        if not l: return ""
        if l.startswith("  [phone]"): continue
        return l.strip()
def cmd(c):
    phone.stdin.write(c + "\n"); phone.stdin.flush(); return readline()

try:
    assert readline() == "READY"
    print("1. phone pairs with A and B (codes shown on the desktops)")
    for n in ("A", "B"):
        p = subprocess.Popen([FERRY, "pair"], env=env(n), stdout=subprocess.PIPE, text=True)
        code = next(l.split()[-1] for l in p.stdout if "Code" in l)
        r = cmd(f"pair 127.0.0.1:{PORTS[n]} {code}")
        check(r == f"OK paired Desk{n}", r)
        p.wait(timeout=10)
    out = ferry("A", "devices")
    check("JvmPhone" in out.split("Nearby")[0] and "phone" in out, "A lists the phone as its device:\n" + out)

    print("2. clipboard: copy on A -> phone -> relayed by the phone to B")
    ea, eb = Ext("A"), Ext("B")
    time.sleep(0.3)
    ea.copy("from desk A")
    r = cmd("expect clip"); check(r == "OK clip from desk A", r)
    check(wait_for(lambda: "from desk A" in eb.clips), f"B got it via the phone {eb.clips}")
    check("from desk A" not in ea.clips, "no echo to A")

    print("3. phone 'Send clipboard' -> all my devices")
    check(cmd("sendclip from the phone") == "OK clip", "sent")
    check(wait_for(lambda: "from the phone" in ea.clips and "from the phone" in eb.clips), "A and B got it")

    print("4. phone sees C as nearby and sends to it -> C's Incoming")
    r = cmd("devices"); check("DeskA(mine)" in r and "DeskB(mine)" in r and "DeskC(nearby)" in r, r)
    f1 = f"{T}/photo.jpg"; open(f1, "wb").write(os.urandom(700_000))
    r = cmd(f"sendto DeskC {f1}"); check(r.startswith("OK") and "accept" in r, r)
    inc = ferry("C", "incoming"); check("JvmPhone sent photo.jpg" in inc, inc)
    ferry("C", "accept", "all")
    check(filecmp.cmp(f1, f"{T}/C-dl/photo.jpg", shallow=False), "accepted on C")

    print("5. C sends to the (unpaired) phone -> phone's Incoming")
    f2 = f"{T}/notes.txt"; open(f2, "w").write("hi phone\n")
    out = ferry("C", "send", "--to", "JvmPhone", f2); check("waits there" in out, out)
    r = cmd("expect incoming"); check(r == "OK incoming DeskC notes.txt", r)

    print("6. queue on the phone: A is off -> queued -> A starts -> delivered")
    procs["A"].kill(); procs["A"].wait()
    f3 = f"{T}/queued.bin"; open(f3, "wb").write(os.urandom(200_000))
    r = cmd(f"sendto DeskA {f3}"); check("queued" in r, r)
    start("A")
    r = cmd("expect queued-result"); check(r.startswith("OK queued-result Sent"), r)
    check(filecmp.cmp(f3, f"{T}/A-dl/queued.bin", shallow=False), "file on A")

    print("7. queue on the desktop: phone is off -> queued -> phone back -> delivered")
    phone.kill(); phone.wait()
    out = ferry("B", "send", "--to", "JvmPhone", f2); check("queued" in out, out)
finally:
    for p in procs.values():
        p.kill()
    phone.kill()

print("\nALL OK" if fails == 0 else f"\n{fails} FAILURE(S)  (logs in {T})")
sys.exit(1 if fails else 0)
