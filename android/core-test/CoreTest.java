import dev.ferry.core.*;

import java.io.*;
import java.net.*;
import java.nio.file.*;
import java.security.*;
import java.security.spec.*;
import java.util.*;
import java.util.concurrent.*;
import javax.crypto.*;
import javax.crypto.spec.*;

/**
 * JVM test for the Android core: RFC vectors, cross-check against the JDK's own
 * X25519 / ChaCha20-Poly1305, and (optionally) live interop with the desktop daemon.
 *
 *   ./run.sh                      crypto tests only
 *   ./run.sh interop <dir>        acts as a "phone" for the interop script
 */
public class CoreTest {
    static int fails = 0;

    static void check(boolean ok, String what) {
        System.out.println((ok ? "  ok   " : "  FAIL ") + what);
        if (!ok) fails++;
    }

    static byte[] h(String s) {
        return Crypto.unhex(s.replaceAll("\\s", ""));
    }

    public static void main(String[] a) throws Exception {
        if (a.length > 0 && a[0].equals("interop")) {
            interop(a);
            return;
        }
        System.out.println("RFC vectors");
        byte[] ask = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        byte[] bsk = h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        byte[] apk = Crypto.x25519(ask, Crypto.X25519_BASE);
        byte[] bpk = Crypto.x25519(bsk, Crypto.X25519_BASE);
        check(Crypto.hex(apk).equals("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"), "x25519 public A");
        check(Crypto.hex(Crypto.x25519(ask, bpk)).equals("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"), "x25519 shared");
        check(Crypto.hex(Crypto.poly1305(h("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b"),
                "Cryptographic Forum Research Group".getBytes())).equals("a8061dc1305136c6c22b8baf0c0127a9"), "poly1305");
        byte[] key = h("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        byte[] nonce = h("070000004041424344454647");
        byte[] aad = h("50515253c0c1c2c3c4c5c6c7");
        byte[] pt = "Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".getBytes();
        byte[] ct = Crypto.seal(key, nonce, aad, pt);
        check(Crypto.hex(Arrays.copyOfRange(ct, pt.length, ct.length)).equals("1ae10b594f09e26a7e902ecbd0600691"), "aead tag");
        check(Arrays.equals(Crypto.open(key, nonce, aad, ct), pt), "aead open");
        ct[5] ^= 1;
        check(Crypto.open(key, nonce, aad, ct) == null, "aead rejects tampering");
        check(Crypto.hex(Crypto.hkdf(h("000102030405060708090a0b0c"), h("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"),
                h("f0f1f2f3f4f5f6f7f8f9"), 42)).equals(
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"), "hkdf");

        System.out.println("Cross-check against JDK providers (random inputs)");
        Random r = new Random(1);
        boolean xok = true, cok = true;
        KeyFactory kf = KeyFactory.getInstance("XDH");
        for (int i = 0; i < 200; i++) {
            byte[] sk = Crypto.random(32), pk = Crypto.x25519(Crypto.random(32), Crypto.X25519_BASE);
            byte[] le = pk.clone();
            byte[] be = new byte[32];
            for (int j = 0; j < 32; j++) be[j] = le[31 - j];
            be[0] &= 0x7f;
            PublicKey jpub = kf.generatePublic(new XECPublicKeySpec(NamedParameterSpec.X25519, new java.math.BigInteger(1, be)));
            PrivateKey jpriv = kf.generatePrivate(new XECPrivateKeySpec(NamedParameterSpec.X25519, sk));
            KeyAgreement ka = KeyAgreement.getInstance("XDH");
            ka.init(jpriv);
            ka.doPhase(jpub, true);
            xok &= Arrays.equals(ka.generateSecret(), Crypto.x25519(sk, pk));

            byte[] k = Crypto.random(32), n = Crypto.random(12), ad = new byte[r.nextInt(40)], m = new byte[r.nextInt(3000)];
            r.nextBytes(ad);
            r.nextBytes(m);
            Cipher c = Cipher.getInstance("ChaCha20-Poly1305");
            c.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(k, "ChaCha20"), new IvParameterSpec(n));
            c.updateAAD(ad);
            cok &= Arrays.equals(c.doFinal(m), Crypto.seal(k, n, ad, m));
        }
        check(xok, "x25519 == JDK XDH (200 random)");
        check(cok, "chacha20-poly1305 == JDK (200 random)");
        frameCipherTests();
        System.out.println(fails == 0 ? "ALL OK" : fails + " FAILURES");
        System.exit(fails == 0 ? 0 : 1);
    }

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

        System.out.println("Log line");
        check(Node.transferLine("sent", "a.jpg", 35_000_000L, 2_100_000_000L, null)
                .equals("sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s)"), "transfer line");
        check(Node.transferLine("sent", "a.jpg", 35_000_000L, 2_100_000_000L, "read 0.2 s")
                .equals("sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s)"), "transfer line with details");
        check(Node.transferLine("received", "empty", 0, 0, null)
                .equals("received empty: 0 B in 0.0 s (instant)"), "transfer line for an empty file in no time");
    }

    // ------------------------------------------------------------ interop "phone"

    static class MemHost implements Node.Host {
        final byte[] id = Crypto.random(16);
        final List<Peer> peers = new CopyOnWriteArrayList<>();
        final Path dir;
        final int port;
        final BlockingQueue<String> events = new LinkedBlockingQueue<>();

        MemHost(Path dir, int port) {
            this.dir = dir;
            this.port = port;
        }

        public byte[] deviceId() { return id; }
        public String deviceName() { return "JvmPhone"; }
        public int listenPort() { return port; }
        public List<Peer> peers() { return peers; }
        public void savePeer(Peer p) { peers.removeIf(x -> Arrays.equals(x.id, p.id)); peers.add(0, p); }
        public void touchPeer(byte[] i, String n, String addr, int kind) {
            for (Peer p : peers) if (Arrays.equals(p.id, i)) { p.name = n; if (addr != null) p.addr = addr; if (kind != 0) p.kind = kind; }
        }
        public void onPaired(Peer p) { events.add("paired " + p.name); }
        public void onPairingClosed(String r) { events.add("pairclosed " + r); }
        public void onClipboard(Peer f, String t) { events.add("clip " + t); }
        public Node.Incoming beginFile(Peer f, String name, long size) throws IOException {
            Path tmp = dir.resolve(name + ".part");
            OutputStream os = Files.newOutputStream(tmp);
            return new Node.Incoming() {
                public OutputStream stream() { return os; }
                public void commit() throws IOException { Files.move(tmp, dir.resolve(name), StandardCopyOption.REPLACE_EXISTING); }
                public void abort() { try { Files.deleteIfExists(tmp); } catch (IOException ignored) {} }
                public String displayName() { return name; }
            };
        }
        public void onFilesReceived(Peer f, List<Node.Incoming> files) {
            for (Node.Incoming i : files) events.add("file " + i.displayName());
        }
        public void log(String m) { System.out.println("  [phone] " + m); }

        // v0.2
        volatile boolean visible = true;
        final List<String> incoming = new CopyOnWriteArrayList<>();
        public boolean visible() { return visible; }
        public boolean isBlocked(byte[] id, String ip) { return false; }
        public boolean hasIncomingRoom(byte[] id, String ip) { return incoming.size() < 10; }
        public long incomingFreeBytes() { return 50_000_000L; }
        public void onQueuedResult(String title, String msg) { events.add("queued-result " + title + ": " + msg); }
        public void onChanged() {}
        public Node.GuestTransfer beginGuest(String fromName, byte[] fromId, String ip) throws IOException {
            Path gdir = Files.createTempDirectory(dir, "incoming-");
            List<String> names = new ArrayList<>();
            String[] text = {null};
            return new Node.GuestTransfer() {
                public OutputStream file(String name, long size) throws IOException { names.add(name); return Files.newOutputStream(gdir.resolve(name)); }
                public void fileDone() {}
                public void text(String t) { text[0] = t; }
                public boolean isEmpty() { return names.isEmpty() && text[0] == null; }
                public void commit() {
                    String what = names.isEmpty() ? "text " + text[0] : String.join(",", names);
                    incoming.add(gdir.toString());
                    events.add("incoming " + fromName + " " + what);
                }
                public void discard() {}
            };
        }
    }

    /**
     * Driven by interop.sh through stdin commands:
     *   listen-pair            -> prints CODE <code>, waits for desktop to pair
     *   pair <addr> <code>     -> phone pairs with desktop showing code
     *   sendfile <path>        -> send to desktop
     *   sendclip <text>
     *   expect <prefix>        -> wait for an event
     */
    static Node.Outgoing fileOut(File file) {
        return new Node.Outgoing(file.getName(), file.length(), () -> new FileInputStream(file), null);
    }

    static void interop(String[] a) throws Exception {
        if ("intree".equals(System.getenv("FERRY_AEAD"))) Aead.forceInTree = true;
        Path dir = Paths.get(a[1]);
        int port = Integer.parseInt(a[2]);
        MemHost host = new MemHost(dir, port);
        Node node = new Node(host);
        ServerSocket ss = new ServerSocket(port);
        Thread t = new Thread(() -> {
            while (true) {
                try {
                    Socket s = ss.accept();
                    new Thread(() -> node.serve(s)).start();
                } catch (IOException e) {
                    return;
                }
            }
        });
        t.setDaemon(true);
        t.start();
        DatagramSocket udp = new DatagramSocket(port);
        udp.setBroadcast(true);
        node.setUdpSocket(udp);
        String extra = System.getenv("FERRY_DISCOVERY_PORTS");
        if (extra != null) for (String x : extra.split(",")) Node.EXTRA_DISCOVERY_PORTS.add(Integer.parseInt(x.trim()));
        Thread u = new Thread(() -> {
            byte[] buf = new byte[64];
            while (true) {
                try {
                    DatagramPacket p = new DatagramPacket(buf, buf.length);
                    udp.receive(p);
                    node.handleUdp(udp, p);
                } catch (IOException e) {
                    return;
                }
            }
        });
        u.setDaemon(true);
        u.start();
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in));
        String line;
        System.out.println("READY");
        while ((line = in.readLine()) != null) {
            String[] f = line.split(" ", 2);
            try {
                switch (f[0]) {
                    case "listen-pair":
                        System.out.println("CODE " + node.startPairing());
                        break;
                    case "pair": {
                        String[] g = f[1].split(" ");
                        Peer p = node.pair(g[0], g[1]);
                        System.out.println("OK paired " + p.name);
                        break;
                    }
                    case "sendfile": { // to my main device
                        File file = new File(f[1]);
                        Peer p = host.peers.get(0);
                        Node.Result r = node.send(p.idHex(), p.name, List.of(fileOut(file)), null, null);
                        System.out.println(r.status == Node.Result.SENT ? "OK sent" : r.status == Node.Result.QUEUED ? "OK queued" : "FAIL " + r.message);
                        break;
                    }
                    case "sendto": { // sendto <device name> <file>  (paired or nearby)
                        String[] g = f[1].split(" ", 2);
                        Node.Device d = null;
                        for (Node.Device x : node.devices(true)) if (x.name.equals(g[0])) d = x;
                        if (d == null) { System.out.println("FAIL no device " + g[0]); break; }
                        Node.Result r = node.send(d.idHex(), d.name, List.of(fileOut(new File(g[1]))), null, null);
                        System.out.println((r.status == Node.Result.REFUSED ? "FAIL " : "OK ") + r.message);
                        break;
                    }
                    case "devices": {
                        StringBuilder b = new StringBuilder("OK");
                        for (Node.Device d : node.devices(true)) b.append(' ').append(d.name).append(d.paired ? "(mine)" : "(nearby)");
                        System.out.println(b);
                        break;
                    }
                    case "sendclip":
                        node.sendClipAll(f[1].replace("\\n", "\n"));
                        System.out.println("OK clip");
                        break;
                    case "unpairall":
                        host.peers.clear();
                        System.out.println("OK");
                        break;
                    case "breakaddr":
                        host.peers.get(0).addr = "10.255.255.1:" + f[1];
                        System.out.println("OK broke");
                        break;
                    case "expect": {
                        String ev = host.events.poll(10, TimeUnit.SECONDS);
                        System.out.println(ev != null && ev.startsWith(f[1]) ? "OK " + ev : "FAIL got " + ev);
                        break;
                    }
                    case "cipher":
                        System.out.println(Aead.usingPlatform() ? "OK platform" : "OK built-in");
                        break;
                    default:
                        System.out.println("FAIL unknown " + f[0]);
                }
            } catch (Exception e) {
                System.out.println("FAIL " + e);
            }
            System.out.flush();
        }
    }
}
