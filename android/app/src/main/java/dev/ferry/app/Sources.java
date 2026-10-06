package dev.ferry.app;

import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

import dev.ferry.core.Node;

/** Turns content URIs (share sheet, file picker) into re-openable send sources. */
final class Sources {
    private Sources() {}

    /** A re-openable source: the file descriptor stays open while the send may be retried. */
    static Node.Outgoing open(Context ctx, Uri u) throws IOException {
        ContentResolver cr = ctx.getContentResolver();
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
        if (stat >= 0) {
            // seekable file: every attempt starts again at position 0
            Node.Source src = () -> {
                FileInputStream f = new FileInputStream(pfd.getFileDescriptor()) {
                    @Override
                    public void close() {
                        // keep the descriptor open for retries; released in onDone
                    }
                };
                f.getChannel().position(0);
                return f;
            };
            return new Node.Outgoing(name, stat, src, () -> {
                try {
                    pfd.close();
                } catch (IOException ignored) {
                }
            });
        }
        // Unknown length (stream-only provider): spool to cache so the size is known.
        File tmp = File.createTempFile("share", ".bin", ctx.getCacheDir());
        try (OutputStream os = new FileOutputStream(tmp);
             InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(pfd)) {
            byte[] b = new byte[65536];
            int n;
            while ((n = in.read(b)) > 0) os.write(b, 0, n);
        }
        size = tmp.length();
        return new Node.Outgoing(name, size, () -> new FileInputStream(tmp), () -> {
            //noinspection ResultOfMethodCallIgnored
            tmp.delete();
        });
    }
}
