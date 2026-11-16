package com.vulkan.toasted.companion;

import android.app.Activity;
import android.content.Intent;
import android.content.res.Configuration;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;
import android.text.InputType;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/** The one screen: the Living Queue switch, how things stand, notification access and pairing. */
public class Main extends Activity {
    static final int ACCENT = Color.parseColor("#7B5FC7");

    final Handler main = new Handler(Looper.getMainLooper());
    boolean dark;
    int text, muted, card, line;

    Button big;
    TextView bigNote, toasted, arcLine, home, access, pairNote, handoffLine, aaLine, batteryLine;
    Button battery;
    Button grant;
    EditText host, code;
    LinearLayout pairBox;

    Companion c() {
        return App.companion;
    }

    int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        dark = (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES;
        text = dark ? Color.parseColor("#ECECF1") : Color.parseColor("#1C1C1F");
        muted = dark ? Color.parseColor("#9A9AA6") : Color.parseColor("#6B6B76");
        card = dark ? Color.parseColor("#232328") : Color.WHITE;
        line = dark ? Color.parseColor("#36363D") : Color.parseColor("#E2E2E8");
        int bg = dark ? Color.parseColor("#17171A") : Color.parseColor("#F4F4F6");

        LinearLayout col = new LinearLayout(this);
        col.setOrientation(LinearLayout.VERTICAL);
        col.setPadding(dp(20), dp(28), dp(20), dp(24));
        col.setBackgroundColor(bg);

        TextView title = label("Toasted Companion", 22, text);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        col.addView(title);
        col.addView(space(16));

        big = new Button(this);
        big.setAllCaps(false);
        big.setTextSize(18);
        big.setMinHeight(dp(72));
        big.setOnClickListener(v -> {
            boolean on = !"hide".equals(c().mode);
            c().pendingSwitch = on;
            c().pressedAt = android.os.SystemClock.elapsedRealtime();
            c().mode = on ? "hide" : "off";
            c().lastSent = 0;
            show();
        });
        col.addView(big, new LinearLayout.LayoutParams(-1, -2));
        bigNote = label("", 13, muted);
        bigNote.setPadding(dp(4), dp(6), dp(4), dp(12));
        col.addView(bigNote);

        LinearLayout status = box(col);
        toasted = row(status, "Toasted");
        arcLine = row(status, "ARC");
        home = row(status, "Playing at home");
        aaLine = row(status, "ARC's car service");
        batteryLine = row(status, "Battery saving");
        handoffLine = row(status, "Last handoff");

        col.addView(space(12));
        LinearLayout perm = box(col);
        access = row(perm, "Notification access");
        grant = new Button(this);
        grant.setAllCaps(false);
        grant.setText("Grant notification access");
        grant.setOnClickListener(v -> startActivity(new Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)));
        perm.addView(grant);
        TextView why = label("ARC's playback is read from its media controls, which Android only shares with apps that have notification access. Nothing else is read.", 12, muted);
        why.setPadding(0, dp(6), 0, 0);
        perm.addView(why);
        battery = new Button(this);
        battery.setAllCaps(false);
        battery.setText("Let it run in the background");
        battery.setOnClickListener(v -> {
            try {
                startActivity(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, android.net.Uri.parse("package:" + getPackageName())));
            } catch (Exception e) {
                startActivity(new Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            }
        });
        perm.addView(battery);
        TextView bwhy = label("Battery saving can put the companion to sleep, so ARC answers Toasted late or not at all.", 12, muted);
        bwhy.setPadding(0, dp(6), 0, 0);
        perm.addView(bwhy);

        col.addView(space(12));
        pairBox = box(col);
        TextView pt = label("Pair with Toasted", 15, text);
        pt.setTypeface(Typeface.DEFAULT_BOLD);
        pairBox.addView(pt);
        TextView ph = label("In Toasted: Settings, Living Queue. The code is under \"Roon ARC, through the companion app\".", 12, muted);
        ph.setPadding(0, dp(4), 0, dp(8));
        pairBox.addView(ph);
        host = field("Toasted's address, like 192.168.0.20", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        pairBox.addView(host);
        Button find = new Button(this);
        find.setAllCaps(false);
        find.setText("Find Toasted on this Wi-Fi");
        find.setOnClickListener(v -> {
            pairNote.setText("Looking...");
            c().find(err -> {
                pairNote.setText(err == null ? "Found " + c().pcName() + " at " + c().host() + "." : err);
                if (err == null) host.setText(c().host());
            });
        });
        pairBox.addView(find);
        code = field("Six-digit code", InputType.TYPE_CLASS_NUMBER);
        pairBox.addView(code);
        Button pair = new Button(this);
        pair.setAllCaps(false);
        pair.setText("Pair");
        pair.setOnClickListener(v -> {
            String h = host.getText().toString().trim();
            String k = code.getText().toString().trim();
            if (h.isEmpty() || k.replace(" ", "").length() != 6) {
                pairNote.setText("Enter Toasted's address (or find it) and the six-digit code.");
                return;
            }
            pairNote.setText("Pairing...");
            c().pair(h, k, err -> {
                pairNote.setText(err == null ? "Paired with " + c().pcName() + "." : err);
                if (err == null) code.setText("");
                show();
            });
        });
        pairBox.addView(pair);
        pairNote = label("", 13, muted);
        pairNote.setPadding(0, dp(6), 0, 0);
        pairBox.addView(pairNote);

        ScrollView scroll = new ScrollView(this);
        scroll.setBackgroundColor(bg);
        scroll.addView(col);
        setContentView(scroll);
        host.setText(c().host());
    }

    TextView label(String s, int size, int color) {
        TextView t = new TextView(this);
        t.setText(s);
        t.setTextSize(size);
        t.setTextColor(color);
        return t;
    }

    View space(int h) {
        View v = new View(this);
        v.setLayoutParams(new LinearLayout.LayoutParams(1, dp(h)));
        return v;
    }

    LinearLayout box(LinearLayout parent) {
        LinearLayout b = new LinearLayout(this);
        b.setOrientation(LinearLayout.VERTICAL);
        b.setPadding(dp(16), dp(12), dp(16), dp(12));
        GradientDrawable d = new GradientDrawable();
        d.setColor(card);
        d.setCornerRadius(dp(14));
        d.setStroke(1, line);
        b.setBackground(d);
        parent.addView(b, new LinearLayout.LayoutParams(-1, -2));
        return b;
    }

    TextView row(LinearLayout parent, String name) {
        LinearLayout r = new LinearLayout(this);
        r.setOrientation(LinearLayout.HORIZONTAL);
        r.setPadding(0, dp(5), 0, dp(5));
        TextView left = label(name, 14, muted);
        TextView right = label("", 14, text);
        right.setGravity(Gravity.END);
        r.addView(left, new LinearLayout.LayoutParams(0, -2, 1));
        r.addView(right, new LinearLayout.LayoutParams(0, -2, 1.6f));
        parent.addView(r);
        return right;
    }

    EditText field(String hint, int type) {
        EditText e = new EditText(this);
        e.setHint(hint);
        e.setInputType(type);
        e.setTextColor(text);
        e.setHintTextColor(muted);
        e.setSingleLine(true);
        return e;
    }

    void show() {
        Companion c = c();
        boolean listening = c.listening();
        boolean paired = c.paired();
        boolean on = "hide".equals(c.mode);
        big.setText(on ? "Living Queue: on" : "Living Queue: off");
        GradientDrawable d = new GradientDrawable();
        d.setCornerRadius(dp(16));
        d.setColor(on ? ACCENT : card);
        d.setStroke(dp(1), on ? ACCENT : line);
        big.setBackground(d);
        big.setTextColor(on ? Color.WHITE : text);
        big.setEnabled(paired && c.connected);
        big.setAlpha(paired && c.connected ? 1f : 0.5f);
        if (!paired) bigNote.setText("Pair with Toasted below to take part.");
        else if (!c.connected) bigNote.setText("Can be changed once Toasted can be reached.");
        else if (on && !c.livingQueueOnInToasted) bigNote.setText("ARC is set to take part, but the Living Queue is switched off in Toasted.");
        else if (on) bigNote.setText("ARC takes part as Hide Current. Tap the Living Queue playlist in ARC to carry on where you were.");
        else bigNote.setText("ARC takes no part in the Living Queue.");
        toasted.setText(paired ? c.toastedLine : "Not paired yet");
        toasted.setTextColor(c.connected ? Color.parseColor("#3FB950") : text);
        arcLine.setText(c.arcLine());
        home.setText(c.connected && !c.homeLine.isEmpty() ? c.homeLine : "Nothing");
        handoffLine.setText(c.handoff.line.isEmpty() ? "None yet" : c.handoff.line);
        aaLine.setText(c.handoff.link);
        android.os.PowerManager pm = (android.os.PowerManager) getSystemService(POWER_SERVICE);
        boolean free = pm.isIgnoringBatteryOptimizations(getPackageName());
        batteryLine.setText(free ? "Off for this app" : "On (may delay controls)");
        batteryLine.setTextColor(free ? Color.parseColor("#3FB950") : Color.parseColor("#E5484D"));
        battery.setVisibility(free ? View.GONE : View.VISIBLE);
        access.setText(listening ? "Granted" : "Not granted");
        access.setTextColor(listening ? Color.parseColor("#3FB950") : Color.parseColor("#E5484D"));
        grant.setVisibility(listening ? View.GONE : View.VISIBLE);
    }

    final Runnable refresh = new Runnable() {
        @Override
        public void run() {
            show();
            main.postDelayed(this, 1000);
        }
    };

    @Override
    protected void onResume() {
        super.onResume();
        c().onChange = this::show;
        c().watched = true;
        if (android.os.Build.VERSION.SDK_INT >= 33 && checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{android.Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
        Link.start(this);
        if (c().listening()) {
            c().start();
            c().main.post(() -> c().handoff.keepLinked());
        }
        main.post(refresh);
    }

    @Override
    protected void onPause() {
        c().onChange = null;
        c().watched = false;
        main.removeCallbacks(refresh);
        super.onPause();
    }
}
