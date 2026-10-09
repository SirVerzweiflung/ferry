package dev.ferry.core;

import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.OutputStream;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** Ferry wire protocol v1. Must stay byte-compatible with desktop/src/proto.rs. */
public final class Proto {
    private Proto() {}

    public static final byte[] MAGIC = {'F', 'R', 'Y', '1'};
    public static final int DEFAULT_PORT = 47800;
    /** MODE_GUEST (v0.2): unpaired "nearby" device; the receiver keeps everything in Incoming. */
    public static final int MODE_SESSION = 1, MODE_PAIR = 2, MODE_GUEST = 3;
    public static final int ST_OK = 0, ST_UNKNOWN_PEER = 1, ST_NOT_PAIRING = 2, ST_NO_GUESTS = 3, ST_BUSY = 4;
    public static final int T_HELLO = 1, T_CLIP = 2, T_FILE = 3, T_DATA = 4, T_ACK = 5, T_BYE = 6, T_OFFER = 7;
    public static final int KIND_PC = 1, KIND_PHONE = 2;
    public static final byte[] PRES_QUERY = {'F', 'R', 'Y', 'P'};
    public static final byte[] PRES_HERE = {'F', 'R', 'Y', 'H'};
    public static final int PRES_GUESTS = 1;
    public static final int MAX_PLAINTEXT = 1 << 20;
    public static final int CHUNK = 64 * 1024;
    public static final int MAX_CLIP = 1 << 19;
    public static final String CODE_ALPHABET = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    public static final int CODE_LEN = 10;
    public static final byte[] DISC_QUERY = {'F', 'R', 'Y', 'Q'};
    public static final byte[] DISC_ANSWER = {'F', 'R', 'Y', 'A'};

    private static final byte[] INFO_SESSION = "ferry/1 session".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] INFO_PAIR = "ferry/1 pair".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] INFO_GUEST = "ferry/1 guest".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] CLIENT = "client".getBytes(StandardCharsets.US_ASCII);
    private static final byte[] SERVER = "server".getBytes(StandardCharsets.US_ASCII);

    public static class ProtoException extends IOException {
        public ProtoException(String m) {
            super(m);
        }
    }

    // ------------------------------------------------------------ pairing code

    public static String newPairCode() {
        StringBuilder s = new StringBuilder();
        while (s.length() < CODE_LEN) {
            int b = Crypto.RNG.nextInt(256);
            if (b < 248) s.append(CODE_ALPHABET.charAt(b % 31));
        }
        return s.toString();
    }

    public static String normalizeCode(String c) {
        StringBuilder s = new StringBuilder();
        for (char ch : c.toCharArray()) {
            if ((ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z')) {
                s.append(Character.toUpperCase(ch));
            }
        }
        return s.toString();
    }

    public static String formatCode(String c) {
        return c.length() == CODE_LEN ? c.substring(0, 5) + "-" + c.substring(5) : c;
    }

    // ------------------------------------------------------------ messages

    public static final class Writer {
        private final ByteArrayOutputStream b = new ByteArrayOutputStream();

        public Writer(int type) {
            b.write(type);
        }

        public Writer u8(int v) {
            b.write(v);
            return this;
        }

        public Writer u16(int v) {
            b.write(v >>> 8);
            b.write(v);
            return this;
        }

        public Writer u32(long v) {
            for (int i = 3; i >= 0; i--) b.write((int) (v >>> (8 * i)));
            return this;
        }

        public Writer u64(long v) {
            for (int i = 7; i >= 0; i--) b.write((int) (v >>> (8 * i)));
            return this;
        }

        public Writer str(String s) {
            byte[] d = s.getBytes(StandardCharsets.UTF_8);
            u32(d.length);
            b.write(d, 0, d.length);
            return this;
        }

        public byte[] bytes() {
            return b.toByteArray();
        }
    }

    public static final class Reader {
        private final byte[] b;
        private int p;

        public Reader(byte[] b, int start) {
            this.b = b;
            this.p = start;
        }

        private void need(int n) throws ProtoException {
            if (p + n > b.length) throw new ProtoException("truncated message");
        }

        public int u8() throws ProtoException {
            need(1);
            return b[p++] & 0xff;
        }

        public int u16() throws ProtoException {
            need(2);
            int v = ((b[p] & 0xff) << 8) | (b[p + 1] & 0xff);
            p += 2;
            return v;
        }

        public long u32() throws ProtoException {
            need(4);
            long v = 0;
            for (int i = 0; i < 4; i++) v = (v << 8) | (b[p++] & 0xff);
            return v;
        }

        public long u64() throws ProtoException {
            need(8);
            long v = 0;
            for (int i = 0; i < 8; i++) v = (v << 8) | (b[p++] & 0xff);
            return v;
        }

        public String str() throws ProtoException {
            long n = u32();
            if (n > b.length - p) throw new ProtoException("truncated string");
            String s = new String(b, p, (int) n, StandardCharsets.UTF_8);
            p += (int) n;
            return s;
        }
    }

    /** A received frame: msg[0] is the type, the body starts at index 1. */
    public static final class Msg {
        public final int type;
        public final byte[] data;

        Msg(byte[] d) {
            this.type = d[0] & 0xff;
            this.data = d;
        }

        public Reader reader() {
            return new Reader(data, 1);
        }

        public int bodyLen() {
            return data.length - 1;
        }
    }

    // ------------------------------------------------------------ secure channel

    public static final class Channel {
        public final Socket socket;
        private final DataInputStream in;
        private final OutputStream out;
        private final byte[] sendKey, recvKey;
        private long sendCtr, recvCtr;

        Channel(Socket s, DataInputStream in, OutputStream out, byte[] sendKey, byte[] recvKey) {
            this.socket = s;
            this.in = in;
            this.out = out;
            this.sendKey = sendKey;
            this.recvKey = recvKey;
        }

        private static byte[] nonce(long ctr) {
            byte[] n = new byte[12];
            for (int i = 0; i < 8; i++) n[4 + i] = (byte) (ctr >>> (8 * (7 - i)));
            return n;
        }

        public void sendRaw(byte[] plaintext) throws IOException {
            if (plaintext.length > MAX_PLAINTEXT) throw new ProtoException("frame too large");
            byte[] ct = Aead.seal(sendKey, nonce(sendCtr++), plaintext);
            byte[] frame = new byte[4 + ct.length];
            int n = ct.length;
            frame[0] = (byte) (n >>> 24);
            frame[1] = (byte) (n >>> 16);
            frame[2] = (byte) (n >>> 8);
            frame[3] = (byte) n;
            System.arraycopy(ct, 0, frame, 4, ct.length);
            out.write(frame);
            out.flush();
        }

        public void send(Writer w) throws IOException {
            sendRaw(w.bytes());
        }

        public Msg recv() throws IOException {
            int len = in.readInt();
            if (len < 17 || len > MAX_PLAINTEXT + 16) throw new ProtoException("bad frame length");
            byte[] ct = new byte[len];
            in.readFully(ct);
            byte[] pt = Aead.open(recvKey, nonce(recvCtr), ct);
            if (pt == null) throw new ProtoException("authentication failed (wrong key?)");
            recvCtr++;
            return new Msg(pt);
        }

        public Msg expect(int type) throws IOException {
            Msg m = recv();
            if (m.type != type) throw new ProtoException("unexpected message " + m.type + " (wanted " + type + ")");
            return m;
        }

        public void ack(boolean ok, String msg) throws IOException {
            send(new Writer(T_ACK).u8(ok ? 1 : 0).str(msg));
        }

        public void waitAck() throws IOException {
            Reader r = expect(T_ACK).reader();
            boolean ok = r.u8() == 1;
            String msg = r.str();
            if (!ok) throw new ProtoException("peer refused: " + msg);
        }

        public void close() {
            try {
                socket.close();
            } catch (IOException ignored) {
            }
        }
    }

    // ------------------------------------------------------------ handshake

    public static final class Established {
        public final Channel ch;
        public final byte[] peerId;
        public final byte[] newKey; // non-null after pairing
        public final boolean guest;

        Established(Channel ch, byte[] peerId, byte[] newKey) {
            this(ch, peerId, newKey, false);
        }

        Established(Channel ch, byte[] peerId, byte[] newKey, boolean guest) {
            this.ch = ch;
            this.peerId = peerId;
            this.newKey = newKey;
            this.guest = guest;
        }
    }

    public static final class Hello {
        public final String name;
        public final int port;
        public final int kind;

        Hello(String n, int p, int k) {
            name = n;
            port = p;
            kind = k;
        }
    }

    public static Writer helloMsg(String name, int port) {
        return new Writer(T_HELLO).str(name).u16(port).u32(KIND_PHONE);
    }

    public static Hello parseHello(Msg m) throws ProtoException {
        Reader r = m.reader();
        String n = r.str();
        int p = r.u16();
        int k = 0;
        try {
            k = (int) (r.u32() & 0xff);
        } catch (ProtoException ignored) { // v0.1 peers may omit it
        }
        return new Hello(n, p, k);
    }

    private static byte[] helloBytes(int tag, byte[] id, byte[] eph) {
        byte[] b = new byte[53];
        System.arraycopy(MAGIC, 0, b, 0, 4);
        b[4] = (byte) tag;
        System.arraycopy(id, 0, b, 5, 16);
        System.arraycopy(eph, 0, b, 21, 32);
        return b;
    }

    private static byte[] dh(byte[] sk, byte[] pk) throws ProtoException {
        byte[] s = Crypto.x25519(sk, pk);
        if (Arrays.equals(s, new byte[32])) throw new ProtoException("invalid public key");
        return s;
    }

    private static byte[] readN(DataInputStream in, int n) throws IOException {
        byte[] b = new byte[n];
        in.readFully(b);
        return b;
    }

    /**
     * @param sessionKey long-term key for MODE_SESSION; null with a {@code code} = pairing;
     *                   both null = guest session with an unpaired device.
     */
    public static Established clientHandshake(Socket s, byte[] myId, byte[] sessionKey, String code)
            throws IOException {
        DataInputStream in = new DataInputStream(new java.io.BufferedInputStream(s.getInputStream(), 1 << 17));
        OutputStream out = new java.io.BufferedOutputStream(s.getOutputStream(), 1 << 17);
        byte[][] kp = Crypto.x25519Keypair();
        int mode = sessionKey != null ? MODE_SESSION : code != null ? MODE_PAIR : MODE_GUEST;
        byte[] ch = helloBytes(mode, myId, kp[1]);
        out.write(ch);
        out.flush();
        byte[] sr = readN(in, 53);
        if (!Arrays.equals(Arrays.copyOf(sr, 4), MAGIC)) throw new ProtoException("not a Ferry device");
        switch (sr[4]) {
            case ST_OK:
                break;
            case ST_UNKNOWN_PEER:
                throw new ProtoException("the other device does not know this phone any more - pair again");
            case ST_NOT_PAIRING:
                throw new ProtoException("the other device is not in pairing mode (or the code expired)");
            case ST_NO_GUESTS:
                throw new ProtoException("the other device does not accept files from unpaired devices");
            case ST_BUSY:
                throw new ProtoException("the other device has too many pending transfers - try again later");
            default:
                throw new ProtoException("handshake rejected (" + sr[4] + ")");
        }
        byte[] peerId = Arrays.copyOfRange(sr, 5, 21);
        byte[] shared = dh(kp[0], Arrays.copyOfRange(sr, 21, 53));
        byte[] transcript = Crypto.concat(ch, sr);
        if (mode == MODE_GUEST) {
            byte[] okm = Crypto.hkdf(INFO_GUEST, shared, Crypto.concat(INFO_GUEST, transcript), 64);
            return new Established(new Channel(s, in, out, Arrays.copyOfRange(okm, 0, 32),
                    Arrays.copyOfRange(okm, 32, 64)), peerId, null, true);
        }
        if (sessionKey != null) {
            byte[] okm = Crypto.hkdf(sessionKey, shared, Crypto.concat(INFO_SESSION, transcript), 64);
            return new Established(new Channel(s, in, out, Arrays.copyOfRange(okm, 0, 32),
                    Arrays.copyOfRange(okm, 32, 64)), peerId, null);
        }
        byte[] okm = Crypto.hkdf(normalizeCode(code).getBytes(StandardCharsets.US_ASCII), shared,
                Crypto.concat(INFO_PAIR, transcript), 128);
        byte[] conf = Arrays.copyOfRange(okm, 0, 32);
        out.write(Crypto.hmac(conf, CLIENT));
        out.flush();
        byte[] stag;
        try {
            stag = readN(in, 32);
        } catch (EOFException e) {
            throw new ProtoException("pairing failed: wrong code?");
        }
        if (!Crypto.ctEq(stag, Crypto.hmac(conf, SERVER))) {
            throw new ProtoException("pairing failed: the other side could not be verified");
        }
        return new Established(new Channel(s, in, out, Arrays.copyOfRange(okm, 64, 96),
                Arrays.copyOfRange(okm, 96, 128)), peerId, Arrays.copyOfRange(okm, 32, 64));
    }

    public interface ServerCtx {
        byte[] peerKey(byte[] id);

        /** Normalized pairing code if a pairing window is open, else null. */
        String pairCode();

        void pairFailed();

        /** ST_OK if an unpaired device may open a guest session now. */
        default int guestStatus(byte[] id, java.net.InetAddress ip) {
            return ST_NO_GUESTS;
        }
    }

    public static Established serverHandshake(Socket s, byte[] myId, ServerCtx ctx) throws IOException {
        DataInputStream in = new DataInputStream(new java.io.BufferedInputStream(s.getInputStream(), 1 << 17));
        OutputStream out = new java.io.BufferedOutputStream(s.getOutputStream(), 1 << 17);
        byte[] ch = readN(in, 53);
        if (!Arrays.equals(Arrays.copyOf(ch, 4), MAGIC)) throw new ProtoException("bad magic");
        int mode = ch[4];
        byte[] peerId = Arrays.copyOfRange(ch, 5, 21);
        byte[] cpk = Arrays.copyOfRange(ch, 21, 53);
        byte[][] kp = Crypto.x25519Keypair();
        if (mode == MODE_SESSION) {
            byte[] key = ctx.peerKey(peerId);
            if (key == null) {
                reject(out, ST_UNKNOWN_PEER);
                throw new ProtoException("unknown device tried to connect");
            }
            byte[] sr = helloBytes(ST_OK, myId, kp[1]);
            out.write(sr);
            out.flush();
            byte[] shared = dh(kp[0], cpk);
            byte[] okm = Crypto.hkdf(key, shared, Crypto.concat(INFO_SESSION, ch, sr), 64);
            return new Established(new Channel(s, in, out, Arrays.copyOfRange(okm, 32, 64),
                    Arrays.copyOfRange(okm, 0, 32)), peerId, null);
        } else if (mode == MODE_PAIR) {
            String code = ctx.pairCode();
            if (code == null) {
                reject(out, ST_NOT_PAIRING);
                throw new ProtoException("pairing attempt while not pairing");
            }
            byte[] sr = helloBytes(ST_OK, myId, kp[1]);
            out.write(sr);
            out.flush();
            byte[] shared = dh(kp[0], cpk);
            byte[] okm = Crypto.hkdf(code.getBytes(StandardCharsets.US_ASCII), shared,
                    Crypto.concat(INFO_PAIR, ch, sr), 128);
            byte[] conf = Arrays.copyOfRange(okm, 0, 32);
            byte[] ctag = readN(in, 32);
            if (!Crypto.ctEq(ctag, Crypto.hmac(conf, CLIENT))) {
                ctx.pairFailed();
                throw new ProtoException("pairing attempt with a wrong code");
            }
            out.write(Crypto.hmac(conf, SERVER));
            out.flush();
            return new Established(new Channel(s, in, out, Arrays.copyOfRange(okm, 96, 128),
                    Arrays.copyOfRange(okm, 64, 96)), peerId, Arrays.copyOfRange(okm, 32, 64));
        }
        if (mode == MODE_GUEST) {
            int st = ctx.guestStatus(peerId, s.getInetAddress());
            if (st != ST_OK) {
                reject(out, st);
                throw new ProtoException("guest refused (" + st + ")");
            }
            byte[] sr = helloBytes(ST_OK, myId, kp[1]);
            out.write(sr);
            out.flush();
            byte[] shared = dh(kp[0], cpk);
            byte[] okm = Crypto.hkdf(INFO_GUEST, shared, Crypto.concat(INFO_GUEST, ch, sr), 64);
            return new Established(new Channel(s, in, out, Arrays.copyOfRange(okm, 32, 64),
                    Arrays.copyOfRange(okm, 0, 32)), peerId, null, true);
        }
        reject(out, 255);
        throw new ProtoException("bad mode");
    }

    private static void reject(OutputStream out, int status) {
        byte[] b = new byte[53];
        System.arraycopy(MAGIC, 0, b, 0, 4);
        b[4] = (byte) status;
        try {
            out.write(b);
            out.flush();
        } catch (IOException ignored) {
        }
    }

    // ------------------------------------------------------------ discovery

    /** v0.2 presence: magic | id[16] | port u16 | kind u8 | flags u8 | name len u8 | name. */
    public static final class Presence {
        public final byte[] magic, id;
        public final int port, kind, flags;
        public final String name;

        Presence(byte[] magic, byte[] id, int port, int kind, int flags, String name) {
            this.magic = magic;
            this.id = id;
            this.port = port;
            this.kind = kind;
            this.flags = flags;
            this.name = name;
        }
    }

    public static byte[] presencePacket(byte[] magic, byte[] id, int port, int kind, int flags, String name) {
        String nm = name;
        while (nm.getBytes(StandardCharsets.UTF_8).length > 64) nm = nm.substring(0, nm.length() - 1);
        byte[] n = nm.getBytes(StandardCharsets.UTF_8);
        int len = n.length;
        byte[] b = new byte[25 + len];
        System.arraycopy(magic, 0, b, 0, 4);
        System.arraycopy(id, 0, b, 4, 16);
        b[20] = (byte) (port >>> 8);
        b[21] = (byte) port;
        b[22] = (byte) kind;
        b[23] = (byte) flags;
        b[24] = (byte) len;
        System.arraycopy(n, 0, b, 25, len);
        return b;
    }

    public static Presence parsePresence(byte[] b, int len) {
        if (len < 25) return null;
        byte[] m = Arrays.copyOf(b, 4);
        if (!Arrays.equals(m, PRES_QUERY) && !Arrays.equals(m, PRES_HERE)) return null;
        int nl = b[24] & 0xff;
        if (len < 25 + nl) return null;
        return new Presence(m, Arrays.copyOfRange(b, 4, 20), ((b[20] & 0xff) << 8) | (b[21] & 0xff),
                b[22] & 0xff, b[23] & 0xff, new String(b, 25, nl, StandardCharsets.UTF_8));
    }

    public static byte[] discPacket(byte[] magic, byte[] id, int port) {
        byte[] b = new byte[22];
        System.arraycopy(magic, 0, b, 0, 4);
        System.arraycopy(id, 0, b, 4, 16);
        b[20] = (byte) (port >>> 8);
        b[21] = (byte) port;
        return b;
    }
}
