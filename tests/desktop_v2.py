#!/usr/bin/env python3
"""v0.2 desktop scenarios with four real daemons on one machine:
   A <-paired-> B <-paired-> D,  C = unpaired stranger on the network.
   python3 tests/desktop_v2.py      (needs: cargo build --release in desktop/)"""
import os, socket, subprocess, sys, tempfile, threading, time, filecmp

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FERRY = os.path.join(ROOT, "desktop/target/release/ferry")
T = tempfile.mkdtemp(prefix="ferry-v2-")
PORTS = {"A": 47840, "B": 47842, "C": 47844, "D": 47846}   # each also uses port+1 for nothing on Linux
DISC = ",".join(str(p) for p in PORTS.values())
fails = 0
procs = {}

def check(ok, what):
    global fails
    print(("  ok   " if ok else "  FAIL ") + what)
    fails += 0 if ok else 1

def env(n):
    return dict(os.environ, FERRY_HOME=f"{T}/{n}", FERRY_SOCKET=f"{T}/{n}.sock", FERRY_DISCOVERY_PORTS=DISC)

def ferry(n, *args, timeout=40):
    r = subprocess.run([FERRY, *args], env=env(n), capture_output=True, text=True, timeout=timeout)
    return (r.stdout + r.stderr).strip()

def start(n):
    procs[n] = subprocess.Popen([FERRY, "daemon"], env=env(n), stderr=open(f"{T}/{n}.log", "a"))
    for _ in range(50):
        if os.path.exists(f"{T}/{n}.sock"):
            try:
                s = socket.socket(socket.AF_UNIX); s.connect(f"{T}/{n}.sock"); s.close(); return
            except OSError:
                pass
        time.sleep(0.1)

def stop(n):
    procs[n].kill(); procs[n].wait()

def pair(shower, enterer):
    p = subprocess.Popen([FERRY, "pair"], env=env(shower), stdout=subprocess.PIPE, text=True)
    code = None
    for l in p.stdout:
        if "Code" in l:
            code = l.split()[-1]; break
    out = ferry(enterer, "pair", f"127.0.0.1:{PORTS[shower]}", code)
    p.wait(timeout=10)
    return out

class Ext:
    """Fake GNOME extension: records setclip events, can report clipboard changes."""
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

for n, port in PORTS.items():
    os.makedirs(f"{T}/{n}"); os.makedirs(f"{T}/{n}-dl")
    open(f"{T}/{n}/config", "w").write(
        f"name = Desk{n}\nport = {port}\ndownload_dir = {T}/{n}-dl\nnotifications = off\n")
    start(n)

try:
    print("1. pairing: A<->B, B<->D")
    check("paired with DeskA" in pair("A", "B"), "B paired with A")
    check("paired with DeskB" in pair("B", "D"), "D paired with B")

    print("2. device list on A: B is mine, C and D are nearby")
    out = ferry("A", "devices")
    mine, near = out.split("Nearby")
    check("DeskB" in mine and "DeskC" in near and "DeskD" in near, "devices output:\n" + out)

    print("3. A -> C (stranger): lands in C's Incoming, accept moves it to Downloads")
    f1 = f"{T}/report.pdf"; open(f1, "wb").write(os.urandom(300_000))
    out = ferry("A", "send", "--to", "DeskC", f1)
    check("waits there until they accept" in out, out)
    check(not os.listdir(f"{T}/C-dl"), "nothing in C's Downloads yet")
    inc = ferry("C", "incoming")
    check("DeskA sent report.pdf" in inc, inc)
    tid = inc.split()[0]
    out = ferry("C", "accept", tid)
    check(filecmp.cmp(f1, f"{T}/C-dl/report.pdf", shallow=False), f"accepted: {out}")
    check("Nothing waiting" in ferry("C", "incoming"), "Incoming empty after accept")

    print("4. text to a stranger waits too; decline deletes")
    check("waits there" in ferry("A", "text", "--to", "DeskC", "hello stranger"), "text sent")
    inc = ferry("C", "incoming"); check("a text" in inc, inc)
    check("declined 1" in ferry("C", "decline", "all"), "declined")

    print("5. block: C blocks A, A can no longer send")
    ferry("A", "text", "--to", "DeskC", "spam 1")
    tid = ferry("C", "incoming").split()[0]
    check("blocked DeskA" in ferry("C", "block", tid), "C blocked A")
    out = ferry("A", "text", "--to", "DeskC", "spam 2")
    check("does not accept" in out, f"A refused: {out}")
    ferry("C", "unblock", "all")

    print("6. size limit for strangers")
    ferry("C", "set", "incoming_limit_mb", "1")
    big = f"{T}/big.bin"; open(big, "wb").write(os.urandom(2_500_000))
    out = ferry("A", "send", "--to", "DeskC", big)
    check("too large" in out, out)
    ferry("C", "set", "incoming_limit_mb", "2048")

    print("7. paired send is direct (no Incoming)")
    out = ferry("A", "send", "--to", "DeskB", big)
    check(out.startswith("Sent") and filecmp.cmp(big, f"{T}/B-dl/big.bin", shallow=False), out)

    print("8. queue: B is off -> queued; B comes back -> delivered automatically")
    stop("B")
    f2 = f"{T}/later.txt"; open(f2, "w").write("sent while you were away\n")
    out = ferry("A", "send", "--to", "DeskB", f2)
    check("queued" in out, out)
    check("later.txt -> DeskB" in ferry("A", "queue"), "listed in queue")
    start("B")   # B announces itself on start -> A retries immediately
    ok = False
    for _ in range(40):
        if os.path.exists(f"{T}/B-dl/later.txt"):
            ok = True; break
        time.sleep(0.25)
    check(ok, "delivered after B came back")
    time.sleep(0.3)
    check("Nothing queued" in ferry("A", "queue"), "queue empty")

    print("9. clipboard: copy on A reaches B and is relayed to D (not paired with A)")
    ea, eb, ed = Ext("A"), Ext("B"), Ext("D")
    time.sleep(0.3)
    ea.copy("copied on A")
    for _ in range(40):
        if "copied on A" in ed.clips: break
        time.sleep(0.1)
    check("copied on A" in eb.clips, f"B got it {eb.clips}")
    check("copied on A" in ed.clips, f"D got it via B {ed.clips}")
    time.sleep(0.5)
    check("copied on A" not in ea.clips, "no echo back to A")

    print("10. invisible: C hides, A no longer lists it and cannot send")
    ferry("C", "set", "visible", "off")
    time.sleep(0.2)
    out = ferry("A", "devices")
    check("DeskC" not in out.split("Nearby")[1], "C hidden")
finally:
    for p in procs.values():
        p.kill()

print("\nALL OK" if fails == 0 else f"\n{fails} FAILURE(S)  (logs in {T})")
sys.exit(1 if fails else 0)
