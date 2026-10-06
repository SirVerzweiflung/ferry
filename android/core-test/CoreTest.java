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
        System.out.println(fails == 0 ? "ALL OK" : fails + " FAILURES");
        System.exit(fails == 0 ? 0 : 1);
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
        public void touchPeer(byte[] i, String n, String addr) {
            for (Peer p : peers) if (Arrays.equals(p.id, i)) { p.name = n; if (addr != null) p.addr = addr; }
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
    }

    /**
     * Driven by interop.sh through stdin commands:
     *   listen-pair            -> prints CODE <code>, waits for desktop to pair
     *   pair <addr> <code>     -> phone pairs with desktop showing code
     *   sendfile <path>        -> send to desktop
     *   sendclip <text>
     *   expect <prefix>        -> wait for an event
     */
    static void interop(String[] a) throws Exception {
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
        Thread u = new Thread(() -> {
            byte[] buf = new byte[64];
            while (true) {
                try {
                    DatagramPacket p = new DatagramPacket(buf, buf.length);
                    udp.receive(p);
                    node.answerDiscovery(udp, p);
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
                    case "sendfile": {
                        File file = new File(f[1]);
                        node.sendFiles(host.peers.get(0), List.of(new Node.Outgoing(file.getName(), file.length(),
                                new FileInputStream(file))), null);
                        System.out.println("OK sent");
                        break;
                    }
                    case "sendclip":
                        node.sendClip(host.peers.get(0), f[1].replace("\\n", "\n"));
                        System.out.println("OK clip");
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
