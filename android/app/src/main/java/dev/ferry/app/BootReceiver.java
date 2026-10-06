package dev.ferry.app;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** Starts Ferry after reboot / app update so the computer can reach the phone. */
public final class BootReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context c, Intent intent) {
        String a = intent.getAction();
        if (Intent.ACTION_BOOT_COMPLETED.equals(a) || Intent.ACTION_MY_PACKAGE_REPLACED.equals(a)) {
            if (!FerryApp.get(c).store.peers().isEmpty()) FerryService.start(c);
        }
    }
}
