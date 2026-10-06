package dev.ferry.app;

import android.app.Notification;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.ConnectivityManager;
import android.net.Network;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

import java.io.IOException;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;

/**
 * Keeps Ferry reachable. While idle it only has two threads blocked in accept()/receive():
 * no polling, no timers, no wake locks. The CPU is woken by the network when a packet
 * arrives, and a partial wake lock is held only while a transfer is in progress.
 */
public final class FerryService extends Service {
    private ServerSocket server;
    private DatagramSocket udp;
    private PowerManager.WakeLock wake;
    private ConnectivityManager.NetworkCallback netCb;
    private volatile boolean stopping;

    static void start(Context c) {
        try {
            c.startForegroundService(new Intent(c, FerryService.class));
        } catch (RuntimeException e) { // background start not allowed right now
            FerryApp.get(c).log("cannot start service: " + e);
        }
    }

    private Notification buildNotification() {
        FerryApp app = FerryApp.get(this);
        PendingIntent openApp = PendingIntent.getActivity(this, 0,
                new Intent(this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE);
        PendingIntent sendClip = PendingIntent.getActivity(this, 1,
                new Intent(this, ClipSendActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_IMMUTABLE);
        int n = app.store.peers().size();
        return app.builder(FerryApp.CH_SERVICE)
                .setContentTitle(n == 0 ? "Ferry - not paired yet" : "Ferry is ready")
                .setContentText(n == 0 ? "Open the app to pair with your computer" : "Waiting for files and clipboard")
                .setContentIntent(openApp)
                .setOngoing(true)
                .setShowWhen(false)
                .addAction(new Notification.Action.Builder(null, "Send clipboard", sendClip).build())
                .build();
    }

    @Override
    public void onCreate() {
        super.onCreate();
        goForeground();
        PowerManager pm = getSystemService(PowerManager.class);
        wake = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ferry:transfer");
        wake.setReferenceCounted(true);
        startListeners();

        ConnectivityManager cm = getSystemService(ConnectivityManager.class);
        netCb = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
                // New network (or Wi-Fi reconnect): tell the desktop where we are now.
                FerryApp.get(FerryService.this).io.execute(() -> {
                    try {
                        Thread.sleep(1500);
                    } catch (InterruptedException ignored) {
                    }
                    FerryApp.get(FerryService.this).announceAll();
                });
            }
        };
        try {
            cm.registerDefaultNetworkCallback(netCb);
        } catch (RuntimeException e) {
            netCb = null;
        }
    }

    private void goForeground() {
        Notification notif = buildNotification();
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(FerryApp.NOTIF_SERVICE, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        } else {
            startForeground(FerryApp.NOTIF_SERVICE, notif);
        }
    }

    private void startListeners() {
        FerryApp app = FerryApp.get(this);
        int port = app.store.port();
        try {
            server = new ServerSocket();
            server.setReuseAddress(true);
            server.bind(new InetSocketAddress(port));
        } catch (IOException e) {
            app.log("cannot listen on port " + port + ": " + e.getMessage());
            server = null;
        }
        if (server != null) {
            Thread t = new Thread(() -> {
                while (!stopping) {
                    try {
                        Socket s = server.accept();
                        app.io.execute(() -> {
                            wake.acquire(30 * 60 * 1000L);
                            try {
                                app.node.serve(s);
                            } finally {
                                if (wake.isHeld()) wake.release();
                            }
                        });
                    } catch (IOException e) {
                        if (!stopping) app.log("accept: " + e.getMessage());
                        if (server.isClosed()) return;
                    }
                }
            }, "ferry-tcp");
            t.setDaemon(true);
            t.start();
        }
        try {
            udp = new DatagramSocket(null);
            udp.setReuseAddress(true);
            udp.bind(new InetSocketAddress(port));
            Thread u = new Thread(() -> {
                byte[] buf = new byte[64];
                while (!stopping) {
                    try {
                        DatagramPacket p = new DatagramPacket(buf, buf.length);
                        udp.receive(p);
                        app.node.answerDiscovery(udp, p);
                    } catch (IOException e) {
                        if (udp.isClosed()) return;
                    }
                }
            }, "ferry-udp");
            u.setDaemon(true);
            u.start();
        } catch (IOException e) {
            app.log("discovery disabled: " + e.getMessage());
        }
    }

    /** Refresh the persistent notification text (e.g. after pairing). */
    static void refresh(Context c) {
        start(c);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // Every startForegroundService() must be answered with startForeground(); this also
        // refreshes the notification text.
        goForeground();
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        stopping = true;
        try {
            if (server != null) server.close();
        } catch (IOException ignored) {
        }
        if (udp != null) udp.close();
        if (netCb != null) {
            try {
                getSystemService(ConnectivityManager.class).unregisterNetworkCallback(netCb);
            } catch (RuntimeException ignored) {
            }
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
