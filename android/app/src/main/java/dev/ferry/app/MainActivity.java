package dev.ferry.app;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.app.StatusBarManager;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.graphics.drawable.Icon;
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
import android.widget.Switch;
import android.widget.TextView;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

import dev.ferry.R;
import dev.ferry.core.Node;
import dev.ferry.core.Peer;

/**
 * The only real screen:
 *   Incoming (from unpaired devices, if any) - My devices - Nearby - a few buttons and settings.
 */
public final class MainActivity extends Activity implements FerryApp.Listener {
    private static final int REQ_PICK = 10;

    private FerryApp app;
    private TextView info;
    private LinearLayout incomingBox, mineBox, nearbyBox, queueBox;
    private Button batteryBtn;
    private Switch visibleSwitch;
    private AlertDialog pairDialog;
    /** Device chosen for the file picker that is open right now. */
    private String pickHex, pickName;
    private List<Node.Device> nearby = new ArrayList<>();
    private boolean scanning;

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

    private TextView heading(String s) {
        TextView t = text(s, 18, true);
        t.setPadding(0, dp(18), 0, dp(2));
        return t;
    }

    private TextView hint(String s) {
        TextView t = text(s, 13, false);
        t.setAlpha(0.65f);
        t.setPadding(0, 0, 0, dp(4));
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

    private LinearLayout vbox() {
        LinearLayout l = new LinearLayout(this);
        l.setOrientation(LinearLayout.VERTICAL);
        return l;
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        app = FerryApp.get(this);

        LinearLayout root = vbox();
        root.setPadding(dp(20), dp(8), dp(20), dp(24));

        info = text("", 15, false);
        info.setOnClickListener(v -> renameDialog());
        root.addView(info);

        incomingBox = vbox();
        root.addView(incomingBox);

        root.addView(heading("My devices"));
        root.addView(hint("Paired with a code. Files arrive directly and the clipboard is shared between all of them."));
        mineBox = vbox();
        root.addView(mineBox);
        root.addView(button("Send clipboard to my devices", v ->
                startActivity(new Intent(this, ClipSendActivity.class))));
        root.addView(button("Pair with a computer (enter its code)", v -> enterCodeDialog()));
        root.addView(button("Pair: show a code on this phone", v -> showCodeDialog()));

        root.addView(heading("Nearby"));
        root.addView(hint("Other Ferry devices on this network. What you send them waits until they accept it."));
        nearbyBox = vbox();
        root.addView(nearbyBox);
        root.addView(button("Look again", v -> scan()));

        queueBox = vbox();
        root.addView(queueBox);

        root.addView(heading("Settings"));
        visibleSwitch = new Switch(this);
        visibleSwitch.setText("Visible to nearby devices");
        visibleSwitch.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        visibleSwitch.setPadding(0, dp(8), 0, dp(4));
        visibleSwitch.setOnCheckedChangeListener((b, on) -> {
            if (on == app.store.visible()) return;
            app.store.setVisible(on);
            app.io.execute(() -> app.node.sendPresence(false)); // tell others right away
            FerryService.refresh(this);
        });
        root.addView(visibleSwitch);
        root.addView(hint("When on, unpaired devices can see this phone and send to it. Their files wait in "
                + "Incoming until you accept them, and are deleted after 24 hours otherwise."));

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
        if (app.store.blockedCount() > 0) {
            root.addView(button("Unblock all blocked devices", v -> {
                app.store.unblockAll();
                app.toast("Unblocked");
                v.setVisibility(View.GONE);
            }));
        }

        TextView help = hint("\nTips:\n• Share any file or text from another app → Ferry. Your paired computers also "
                + "appear directly in the share sheet.\n"
                + "• Text copied on a paired computer appears on this phone automatically.\n"
                + "• Received files are saved in Download/Ferry.\n"
                + "• Tap the name at the top to rename this phone.");
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
        app.scheduleExpiry();
        refresh();
        scan();
    }

    @Override
    protected void onPause() {
        app.removeListener(this);
        super.onPause();
    }

    private void scan() {
        if (scanning) return;
        scanning = true;
        app.io.execute(() -> {
            List<Node.Device> devs = app.node.devices(true);
            runOnUiThread(() -> {
                scanning = false;
                nearby = devs;
                refresh();
            });
        });
    }

    private void refresh() {
        List<String> addrs = FerryApp.localAddresses();
        info.setText("This phone: " + app.store.name() + "  ✎\nAddress: "
                + (addrs.isEmpty() ? "no network" : String.join(", ", addrs)) + "  (port " + app.store.port() + ")");
        visibleSwitch.setChecked(app.store.visible());

        // Incoming
        incomingBox.removeAllViews();
        List<Inbox.Item> items = app.inbox.list();
        if (!items.isEmpty()) {
            incomingBox.addView(heading("Incoming (" + items.size() + ")"));
            incomingBox.addView(hint("From devices that are not paired. Deleted after 24 h unless you accept."));
            for (Inbox.Item it : items) {
                incomingBox.addView(text(it.fromName + " sent " + it.summary(), 15, false));
                LinearLayout row = new LinearLayout(this);
                row.setOrientation(LinearLayout.HORIZONTAL);
                Button acc = button("Accept", v -> app.accept(it.id));
                Button dec = button("Decline", v -> app.decline(it.id));
                Button blk = button("Block", v -> new AlertDialog.Builder(this)
                        .setMessage("Decline and never accept anything from " + it.fromName + " again?")
                        .setPositiveButton("Block", (d, w) -> app.block(it.id))
                        .setNegativeButton("Cancel", null)
                        .show());
                for (Button b : new Button[] {acc, dec, blk}) {
                    LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1);
                    lp.rightMargin = dp(4);
                    b.setLayoutParams(lp);
                    row.addView(b);
                }
                incomingBox.addView(row);
            }
        }

        // My devices
        mineBox.removeAllViews();
        List<Peer> peers = app.store.peers();
        if (peers.isEmpty()) {
            mineBox.addView(hint("None yet. On the computer click the Ferry icon → \"Pair a new device…\", "
                    + "then tap \"Pair with a computer\" below."));
        }
        for (int i = 0; i < peers.size(); i++) {
            Peer p = peers.get(i);
            boolean online = false;
            for (Node.Device d : nearby) if (d.paired && d.idHex().equals(p.idHex())) online = d.online;
            TextView t = text((i == 0 ? "★ " : "• ") + p.name + "   " + FerryApp.kindLabel(p.kind)
                    + (online ? " · online" : ""), 16, false);
            final boolean isMain = i == 0;
            t.setOnClickListener(v -> deviceMenu(p, isMain));
            mineBox.addView(t);
        }

        // Nearby
        nearbyBox.removeAllViews();
        int n = 0;
        for (Node.Device d : nearby) {
            if (d.paired) continue;
            n++;
            TextView t = text("• " + d.name + "   " + FerryApp.kindLabel(d.kind), 16, false);
            t.setOnClickListener(v -> new AlertDialog.Builder(this)
                    .setTitle(d.name)
                    .setMessage(d.name + " is not paired. What you send waits there until it is accepted.")
                    .setPositiveButton("Send files…", (dd, w) -> pickFor(d.idHex(), d.name))
                    .setNegativeButton("Cancel", null)
                    .show());
            nearbyBox.addView(t);
        }
        if (n == 0) nearbyBox.addView(hint(scanning ? "Looking…" : "None found."));

        // Queue
        queueBox.removeAllViews();
        List<String> q = app.node.queueDescriptions();
        if (!q.isEmpty()) {
            queueBox.addView(heading("Waiting to send"));
            queueBox.addView(hint("These devices are not reachable right now. Ferry sends as soon as they are back."));
            for (String s : q) queueBox.addView(text("• " + s, 15, false));
            queueBox.addView(button("Cancel all", v -> {
                app.node.cancelQueue();
                refresh();
            }));
        }

        PowerManager pm = getSystemService(PowerManager.class);
        batteryBtn.setVisibility(pm.isIgnoringBatteryOptimizations(getPackageName()) ? View.GONE : View.VISIBLE);
    }

    private void deviceMenu(Peer p, boolean isMain) {
        List<String> opts = new ArrayList<>();
        opts.add("Send files…");
        opts.add("Send clipboard");
        if (!isMain) opts.add("Make main device (★)");
        opts.add("Forget this device");
        new AlertDialog.Builder(this)
                .setTitle(p.name)
                .setItems(opts.toArray(new String[0]), (d, w) -> {
                    String o = opts.get(w);
                    if (o.startsWith("Send files")) {
                        pickFor(p.idHex(), p.name);
                    } else if (o.startsWith("Send clipboard")) {
                        startActivity(new Intent(this, ClipSendActivity.class));
                    } else if (o.startsWith("Make main")) {
                        app.store.makeMain(p.id);
                        app.updateShortcuts();
                        refresh();
                    } else {
                        new AlertDialog.Builder(this)
                                .setMessage("Forget " + p.name + "? You will need to pair again.")
                                .setPositiveButton("Forget", (dd, ww) -> {
                                    app.store.remove(p.id);
                                    app.updateShortcuts();
                                    refresh();
                                })
                                .setNegativeButton("Cancel", null)
                                .show();
                    }
                })
                .show();
    }

    private void pickFor(String hex, String name) {
        pickHex = hex;
        pickName = name;
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT)
                .addCategory(Intent.CATEGORY_OPENABLE)
                .setType("*/*")
                .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        startActivityForResult(i, REQ_PICK);
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
        LinearLayout box = vbox();
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
                .setTitle("Pair with a computer")
                .setMessage("On the computer: Ferry icon → \"Pair a new device…\" (or run: ferry pair).")
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
        if (req != REQ_PICK || res != RESULT_OK || data == null || pickHex == null) return;
        List<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) uris.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        String hex = pickHex, name = pickName;
        app.io.execute(() -> {
            List<Node.Outgoing> files = new ArrayList<>();
            for (Uri u : uris) {
                try {
                    files.add(Sources.open(this, u));
                } catch (IOException | RuntimeException e) {
                    app.toast("Cannot read a file: " + e.getMessage());
                }
            }
            if (!files.isEmpty()) app.sendFiles(hex, name, files);
        });
    }
}
