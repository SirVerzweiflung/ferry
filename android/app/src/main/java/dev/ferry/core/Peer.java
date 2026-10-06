package dev.ferry.core;

/** A paired device. */
public final class Peer {
    public final byte[] id;
    public String name;
    public final byte[] key;
    /** Last known "host:port" or null. */
    public String addr;
    /** Proto.KIND_PC / KIND_PHONE, 0 = unknown (paired with v0.1). */
    public int kind;

    public Peer(byte[] id, String name, byte[] key, String addr, int kind) {
        this.id = id;
        this.name = name;
        this.key = key;
        this.addr = addr;
        this.kind = kind;
    }

    public String idHex() {
        return Crypto.hex(id);
    }

    /** Serialized as one line: id \t name \t key \t addr (same as the desktop peers file). */
    public String serialize() {
        String n = name.replace('\t', ' ').replace('\n', ' ');
        return Crypto.hex(id) + "\t" + n + "\t" + Crypto.hex(key) + "\t" + (addr == null ? "" : addr) + "\t" + kind;
    }

    public static Peer parse(String line) {
        String[] f = line.split("\t", -1);
        if (f.length < 4) return null;
        try {
            byte[] id = Crypto.unhex(f[0]);
            byte[] key = Crypto.unhex(f[2]);
            if (id.length != 16 || key.length != 32) return null;
            int kind = 0;
            if (f.length > 4) {
                try {
                    kind = Integer.parseInt(f[4]);
                } catch (NumberFormatException ignored) {
                }
            }
            return new Peer(id, f[1], key, f[3].isEmpty() ? null : f[3], kind);
        } catch (RuntimeException e) {
            return null;
        }
    }
}
