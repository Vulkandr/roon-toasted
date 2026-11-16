package com.vulkan.toasted.companion;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

/**
 * A foreground service with a quiet notification: it keeps Android from putting the companion to sleep
 * (battery saving held back its word with Toasted, so ARC's controls in Toasted answered late or not
 * at all while the app's screen was closed). The work itself runs in Companion; this only keeps the
 * app awake while it does it.
 */
public class Link extends Service {
    static final String CHANNEL = "link";

    static void start(Context context) {
        try {
            Intent i = new Intent(context, Link.class);
            if (Build.VERSION.SDK_INT >= 26) context.startForegroundService(i);
            else context.startService(i);
        } catch (Exception e) {
            // (Android doesn't let a background app start one at every moment; the screen or a restart will)
            Log.w(Companion.TAG, "couldn't start the foreground service: " + e);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (nm.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel ch = new NotificationChannel(CHANNEL, "Connection to Toasted", NotificationManager.IMPORTANCE_MIN);
            ch.setDescription("Keeps the companion awake so ARC answers Toasted at once.");
            ch.setShowBadge(false);
            nm.createNotificationChannel(ch);
        }
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, Main.class), PendingIntent.FLAG_IMMUTABLE);
        Notification n = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(getApplicationInfo().icon)
                .setContentTitle("Toasted Companion")
                .setContentText("Keeping ARC in touch with Toasted")
                .setContentIntent(open)
                .setOngoing(true)
                .build();
        if (Build.VERSION.SDK_INT >= 34) startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        else startForeground(1, n);
        App.companion.start();
        return START_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
