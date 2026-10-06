package dev.ferry.core;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * Platform independent Ferry engine (no Android APIs, so it can be unit tested on a JVM).
 * The Android layer supplies a {@link Host} for storage, clipboard and file access.
 */
public final class Node implements Proto.ServerCtx {

    public interface Host {
        byte[] deviceId();

        String deviceName();

        int listenPort();

        List<Peer> peers();

        /** Insert or replace (by id) and make it the default target. */
        void savePeer(Peer p);

        /** Update name / address of a known peer after a successful contact. */
        void touchPeer(byte[] id, String name, String addr);

        void onPaired(Peer p);

        void onPairingClosed(String reason);

        void onClipboard(Peer from, String text);

        Incoming beginFile(Peer from, String name, long size) throws IOException;

        void onFilesReceived(Peer from, List<Incoming> files);

        void log(String msg);
    }

    public interface Incoming {
        OutputStream stream() throws IOException;

        void commit() throws IOException;

        void abort();

        String displayName();
    }

    public static final class Outgoing {
        public final String name;
        public final long size;
        public final InputStream in;

        public Outgoing(String name, long size, InputStream in) {
            this.name = name;
            this.size = size;
            this.in = in;
        }
    }

    public interface Progress {
        void update(long done, long total);
    }

    private static final int CONNECT_TIMEOUT_MS = 4000;
    private static final int IO_TIMEOUT_MS = 60000;
    private static final long PAIR_WINDOW_MS = 5 * 60 * 1000;
    private static final int PAIR_ATTEMPTS = 3;

    private final Host host;
    private String pairCode;
    private long pairExpires;
    private int pairAttempts;

    public Node(Host host) {
        this.host = host;
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

    public Peer findPeer(byte[] id) {
        for (Peer p : host.peers()) if (Arrays.equals(p.id, id)) return p;
        return null;
    }

    // ------------------------------------------------------------ incoming

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
            String addr = hostPort(remote, h.port);
            if (e.newKey != null) {
                peer = new Peer(e.peerId, h.name, e.newKey, addr);
                host.savePeer(peer);
                stopPairing();
                host.onPaired(peer);
            } else {
                host.touchPeer(e.peerId, h.name, addr);
                peer = findPeer(e.peerId);
            }
            while (true) {
                Proto.Msg m;
                try {
                    m = ch.recv();
                } catch (EOFException eof) {
                    break;
                }
                if (m.type == Proto.T_CLIP) {
                    host.onClipboard(peer, m.reader().str());
                    ch.ack(true, "");
                } else if (m.type == Proto.T_FILE) {
                    Proto.Reader r = m.reader();
                    String name = r.str();
                    long size = r.u64();
                    Incoming inc = host.beginFile(peer, sanitize(name), size);
                    try {
                        receiveInto(ch, inc.stream(), size);
                        inc.commit();
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
        if (s.isEmpty() || s.equals(".") || s.equals("..")) s = "received-file";
        if (s.length() > 200) s = s.substring(0, 200);
        return s;
    }

    /** Answers a discovery query from a paired device (call for each UDP packet). */
    public void answerDiscovery(DatagramSocket sock, DatagramPacket p) {
        byte[] d = Arrays.copyOfRange(p.getData(), p.getOffset(), p.getOffset() + p.getLength());
        if (d.length < 22 || !Arrays.equals(Arrays.copyOf(d, 4), Proto.DISC_QUERY)) return;
        byte[] id = Arrays.copyOfRange(d, 4, 20);
        if (Arrays.equals(id, host.deviceId()) || findPeer(id) == null) return;
        byte[] ans = Proto.discPacket(Proto.DISC_ANSWER, host.deviceId(), host.listenPort());
        try {
            sock.send(new DatagramPacket(ans, ans.length, p.getSocketAddress()));
        } catch (IOException ignored) {
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
        InetSocketAddress sa = parseAddr(addr, Proto.DEFAULT_PORT);
        if (sa.isUnresolved()) throw new IOException("cannot resolve " + sa.getHostString());
        Socket s = new Socket();
        try {
            s.connect(sa, CONNECT_TIMEOUT_MS);
            s.setSoTimeout(IO_TIMEOUT_MS);
            s.setTcpNoDelay(true);
            return s;
        } catch (IOException e) {
            s.close();
            throw e;
        }
    }

    /** UDP broadcast lookup for a peer whose address changed. */
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
            byte[] buf = new byte[64];
            while (System.currentTimeMillis() < deadline) {
                InetAddress bc = InetAddress.getByName("255.255.255.255");
                s.send(new DatagramPacket(q, q.length, bc, port));
                if (port != Proto.DEFAULT_PORT) s.send(new DatagramPacket(q, q.length, bc, Proto.DEFAULT_PORT));
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
            String a = discover(p);
            if (a != null) {
                try {
                    s = connect(a);
                    used = a;
                } catch (IOException ignored) {
                }
            }
        }
        if (s == null) throw new IOException(p.name + " is not reachable. Is Ferry running there, on the same network?");
        try {
            Proto.Established e = Proto.clientHandshake(s, host.deviceId(), p.key, null);
            if (!Arrays.equals(e.peerId, p.id)) throw new Proto.ProtoException("connected to the wrong device");
            e.ch.send(Proto.helloMsg(host.deviceName(), host.listenPort()));
            Proto.Hello h = Proto.parseHello(e.ch.expect(Proto.T_HELLO));
            host.touchPeer(p.id, h.name, used);
            return e.ch;
        } catch (IOException ex) {
            s.close();
            throw ex;
        }
    }

    public Peer pair(String addr, String code) throws IOException {
        Socket s = connect(addr);
        try {
            Proto.Established e = Proto.clientHandshake(s, host.deviceId(), null, code);
            e.ch.send(Proto.helloMsg(host.deviceName(), host.listenPort()));
            Proto.Hello h = Proto.parseHello(e.ch.expect(Proto.T_HELLO));
            try {
                e.ch.send(new Proto.Writer(Proto.T_BYE));
            } catch (IOException ignored) {
            }
            InetSocketAddress sa = parseAddr(addr, Proto.DEFAULT_PORT);
            String a = hostPort(s.getInetAddress().getHostAddress(), sa.getPort());
            Peer p = new Peer(e.peerId, h.name, e.newKey, a);
            host.savePeer(p);
            return p;
        } finally {
            s.close();
        }
    }

    public void sendClip(Peer p, String text) throws IOException {
        if (text.getBytes(java.nio.charset.StandardCharsets.UTF_8).length > Proto.MAX_CLIP) {
            throw new IOException("text is too long for the clipboard (max 512 KB)");
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

    public void sendFiles(Peer p, List<Outgoing> files, Progress prog) throws IOException {
        long total = 0;
        for (Outgoing o : files) total += o.size;
        long done = 0;
        Proto.Channel ch = openSession(p);
        try {
            byte[] buf = new byte[Proto.CHUNK + 1];
            for (Outgoing o : files) {
                ch.send(new Proto.Writer(Proto.T_FILE).str(o.name).u64(o.size));
                long left = o.size;
                try (InputStream in = o.in) {
                    while (left > 0) {
                        int want = (int) Math.min(left, Proto.CHUNK);
                        int off = 0;
                        while (off < want) {
                            int n = in.read(buf, 1 + off, want - off);
                            if (n < 0) throw new IOException(o.name + " is shorter than expected");
                            off += n;
                        }
                        buf[0] = Proto.T_DATA;
                        ch.sendRaw(Arrays.copyOf(buf, want + 1));
                        left -= want;
                        done += want;
                        if (prog != null) prog.update(done, total);
                    }
                }
                ch.waitAck();
            }
            ch.send(new Proto.Writer(Proto.T_BYE));
        } finally {
            ch.close();
        }
    }

    /** Lets a peer know our current address (after a network change). */
    public void announce(Peer p) throws IOException {
        Proto.Channel ch = openSession(p);
        try {
            ch.send(new Proto.Writer(Proto.T_BYE));
        } finally {
            ch.close();
        }
    }
}
