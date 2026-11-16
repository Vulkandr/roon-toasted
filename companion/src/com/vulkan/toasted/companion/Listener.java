package com.vulkan.toasted.companion;

import android.service.notification.NotificationListenerService;

/**
 * Notification access is what lets the app see ARC's media session, and Android keeps a
 * notification listener running in the background: the companion's work runs from here.
 */
public class Listener extends NotificationListenerService {
    @Override
    public void onListenerConnected() {
        App.companion.start();
        Link.start(this);
    }

    @Override
    public void onListenerDisconnected() {
        App.companion.stop();
    }
}
