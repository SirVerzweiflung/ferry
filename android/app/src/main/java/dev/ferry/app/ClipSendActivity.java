package dev.ferry.app;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.os.Bundle;

import dev.ferry.core.Peer;

/**
 * Invisible activity that reads the clipboard and sends it to the default computer.
 * Android 10+ only lets the app that has window focus read the clipboard, so the
 * Quick Settings tile and the notification action route through here.
 */
public final class ClipSendActivity extends Activity {
    private boolean done;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        FerryService.start(this);
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (!hasFocus || done) return;
        done = true;
        FerryApp app = FerryApp.get(this);
        Peer p = app.store.defaultPeer();
        if (p == null) {
            app.toast("Pair Ferry with your computer first");
            finish();
            return;
        }
        ClipboardManager cm = getSystemService(ClipboardManager.class);
        ClipData cd = cm.getPrimaryClip();
        CharSequence text = null;
        if (cd != null && cd.getItemCount() > 0) text = cd.getItemAt(0).coerceToText(this);
        if (text == null || text.length() == 0) {
            app.toast("The clipboard is empty");
        } else {
            app.sendClip(p, text.toString());
        }
        finish();
        overridePendingTransition(0, 0);
    }
}
