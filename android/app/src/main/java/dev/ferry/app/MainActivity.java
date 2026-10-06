package dev.ferry.app;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.app.StatusBarManager;
import android.content.ComponentName;
import android.content.Intent;
import android.graphics.drawable.Icon;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.Settings;
import android.text.InputType;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

import dev.ferry.R;
import dev.ferry.core.Peer;

/** The only real screen: status, paired devices and a few buttons. */
public final class MainActivity extends Activity implements FerryApp.Listener {
    private static final int REQ_PICK = 10;

    private FerryApp app;
    private LinearLayout peersBox;
    private TextView info;
    private Button batteryBtn;
    private AlertDialog pairDialog;

    private int dp(int v) {
        return (int) TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, getResources().getDisplayMetrics());
    }

    private TextView text(String s, int sp, boolean bold) {
        TextView t = new TextView(this);
        t.setText(s);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, sp);
        if (bold) t.setTypeface(Typeface.DEFAULT_BOLD);
        t.setPadding(0, dp(6), 0, dp(6));
        return t;
    }

    private Button button(String label, View.OnClickListener l) {
        Button b = new Button(this);
        b.setText(label);
        b.setAllCaps(false);
        b.setOnClickListener(l);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        lp.topMargin = dp(6);
        b.setLayoutParams(lp);
        return b;
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        app = FerryApp.get(this);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(dp(20), dp(12), dp(20), dp(20));

        info = text("", 15, false);
        info.setOnClickListener(v -> renameDialog());
        root.addView(info);

        root.addView(text("Paired computers", 18, true));
        peersBox = new LinearLayout(this);
        peersBox.setOrientation(LinearLayout.VERTICAL);
        root.addView(peersBox);

        root.addView(button("Pair with computer (enter code)", v -> enterCodeDialog()));
        root.addView(button("Pair: show a code on this phone", v -> showCodeDialog()));
        root.addView(button("Send clipboard", v ->
                startActivity(new Intent(this, ClipSendActivity.class))));
        root.addView(button("Send files…", v -> {
            Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT)
                    .addCategory(Intent.CATEGORY_OPENABLE)
                    .setType("*/*")
                    .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            startActivityForResult(i, REQ_PICK);
        }));
        batteryBtn = button("Allow Ferry to run in the background", v -> {
            Intent i = new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    Uri.parse("package:" + getPackageName()));
            try {
                startActivity(i);
            } catch (RuntimeException e) {
                startActivity(new Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            }
        });
        root.addView(batteryBtn);
        if (Build.VERSION.SDK_INT >= 33) {
            root.addView(button("Add \"Clipboard → PC\" to Quick Settings", v -> requestTile()));
        }

        TextView help = text("Tips:\n• Share any file or text from another app → \"Send to desktop\".\n"
                + "• Add the \"Clipboard → PC\" tile to Quick Settings for one-tap clipboard sending.\n"
                + "• Text copied on the computer appears on this phone automatically.\n"
                + "• Received files are saved in Download/Ferry.\n"
                + "• Tap the device name above to rename this phone.", 13, false);
        help.setAlpha(0.7f);
        help.setPadding(0, dp(18), 0, 0);
        root.addView(help);

        ScrollView sv = new ScrollView(this);
        sv.addView(root);
        setContentView(sv);

        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
        FerryService.start(this);
    }

    @Override
    protected void onResume() {
        super.onResume();
        app.addListener(this);
        refresh();
    }

    @Override
    protected void onPause() {
        app.removeListener(this);
        super.onPause();
    }

    private void refresh() {
        List<String> addrs = FerryApp.localAddresses();
        info.setText("This phone: " + app.store.name() + "  ✎\nAddress: "
                + (addrs.isEmpty() ? "no network" : String.join(", ", addrs)) + "  (port " + app.store.port() + ")");
        peersBox.removeAllViews();
        List<Peer> peers = app.store.peers();
        if (peers.isEmpty()) {
            TextView t = text("None yet. On the computer click the Ferry icon → \"Pair new phone…\", "
                    + "then tap \"Pair with computer\" here.", 14, false);
            t.setAlpha(0.7f);
            peersBox.addView(t);
        }
        for (int i = 0; i < peers.size(); i++) {
            Peer p = peers.get(i);
            TextView t = text((i == 0 ? "★ " : "• ") + p.name + (p.addr != null ? "   " + p.addr : ""), 15, false);
            t.setOnClickListener(v -> new AlertDialog.Builder(this)
                    .setTitle(p.name)
                    .setMessage("Forget this computer? You will need to pair again.")
                    .setPositiveButton("Forget", (d, w) -> {
                        app.store.remove(p.id);
                        refresh();
                    })
                    .setNegativeButton("Cancel", null)
                    .show());
            peersBox.addView(t);
        }
        PowerManager pm = getSystemService(PowerManager.class);
        batteryBtn.setVisibility(pm.isIgnoringBatteryOptimizations(getPackageName()) ? View.GONE : View.VISIBLE);
    }

    @Override
    public void onFerryEvent(String event, String detail) {
        if (event.equals("paired")) {
            if (pairDialog != null) pairDialog.dismiss();
            app.toast("Paired with " + detail);
            FerryService.refresh(this);
        } else if (event.equals("pairclosed")) {
            if (pairDialog != null) pairDialog.dismiss();
            app.toast(detail);
        }
        refresh();
    }

    /** Android 13+: system dialog that adds our tile to Quick Settings with one tap. */
    private void requestTile() {
        if (Build.VERSION.SDK_INT < 33) return;
        StatusBarManager sbm = getSystemService(StatusBarManager.class);
        sbm.requestAddTileService(new ComponentName(this, ClipTileService.class), "Clipboard → PC",
                Icon.createWithResource(this, R.drawable.ic_stat), getMainExecutor(), result -> {
                    if (result == StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ALREADY_ADDED) {
                        app.toast("The tile is already in Quick Settings");
                    } else if (result == StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ADDED) {
                        app.toast("Tile added - swipe down twice to see it");
                    }
                });
    }

    private EditText field(String hint, int type) {
        EditText e = new EditText(this);
        e.setHint(hint);
        e.setInputType(type);
        e.setSingleLine(true);
        return e;
    }

    private void enterCodeDialog() {
        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setPadding(dp(20), dp(8), dp(20), 0);
        EditText addr = field("Computer address, e.g. 192.168.1.20", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        EditText code = field("Code, e.g. 7KQ2M-X9PRT", InputType.TYPE_CLASS_TEXT
                | InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD);
        String last = getSharedPreferences("ferry", MODE_PRIVATE).getString("lastAddr", "");
        addr.setText(last);
        box.addView(addr);
        box.addView(code);
        new AlertDialog.Builder(this)
                .setTitle("Pair with computer")
                .setMessage("On the computer: Ferry icon in the top bar → \"Pair new phone…\" (or run: ferry pair).")
                .setView(box)
                .setPositiveButton("Pair", (d, w) -> {
                    String a = addr.getText().toString().trim();
                    String c = code.getText().toString();
                    getSharedPreferences("ferry", MODE_PRIVATE).edit().putString("lastAddr", a).apply();
                    app.io.execute(() -> {
                        try {
                            Peer p = app.node.pair(a, c);
                            app.toast("Paired with " + p.name);
                            runOnUiThread(() -> {
                                refresh();
                                FerryService.refresh(this);
                            });
                        } catch (IOException | RuntimeException e) {
                            app.toast("Pairing failed: " + e.getMessage());
                        }
                    });
                })
                .setNegativeButton("Cancel", null)
                .show();
    }

    private void showCodeDialog() {
        String code = app.node.startPairing();
        List<String> addrs = FerryApp.localAddresses();
        TextView big = new TextView(this);
        big.setText(code);
        big.setTextSize(TypedValue.COMPLEX_UNIT_SP, 32);
        big.setTypeface(Typeface.MONOSPACE, Typeface.BOLD);
        big.setGravity(Gravity.CENTER);
        big.setTextIsSelectable(true);
        big.setPadding(0, dp(16), 0, dp(16));
        String a = addrs.isEmpty() ? "(no network)" : String.join("  or  ", addrs);
        pairDialog = new AlertDialog.Builder(this)
                .setTitle("Pair a computer")
                .setMessage("On the computer run:\n\nferry pair " + (addrs.isEmpty() ? "<phone-ip>" : addrs.get(0))
                        + " " + code + "\n\nPhone address: " + a + "\nValid for 5 minutes.")
                .setView(big)
                .setNegativeButton("Cancel", (d, w) -> app.node.stopPairing())
                .setOnDismissListener(d -> pairDialog = null)
                .show();
    }

    private void renameDialog() {
        EditText e = field("Device name", InputType.TYPE_CLASS_TEXT);
        e.setText(app.store.name());
        LinearLayout box = new LinearLayout(this);
        box.setPadding(dp(20), dp(8), dp(20), 0);
        box.addView(e, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT));
        new AlertDialog.Builder(this)
                .setTitle("Name of this phone")
                .setView(box)
                .setPositiveButton("Save", (d, w) -> {
                    app.store.setName(e.getText().toString());
                    refresh();
                })
                .setNegativeButton("Cancel", null)
                .show();
    }

    @Override
    protected void onActivityResult(int req, int res, Intent data) {
        super.onActivityResult(req, res, data);
        if (req != REQ_PICK || res != RESULT_OK || data == null) return;
        ArrayList<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) uris.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        if (uris.isEmpty()) return;
        Intent i = new Intent(this, ShareActivity.class)
                .setAction(Intent.ACTION_SEND_MULTIPLE)
                .setType("*/*")
                .putParcelableArrayListExtra(Intent.EXTRA_STREAM, uris)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        startActivity(i);
    }
}
