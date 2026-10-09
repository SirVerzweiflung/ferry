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
import android.content.pm.ShortcutInfo;
import android.content.pm.ShortcutManager;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.MediaStore;
import android.util.Log;
import android.webkit.MimeTypeMap;
import android.widget.Toast;

import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
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
import dev.ferry.core.Crypto;
import dev.ferry.core.Node;
import dev.ferry.core.Peer;
import dev.ferry.core.Proto;

/** Process-wide state: the protocol engine plus its Android "host" implementation. */
public final class FerryApp extends Application implements Node.Host {
    static final String TAG = "Ferry";
    static final String CH_SERVICE = "service";
    static final String CH_TRANSFER = "transfer";
    static final String CH_INCOMING = "incoming";
    static final int NOTIF_SERVICE = 1;
    static final String SHARE_CATEGORY = "dev.ferry.SHARE_TARGET";

    Store store;
    Node node;
    Inbox inbox;
    final ExecutorService io = Executors.newCachedThreadPool();
    private Handler main;
    private NotificationManager nm;
    private PowerManager.WakeLock sendWake;
    private WifiManager.WifiLock sendWifi;
    private final AtomicInteger notifIds = new AtomicInteger(100);
    private final Runnable expiryTask = this::scheduleExpiry;

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
        inbox = new Inbox(new File(getFilesDir(), "incoming"));
        nm = getSystemService(NotificationManager.class);
        try { // optional: without them sends still work, only possibly slower
            sendWake = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ferry:send");
            sendWifi = getSystemService(WifiManager.class).createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "ferry:send");
        } catch (RuntimeException e) {
            log("send locks unavailable: " + e);
        }
        NotificationChannel svc = new NotificationChannel(CH_SERVICE, "Background service",
                NotificationManager.IMPORTANCE_MIN);
        svc.setDescription("Shown while Ferry waits for files from your computers. You can hide it.");
        svc.setShowBadge(false);
        NotificationChannel tr = new NotificationChannel(CH_TRANSFER, "Transfers",
                NotificationManager.IMPORTANCE_DEFAULT);
        tr.setSound(null, null);
        // Default importance with no sound: listed in the shade, never a pop-up banner.
        NotificationChannel inc = new NotificationChannel(CH_INCOMING, "Files from unpaired devices",
                NotificationManager.IMPORTANCE_DEFAULT);
        inc.setDescription("Something a nearby (not paired) device sent you, waiting for Accept or Decline.");
        inc.setSound(null, null);
        nm.createNotificationChannel(svc);
        nm.createNotificationChannel(tr);
        nm.createNotificationChannel(inc);
        scheduleExpiry();
        updateShortcuts();
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
        updateShortcuts();
        emit("peers", null);
    }

    @Override
    public void touchPeer(byte[] id, String name, String addr, int kind) {
        store.touch(id, name, addr, kind);
    }

    @Override
    public void onPaired(Peer p) {
        emit("paired", p.name);
        notifySimple("Paired with " + p.name, "Files arrive directly and the clipboard is shared.", null);
    }

    @Override
    public void onPairingClosed(String reason) {
        emit("pairclosed", reason);
    }

    @Override
    public void onClipboard(Peer from, String text) {
        setClipboard(text, from == null ? null : from.name);
    }

    private void setClipboard(String text, String from) {
        main.post(() -> {
            ClipboardManager cm = getSystemService(ClipboardManager.class);
            cm.setPrimaryClip(ClipData.newPlainText("Ferry", text));
            // Android 13+ shows its own confirmation when the clipboard changes.
            if (Build.VERSION.SDK_INT < 33 && from != null) {
                Toast.makeText(this, "Clipboard from " + from, Toast.LENGTH_SHORT).show();
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

    /** A new (pending) entry in Download/Ferry via MediaStore (no storage permission needed). */
    private Uri insertDownload(String name) throws IOException {
        ContentValues v = new ContentValues();
        v.put(MediaStore.MediaColumns.DISPLAY_NAME, name);
        v.put(MediaStore.MediaColumns.MIME_TYPE, mimeFor(name));
        v.put(MediaStore.MediaColumns.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/Ferry");
        v.put(MediaStore.MediaColumns.IS_PENDING, 1);
        Uri uri = getContentResolver().insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, v);
        if (uri == null) throw new IOException("cannot create file in Downloads");
        return uri;
    }

    private void publishDownload(Uri uri) {
        ContentValues done = new ContentValues();
        done.put(MediaStore.MediaColumns.IS_PENDING, 0);
        getContentResolver().update(uri, done, null, null);
    }

    @Override
    public Node.Incoming beginFile(Peer from, String name, long size) throws IOException {
        ContentResolver cr = getContentResolver();
        Uri uri = insertDownload(name);
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
                publishDownload(uri);
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
        List<Uri> uris = new ArrayList<>();
        List<String> names = new ArrayList<>();
        for (Node.Incoming f : files) {
            String[] p = f.displayName().split("\u0000", 2);
            names.add(p[0]);
            uris.add(Uri.parse(p[1]));
        }
        notifyFiles("Received from " + from.name, names, uris);
    }

    private void notifyFiles(String title, List<String> names, List<Uri> uris) {
        Intent open;
        String text;
        if (uris.size() == 1) {
            text = names.get(0);
            open = new Intent(Intent.ACTION_VIEW)
                    .setDataAndType(uris.get(0), mimeFor(names.get(0)))
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_ACTIVITY_NEW_TASK);
        } else {
            text = uris.size() + " files in Download/Ferry";
            open = new Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        notifySimple(title, text, PendingIntent.getActivity(this, notifIds.get(), open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
    }

    @Override
    public void log(String msg) {
        Log.i(TAG, msg);
    }

    // ---- v0.2: unpaired devices

    @Override
    public boolean visible() {
        return store.visible();
    }

    @Override
    public boolean isBlocked(byte[] id, String ip) {
        return store.isBlocked(id, ip);
    }

    @Override
    public boolean hasIncomingRoom(byte[] id, String ip) {
        return inbox.hasRoomFor(Crypto.hex(id), ip);
    }

    @Override
    public long incomingFreeBytes() {
        return Math.max(0, store.incomingLimitBytes() - inbox.bytes());
    }

    @Override
    public Node.GuestTransfer beginGuest(String fromName, byte[] fromId, String ip) throws IOException {
        return inbox.begin(fromName, fromId, ip, item -> {
            notifyIncoming(item);
            scheduleExpiry();
            emit("incoming", null);
        });
    }

    @Override
    public void onQueuedResult(String title, String message) {
        notifySimple(title, message, null);
        emit("queue", null);
    }

    @Override
    public void onChanged() {
        emit("queue", null);
    }

    // ------------------------------------------------------------ Incoming

    private static int incomingNotifId(String id) {
        return 0x10000 + (id.hashCode() & 0xffff);
    }

    /** One quiet notification per waiting transfer, with Accept / Decline buttons. */
    private void notifyIncoming(Inbox.Item it) {
        PendingIntent open = PendingIntent.getActivity(this, 2, new Intent(this, MainActivity.class),
                PendingIntent.FLAG_IMMUTABLE);
        Notification n = builder(CH_INCOMING)
                .setContentTitle(it.fromName + " wants to send you " + it.summary())
                .setContentText("Not one of your paired devices. Accept to keep it.")
                .setStyle(new Notification.BigTextStyle().bigText(it.fromName
                        + " is not one of your paired devices. Accept to keep it - otherwise it is deleted in "
                        + store.incomingHours() + " h."))
                .setContentIntent(open)
                .setOnlyAlertOnce(true)
                .addAction(new Notification.Action.Builder(null, "Accept", IncomingReceiver.intent(this, "accept", it.id)).build())
                .addAction(new Notification.Action.Builder(null, "Decline", IncomingReceiver.intent(this, "decline", it.id)).build())
                .build();
        try {
            nm.notify(incomingNotifId(it.id), n);
        } catch (SecurityException ignored) {
        }
    }

    /** Deletes expired transfers, then sleeps until the next one expires (no polling). */
    void scheduleExpiry() {
        main.removeCallbacks(expiryTask);
        List<String> before = new ArrayList<>();
        for (Inbox.Item i : inbox.list()) before.add(i.id);
        long next = inbox.expire(store.incomingHours());
        List<String> after = new ArrayList<>();
        for (Inbox.Item i : inbox.list()) after.add(i.id);
        if (after.size() != before.size()) {
            for (String id : before) if (!after.contains(id)) nm.cancel(incomingNotifId(id));
            emit("incoming", null);
        }
        if (next > 0) main.postDelayed(expiryTask, Math.max(1000, next - System.currentTimeMillis() + 1000));
    }

    void accept(String id) {
        io.execute(() -> {
            Inbox.Item it = inbox.take(id);
            nm.cancel(incomingNotifId(id));
            if (it == null) {
                toast("That transfer is no longer there");
                return;
            }
            List<Uri> uris = new ArrayList<>();
            try {
                for (int i = 0; i < it.names.size(); i++) {
                    String name = it.names.get(i);
                    Uri uri = insertDownload(name);
                    try (InputStream in = new FileInputStream(new File(it.dir, name));
                         OutputStream out = getContentResolver().openOutputStream(uri, "w")) {
                        if (out == null) throw new IOException("cannot write to Downloads");
                        byte[] b = new byte[65536];
                        int n;
                        while ((n = in.read(b)) > 0) out.write(b, 0, n);
                    }
                    publishDownload(uri);
                    uris.add(uri);
                }
                if (it.text != null) setClipboard(it.text, it.fromName);
                if (!uris.isEmpty()) notifyFiles("Saved from " + it.fromName, it.names, uris);
                else toast("Text from " + it.fromName + " copied");
            } catch (IOException e) {
                notifySimple("Could not save", String.valueOf(e.getMessage()), null);
            } finally {
                Inbox.deleteTree(it.dir);
                scheduleExpiry();
                emit("incoming", null);
            }
        });
    }

    void decline(String id) {
        Inbox.Item it = inbox.take(id);
        nm.cancel(incomingNotifId(id));
        if (it != null) Inbox.deleteTree(it.dir);
        scheduleExpiry();
        emit("incoming", null);
    }

    /** Declines everything from that device and refuses it in future. */
    void block(String id) {
        Inbox.Item it = null;
        for (Inbox.Item i : inbox.list()) if (i.id.equals(id)) it = i;
        if (it == null) return;
        store.block(it.fromId, it.ip);
        for (Inbox.Item i : inbox.takeFrom(it.fromId)) {
            nm.cancel(incomingNotifId(i.id));
            Inbox.deleteTree(i.dir);
        }
        node.forgetNearby(Crypto.unhex(it.fromId));
        toast(it.fromName + " is blocked");
        scheduleExpiry();
        emit("incoming", null);
    }

    // ------------------------------------------------------------ notifications

    Notification.Builder builder(String channel) {
        return new Notification.Builder(this, channel).setSmallIcon(R.drawable.ic_stat);
    }

    int notifySimple(String title, String text, PendingIntent tap) {
        int id = notifIds.incrementAndGet();
        Notification.Builder b = builder(CH_TRANSFER).setContentTitle(title).setContentText(text)
                .setStyle(new Notification.BigTextStyle().bigText(text)).setAutoCancel(true);
        if (tap != null) b.setContentIntent(tap);
        try {
            nm.notify(id, b.build());
        } catch (SecurityException ignored) { // notifications not allowed
        }
        return id;
    }

    private void post(int id, Notification.Builder b) {
        try {
            nm.notify(id, b.build());
        } catch (SecurityException ignored) {
        }
    }

    // ------------------------------------------------------------ actions used by the UI

    /** Clipboard to all my devices. */
    void sendClip(String text) {
        io.execute(() -> {
            try {
                List<String> ok = node.sendClipAll(text);
                toast("Clipboard sent to " + String.join(", ", ok));
            } catch (IOException e) {
                toast("Not sent: " + e.getMessage());
            }
        });
    }

    /**
     * Runs a send with the CPU and Wi-Fi kept out of power-save, like FerryService does for
     * receiving. The wake lock times out by itself; the Wi-Fi lock is released in finally.
     */
    private Node.Result sendAwake(String targetHex, String targetName, List<Node.Outgoing> files, String text,
                                  Node.Progress progress) {
        boolean wake = false, wifi = false;
        try {
            try {
                if (sendWake != null) {
                    sendWake.acquire(30 * 60 * 1000L);
                    wake = true;
                }
                if (sendWifi != null) {
                    sendWifi.acquire();
                    wifi = true;
                }
            } catch (RuntimeException e) {
                log("send locks: " + e);
            }
            return node.send(targetHex, targetName, files, text, progress);
        } finally {
            try {
                if (wifi) sendWifi.release();
            } catch (RuntimeException ignored) {
            }
            try {
                if (wake && sendWake.isHeld()) sendWake.release();
            } catch (RuntimeException ignored) {
            }
        }
    }

    /** Text to one device (paired: its clipboard; nearby: waits in its Incoming). */
    void sendText(String targetHex, String targetName, String text) {
        io.execute(() -> report(sendAwake(targetHex, targetName, null, text, null), targetName, -1));
    }

    void sendFiles(String targetHex, String targetName, List<Node.Outgoing> files) {
        io.execute(() -> {
            int id = notifIds.incrementAndGet();
            String what = files.size() == 1 ? files.get(0).name : files.size() + " files";
            Notification.Builder b = builder(CH_TRANSFER)
                    .setContentTitle("Sending " + what + " to " + targetName)
                    .setOngoing(true)
                    .setOnlyAlertOnce(true)
                    .setProgress(100, 0, true);
            post(id, b);
            final long[] last = {0};
            Node.Result r = sendAwake(targetHex, targetName, files, null, (done, total) -> {
                long now = System.currentTimeMillis();
                if (now - last[0] > 500 && total > 0) {
                    last[0] = now;
                    b.setProgress(100, (int) (done * 100 / total), false);
                    post(id, b);
                }
            });
            nm.cancel(id);
            report(r, targetName, id);
        });
    }

    private void report(Node.Result r, String targetName, int notifId) {
        switch (r.status) {
            case Node.Result.SENT:
                toast(r.message);
                break;
            case Node.Result.QUEUED:
                notifySimple("Waiting for " + targetName, r.message, null);
                emit("queue", null);
                break;
            default:
                notifySimple("Not sent", r.message, null);
        }
    }

    void announceAll() {
        if (store.visible()) node.sendPresence(false);
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

    /** Direct Share: each paired computer appears as its own target in the share sheet. */
    void updateShortcuts() {
        try {
            ShortcutManager sm = getSystemService(ShortcutManager.class);
            if (sm == null) return;
            List<ShortcutInfo> list = new ArrayList<>();
            int max = Math.min(4, sm.getMaxShortcutCountPerActivity());
            int rank = 0;
            for (Peer p : store.peers()) {
                if (list.size() >= max) break;
                ShortcutInfo.Builder b = new ShortcutInfo.Builder(this, "dev_" + p.idHex())
                        .setShortLabel(p.name)
                        .setLongLabel("Send to " + p.name)
                        .setIcon(Icon.createWithResource(this, R.mipmap.ic_launcher))
                        .setCategories(Collections.singleton(SHARE_CATEGORY))
                        .setIntent(new Intent(Intent.ACTION_MAIN).setClass(this, MainActivity.class))
                        .setRank(rank++);
                if (Build.VERSION.SDK_INT >= 30) b.setLongLived(true);
                list.add(b.build());
            }
            sm.setDynamicShortcuts(list);
        } catch (RuntimeException e) {
            log("shortcuts: " + e);
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

    static String kindLabel(int kind) {
        return kind == Proto.KIND_PHONE ? "phone" : kind == Proto.KIND_PC ? "computer" : "device";
    }
}
