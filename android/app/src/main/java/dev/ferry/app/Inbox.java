package dev.ferry.app;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;

import dev.ferry.core.Crypto;
import dev.ferry.core.Node;

/**
 * "Incoming": what unpaired devices send waits in app-private storage until the user
 * accepts it (moved to Download/Ferry) or declines it. Deleted after 24 h otherwise.
 * One directory per transfer; a "meta" file is written last.
 */
final class Inbox {
    static final int MAX_PENDING = 10;
    static final int MAX_PENDING_PER_SENDER = 2;

    static final class Item {
        String id, fromName, fromId, ip, text;
        long time;
        final List<String> names = new ArrayList<>();
        final List<Long> sizes = new ArrayList<>();
        File dir;

        long bytes() {
            long t = 0;
            for (long s : sizes) t += s;
            return t;
        }

        /** "3 files (12.4 MB)" / "a text" / "photo.jpg (2.1 MB) and a text" */
        String summary() {
            List<String> parts = new ArrayList<>();
            if (names.size() == 1) parts.add(names.get(0) + " (" + Node.human(bytes()) + ")");
            else if (names.size() > 1) parts.add(names.size() + " files (" + Node.human(bytes()) + ")");
            if (text != null) parts.add("a text");
            return String.join(" and ", parts);
        }
    }

    private final File root;
    private final List<Item> items = new ArrayList<>();

    Inbox(File root) {
        this.root = root;
        //noinspection ResultOfMethodCallIgnored
        root.mkdirs();
        File[] dirs = root.listFiles();
        if (dirs != null) {
            for (File d : dirs) {
                Item it = read(d);
                if (it != null) items.add(it);
                else deleteTree(d);
            }
        }
        items.sort((a, b) -> Long.compare(a.time, b.time));
    }

    private static String esc(String s) {
        return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n").replace("\r", "\\r");
    }

    private static String unesc(String s) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '\\' && i + 1 < s.length()) {
                char n = s.charAt(++i);
                b.append(n == 't' ? '\t' : n == 'n' ? '\n' : n == 'r' ? '\r' : n);
            } else {
                b.append(c);
            }
        }
        return b.toString();
    }

    private static Item read(File d) {
        File meta = new File(d, "meta");
        if (!d.isDirectory() || !meta.isFile()) return null;
        try {
            Item it = new Item();
            it.id = d.getName();
            it.dir = d;
            for (String line : new String(Files.readAllBytes(meta.toPath()), StandardCharsets.UTF_8).split("\n")) {
                String[] f = line.split("\t", -1);
                switch (f[0]) {
                    case "from": it.fromName = unesc(f[1]); break;
                    case "fromid": it.fromId = f[1]; break;
                    case "ip": it.ip = f[1]; break;
                    case "time": it.time = Long.parseLong(f[1]); break;
                    case "file": it.names.add(unesc(f[1])); it.sizes.add(Long.parseLong(f[2])); break;
                    case "text": it.text = unesc(f[1]); break;
                    default: break;
                }
            }
            return it.fromName == null ? null : it;
        } catch (IOException | RuntimeException e) {
            return null;
        }
    }

    static void deleteTree(File f) {
        File[] kids = f.listFiles();
        if (kids != null) for (File k : kids) deleteTree(k);
        //noinspection ResultOfMethodCallIgnored
        f.delete();
    }

    synchronized List<Item> list() {
        return new ArrayList<>(items);
    }

    synchronized long bytes() {
        long t = 0;
        for (Item i : items) t += i.bytes();
        return t;
    }

    synchronized boolean hasRoomFor(String fromId, String ip) {
        int same = 0;
        for (Item i : items) if (i.fromId.equals(fromId) || i.ip.equals(ip)) same++;
        return items.size() < MAX_PENDING && same < MAX_PENDING_PER_SENDER;
    }

    synchronized Item take(String id) {
        for (int i = 0; i < items.size(); i++) if (items.get(i).id.equals(id)) return items.remove(i);
        return null;
    }

    synchronized List<Item> takeFrom(String fromId) {
        List<Item> out = new ArrayList<>();
        for (int i = items.size() - 1; i >= 0; i--) if (items.get(i).fromId.equals(fromId)) out.add(items.remove(i));
        return out;
    }

    /** Removes expired items and returns the time (ms) of the next expiry, or -1. */
    synchronized long expire(long hours) {
        long ttl = hours * 3600_000L;
        long now = System.currentTimeMillis();
        long next = -1;
        for (int i = items.size() - 1; i >= 0; i--) {
            Item it = items.get(i);
            long end = it.time + ttl;
            if (end <= now) {
                deleteTree(it.dir);
                items.remove(i);
            } else if (next < 0 || end < next) {
                next = end;
            }
        }
        return next;
    }

    /** Storage for one transfer that is being received. */
    Node.GuestTransfer begin(String fromName, byte[] fromId, String ip, java.util.function.Consumer<Item> onCommit)
            throws IOException {
        Item it = new Item();
        it.id = Crypto.hex(Crypto.random(5));
        it.fromName = fromName;
        it.fromId = Crypto.hex(fromId);
        it.ip = ip;
        it.time = System.currentTimeMillis();
        it.dir = new File(root, it.id);
        if (!it.dir.mkdirs()) throw new IOException("cannot create " + it.dir);
        return new Node.GuestTransfer() {
            String current;
            long currentSize;

            @Override
            public OutputStream file(String name, long size) throws IOException {
                String n = name;
                for (int k = 1; new File(it.dir, n).exists(); k++) {
                    int dot = name.lastIndexOf('.');
                    n = dot > 0 ? name.substring(0, dot) + " (" + k + ")" + name.substring(dot) : name + " (" + k + ")";
                }
                current = n;
                currentSize = size;
                return new FileOutputStream(new File(it.dir, n));
            }

            @Override
            public void fileDone() {
                it.names.add(current);
                it.sizes.add(currentSize);
            }

            @Override
            public void text(String t) {
                it.text = t;
            }

            @Override
            public boolean isEmpty() {
                return it.names.isEmpty() && it.text == null;
            }

            @Override
            public void commit() throws IOException {
                StringBuilder m = new StringBuilder();
                m.append("from\t").append(esc(it.fromName)).append('\n');
                m.append("fromid\t").append(it.fromId).append('\n');
                m.append("ip\t").append(it.ip).append('\n');
                m.append("time\t").append(it.time).append('\n');
                for (int i = 0; i < it.names.size(); i++) {
                    m.append("file\t").append(esc(it.names.get(i))).append('\t').append(it.sizes.get(i)).append('\n');
                }
                if (it.text != null) m.append("text\t").append(esc(it.text)).append('\n');
                File tmp = new File(it.dir, "meta.tmp");
                Files.write(tmp.toPath(), m.toString().getBytes(StandardCharsets.UTF_8));
                if (!tmp.renameTo(new File(it.dir, "meta"))) throw new IOException("cannot write meta");
                synchronized (Inbox.this) {
                    items.add(it);
                }
                onCommit.accept(it);
            }

            @Override
            public void discard() {
                deleteTree(it.dir);
            }
        };
    }
}
