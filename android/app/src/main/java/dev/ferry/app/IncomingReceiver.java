package dev.ferry.app;

import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** Accept / Decline buttons on the "wants to send you" notification. */
public final class IncomingReceiver extends BroadcastReceiver {
    static PendingIntent intent(Context c, String action, String id) {
        Intent i = new Intent(c, IncomingReceiver.class).setAction("dev.ferry." + action).putExtra("id", id);
        return PendingIntent.getBroadcast(c, (action + id).hashCode(), i,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    @Override
    public void onReceive(Context c, Intent intent) {
        String id = intent.getStringExtra("id");
        if (id == null) return;
        FerryApp app = FerryApp.get(c);
        if ("dev.ferry.accept".equals(intent.getAction())) app.accept(id);
        else if ("dev.ferry.decline".equals(intent.getAction())) app.decline(id);
    }
}
