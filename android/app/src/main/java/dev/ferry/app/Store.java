package dev.ferry.app;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.provider.Settings;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

import dev.ferry.core.Crypto;
import dev.ferry.core.Peer;
import dev.ferry.core.Proto;

/** Settings, identity and paired devices (SharedPreferences, app-private). */
final class Store {
    private final SharedPreferences sp;
    private final Context ctx;
    private byte[] id;
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

    synchronized List<Peer> peers() {
        return new ArrayList<>(peers);
    }

    synchronized Peer defaultPeer() {
        return peers.isEmpty() ? null : peers.get(0);
    }

    synchronized void savePeer(Peer p) {
        removeLocked(p.id);
        peers.add(0, p);
        persist();
    }

    synchronized void touch(byte[] pid, String name, String addr) {
        for (int i = 0; i < peers.size(); i++) {
            Peer p = peers.get(i);
            if (Arrays.equals(p.id, pid)) {
                boolean changed = i != 0;
                if (name != null && !name.isEmpty() && !name.equals(p.name)) {
                    p.name = name;
                    changed = true;
                }
                if (addr != null && !addr.equals(p.addr)) {
                    p.addr = addr;
                    changed = true;
                }
                if (changed) {
                    peers.remove(i);
                    peers.add(0, p);
                    persist();
                }
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
}
