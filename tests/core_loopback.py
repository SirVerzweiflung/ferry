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
