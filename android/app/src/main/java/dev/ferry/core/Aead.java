package dev.ferry.core;

import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.util.Arrays;
import java.util.Random;
import java.util.function.Consumer;
import javax.crypto.AEADBadTagException;
import javax.crypto.Cipher;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;

/**
 * ChaCha20-Poly1305 for frames (empty AAD). Uses the platform's native cipher when it
 * provably matches the built-in implementation in {@link Crypto}, and the built-in one
 * otherwise. Both produce the same bytes, so the peer cannot tell which is in use.
 */
public final class Aead {
    private Aead() {}

    /** Tests: always use the built-in implementation. */
    public static volatile boolean forceInTree;
    /** Tests: make the next platform call fail, to exercise the fallback. */
    public static volatile boolean testFailPlatform;
    /** Receives the one-time note when the platform cipher is not used. */
    public static volatile Consumer<String> log;

    /** Android's name first, then the JDK's. */
    private static final String[] NAMES = {"ChaCha20/Poly1305/NoPadding", "ChaCha20-Poly1305"};
    private static final byte[] NO_AAD = new byte[0];
    private static final int UNKNOWN = 0, PLATFORM = 1, IN_TREE = 2;
    private static volatile int state = UNKNOWN;
    private static volatile String name;

    public static boolean usingPlatform() {
        return !forceInTree && mode() == PLATFORM;
    }

    /** Returns ciphertext || tag. */
    public static byte[] seal(byte[] key, byte[] nonce, byte[] pt) {
        if (usingPlatform()) {
            try {
                return platform(name, Cipher.ENCRYPT_MODE, key, nonce, NO_AAD, pt);
            } catch (GeneralSecurityException | RuntimeException | LinkageError e) {
                disable(name + ": " + e);
            }
        }
        return Crypto.seal(key, nonce, NO_AAD, pt);
    }

    /** Returns the plaintext, or null if authentication fails. */
    public static byte[] open(byte[] key, byte[] nonce, byte[] ct) {
        if (ct.length < 16) return null;
        if (usingPlatform()) {
            try {
                return platform(name, Cipher.DECRYPT_MODE, key, nonce, NO_AAD, ct);
            } catch (AEADBadTagException e) {
                return null;
            } catch (GeneralSecurityException | RuntimeException | LinkageError e) {
                disable(name + ": " + e);
            }
        }
        return Crypto.open(key, nonce, NO_AAD, ct);
    }

    private static int mode() {
        int s = state;
        if (s != UNKNOWN) return s;
        synchronized (Aead.class) {
            if (state != UNKNOWN) return state;
            String why = "the platform has no ChaCha20-Poly1305";
            for (String n : NAMES) {
                try {
                    if (selfTest(n)) {
                        name = n;
                        state = PLATFORM;
                        return PLATFORM;
                    }
                    why = n + " does not match the built-in cipher";
                } catch (java.security.NoSuchAlgorithmException e) {
                    // try the next name
                } catch (GeneralSecurityException | RuntimeException | LinkageError e) {
                    why = n + ": " + e;
                }
            }
            disable(why);
            return state;
        }
    }

    private static synchronized void disable(String why) {
        if (state == IN_TREE) return;
        state = IN_TREE;
        Consumer<String> l = log;
        if (l != null) l.accept("using built-in encryption (" + why + ")");
    }

    /** A fresh Cipher per call: instances refuse to encrypt twice with the same key and nonce. */
    private static byte[] platform(String n, int op, byte[] key, byte[] nonce, byte[] aad, byte[] in)
            throws GeneralSecurityException {
        if (testFailPlatform) {
            testFailPlatform = false;
            throw new LinkageError("simulated failure");
        }
        Cipher c = Cipher.getInstance(n);
        c.init(op, new SecretKeySpec(key, "ChaCha20"), new IvParameterSpec(nonce));
        if (aad.length > 0) c.updateAAD(aad);
        return c.doFinal(in);
    }

    /** RFC 8439 section 2.8.2, then random frames compared with the built-in cipher. */
    private static boolean selfTest(String n) throws GeneralSecurityException {
        byte[] key = Crypto.unhex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        byte[] nonce = Crypto.unhex("070000004041424344454647");
        byte[] aad = Crypto.unhex("50515253c0c1c2c3c4c5c6c7");
        byte[] pt = ("Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the "
                + "future, sunscreen would be it.").getBytes(StandardCharsets.US_ASCII);
        byte[] ct = platform(n, Cipher.ENCRYPT_MODE, key, nonce, aad, pt);
        if (ct.length != pt.length + 16) return false;
        if (!Crypto.hex(Arrays.copyOfRange(ct, pt.length, ct.length)).equals("1ae10b594f09e26a7e902ecbd0600691")) {
            return false;
        }
        Random r = new Random(7);
        for (int len : new int[] {0, 1, 15, 16, 17, 63, 64, 65, 1000, 65537}) {
            byte[] k = new byte[32], nn = new byte[12], m = new byte[len];
            r.nextBytes(k);
            r.nextBytes(nn);
            r.nextBytes(m);
            byte[] ref = Crypto.seal(k, nn, NO_AAD, m);
            if (!Arrays.equals(platform(n, Cipher.ENCRYPT_MODE, k, nn, NO_AAD, m), ref)) return false;
            if (!Arrays.equals(platform(n, Cipher.DECRYPT_MODE, k, nn, NO_AAD, ref), m)) return false;
            ref[ref.length - 1] ^= 1;
            try {
                platform(n, Cipher.DECRYPT_MODE, k, nn, NO_AAD, ref);
                return false; // accepted a forged frame
            } catch (AEADBadTagException expected) {
                // correct
            }
        }
        return true;
    }
}
