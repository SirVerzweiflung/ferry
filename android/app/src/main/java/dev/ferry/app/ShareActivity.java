package dev.ferry.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.Intent;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.util.TypedValue;
import android.view.View;
import android.view.ViewGroup;
import android.widget.BaseAdapter;
import android.widget.ListView;
import android.widget.TextView;

import java.io.IOException;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;

import dev.ferry.core.Node;
import dev.ferry.core.Peer;

/**
 * Target of the Android share sheet. Shows a small chooser (my devices first, nearby
 * devices below), or sends directly when picked through a Direct Share shortcut. Opens
 * the shared files while it still holds the read permission, then closes itself.
 */
public final class ShareActivity extends Activity {
    private FerryApp app;

    /** One row of the chooser: a header or a device. */
    private static final class Row {
        final String label, idHex, name;

        Row(String label, String idHex, String name) {
            this.label = label;
            this.idHex = idHex;
            this.name = name;
        }

        boolean header() {
            return idHex == null;
        }
    }

    private final List<Row> rows = new ArrayList<>();
    private BaseAdapter adapter;
    private AlertDialog dialog;
    private boolean sending;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        app = FerryApp.get(this);
        FerryService.start(this);

        // Direct Share shortcut ("dev_<id>") -> straight to that device
        String sc = getIntent().getStringExtra(Intent.EXTRA_SHORTCUT_ID);
        if (sc != null && sc.startsWith("dev_")) {
            Peer p = app.store.find(sc.substring(4));
            if (p != null) {
                go(p.idHex(), p.name);
                return;
            }
        }
        showChooser();
    }

    private int dp(int v) {
        return (int) TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, getResources().getDisplayMetrics());
    }

    private void showChooser() {
        List<Peer> peers = app.store.peers();
        rows.add(new Row("My devices", null, null));
        if (peers.isEmpty()) rows.add(new Row("  none paired yet", null, null));
        for (int i = 0; i < peers.size(); i++) {
            Peer p = peers.get(i);
            rows.add(new Row((i == 0 ? "★ " : "") + p.name, p.idHex(), p.name));
        }
        rows.add(new Row("Nearby - they have to accept", null, null));
        rows.add(new Row("  looking…", null, null));

        adapter = new BaseAdapter() {
            @Override
            public int getCount() {
                return rows.size();
            }

            @Override
            public Object getItem(int i) {
                return rows.get(i);
            }

            @Override
            public long getItemId(int i) {
                return i;
            }

            @Override
            public boolean isEnabled(int i) {
                return !rows.get(i).header();
            }

            @Override
            public boolean areAllItemsEnabled() {
                return false;
            }

            @Override
            public View getView(int i, View convert, ViewGroup parent) {
                TextView t = convert instanceof TextView ? (TextView) convert : new TextView(ShareActivity.this);
                Row r = rows.get(i);
                t.setText(r.label);
                if (r.header()) {
                    t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
                    t.setTypeface(Typeface.DEFAULT_BOLD);
                    t.setAlpha(0.6f);
                    t.setPadding(dp(24), dp(14), dp(24), dp(4));
                } else {
                    t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 17);
                    t.setTypeface(Typeface.DEFAULT);
                    t.setAlpha(1f);
                    t.setPadding(dp(24), dp(12), dp(24), dp(12));
                }
                return t;
            }
        };
        ListView list = new ListView(this);
        list.setAdapter(adapter);
        list.setOnItemClickListener((parent, view, pos, id) -> {
            Row r = rows.get(pos);
            if (r.header()) return;
            sending = true;
            dialog.dismiss();
            go(r.idHex, r.name);
        });
        dialog = new AlertDialog.Builder(this)
                .setTitle("Send with Ferry")
                .setView(list)
                .setNegativeButton("Cancel", null)
                .setOnDismissListener(d -> {
                    if (!sending) finish();
                })
                .show();

        // fill in nearby devices (short network scan)
        app.io.execute(() -> {
            List<Node.Device> devs = app.node.devices(true);
            runOnUiThread(() -> {
                if (isFinishing()) return;
                for (int i = rows.size() - 1; i >= 0; i--) if (rows.get(i).label.startsWith("  looking")) rows.remove(i);
                int added = 0;
                for (Node.Device d : devs) {
                    if (d.paired) continue;
                    rows.add(new Row(d.name + "  (" + FerryApp.kindLabel(d.kind) + ")", d.idHex(), d.name));
                    added++;
                }
                if (added == 0) rows.add(new Row("  none found", null, null));
                adapter.notifyDataSetChanged();
            });
        });
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

    private void go(String targetHex, String targetName) {
        Intent in = getIntent();
        List<Uri> uris = streams(in);
        CharSequence text = in.getCharSequenceExtra(Intent.EXTRA_TEXT);
        if (uris.isEmpty()) {
            if (text != null && text.length() > 0) app.sendText(targetHex, targetName, text.toString());
            else app.toast("Nothing to send");
            finish();
            return;
        }
        // Open everything in the background (cloud providers may download first), but keep
        // this activity alive until done so the read permission stays valid.
        app.io.execute(() -> {
            List<Node.Outgoing> files = new ArrayList<>();
            for (Uri u : uris) {
                try {
                    files.add(Sources.open(this, u));
                } catch (IOException | RuntimeException e) {
                    app.toast("Cannot read a file: " + e.getMessage());
                }
            }
            runOnUiThread(() -> {
                if (!files.isEmpty()) {
                    app.toast("Sending to " + targetName + "…");
                    app.sendFiles(targetHex, targetName, files);
                }
                finish();
            });
        });
    }
}
