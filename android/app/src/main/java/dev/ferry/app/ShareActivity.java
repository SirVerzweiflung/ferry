package dev.ferry.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;

import dev.ferry.core.Node;
import dev.ferry.core.Peer;

/**
 * Target of the Android share sheet. Opens the shared files while it still holds the
 * temporary read permission, hands them to the background engine and closes itself.
 */
public final class ShareActivity extends Activity {
    private FerryApp app;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        app = FerryApp.get(this);
        FerryService.start(this);
        List<Peer> peers = app.store.peers();
        if (peers.isEmpty()) {
            app.toast("Pair Ferry with your computer first");
            startActivity(new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            finish();
            return;
        }
        if (peers.size() == 1) {
            go(peers.get(0));
            return;
        }
        String[] names = new String[peers.size()];
        for (int i = 0; i < names.length; i++) names[i] = peers.get(i).name;
        new AlertDialog.Builder(this)
                .setTitle("Send to")
                .setItems(names, (d, w) -> go(peers.get(w)))
                .setOnCancelListener(d -> finish())
                .show();
    }

    @SuppressWarnings("deprecation")
    private List<Uri> streams(Intent in) {
        LinkedHashSet<Uri> out = new LinkedHashSet<>();
        if (Intent.ACTION_SEND.equals(in.getAction())) {
            Uri u = Build.VERSION.SDK_INT >= 33
                    ? in.getParcelableExtra(Intent.EXTRA_STREAM, Uri.class)
                    : in.getParcelableExtra(Intent.EXTRA_STREAM);
            if (u != null) out.add(u);
        } else if (Intent.ACTION_SEND_MULTIPLE.equals(in.getAction())) {
            ArrayList<Uri> l = Build.VERSION.SDK_INT >= 33
                    ? in.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri.class)
                    : in.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
            if (l != null) out.addAll(l);
        }
        if (out.isEmpty() && in.getClipData() != null) {
            ClipData cd = in.getClipData();
            for (int i = 0; i < cd.getItemCount(); i++) if (cd.getItemAt(i).getUri() != null) out.add(cd.getItemAt(i).getUri());
        }
        return new ArrayList<>(out);
    }

    private void go(Peer peer) {
        Intent in = getIntent();
        List<Uri> uris = streams(in);
        CharSequence text = in.getCharSequenceExtra(Intent.EXTRA_TEXT);
        if (uris.isEmpty()) {
            if (text != null && text.length() > 0) app.sendClip(peer, text.toString());
            else app.toast("Nothing to send");
            finish();
            return;
        }
        // Open everything in the background (cloud providers may download first),
        // but keep this activity alive until done so the read grant stays valid.
        app.io.execute(() -> {
            List<Node.Outgoing> files = new ArrayList<>();
            for (Uri u : uris) {
                try {
                    files.add(open(u));
                } catch (IOException | RuntimeException e) {
                    app.toast("Cannot read a file: " + e.getMessage());
                }
            }
            runOnUiThread(() -> {
                if (!files.isEmpty()) {
                    app.toast("Sending to " + peer.name + "…");
                    app.sendFiles(peer, files);
                }
                finish();
            });
        });
    }

    private Node.Outgoing open(Uri u) throws IOException {
        ContentResolver cr = getContentResolver();
        String name = null;
        long size = -1;
        try (Cursor c = cr.query(u, new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE}, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                int ni = c.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                int si = c.getColumnIndex(OpenableColumns.SIZE);
                if (ni >= 0 && !c.isNull(ni)) name = c.getString(ni);
                if (si >= 0 && !c.isNull(si)) size = c.getLong(si);
            }
        } catch (RuntimeException ignored) {
        }
        if (name == null || name.isEmpty()) {
            name = u.getLastPathSegment() != null ? new File(u.getLastPathSegment()).getName() : "shared-file";
        }
        ParcelFileDescriptor pfd = cr.openFileDescriptor(u, "r");
        if (pfd == null) throw new IOException("provider returned nothing for " + name);
        long stat = pfd.getStatSize();
        if (stat >= 0) size = stat;
        InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(pfd);
        if (size < 0) {
            // Unknown length (stream-only provider): spool to cache so we can announce the size.
            File tmp = File.createTempFile("share", ".bin", getCacheDir());
            tmp.deleteOnExit();
            try (OutputStream os = new FileOutputStream(tmp); InputStream src = in) {
                byte[] b = new byte[65536];
                int n;
                while ((n = src.read(b)) > 0) os.write(b, 0, n);
            }
            size = tmp.length();
            in = new FileInputStream(tmp) {
                @Override
                public void close() throws IOException {
                    super.close();
                    //noinspection ResultOfMethodCallIgnored
                    tmp.delete();
                }
            };
        }
        return new Node.Outgoing(name, size, in);
    }
}
