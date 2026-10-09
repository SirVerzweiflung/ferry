package dev.ferry.core;

import java.security.MessageDigest;
import java.security.SecureRandom;
import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

/**
 * Crypto primitives used by Ferry. SHA-256/HMAC come from the platform; X25519 and
 * ChaCha20-Poly1305 are implemented here (pure Java, no dependencies) so the app works
 * the same on every Android version. Verified against RFC 7748 / RFC 8439 test vectors
 * and cross-tested with the desktop implementation.
 */
public final class Crypto {
    private Crypto() {}

    public static final SecureRandom RNG = new SecureRandom();

    public static byte[] random(int n) {
        byte[] b = new byte[n];
        RNG.nextBytes(b);
        return b;
    }

    public static boolean ctEq(byte[] a, byte[] b) {
        return MessageDigest.isEqual(a, b);
    }

    public static byte[] concat(byte[]... parts) {
        int n = 0;
        for (byte[] p : parts) n += p.length;
        byte[] o = new byte[n];
        int off = 0;
        for (byte[] p : parts) {
            System.arraycopy(p, 0, o, off, p.length);
            off += p.length;
        }
        return o;
    }

    public static byte[] hmac(byte[] key, byte[]... parts) {
        try {
            Mac m = Mac.getInstance("HmacSHA256");
            // SecretKeySpec rejects empty keys; HMAC with an empty key equals a zero block key.
            m.init(new SecretKeySpec(key.length == 0 ? new byte[64] : key, "HmacSHA256"));
            for (byte[] p : parts) m.update(p);
            return m.doFinal();
        } catch (Exception e) {
            throw new IllegalStateException(e);
        }
    }

    public static byte[] hkdf(byte[] salt, byte[] ikm, byte[] info, int len) {
        byte[] prk = hmac(salt, ikm);
        byte[] out = new byte[len];
        byte[] t = new byte[0];
        int pos = 0;
        int ctr = 1;
        while (pos < len) {
            t = hmac(prk, t, info, new byte[] {(byte) ctr});
            int n = Math.min(32, len - pos);
            System.arraycopy(t, 0, out, pos, n);
            pos += n;
            ctr++;
        }
        return out;
    }

    // ------------------------------------------------------------ hex

    public static String hex(byte[] b) {
        StringBuilder s = new StringBuilder();
        for (byte x : b) s.append(String.format("%02x", x & 0xff));
        return s.toString();
    }

    public static byte[] unhex(String s) {
        s = s.trim();
        if (s.length() % 2 != 0) throw new IllegalArgumentException("odd hex length");
        byte[] o = new byte[s.length() / 2];
        for (int i = 0; i < o.length; i++) o[i] = (byte) Integer.parseInt(s.substring(2 * i, 2 * i + 2), 16);
        return o;
    }

    // ------------------------------------------------------------ X25519 (TweetNaCl field arithmetic)

    private static final long[] GF_121665 = {0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    public static final byte[] X25519_BASE = new byte[32];

    static {
        X25519_BASE[0] = 9;
    }

    private static void car(long[] o) {
        for (int i = 0; i < 16; i++) {
            o[i] += 1L << 16;
            long c = o[i] >> 16;
            if (i < 15) o[i + 1] += c - 1;
            else o[0] += 38 * (c - 1);
            o[i] -= c << 16;
        }
    }

    private static void sel(long[] p, long[] q, long b) {
        long c = ~(b - 1);
        for (int i = 0; i < 16; i++) {
            long t = c & (p[i] ^ q[i]);
            p[i] ^= t;
            q[i] ^= t;
        }
    }

    private static byte[] pack(long[] n) {
        long[] t = n.clone();
        long[] m = new long[16];
        car(t);
        car(t);
        car(t);
        for (int j = 0; j < 2; j++) {
            m[0] = t[0] - 0xffed;
            for (int i = 1; i < 15; i++) {
                m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
                m[i - 1] &= 0xffff;
            }
            m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
            long b = (m[15] >> 16) & 1;
            m[14] &= 0xffff;
            sel(t, m, 1 - b);
        }
        byte[] o = new byte[32];
        for (int i = 0; i < 16; i++) {
            o[2 * i] = (byte) (t[i] & 0xff);
            o[2 * i + 1] = (byte) ((t[i] >> 8) & 0xff);
        }
        return o;
    }

    private static long[] unpack(byte[] n) {
        long[] o = new long[16];
        for (int i = 0; i < 16; i++) o[i] = (n[2 * i] & 0xff) + ((long) (n[2 * i + 1] & 0xff) << 8);
        o[15] &= 0x7fff;
        return o;
    }

    private static long[] add(long[] a, long[] b) {
        long[] o = new long[16];
        for (int i = 0; i < 16; i++) o[i] = a[i] + b[i];
        return o;
    }

    private static long[] sub(long[] a, long[] b) {
        long[] o = new long[16];
        for (int i = 0; i < 16; i++) o[i] = a[i] - b[i];
        return o;
    }

    private static long[] mul(long[] a, long[] b) {
        long[] t = new long[31];
        for (int i = 0; i < 16; i++) for (int j = 0; j < 16; j++) t[i + j] += a[i] * b[j];
        for (int i = 0; i < 15; i++) t[i] += 38 * t[i + 16];
        long[] o = new long[16];
        System.arraycopy(t, 0, o, 0, 16);
        car(o);
        car(o);
        return o;
    }

    private static long[] inv(long[] i) {
        long[] c = i.clone();
        for (int a = 253; a >= 0; a--) {
            c = mul(c, c);
            if (a != 2 && a != 4) c = mul(c, i);
        }
        return c;
    }

    public static byte[] x25519(byte[] scalar, byte[] point) {
        byte[] z = scalar.clone();
        z[31] = (byte) ((z[31] & 127) | 64);
        z[0] &= (byte) 248;
        long[] x = unpack(point);
        long[] a = new long[16], b = x.clone(), c = new long[16], d = new long[16];
        a[0] = 1;
        d[0] = 1;
        for (int i = 254; i >= 0; i--) {
            long r = ((z[i >>> 3] & 0xff) >>> (i & 7)) & 1;
            sel(a, b, r);
            sel(c, d, r);
            long[] e = add(a, c);
            a = sub(a, c);
            c = add(b, d);
            b = sub(b, d);
            d = mul(e, e);
            long[] f = mul(a, a);
            a = mul(c, a);
            c = mul(b, e);
            e = add(a, c);
            a = sub(a, c);
            b = mul(a, a);
            c = sub(d, f);
            a = mul(c, GF_121665);
            a = add(a, d);
            c = mul(c, a);
            a = mul(d, f);
            d = mul(b, x);
            b = mul(e, e);
            sel(a, b, r);
            sel(c, d, r);
        }
        return pack(mul(a, inv(c)));
    }

    /** Returns {secret, public}. */
    public static byte[][] x25519Keypair() {
        byte[] sk = random(32);
        return new byte[][] {sk, x25519(sk, X25519_BASE)};
    }

    // ------------------------------------------------------------ ChaCha20

    private static int le32(byte[] b, int o) {
        return (b[o] & 0xff) | (b[o + 1] & 0xff) << 8 | (b[o + 2] & 0xff) << 16 | (b[o + 3] & 0xff) << 24;
    }

    private static void put32(byte[] b, int o, int v) {
        b[o] = (byte) v;
        b[o + 1] = (byte) (v >>> 8);
        b[o + 2] = (byte) (v >>> 16);
        b[o + 3] = (byte) (v >>> 24);
    }

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

    private static void qr(int[] s, int a, int b, int c, int d) {
        s[a] += s[b];
        s[d] = Integer.rotateLeft(s[d] ^ s[a], 16);
        s[c] += s[d];
        s[b] = Integer.rotateLeft(s[b] ^ s[c], 12);
        s[a] += s[b];
        s[d] = Integer.rotateLeft(s[d] ^ s[a], 8);
        s[c] += s[d];
        s[b] = Integer.rotateLeft(s[b] ^ s[c], 7);
    }

    private static int[] words(byte[] b, int n) {
        int[] w = new int[n];
        for (int i = 0; i < n; i++) w[i] = le32(b, 4 * i);
        return w;
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

    // ------------------------------------------------------------ Poly1305 (donna, 26-bit limbs)

    private static long u32(byte[] b, int o) {
        return le32(b, o) & 0xffffffffL;
    }

    static final class Poly1305 {
        private final long r0, r1, r2, r3, r4, s1, s2, s3, s4;
        private final long p0, p1, p2, p3;
        private long h0, h1, h2, h3, h4;
        private final byte[] buf = new byte[16];
        private int left;

        Poly1305(byte[] key) {
            r0 = u32(key, 0) & 0x3ffffff;
            r1 = (u32(key, 3) >>> 2) & 0x3ffff03;
            r2 = (u32(key, 6) >>> 4) & 0x3ffc0ff;
            r3 = (u32(key, 9) >>> 6) & 0x3f03fff;
            r4 = (u32(key, 12) >>> 8) & 0x00fffff;
            s1 = r1 * 5;
            s2 = r2 * 5;
            s3 = r3 * 5;
            s4 = r4 * 5;
            p0 = u32(key, 16);
            p1 = u32(key, 20);
            p2 = u32(key, 24);
            p3 = u32(key, 28);
        }

        private void blk(byte[] m, int o, long hibit) {
            h0 += u32(m, o) & 0x3ffffff;
            h1 += (u32(m, o + 3) >>> 2) & 0x3ffffff;
            h2 += (u32(m, o + 6) >>> 4) & 0x3ffffff;
            h3 += (u32(m, o + 9) >>> 6) & 0x3ffffff;
            h4 += (u32(m, o + 12) >>> 8) | hibit;
            long d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
            long d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
            long d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
            long d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
            long d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
            long c = d0 >>> 26;
            h0 = d0 & 0x3ffffff;
            d1 += c;
            c = d1 >>> 26;
            h1 = d1 & 0x3ffffff;
            d2 += c;
            c = d2 >>> 26;
            h2 = d2 & 0x3ffffff;
            d3 += c;
            c = d3 >>> 26;
            h3 = d3 & 0x3ffffff;
            d4 += c;
            c = d4 >>> 26;
            h4 = d4 & 0x3ffffff;
            h0 += c * 5;
            c = h0 >>> 26;
            h0 &= 0x3ffffff;
            h1 += c;
        }

        void update(byte[] d, int off, int len) {
            if (left > 0) {
                int n = Math.min(16 - left, len);
                System.arraycopy(d, off, buf, left, n);
                left += n;
                off += n;
                len -= n;
                if (left < 16) return;
                blk(buf, 0, 1L << 24);
                left = 0;
            }
            while (len >= 16) {
                blk(d, off, 1L << 24);
                off += 16;
                len -= 16;
            }
            if (len > 0) {
                System.arraycopy(d, off, buf, 0, len);
                left = len;
            }
        }

        byte[] finish() {
            if (left > 0) {
                byte[] b = new byte[16];
                System.arraycopy(buf, 0, b, 0, left);
                b[left] = 1;
                blk(b, 0, 0);
            }
            long c = h1 >>> 26;
            h1 &= 0x3ffffff;
            h2 += c;
            c = h2 >>> 26;
            h2 &= 0x3ffffff;
            h3 += c;
            c = h3 >>> 26;
            h3 &= 0x3ffffff;
            h4 += c;
            c = h4 >>> 26;
            h4 &= 0x3ffffff;
            h0 += c * 5;
            c = h0 >>> 26;
            h0 &= 0x3ffffff;
            h1 += c;

            long g0 = h0 + 5;
            c = g0 >>> 26;
            g0 &= 0x3ffffff;
            long g1 = h1 + c;
            c = g1 >>> 26;
            g1 &= 0x3ffffff;
            long g2 = h2 + c;
            c = g2 >>> 26;
            g2 &= 0x3ffffff;
            long g3 = h3 + c;
            c = g3 >>> 26;
            g3 &= 0x3ffffff;
            long g4 = h4 + c - (1L << 26);
            long mask = (g4 >>> 63) - 1; // all ones if h >= p
            h0 = (h0 & ~mask) | (g0 & mask);
            h1 = (h1 & ~mask) | (g1 & mask);
            h2 = (h2 & ~mask) | (g2 & mask);
            h3 = (h3 & ~mask) | (g3 & mask);
            h4 = (h4 & ~mask) | (g4 & mask);

            long w0 = (h0 | (h1 << 26)) & 0xffffffffL;
            long w1 = ((h1 >>> 6) | (h2 << 20)) & 0xffffffffL;
            long w2 = ((h2 >>> 12) | (h3 << 14)) & 0xffffffffL;
            long w3 = ((h3 >>> 18) | (h4 << 8)) & 0xffffffffL;
            byte[] out = new byte[16];
            long f = w0 + p0;
            put32(out, 0, (int) f);
            f = w1 + p1 + (f >>> 32);
            put32(out, 4, (int) f);
            f = w2 + p2 + (f >>> 32);
            put32(out, 8, (int) f);
            f = w3 + p3 + (f >>> 32);
            put32(out, 12, (int) f);
            return out;
        }
    }

    public static byte[] poly1305(byte[] key, byte[] msg) {
        Poly1305 p = new Poly1305(key);
        p.update(msg, 0, msg.length);
        return p.finish();
    }

    // ------------------------------------------------------------ AEAD (RFC 8439), empty AAD supported

    private static byte[] tag(byte[] key, byte[] nonce, byte[] aad, byte[] ct, int off, int len) {
        byte[] otk = new byte[64];
        block(words(key, 8), 0, words(nonce, 3), otk);
        Poly1305 p = new Poly1305(java.util.Arrays.copyOf(otk, 32));
        byte[] zeros = new byte[16];
        p.update(aad, 0, aad.length);
        p.update(zeros, 0, (16 - aad.length % 16) % 16);
        p.update(ct, off, len);
        p.update(zeros, 0, (16 - len % 16) % 16);
        byte[] lens = new byte[16];
        long al = aad.length, cl = len;
        for (int i = 0; i < 8; i++) {
            lens[i] = (byte) (al >>> (8 * i));
            lens[8 + i] = (byte) (cl >>> (8 * i));
        }
        p.update(lens, 0, 16);
        return p.finish();
    }

    /** Returns ciphertext || tag. */
    public static byte[] seal(byte[] key, byte[] nonce, byte[] aad, byte[] pt) {
        byte[] out = java.util.Arrays.copyOf(pt, pt.length + 16);
        chacha20Xor(key, 1, nonce, out, 0, pt.length);
        byte[] t = tag(key, nonce, aad, out, 0, pt.length);
        System.arraycopy(t, 0, out, pt.length, 16);
        return out;
    }

    /** Returns plaintext or null if authentication fails. */
    public static byte[] open(byte[] key, byte[] nonce, byte[] aad, byte[] ct) {
        if (ct.length < 16) return null;
        int n = ct.length - 16;
        byte[] t = tag(key, nonce, aad, ct, 0, n);
        if (!ctEq(t, java.util.Arrays.copyOfRange(ct, n, ct.length))) return null;
        byte[] pt = java.util.Arrays.copyOf(ct, n);
        chacha20Xor(key, 1, nonce, pt, 0, n);
        return pt;
    }
}
