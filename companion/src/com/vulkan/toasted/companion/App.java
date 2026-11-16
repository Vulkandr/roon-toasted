package com.vulkan.toasted.companion;

import android.app.Application;

public class App extends Application {
    static Companion companion;

    @Override
    public void onCreate() {
        super.onCreate();
        companion = new Companion(this);
    }
}
