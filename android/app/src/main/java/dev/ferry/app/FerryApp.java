package dev.ferry.app;

import android.app.Application;
import android.app.DownloadManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.provider.MediaStore;
import android.util.Log;
import android.webkit.MimeTypeMap;
import android.widget.Toast;

import java.io.IOException;
import java.io.OutputStream;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;

import dev.ferry.R;
import dev.ferry.core.Node;
import dev.ferry.core.Peer;

/** Process-wide state: the protocol engine plus its Android "host" implementation. */
public final class FerryApp extends Application implements Node.Host {
    static final String TAG = "Ferry";
    static final String CH_SERVICE = "service";
    static final String CH_TRANSFER = "transfer";
    static final int NOTIF_SERVICE = 1;

    Store store;
    Node node;
    final ExecutorService io = Executors.newCachedThreadPool();
    private Handler main;
    private NotificationManager nm;
    private final AtomicInteger notifIds = new AtomicInteger(100);

    /** UI callbacks (main thread). */
    interface Listener {
        void onFerryEvent(String event, String detail);
    }

    private final List<Listener> listeners = new CopyOnWriteArrayList<>();

    static FerryApp get(Context c) {
        return (FerryApp) c.getApplicationContext();
    }

    @Override
    public void onCreate() {
        super.onCreate();
        main = new Handler(Looper.getMainLooper());
        store = new Store(this);
        node = new Node(this);
        nm = getSystemService(NotificationManager.class);
        NotificationChannel svc = new NotificationChannel(CH_SERVICE, "Background service",
                NotificationManager.IMPORTANCE_MIN);
        svc.setDescription("Shown while Ferry waits for files from your computer. You can hide it.");
        svc.setShowBadge(false);
        NotificationChannel tr = new NotificationChannel(CH_TRANSFER, "Transfers",
                NotificationManager.IMPORTANCE_DEFAULT);
        tr.setSound(null, null);
        nm.createNotificationChannel(svc);
        nm.createNotificationChannel(tr);
    }

    void addListener(Listener l) {
        listeners.add(l);
    }

    void removeListener(Listener l) {
        listeners.remove(l);
    }

    private void emit(String ev, String detail) {
        main.post(() -> {
            for (Listener l : listeners) l.onFerryEvent(ev, detail);
        });
    }

    void toast(String msg) {
        main.post(() -> Toast.makeText(this, msg, Toast.LENGTH_SHORT).show());
    }

    // ------------------------------------------------------------ Node.Host

    @Override
    public byte[] deviceId() {
        return store.id();
    }

    @Override
    public String deviceName() {
        return store.name();
    }

    @Override
    public int listenPort() {
        return store.port();
    }

    @Override
    public List<Peer> peers() {
        return store.peers();
    }

    @Override
    public void savePeer(Peer p) {
        store.savePeer(p);
        emit("peers", null);
    }

    @Override
    public void touchPeer(byte[] id, String name, String addr) {
        store.touch(id, name, addr);
    }

    @Override
    public void onPaired(Peer p) {
        emit("paired", p.name);
        notifySimple("Paired with " + p.name, "You can now share files and the clipboard.", null);
    }

    @Override
    public void onPairingClosed(String reason) {
        emit("pairclosed", reason);
    }

    @Override
    public void onClipboard(Peer from, String text) {
        main.post(() -> {
            ClipboardManager cm = getSystemService(ClipboardManager.class);
            cm.setPrimaryClip(ClipData.newPlainText("Ferry", text));
            // Android 13+ shows its own confirmation when the clipboard changes.
            if (Build.VERSION.SDK_INT < 33) {
                Toast.makeText(this, "Clipboard from " + from.name, Toast.LENGTH_SHORT).show();
            }
        });
    }

    static String mimeFor(String name) {
        int dot = name.lastIndexOf('.');
        if (dot >= 0) {
            String m = MimeTypeMap.getSingleton()
                    .getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase(Locale.ROOT));
            if (m != null) return m;
        }
        return "application/octet-stream";
    }

    /** Received files go to Download/Ferry via MediaStore (no storage permission needed). */
    @Override
    public Node.Incoming beginFile(Peer from, String name, long size) throws IOException {
        ContentResolver cr = getContentResolver();
        ContentValues v = new ContentValues();
        v.put(MediaStore.MediaColumns.DISPLAY_NAME, name);
        v.put(MediaStore.MediaColumns.MIME_TYPE, mimeFor(name));
        v.put(MediaStore.MediaColumns.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/Ferry");
        v.put(MediaStore.MediaColumns.IS_PENDING, 1);
        Uri uri = cr.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, v);
        if (uri == null) throw new IOException("cannot create file in Downloads");
        return new Node.Incoming() {
            OutputStream os;

            @Override
            public OutputStream stream() throws IOException {
                os = cr.openOutputStream(uri, "w");
                if (os == null) throw new IOException("cannot write to Downloads");
                return os;
            }

            @Override
            public void commit() {
                ContentValues done = new ContentValues();
                done.put(MediaStore.MediaColumns.IS_PENDING, 0);
                cr.update(uri, done, null, null);
            }

            @Override
            public void abort() {
                try {
                    if (os != null) os.close();
                } catch (IOException ignored) {
                }
                cr.delete(uri, null, null);
            }

            @Override
            public String displayName() {
                return name + "\u0000" + uri;
            }
        };
    }

    @Override
    public void onFilesReceived(Peer from, List<Node.Incoming> files) {
        String[] first = files.get(0).displayName().split("\u0000", 2);
        Intent open;
        String text;
        if (files.size() == 1) {
            text = first[0];
            open = new Intent(Intent.ACTION_VIEW)
                    .setDataAndType(Uri.parse(first[1]), mimeFor(first[0]))
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_ACTIVITY_NEW_TASK);
        } else {
            text = files.size() + " files in Download/Ferry";
            open = new Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        notifySimple("Received from " + from.name, text,
                PendingIntent.getActivity(this, notifIds.get(), open,
                        PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
    }

    @Override
    public void log(String msg) {
        Log.i(TAG, msg);
    }

    // ------------------------------------------------------------ notifications

    Notification.Builder builder(String channel) {
        return new Notification.Builder(this, channel).setSmallIcon(R.drawable.ic_stat);
    }

    int notifySimple(String title, String text, PendingIntent tap) {
        int id = notifIds.incrementAndGet();
        Notification.Builder b = builder(CH_TRANSFER).setContentTitle(title).setContentText(text).setAutoCancel(true);
        if (tap != null) b.setContentIntent(tap);
        try {
            nm.notify(id, b.build());
        } catch (SecurityException ignored) { // notifications not allowed
        }
        return id;
    }

    // ------------------------------------------------------------ actions used by the UI

    void sendClip(Peer p, String text) {
        io.execute(() -> {
            try {
                node.sendClip(p, text);
                toast("Clipboard sent to " + p.name);
            } catch (IOException e) {
                toast("Not sent: " + e.getMessage());
            }
        });
    }

    void sendFiles(Peer p, List<Node.Outgoing> files) {
        io.execute(() -> {
            int id = notifIds.incrementAndGet();
            String what = files.size() == 1 ? files.get(0).name : files.size() + " files";
            Notification.Builder b = builder(CH_TRANSFER)
                    .setContentTitle("Sending " + what + " to " + p.name)
                    .setOngoing(true)
                    .setOnlyAlertOnce(true)
                    .setProgress(100, 0, true);
            post(id, b);
            final long[] last = {0};
            try {
                node.sendFiles(p, files, (done, total) -> {
                    long now = System.currentTimeMillis();
                    if (now - last[0] > 500 && total > 0) {
                        last[0] = now;
                        b.setProgress(100, (int) (done * 100 / total), false);
                        post(id, b);
                    }
                });
                nm.cancel(id);
                toast("Sent " + what + " to " + p.name);
            } catch (IOException e) {
                for (Node.Outgoing o : files) {
                    try {
                        o.in.close();
                    } catch (IOException ignored) {
                    }
                }
                nm.cancel(id);
                notifySimple("Sending failed", e.getMessage(), null);
            }
        });
    }

    private void post(int id, Notification.Builder b) {
        try {
            nm.notify(id, b.build());
        } catch (SecurityException ignored) {
        }
    }

    void announceAll() {
        for (Peer p : store.peers()) {
            if (p.addr == null) continue;
            io.execute(() -> {
                try {
                    node.announce(p);
                } catch (IOException e) {
                    log("announce to " + p.name + ": " + e.getMessage());
                }
            });
        }
    }

    static List<String> localAddresses() {
        List<String> out = new ArrayList<>();
        try {
            for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!ni.isUp() || ni.isLoopback()) continue;
                for (InetAddress a : Collections.list(ni.getInetAddresses())) {
                    if (a instanceof Inet4Address && !a.isLinkLocalAddress()) out.add(a.getHostAddress());
                }
            }
        } catch (Exception ignored) {
        }
        return out;
    }
}
