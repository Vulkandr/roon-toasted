package com.vulkan.toasted.companion;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** After a restart of the phone, the companion starts by itself. */
public class Boot extends BroadcastReceiver {
    @Override
    public void onReceive(Context context, Intent intent) {
        Link.start(context);
    }
}
