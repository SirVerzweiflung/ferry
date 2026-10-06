package dev.ferry.app;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.provider.Settings;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import dev.ferry.core.Crypto;
import dev.ferry.core.Peer;
import dev.ferry.core.Proto;

/** Settings, identity, paired devices and blocked devices (SharedPreferences, app-private). */
final class Store {
    private final SharedPreferences sp;
    private final Context ctx;
    private final byte[] id;
    /** Paired devices; index 0 = main device (most recently paired). */
    private final List<Peer> peers = new ArrayList<>();

    Store(Context c) {
        ctx = c;
        sp = c.getSharedPreferences("ferry", Context.MODE_PRIVATE);
        String hex = sp.getString("id", null);
        if (hex == null || hex.length() != 32) {
            id = Crypto.random(16);
            sp.edit().putString("id", Crypto.hex(id)).apply();
        } else {
            id = Crypto.unhex(hex);
        }
        for (String line : sp.getString("peers", "").split("\n")) {
            Peer p = Peer.parse(line);
            if (p != null) peers.add(p);
        }
    }

    byte[] id() {
        return id;
    }

    String name() {
        String n = sp.getString("name", null);
        if (n != null && !n.isEmpty()) return n;
        try {
            String dn = Settings.Global.getString(ctx.getContentResolver(), Settings.Global.DEVICE_NAME);
            if (dn != null && !dn.isEmpty()) return dn;
        } catch (RuntimeException ignored) {
        }
        return Build.MODEL;
    }

    void setName(String n) {
        sp.edit().putString("name", n.trim()).apply();
    }

    int port() {
        return Proto.DEFAULT_PORT;
    }

    /** Visible to unpaired devices on the network (they can send to Incoming). */
    boolean visible() {
        return sp.getBoolean("visible", true);
    }

    void setVisible(boolean v) {
        sp.edit().putBoolean("visible", v).apply();
    }

    long incomingLimitBytes() {
        return 2048L * 1_000_000L;
    }

    long incomingHours() {
        return 24;
    }

    // ------------------------------------------------------------ paired devices

    synchronized List<Peer> peers() {
        return new ArrayList<>(peers);
    }

    synchronized Peer mainPeer() {
        return peers.isEmpty() ? null : peers.get(0);
    }

    synchronized Peer find(String idHex) {
        for (Peer p : peers) if (p.idHex().equals(idHex)) return p;
        return null;
    }

    /** New pairing: becomes the main device. */
    synchronized void savePeer(Peer p) {
        removeLocked(p.id);
        peers.add(0, p);
        persist();
    }

    synchronized void makeMain(byte[] pid) {
        for (int i = 0; i < peers.size(); i++) {
            if (Arrays.equals(peers.get(i).id, pid)) {
                peers.add(0, peers.remove(i));
                persist();
                return;
            }
        }
    }

    synchronized void touch(byte[] pid, String name, String addr, int kind) {
        for (Peer p : peers) {
            if (Arrays.equals(p.id, pid)) {
                boolean changed = false;
                if (name != null && !name.isEmpty() && !name.equals(p.name)) {
                    p.name = name;
                    changed = true;
                }
                if (addr != null && !addr.equals(p.addr)) {
                    p.addr = addr;
                    changed = true;
                }
                if (kind != 0 && kind != p.kind) {
                    p.kind = kind;
                    changed = true;
                }
                if (changed) persist();
                return;
            }
        }
    }

    synchronized void remove(byte[] pid) {
        removeLocked(pid);
        persist();
    }

    private void removeLocked(byte[] pid) {
        for (int i = peers.size() - 1; i >= 0; i--) if (Arrays.equals(peers.get(i).id, pid)) peers.remove(i);
    }

    private void persist() {
        StringBuilder b = new StringBuilder();
        for (Peer p : peers) b.append(p.serialize()).append('\n');
        sp.edit().putString("peers", b.toString()).apply();
    }

    // ------------------------------------------------------------ blocked unpaired devices

    synchronized boolean isBlocked(byte[] pid, String ip) {
        Set<String> b = sp.getStringSet("blocked", new HashSet<>());
        return b.contains("id:" + Crypto.hex(pid)) || (ip != null && !ip.isEmpty() && b.contains("ip:" + ip));
    }

    synchronized void block(String idHex, String ip) {
        Set<String> b = new HashSet<>(sp.getStringSet("blocked", new HashSet<>()));
        b.add("id:" + idHex);
        if (ip != null && !ip.isEmpty()) b.add("ip:" + ip);
        sp.edit().putStringSet("blocked", b).apply();
    }

    synchronized int blockedCount() {
        return sp.getStringSet("blocked", new HashSet<>()).size();
    }

    synchronized void unblockAll() {
        sp.edit().remove("blocked").apply();
    }
}
