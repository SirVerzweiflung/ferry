package dev.ferry.core;

import java.io.EOFException;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Platform independent Ferry engine (no Android APIs, so it can be tested on a JVM).
 *
 * <ul>
 *   <li>My devices (paired): files saved directly, clipboard relayed between all of them.</li>
 *   <li>Nearby (unpaired): can be sent to; what they send goes to Incoming via the Host.</li>
 *   <li>Sends to unreachable devices are queued and retried when the device shows up.</li>
 * </ul>
 */
public final class Node implements Proto.ServerCtx {

    public interface Host {
        byte[] deviceId();

        String deviceName();

        int listenPort();

        List<Peer> peers();

        /** Insert or replace (by id). */
        void savePeer(Peer p);

        /** Update name / address / kind of a known peer after a successful contact. */
        void touchPeer(byte[] id, String name, String addr, int kind);

        void onPaired(Peer p);

        void onPairingClosed(String reason);

        /** Text from a paired device: put it on the clipboard. */
        void onClipboard(Peer from, String text);

        Incoming beginFile(Peer from, String name, long size) throws IOException;

        void onFilesReceived(Peer from, List<Incoming> files);

        // ---- v0.2: unpaired devices

        /** Visible to unpaired devices (they can send to Incoming)? */
        boolean visible();

        boolean isBlocked(byte[] id, String ip);

        /** May another transfer from this sender wait in Incoming (count limits)? */
        boolean hasIncomingRoom(byte[] id, String ip);

        /** Bytes still allowed in Incoming. */
        long incomingFreeBytes();

        GuestTransfer beginGuest(String fromName, byte[] fromId, String ip) throws IOException;

        /** Result of a send that was queued earlier (for a notification). */
        void onQueuedResult(String title, String message);

        /** The device list or the queue changed (refresh UI). */
        void onChanged();

        void log(String msg);
    }

    public interface Incoming {
        OutputStream stream() throws IOException;

        void commit() throws IOException;

        void abort();

        String displayName();
    }

    /** A transfer from an unpaired device being stored in Incoming. */
    public interface GuestTransfer {
        OutputStream file(String name, long size) throws IOException;

        void fileDone() throws IOException;

        void text(String text);

        boolean isEmpty();

        void commit() throws IOException;

        void discard();
    }

    /** Re-openable content (queued sends may need several attempts). */
    public interface Source {
        InputStream open() throws IOException;
    }

    public static final class Outgoing {
        public final String name;
        public final long size;
        public final Source source;
        private final Runnable onDone;

        public Outgoing(String name, long size, Source source, Runnable onDone) {
            this.name = name;
            this.size = size;
            this.source = source;
            this.onDone = onDone;
        }

        /** Single-use stream (cannot be retried after a failure). */
        public Outgoing(String name, long size, InputStream in) {
            this(name, size, new Source() {
                boolean used;

                @Override
                public InputStream open() throws IOException {
                    if (used) throw new FileNotFoundException(name + " can no longer be read");
                    used = true;
                    return in;
                }
            }, () -> {
                try {
                    in.close();
                } catch (IOException ignored) {
                }
            });
        }

        void done() {
            if (onDone != null) onDone.run();
        }
    }

    public static final class Device {
        public final byte[] id;
        public final String name, addr;
        public final int kind;
        public final boolean paired, online;

        Device(byte[] id, String name, int kind, boolean paired, boolean online, String addr) {
            this.id = id;
            this.name = name;
            this.kind = kind;
            this.paired = paired;
            this.online = online;
            this.addr = addr;
        }

        public String idHex() {
            return Crypto.hex(id);
        }
    }

    public static final class Result {
        public static final int SENT = 0, QUEUED = 1, REFUSED = 2;
        public final int status;
        public final String message;

        Result(int status, String message) {
            this.status = status;
            this.message = message;
        }
    }

    public interface Progress {
        void update(long done, long total);
    }

    /** Thrown when a device cannot be reached: queue and retry. */
    static final class Unreachable extends IOException {
        Unreachable(String m) {
            super(m);
        }
    }

    private static final class Nearby {
        byte[] id;
        String name, addr;
        int kind;
        boolean accepts;
        long seen;
    }

    private static final class Job {
        long id;
        byte[] target;
        String targetName;
        List<Outgoing> files; // or null
        String text;          // or null
        int done;
        long created, next;
        int attempts;
        Progress progress;

        String describe() {
            if (files == null) return "text";
            return files.size() == 1 ? files.get(0).name : files.size() + " files";
        }

        void release() {
            if (files != null) for (Outgoing o : files) o.done();
        }
    }

    private static final int CONNECT_TIMEOUT_MS = 4000;
    private static final int IO_TIMEOUT_MS = 60000;
    private static final long PAIR_WINDOW_MS = 5 * 60 * 1000;
    private static final int PAIR_ATTEMPTS = 3;
    private static final long NEARBY_FRESH_MS = 180_000;
    private static final long SCAN_WAIT_MS = 700;
    private static final long QUEUE_MAX_AGE_MS = 24L * 3600 * 1000;
    private static final long[] RETRY_STEPS_MS = {30_000, 120_000, 600_000, 1_800_000, 3_600_000};
    private static final long GUEST_RATE_WINDOW_MS = 600_000;
    private static final int GUEST_RATE_MAX = 20;

    /** Extra UDP ports for discovery broadcasts (tests run several devices on one machine). */
    public static final List<Integer> EXTRA_DISCOVERY_PORTS = Collections.synchronizedList(new ArrayList<>());

    private final Host host;
    private String pairCode;
    private long pairExpires;
    private int pairAttempts;
    private volatile String lastClip = "";
    private volatile DatagramSocket udp;
    private final Map<String, Nearby> nearby = new HashMap<>();
    private final Map<String, List<Long>> guestRate = new HashMap<>();
    private final List<Job> outbox = new ArrayList<>();
    private long nextJob;
    private Thread outboxThread;

    public Node(Host host) {
        this.host = host;
        Aead.log = host::log;
    }

    // ------------------------------------------------------------ pairing window

    public synchronized String startPairing() {
        pairCode = Proto.newPairCode();
        pairExpires = System.currentTimeMillis() + PAIR_WINDOW_MS;
        pairAttempts = 0;
        return Proto.formatCode(pairCode);
    }

    public synchronized void stopPairing() {
        pairCode = null;
    }

    @Override
    public synchronized String pairCode() {
        if (pairCode != null && System.currentTimeMillis() > pairExpires) pairCode = null;
        return pairCode;
    }

    @Override
    public void pairFailed() {
        boolean closed = false;
        synchronized (this) {
            if (pairCode != null && ++pairAttempts >= PAIR_ATTEMPTS) {
                pairCode = null;
                closed = true;
            }
        }
        if (closed) host.onPairingClosed("Too many wrong codes - start pairing again.");
    }

    @Override
    public byte[] peerKey(byte[] id) {
        Peer p = findPeer(id);
        return p == null ? null : p.key;
    }

    @Override
    public int guestStatus(byte[] id, InetAddress ip) {
        String ips = ip == null ? "" : ip.getHostAddress();
        if (!host.visible() || host.isBlocked(id, ips)) return Proto.ST_NO_GUESTS;
        synchronized (guestRate) {
            List<Long> v = guestRate.get(ips);
            if (v == null) guestRate.put(ips, v = new ArrayList<>());
            long now = System.currentTimeMillis();
            v.removeIf(t -> now - t > GUEST_RATE_WINDOW_MS);
            if (v.size() >= GUEST_RATE_MAX) return Proto.ST_BUSY;
            v.add(now);
        }
        return host.hasIncomingRoom(id, ips) ? Proto.ST_OK : Proto.ST_BUSY;
    }

    public Peer findPeer(byte[] id) {
        for (Peer p : host.peers()) if (Arrays.equals(p.id, id)) return p;
        return null;
    }

    // ------------------------------------------------------------ incoming TCP

    /** Handles one incoming TCP connection to completion. Call on a worker thread. */
    public void serve(Socket s) {
        String remote = s.getInetAddress() == null ? "?" : s.getInetAddress().getHostAddress();
        List<Incoming> received = new ArrayList<>();
        Peer peer = null;
        try {
            s.setSoTimeout(IO_TIMEOUT_MS);
            s.setTcpNoDelay(true);
            Proto.Established e = Proto.serverHandshake(s, host.deviceId(), this);
            Proto.Channel ch = e.ch;
            Proto.Hello h = Proto.parseHello(ch.expect(Proto.T_HELLO));
            ch.send(Proto.helloMsg(host.deviceName(), host.listenPort()));
            if (e.guest) {
                serveGuest(ch, e.peerId, h.name.trim().isEmpty() ? "Unknown device" : h.name, remote);
                return;
            }
            String addr = hostPort(remote, h.port);
            if (e.newKey != null) {
                peer = new Peer(e.peerId, h.name, e.newKey, addr, h.kind);
                host.savePeer(peer);
                stopPairing();
                host.onPaired(peer);
            } else {
                host.touchPeer(e.peerId, h.name, addr, h.kind);
                peer = findPeer(e.peerId);
                kick(e.peerId);
            }
            while (true) {
                Proto.Msg m;
                try {
                    m = ch.recv();
                } catch (EOFException eof) {
                    break;
                }
                if (m.type == Proto.T_CLIP) {
                    String text = m.reader().str();
                    if (!text.equals(lastClip)) {
                        lastClip = text;
                        host.onClipboard(peer, text);
                        relayClip(text, e.peerId); // pass it on to my other devices
                    }
                    ch.ack(true, "");
                } else if (m.type == Proto.T_FILE) {
                    Proto.Reader r = m.reader();
                    String name = r.str();
                    long size = r.u64();
                    String clean = sanitize(name);
                    long t0 = System.nanoTime();
                    Incoming inc = host.beginFile(peer, clean, size);
                    try {
                        receiveInto(ch, inc.stream(), size);
                        inc.commit();
                        debug(transferLine("received", clean, size, System.nanoTime() - t0, null));
                    } catch (IOException ex) {
                        inc.abort();
                        try {
                            ch.ack(false, String.valueOf(ex.getMessage()));
                        } catch (IOException ignored) {
                        }
                        throw ex;
                    }
                    received.add(inc);
                    ch.ack(true, "");
                } else if (m.type == Proto.T_BYE) {
                    break;
                } else if (m.type != Proto.T_HELLO) {
                    throw new Proto.ProtoException("unknown message type " + m.type);
                }
            }
        } catch (IOException ex) {
            host.log("connection from " + remote + ": " + ex.getMessage());
        } finally {
            try {
                s.close();
            } catch (IOException ignored) {
            }
            if (!received.isEmpty() && peer != null) host.onFilesReceived(peer, received);
        }
    }

    private void serveGuest(Proto.Channel ch, byte[] from, String name, String ip) throws IOException {
        Proto.Reader r = ch.expect(Proto.T_OFFER).reader();
        long count = r.u32();
        long total = r.u64();
        long free = host.incomingFreeBytes();
        if (total > free) {
            ch.ack(false, "too large - " + host.deviceName() + " accepts up to " + human(free)
                    + " from unpaired devices right now");
            return;
        }
        ch.ack(true, "");
        GuestTransfer t = host.beginGuest(name, from, ip);
        boolean ok = false;
        try {
            long got = 0;
            int files = 0;
            while (true) {
                Proto.Msg m;
                try {
                    m = ch.recv();
                } catch (EOFException eof) {
                    break;
                }
                if (m.type == Proto.T_FILE) {
                    Proto.Reader fr = m.reader();
                    String fname = sanitize(fr.str());
                    long size = fr.u64();
                    got += size;
                    if (got > total || files >= count) {
                        ch.ack(false, "more than offered");
                        throw new Proto.ProtoException("guest sent more than offered");
                    }
                    long t0 = System.nanoTime();
                    receiveInto(ch, t.file(fname, size), size);
                    t.fileDone();
                    debug(transferLine("received", fname, size, System.nanoTime() - t0, null));
                    files++;
                    ch.ack(true, "");
                } else if (m.type == Proto.T_CLIP) {
                    t.text(m.reader().str());
                    ch.ack(true, "");
                } else if (m.type == Proto.T_BYE) {
                    break;
                } else if (m.type != Proto.T_HELLO) {
                    throw new Proto.ProtoException("unknown message type " + m.type);
                }
            }
            if (!t.isEmpty()) {
                t.commit();
                ok = true;
            }
        } finally {
            if (!ok) t.discard();
        }
    }

    private static void receiveInto(Proto.Channel ch, OutputStream out, long size) throws IOException {
        long got = 0;
        try {
            while (got < size) {
                Proto.Msg m = ch.recv();
                if (m.type != Proto.T_DATA) throw new Proto.ProtoException("unexpected message during transfer");
                int n = m.bodyLen();
                got += n;
                if (got > size) throw new Proto.ProtoException("peer sent more data than announced");
                out.write(m.data, 1, n);
            }
        } finally {
            out.close();
        }
    }

    public static String sanitize(String n) {
        int i = Math.max(n.lastIndexOf('/'), n.lastIndexOf('\\'));
        String s = n.substring(i + 1);
        StringBuilder b = new StringBuilder();
        for (char c : s.toCharArray()) if (!Character.isISOControl(c)) b.append(c);
        s = b.toString().trim();
        if (s.isEmpty() || s.equals(".") || s.equals("..") || s.equals("meta")) s = "received-file";
        if (s.length() > 200) s = s.substring(0, 200);
        return s;
    }

    public static String human(long b) {
        if (b >= 1_000_000_000L) return String.format(java.util.Locale.ROOT, "%.1f GB", b / 1e9);
        if (b >= 1_000_000L) return String.format(java.util.Locale.ROOT, "%.1f MB", b / 1e6);
        if (b >= 1_000L) return String.format(java.util.Locale.ROOT, "%.0f KB", b / 1e3);
        return b + " B";
    }

    /** Debug logging must never disturb a transfer. */
    private void debug(String line) {
        try {
            host.log(line);
        } catch (RuntimeException ignored) {
        }
    }

    /** Debug line for the log, e.g. "sent a.jpg: 35.0 MB in 2.1 s (16.7 MB/s; read 0.2 s)". */
    public static String transferLine(String verb, String name, long bytes, long nanos, String extra) {
        double s = nanos / 1e9;
        String speed = s > 0 ? String.format(java.util.Locale.ROOT, "%.1f MB/s", bytes / 1e6 / s) : "instant";
        return String.format(java.util.Locale.ROOT, "%s %s: %s in %.1f s (%s%s)", verb, name, human(bytes), s,
                speed, extra == null ? "" : "; " + extra);
    }

    // ------------------------------------------------------------ UDP: discovery + presence

    /** The service's bound UDP socket, used to send presence broadcasts. */
    public void setUdpSocket(DatagramSocket s) {
        udp = s;
    }

    private List<Integer> broadcastPorts(int own) {
        List<Integer> v = new ArrayList<>();
        v.add(Proto.DEFAULT_PORT);
        if (own != Proto.DEFAULT_PORT) v.add(own);
        synchronized (EXTRA_DISCOVERY_PORTS) {
            for (int p : EXTRA_DISCOVERY_PORTS) if (!v.contains(p)) v.add(p);
        }
        return v;
    }

    private byte[] presence(byte[] magic) {
        return Proto.presencePacket(magic, host.deviceId(), host.listenPort(), Proto.KIND_PHONE,
                host.visible() ? Proto.PRES_GUESTS : 0, host.deviceName());
    }

    /** Broadcasts our presence ("here I am" or "who is there?"). */
    public void sendPresence(boolean query) {
        DatagramSocket s = udp;
        if (s == null) return;
        byte[] p = presence(query ? Proto.PRES_QUERY : Proto.PRES_HERE);
        try {
            InetAddress bc = InetAddress.getByName("255.255.255.255");
            for (int port : broadcastPorts(host.listenPort())) s.send(new DatagramPacket(p, p.length, bc, port));
        } catch (IOException e) {
            host.log("presence: " + e.getMessage());
        }
    }

    /** Asks the network who is there and waits briefly for answers. */
    public void scan() {
        sendPresence(true);
        try {
            Thread.sleep(SCAN_WAIT_MS);
        } catch (InterruptedException ignored) {
        }
    }

    /** Handle one UDP packet received on the service's socket. */
    public void handleUdp(DatagramSocket sock, DatagramPacket p) {
        byte[] d = Arrays.copyOfRange(p.getData(), p.getOffset(), p.getOffset() + p.getLength());
        // v0.1 lookup by a paired device
        if (d.length >= 22 && Arrays.equals(Arrays.copyOf(d, 4), Proto.DISC_QUERY)) {
            byte[] id = Arrays.copyOfRange(d, 4, 20);
            if (Arrays.equals(id, host.deviceId()) || findPeer(id) == null) return;
            byte[] ans = Proto.discPacket(Proto.DISC_ANSWER, host.deviceId(), host.listenPort());
            try {
                sock.send(new DatagramPacket(ans, ans.length, p.getSocketAddress()));
            } catch (IOException ignored) {
            }
            return;
        }
        Proto.Presence pr = Proto.parsePresence(d, d.length);
        if (pr == null || Arrays.equals(pr.id, host.deviceId())) return;
        String ip = p.getAddress().getHostAddress();
        if (!host.isBlocked(pr.id, "")) {
            Nearby n = new Nearby();
            n.id = pr.id;
            n.name = pr.name;
            n.kind = pr.kind;
            n.addr = hostPort(ip, pr.port);
            n.accepts = (pr.flags & Proto.PRES_GUESTS) != 0;
            n.seen = System.currentTimeMillis();
            synchronized (nearby) {
                nearby.put(Crypto.hex(pr.id), n);
            }
        }
        if (Arrays.equals(pr.magic, Proto.PRES_QUERY) && (host.visible() || findPeer(pr.id) != null)) {
            byte[] ans = presence(Proto.PRES_HERE);
            try {
                sock.send(new DatagramPacket(ans, ans.length, p.getSocketAddress()));
            } catch (IOException ignored) {
            }
        }
        kick(pr.id);
    }

    private Nearby freshNearby(byte[] id) {
        synchronized (nearby) {
            Nearby n = nearby.get(Crypto.hex(id));
            return n != null && System.currentTimeMillis() - n.seen < NEARBY_FRESH_MS ? n : null;
        }
    }

    /** My devices (main device first) followed by nearby unpaired devices. */
    public List<Device> devices(boolean doScan) {
        if (doScan) scan();
        List<Device> out = new ArrayList<>();
        List<Peer> peers = new ArrayList<>(host.peers());
        Peer main = mainPeer();
        if (main != null) {
            peers.remove(main);
            peers.add(0, main);
        }
        for (Peer p : peers) {
            out.add(new Device(p.id, p.name, p.kind, true, freshNearby(p.id) != null, p.addr == null ? "" : p.addr));
        }
        List<Nearby> others = new ArrayList<>();
        synchronized (nearby) {
            long now = System.currentTimeMillis();
            for (Nearby n : nearby.values()) {
                if (n.accepts && now - n.seen < NEARBY_FRESH_MS && findPeer(n.id) == null) others.add(n);
            }
        }
        others.sort((a, b) -> a.name.compareToIgnoreCase(b.name));
        for (Nearby n : others) out.add(new Device(n.id, n.name, n.kind, false, true, n.addr));
        return out;
    }

    /** The main device for quick actions: the most recently paired one. */
    public Peer mainPeer() {
        List<Peer> p = host.peers();
        return p.isEmpty() ? null : p.get(0);
    }

    public void forgetNearby(byte[] id) {
        synchronized (nearby) {
            nearby.remove(Crypto.hex(id));
        }
    }

    // ------------------------------------------------------------ outgoing

    static String hostPort(String host, int port) {
        return host.contains(":") ? "[" + host + "]:" + port : host + ":" + port;
    }

    /** Parses "host", "host:port", "[v6]:port" or "v6". */
    static InetSocketAddress parseAddr(String a, int defPort) {
        a = a.trim();
        if (a.startsWith("[")) {
            int e = a.indexOf(']');
            String h = a.substring(1, e);
            int p = a.length() > e + 2 && a.charAt(e + 1) == ':' ? Integer.parseInt(a.substring(e + 2)) : defPort;
            return new InetSocketAddress(h, p);
        }
        int c = a.indexOf(':');
        if (c >= 0 && c == a.lastIndexOf(':')) {
            return new InetSocketAddress(a.substring(0, c), Integer.parseInt(a.substring(c + 1)));
        }
        return new InetSocketAddress(a, defPort);
    }

    private static Socket connect(String addr) throws IOException {
        InetSocketAddress sa;
        try {
            sa = parseAddr(addr, Proto.DEFAULT_PORT);
        } catch (RuntimeException e) {
            throw new Unreachable("bad address " + addr);
        }
        if (sa.isUnresolved()) throw new Unreachable("cannot resolve " + sa.getHostString());
        Socket s = new Socket();
        try {
            s.connect(sa, CONNECT_TIMEOUT_MS);
            s.setSoTimeout(IO_TIMEOUT_MS);
            s.setTcpNoDelay(true);
            return s;
        } catch (IOException e) {
            s.close();
            throw new Unreachable(addr + ": " + e.getMessage());
        }
    }

    /** v0.1-compatible UDP lookup for a paired device whose address changed. */
    public String discover(Peer p) {
        int port = Proto.DEFAULT_PORT;
        if (p.addr != null) {
            try {
                port = parseAddr(p.addr, Proto.DEFAULT_PORT).getPort();
            } catch (RuntimeException ignored) {
            }
        }
        byte[] q = Proto.discPacket(Proto.DISC_QUERY, host.deviceId(), host.listenPort());
        try (DatagramSocket s = new DatagramSocket()) {
            s.setBroadcast(true);
            s.setSoTimeout(400);
            long deadline = System.currentTimeMillis() + 1600;
            byte[] buf = new byte[128];
            InetAddress bc = InetAddress.getByName("255.255.255.255");
            while (System.currentTimeMillis() < deadline) {
                for (int bp : broadcastPorts(port)) s.send(new DatagramPacket(q, q.length, bc, bp));
                while (true) {
                    DatagramPacket r = new DatagramPacket(buf, buf.length);
                    try {
                        s.receive(r);
                    } catch (SocketTimeoutException t) {
                        break;
                    }
                    byte[] d = Arrays.copyOf(r.getData(), r.getLength());
                    if (d.length >= 22 && Arrays.equals(Arrays.copyOf(d, 4), Proto.DISC_ANSWER)
                            && Arrays.equals(Arrays.copyOfRange(d, 4, 20), p.id)) {
                        int pp = ((d[20] & 0xff) << 8) | (d[21] & 0xff);
                        return hostPort(r.getAddress().getHostAddress(), pp);
                    }
                }
            }
        } catch (IOException e) {
            host.log("discovery failed: " + e.getMessage());
        }
        return null;
    }

    /** Opens an authenticated session with HELLO exchanged. */
    public Proto.Channel openSession(Peer p) throws IOException {
        Socket s = null;
        String used = null;
        if (p.addr != null) {
            try {
                s = connect(p.addr);
                used = p.addr;
            } catch (IOException ignored) {
            }
        }
        if (s == null) {
            Nearby n = freshNearby(p.id);
            String a = n != null ? n.addr : discover(p);
            if (a != null) {
                try {
                    s = connect(a);
                    used = a;
                } catch (IOException ignored) {
                }
            }
        }
        if (s == null) throw new Unreachable(p.name + " is not reachable. Is Ferry running there, on the same network?");
        try {
            Proto.Established e = Proto.clientHandshake(s, host.deviceId(), p.key, null);
            if (!Arrays.equals(e.peerId, p.id)) throw new Proto.ProtoException("connected to the wrong device");
            e.ch.send(Proto.helloMsg(host.deviceName(), host.listenPort()));
            Proto.Hello h = Proto.parseHello(e.ch.expect(Proto.T_HELLO));
            host.touchPeer(p.id, h.name, used, h.kind);
            return e.ch;
        } catch (IOException ex) {
            s.close();
            throw ex;
        }
    }

    /** Unauthenticated session to a nearby (unpaired) device. */
    private Proto.Channel openGuest(byte[] id, String name) throws IOException {
        Nearby n = freshNearby(id);
        if (n == null) {
            scan();
            n = freshNearby(id);
        }
        if (n == null) throw new Unreachable(name + " is not on the network right now");
        Socket s = connect(n.addr);
        try {
            Proto.Established e = Proto.clientHandshake(s, host.deviceId(), null, null);
            if (!Arrays.equals(e.peerId, id)) {
                forgetNearby(id);
                throw new Unreachable(name + " moved to a different address");
            }
            e.ch.send(Proto.helloMsg(host.deviceName(), host.listenPort()));
            Proto.parseHello(e.ch.expect(Proto.T_HELLO));
            return e.ch;
        } catch (IOException ex) {
            s.close();
            throw ex;
        }
    }

    public Peer pair(String addr, String code) throws IOException {
        Socket s = connect(addr);
        try {
            Proto.Established e = Proto.clientHandshake(s, host.deviceId(), null, Proto.normalizeCode(code));
            e.ch.send(Proto.helloMsg(host.deviceName(), host.listenPort()));
            Proto.Hello h = Proto.parseHello(e.ch.expect(Proto.T_HELLO));
            try {
                e.ch.send(new Proto.Writer(Proto.T_BYE));
            } catch (IOException ignored) {
            }
            InetSocketAddress sa = parseAddr(addr, Proto.DEFAULT_PORT);
            String a = hostPort(s.getInetAddress().getHostAddress(), sa.getPort());
            Peer p = new Peer(e.peerId, h.name, e.newKey, a, h.kind);
            host.savePeer(p);
            return p;
        } finally {
            s.close();
        }
    }

    private void sendClipTo(Peer p, String text) throws IOException {
        if (text.getBytes(StandardCharsets.UTF_8).length > Proto.MAX_CLIP) {
            throw new Proto.ProtoException("text is too long for the clipboard (max 512 KB)");
        }
        Proto.Channel ch = openSession(p);
        try {
            ch.send(new Proto.Writer(Proto.T_CLIP).str(text));
            ch.waitAck();
            ch.send(new Proto.Writer(Proto.T_BYE));
        } finally {
            ch.close();
        }
    }

    /** Sends text to the clipboards of all my devices. Returns the names that got it. */
    public List<String> sendClipAll(String text) throws IOException {
        lastClip = text;
        List<String> ok = new ArrayList<>();
        IOException last = null;
        for (Peer p : host.peers()) {
            try {
                sendClipTo(p, text);
                ok.add(p.name);
            } catch (IOException e) {
                last = e;
            }
        }
        if (ok.isEmpty()) {
            if (last != null) throw last;
            throw new Proto.ProtoException("no paired devices yet");
        }
        return ok;
    }

    private void relayClip(String text, byte[] from) {
        List<Peer> others = new ArrayList<>();
        for (Peer p : host.peers()) if (!Arrays.equals(p.id, from)) others.add(p);
        if (others.isEmpty()) return;
        Thread t = new Thread(() -> {
            for (Peer p : others) {
                try {
                    sendClipTo(p, text);
                } catch (IOException e) {
                    host.log("clipboard relay to " + p.name + ": " + e.getMessage());
                }
            }
        }, "ferry-clip-relay");
        t.setDaemon(true);
        t.start();
    }

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
        debug(transferLine("sent", o.name, o.size, System.nanoTime() - t0, String.format(java.util.Locale.ROOT,
                "read %.1f s, encrypt %.1f s, network %.1f s%s", readNanos / 1e9, (ch.sealNanos - seal0) / 1e9,
                (ch.writeNanos - write0) / 1e9, Aead.usingPlatform() ? "" : ", built-in cipher")));
    }

    /** One delivery attempt. Throws Unreachable (retry) or another IOException (refused). */
    private String attempt(Job j) throws IOException {
        Peer peer = findPeer(j.target);
        long total = 0;
        if (j.files != null) for (Outgoing o : j.files) total += o.size;
        long[] done = {0};
        if (peer != null) {
            Proto.Channel ch = openSession(peer);
            try {
                if (j.text != null) {
                    ch.send(new Proto.Writer(Proto.T_CLIP).str(j.text));
                    ch.waitAck();
                } else {
                    for (int i = 0; i < j.done; i++) done[0] += j.files.get(i).size;
                    while (j.done < j.files.size()) {
                        sendFile(ch, j.files.get(j.done), done, total, j.progress);
                        j.done++;
                    }
                }
                ch.send(new Proto.Writer(Proto.T_BYE));
            } finally {
                ch.close();
            }
            return "Sent " + j.describe() + " to " + peer.name;
        }
        Proto.Channel ch = openGuest(j.target, j.targetName);
        try {
            ch.send(new Proto.Writer(Proto.T_OFFER).u32(j.files == null ? 0 : j.files.size()).u64(total)
                    .u8(j.text != null ? 1 : 0));
            ch.waitAck();
            if (j.text != null) {
                ch.send(new Proto.Writer(Proto.T_CLIP).str(j.text));
                ch.waitAck();
            } else {
                for (Outgoing o : j.files) sendFile(ch, o, done, total, j.progress);
            }
            ch.send(new Proto.Writer(Proto.T_BYE));
        } finally {
            ch.close();
        }
        return "Sent " + j.describe() + " to " + j.targetName + " - it waits there until they accept it";
    }

    private static boolean isRetryable(IOException e) {
        return !(e instanceof Proto.ProtoException) && !(e instanceof FileNotFoundException);
    }

    /**
     * Sends files (or, with {@code files == null}, a text) to a device by id hex. The first
     * attempt runs on the calling thread; if the device is unreachable the send is queued.
     */
    public Result send(String targetHex, String targetName, List<Outgoing> files, String text, Progress prog) {
        Job j = new Job();
        synchronized (outbox) {
            j.id = ++nextJob;
        }
        j.target = Crypto.unhex(targetHex);
        j.targetName = targetName;
        j.files = files;
        j.text = text;
        j.created = System.currentTimeMillis();
        j.progress = prog;
        try {
            String msg = attempt(j);
            j.release();
            return new Result(Result.SENT, msg);
        } catch (IOException e) {
            if (!isRetryable(e)) {
                j.release();
                return new Result(Result.REFUSED, e.getMessage());
            }
            host.log("queued for " + targetName + ": " + e.getMessage());
            j.attempts = 1;
            j.next = System.currentTimeMillis() + RETRY_STEPS_MS[0];
            synchronized (outbox) {
                outbox.add(j);
                ensureOutboxThread();
                outbox.notifyAll();
            }
            host.onChanged();
            return new Result(Result.QUEUED, targetName + " is not reachable right now - queued. Ferry sends it as soon as "
                    + targetName + " is back (up to 24 h).");
        }
    }

    /** Retry queued sends for a device now (it just showed up). */
    public void kick(byte[] id) {
        synchronized (outbox) {
            boolean any = false;
            for (Job j : outbox) {
                if (Arrays.equals(j.target, id)) {
                    j.next = 0;
                    any = true;
                }
            }
            if (any) outbox.notifyAll();
        }
    }

    public List<String> queueDescriptions() {
        List<String> out = new ArrayList<>();
        synchronized (outbox) {
            for (Job j : outbox) out.add(j.describe() + " → " + j.targetName);
        }
        return out;
    }

    public void cancelQueue() {
        synchronized (outbox) {
            for (Job j : outbox) j.release();
            outbox.clear();
        }
        host.onChanged();
    }

    private void ensureOutboxThread() {
        if (outboxThread != null) return;
        outboxThread = new Thread(this::outboxLoop, "ferry-outbox");
        outboxThread.setDaemon(true);
        outboxThread.start();
    }

    private void outboxLoop() {
        while (true) {
            Job due = null;
            synchronized (outbox) {
                long now = System.currentTimeMillis();
                long wait = Long.MAX_VALUE;
                for (Job j : outbox) {
                    if (j.next <= now) {
                        due = j;
                        break;
                    }
                    wait = Math.min(wait, j.next - now);
                }
                if (due == null) {
                    try {
                        if (wait == Long.MAX_VALUE) outbox.wait();
                        else outbox.wait(wait);
                    } catch (InterruptedException e) {
                        return;
                    }
                    continue;
                }
                outbox.remove(due);
            }
            try {
                String msg = attempt(due);
                due.release();
                host.onQueuedResult("Sent", msg);
            } catch (IOException e) {
                if (!isRetryable(e)) {
                    due.release();
                    host.onQueuedResult("Not sent to " + due.targetName, e.getMessage());
                } else if (System.currentTimeMillis() - due.created > QUEUE_MAX_AGE_MS) {
                    due.release();
                    host.onQueuedResult("Not sent", "Gave up sending " + due.describe() + " to "
                            + due.targetName + " - it was not reachable for 24 h.");
                } else {
                    long step = RETRY_STEPS_MS[Math.min(due.attempts, RETRY_STEPS_MS.length - 1)];
                    due.attempts++;
                    due.next = System.currentTimeMillis() + step;
                    synchronized (outbox) {
                        outbox.add(due);
                    }
                    continue;
                }
            }
            host.onChanged();
        }
    }

    /** Lets my devices know our current address (after a network change). */
    public void announce(Peer p) throws IOException {
        Proto.Channel ch = openSession(p);
        try {
            ch.send(new Proto.Writer(Proto.T_BYE));
        } finally {
            ch.close();
        }
    }
}
