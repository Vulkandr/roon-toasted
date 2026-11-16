package com.vulkan.toasted.companion;

import android.content.ComponentName;
import android.content.Context;
import android.content.SharedPreferences;
import android.media.MediaMetadata;
import android.media.session.MediaController;
import android.media.session.MediaSessionManager;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Looper;
import android.os.SystemClock;
import android.provider.Settings;
import android.util.Log;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.HttpURLConnection;
import java.net.InetAddress;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.List;

/**
 * Watches Roon ARC's media session and tells Toasted what ARC is doing (companion.rs on the PC):
 * every second while ARC plays, every 15 seconds otherwise, and at once when ARC changes.
 */
public class Companion {
    static final String TAG = "ToastedCompanion";
    static final String ARC = "com.roon.onthego";
    static final int PORT = 58422;

    final Context app;
    final SharedPreferences prefs;
    final Handler main = new Handler(Looper.getMainLooper());
    final Handler net;

    MediaSessionManager sessions;
    MediaController arc;
    boolean running;

    // What the screen shows
    volatile String toastedLine = "Not paired yet";
    volatile boolean connected;
    volatile String homeLine = "";
    volatile String mode = "off";
    volatile boolean livingQueueOnInToasted;
    /** The phone's switch, pressed and not sent yet. */
    volatile Boolean pendingSwitch;
    /** When the switch was last pressed: an answer to a report sent before that is out of date. */
    volatile long pressedAt;
    Runnable onChange;
    /** The screen is open: Toasted is asked every second, so a change made there shows at once. */
    volatile boolean watched;

    volatile long lastSent;
    final Handoff handoff;
    /** The leading zone at home as Toasted last told it, and when. */
    volatile JSONObject leader;
    volatile long leaderAt;
    /** The silent track was seen and a handoff run for it: not again until ARC is off it. */
    boolean handedOff;
    String lastSentKey = "";

    Companion(Context app) {
        this.app = app;
        prefs = app.getSharedPreferences("companion", Context.MODE_PRIVATE);
        HandlerThread t = new HandlerThread("toasted-net");
        t.start();
        net = new Handler(t.getLooper());
        handoff = new Handoff(this);
        waitLoop();
    }

    boolean listening() {
        String on = Settings.Secure.getString(app.getContentResolver(), "enabled_notification_listeners");
        return on != null && on.contains(app.getPackageName());
    }

    boolean paired() {
        return !prefs.getString("token", "").isEmpty();
    }

    String host() {
        return prefs.getString("host", "");
    }

    String pcName() {
        return prefs.getString("pc", "");
    }

    void changed() {
        Runnable r = onChange;
        if (r != null) main.post(r);
    }

    // ===== ARC's session =====

    final MediaSessionManager.OnActiveSessionsChangedListener sessionsChanged = this::pickArc;

    final MediaController.Callback arcChanged = new MediaController.Callback() {
        @Override
        public void onPlaybackStateChanged(PlaybackState state) {
            sendSoon();
        }

        @Override
        public void onMetadataChanged(MediaMetadata metadata) {
            sendSoon();
        }

        @Override
        public void onSessionDestroyed() {
            pickArc(null);
        }
    };

    void start() {
        main.post(() -> {
            if (running) return;
            running = true;
            try {
                sessions = (MediaSessionManager) app.getSystemService(Context.MEDIA_SESSION_SERVICE);
                ComponentName me = new ComponentName(app, Listener.class);
                sessions.addOnActiveSessionsChangedListener(sessionsChanged, me, main);
                pickArc(sessions.getActiveSessions(me));
                handoff.keepLinked();
                // (and kept: ARC closing drops the connection)
                main.postDelayed(new Runnable() {
                    @Override
                    public void run() {
                        if (!running) return;
                        handoff.keepLinked();
                        main.postDelayed(this, 30000);
                    }
                }, 30000);
            } catch (SecurityException e) {
                Log.w(TAG, "no notification access yet: " + e);
            }
            net.post(this::tick);
        });
    }

    void stop() {
        main.post(() -> {
            running = false;
            try {
                if (sessions != null) sessions.removeOnActiveSessionsChangedListener(sessionsChanged);
            } catch (Exception ignored) {
            }
            if (arc != null) arc.unregisterCallback(arcChanged);
            arc = null;
        });
    }

    /** ARC can have more than one session; the one playing, else one with a song, else any. */
    void pickArc(List<MediaController> list) {
        if (list == null && sessions != null) {
            try {
                list = sessions.getActiveSessions(new ComponentName(app, Listener.class));
            } catch (SecurityException e) {
                return;
            }
        }
        MediaController best = null;
        if (list != null) {
            for (MediaController c : list) {
                if (!ARC.equals(c.getPackageName())) continue;
                PlaybackState s = c.getPlaybackState();
                boolean playing = s != null && s.getState() == PlaybackState.STATE_PLAYING;
                boolean song = c.getMetadata() != null;
                if (best == null || playing || (song && best.getMetadata() == null)) best = c;
                if (playing) break;
            }
        }
        if (arc != null && (best == null || !best.getSessionToken().equals(arc.getSessionToken()))) arc.unregisterCallback(arcChanged);
        if (best != null && (arc == null || !best.getSessionToken().equals(arc.getSessionToken()))) best.registerCallback(arcChanged, main);
        arc = best;
        sendSoon();
    }

    /** ARC as Toasted wants it, or null when ARC has no session. */
    JSONObject arcState() {
        MediaController c = arc;
        if (c == null) return null;
        try {
            JSONObject o = new JSONObject();
            PlaybackState s = c.getPlaybackState();
            int st = s == null ? PlaybackState.STATE_NONE : s.getState();
            String state;
            switch (st) {
                case PlaybackState.STATE_PLAYING: state = "playing"; break;
                case PlaybackState.STATE_PAUSED: state = "paused"; break;
                case PlaybackState.STATE_BUFFERING:
                case PlaybackState.STATE_CONNECTING:
                case PlaybackState.STATE_SKIPPING_TO_NEXT:
                case PlaybackState.STATE_SKIPPING_TO_PREVIOUS: state = "buffering"; break;
                case PlaybackState.STATE_NONE: state = "none"; break;
                default: state = "stopped";
            }
            long pos = 0;
            if (s != null) {
                pos = s.getPosition();
                if (st == PlaybackState.STATE_PLAYING && s.getLastPositionUpdateTime() > 0) {
                    pos += (long) ((SystemClock.elapsedRealtime() - s.getLastPositionUpdateTime()) * s.getPlaybackSpeed());
                }
            }
            MediaMetadata m = c.getMetadata();
            o.put("state", state);
            o.put("title", m == null ? "" : nz(m.getString(MediaMetadata.METADATA_KEY_TITLE)));
            o.put("artist", m == null ? "" : nz(m.getString(MediaMetadata.METADATA_KEY_ARTIST)));
            o.put("album", m == null ? "" : nz(m.getString(MediaMetadata.METADATA_KEY_ALBUM)));
            o.put("positionMs", Math.max(0, pos));
            o.put("durationMs", m == null ? 0 : m.getLong(MediaMetadata.METADATA_KEY_DURATION));
            return o;
        } catch (Exception e) {
            return null;
        }
    }

    static String nz(String s) {
        return s == null ? "" : s;
    }

    String arcLine() {
        if (!listening()) return "Needs notification access";
        JSONObject a = arcState();
        if (a == null) return "Not running";
        String state = a.optString("state");
        String title = a.optString("title");
        if (title.isEmpty() || state.equals("none") || state.equals("stopped")) return "Not playing";
        String word = state.equals("playing") ? "Playing" : state.equals("paused") ? "Paused" : "Loading";
        return word + ", " + title + " at " + clock(a.optLong("positionMs"));
    }

    static String clock(long ms) {
        long s = Math.max(0, Math.round(ms / 1000.0));
        return (s / 60) + ":" + String.format("%02d", s % 60);
    }

    // ===== Telling Toasted =====

    void sendSoon() {
        changed();
        lastSent = 0;
        main.post(this::maybeHandoff);
    }

    /** ARC is on the silent track and the switch is on: hand the queue over (once per time). */
    void maybeHandoff() {
        MediaController c0 = arc;
        MediaMetadata m = c0 == null ? null : c0.getMetadata();
        PlaybackState s = c0 == null ? null : c0.getPlaybackState();
        boolean onSilent = Handoff.isSilent(m) && s != null && (s.getState() == PlaybackState.STATE_PLAYING || s.getState() == PlaybackState.STATE_BUFFERING);
        if (!Handoff.isSilent(m)) handedOff = false;
        if (!onSilent || handedOff || !"hide".equals(mode) && paired()) return;
        handedOff = true;
        // (a fresh word from Toasted first, when it can be reached: what home is on, to the second)
        net.post(() -> {
            if (paired()) report(arcState());
            JSONObject l = connected ? leader : null;
            long at = leaderAt;
            main.post(() -> handoff.start(l, at));
        });
    }

    /** Runs on the network thread for as long as the listener runs. */
    void tick() {
        if (!running) return;
        try {
            JSONObject a = arcState();
            boolean playing = a != null && "playing".equals(a.optString("state"));
            String key = a == null ? "" : a.optString("state") + "|" + a.optString("title");
            long gap = playing || watched ? 1000 : 15000;
            long now = SystemClock.elapsedRealtime();
            if (paired() && (now - lastSent >= gap || !key.equals(lastSentKey) || pendingSwitch != null)) {
                lastSent = now;
                lastSentKey = key;
                report(a);
            }
        } catch (Exception e) {
            Log.w(TAG, "tick: " + e);
        }
        net.postDelayed(this::tick, 250);
    }

    void report(JSONObject a) {
        long started = SystemClock.elapsedRealtime();
        try {
            JSONObject body = new JSONObject();
            body.put("arc", a == null ? JSONObject.NULL : a);
            Boolean sw = pendingSwitch;
            if (sw != null) body.put("livingQueue", sw.booleanValue());
            JSONObject reply = post("/companion/report", body, prefs.getString("token", ""));
            if (sw != null && sw.equals(pendingSwitch)) pendingSwitch = null;
            connected = true;
            if (pressedAt < started) mode = reply.optString("livingQueue", "off");
            livingQueueOnInToasted = reply.optBoolean("on");
            leader = reply.optJSONObject("leader");
            leaderAt = SystemClock.elapsedRealtime();
            JSONObject home = reply.optJSONObject("home");
            homeLine = home == null ? "" : home.optString("title") + " on " + home.optString("zone") + ", " + clock(home.optLong("seconds") * 1000);
            toastedLine = "Connected, " + pcName();
            org.json.JSONArray todo = reply.optJSONArray("commands");
            for (int i = 0; todo != null && i < todo.length(); i++) obey(todo.optString(i));
        } catch (Http e) {
            connected = false;
            toastedLine = e.code == 401 ? "Not paired any more. Pair again below." : "Can't reach " + pcName();
            if (e.code == 401) prefs.edit().remove("token").apply();
        } catch (Exception e) {
            connected = false;
            toastedLine = "Can't reach " + pcName() + " (" + host() + ")";
        }
        changed();
    }

    /**
     * Something Toasted wants done to ARC: "play", "pause", "playpause", "next", "previous", "seek:<ms>"
     * (ARC is a zone in Toasted's player, and a zone at home carrying on from ARC pauses it).
     */
    void obey(String what) {
        main.post(() -> {
            // (through ARC's car service when it's connected: skipping and seeking through ARC's plain media
            // controls stopped ARC answering at all; the car service took them fine in the probe's tests)
            MediaController plain = arc;
            MediaController car = handoff.aa;
            MediaController c = car != null ? car : plain;
            if (c == null) return;
            Log.i(TAG, "Toasted asked: " + what + " (through " + (c == car ? "the car service" : "the plain controls") + ")");
            MediaController.TransportControls go = c.getTransportControls();
            PlaybackState s = c.getPlaybackState();
            boolean playing = s != null && s.getState() == PlaybackState.STATE_PLAYING;
            long actions = s == null ? 0 : s.getActions();
            switch (what) {
                case "pause":
                    go.pause();
                    break;
                case "play":
                    play(c, go, actions);
                    break;
                case "playpause":
                    if (playing) go.pause();
                    else play(c, go, actions);
                    break;
                case "next":
                    go.skipToNext();
                    break;
                case "previous":
                    go.skipToPrevious();
                    break;
                default:
                    if (what.startsWith("seek:")) {
                        try {
                            go.seekTo(Long.parseLong(what.substring(5)));
                        } catch (NumberFormatException ignored) {
                        }
                    }
            }
            sendSoon();
        });
    }

    /** ARC's plain session doesn't list "play" among its actions, only play/pause: a media key then. */
    static void play(MediaController c, MediaController.TransportControls go, long actions) {
        if ((actions & PlaybackState.ACTION_PLAY) != 0) {
            go.play();
        } else {
            c.dispatchMediaButtonEvent(new android.view.KeyEvent(android.view.KeyEvent.ACTION_DOWN, android.view.KeyEvent.KEYCODE_MEDIA_PLAY));
            c.dispatchMediaButtonEvent(new android.view.KeyEvent(android.view.KeyEvent.ACTION_UP, android.view.KeyEvent.KEYCODE_MEDIA_PLAY));
        }
    }

    /** A request Toasted holds open until it has something for ARC: commands arrive at once. */
    void waitLoop() {
        Thread t = new Thread(() -> {
            while (true) {
                try {
                    if (!running || !paired()) {
                        Thread.sleep(2000);
                        continue;
                    }
                    URL url = new URL("http://" + host() + ":" + PORT + "/companion/wait");
                    HttpURLConnection h = (HttpURLConnection) url.openConnection();
                    h.setConnectTimeout(3000);
                    h.setReadTimeout(30000);
                    h.setRequestProperty("Authorization", "Bearer " + prefs.getString("token", ""));
                    int code = h.getResponseCode();
                    String text = code < 400 ? read(h.getInputStream()) : "";
                    h.disconnect();
                    if (code >= 400) {
                        Thread.sleep(5000);
                        continue;
                    }
                    org.json.JSONArray todo = new JSONObject(text).optJSONArray("commands");
                    for (int i = 0; todo != null && i < todo.length(); i++) obey(todo.optString(i));
                } catch (Exception e) {
                    try {
                        Thread.sleep(5000);
                    } catch (InterruptedException ignored) {
                        return;
                    }
                }
            }
        }, "toasted-wait");
        t.setDaemon(true);
        t.start();
    }

    static class Http extends Exception {
        final int code;

        Http(int code, String message) {
            super(message);
            this.code = code;
        }
    }

    JSONObject post(String path, JSONObject body, String token) throws Exception {
        return request(host(), "POST", path, body, token);
    }

    static JSONObject request(String host, String method, String path, JSONObject body, String token) throws Exception {
        URL url = new URL("http://" + host + ":" + PORT + path);
        HttpURLConnection c = (HttpURLConnection) url.openConnection();
        c.setConnectTimeout(3000);
        c.setReadTimeout(4000);
        c.setRequestMethod(method);
        if (token != null && !token.isEmpty()) c.setRequestProperty("Authorization", "Bearer " + token);
        if (body != null) {
            byte[] bytes = body.toString().getBytes(StandardCharsets.UTF_8);
            c.setDoOutput(true);
            c.setRequestProperty("Content-Type", "application/json");
            c.setFixedLengthStreamingMode(bytes.length);
            try (OutputStream out = c.getOutputStream()) {
                out.write(bytes);
            }
        }
        int code = c.getResponseCode();
        InputStream in = code < 400 ? c.getInputStream() : c.getErrorStream();
        String text = in == null ? "" : read(in);
        c.disconnect();
        JSONObject reply = text.isEmpty() ? new JSONObject() : new JSONObject(text);
        if (code >= 400) throw new Http(code, reply.optString("error", "HTTP " + code));
        return reply;
    }

    static String read(InputStream in) throws Exception {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buf = new byte[4096];
        int n;
        while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
        in.close();
        return out.toString("UTF-8");
    }

    // ===== Finding and pairing =====

    interface Done {
        void done(String error);
    }

    /** Asks the Wi-Fi for Toasted; the first to answer is kept. */
    void find(Done done) {
        net.post(() -> {
            String error = null;
            try (DatagramSocket socket = new DatagramSocket()) {
                socket.setBroadcast(true);
                socket.setSoTimeout(1500);
                byte[] ask = "ROON-TOASTED?".getBytes(StandardCharsets.UTF_8);
                socket.send(new DatagramPacket(ask, ask.length, InetAddress.getByName("255.255.255.255"), PORT));
                byte[] buf = new byte[1024];
                DatagramPacket got = new DatagramPacket(buf, buf.length);
                socket.receive(got);
                JSONObject hello = new JSONObject(new String(got.getData(), 0, got.getLength(), StandardCharsets.UTF_8));
                prefs.edit().putString("host", got.getAddress().getHostAddress()).putString("pc", hello.optString("name")).apply();
            } catch (java.net.SocketTimeoutException e) {
                error = "Toasted didn't answer on this Wi-Fi. Check it's running, and that Windows Firewall allows it on private networks. Or type its address.";
            } catch (Exception e) {
                error = "Couldn't look for Toasted: " + e.getMessage();
            }
            final String err = error;
            main.post(() -> done.done(err));
        });
    }

    String deviceName() {
        String name = Settings.Global.getString(app.getContentResolver(), "device_name");
        return name == null || name.isEmpty() ? Build.MODEL : name;
    }

    void pair(String host, String code, Done done) {
        net.post(() -> {
            String error = null;
            try {
                JSONObject hello = request(host, "GET", "/companion/hello", null, null);
                JSONObject body = new JSONObject();
                body.put("code", code.replace(" ", ""));
                body.put("device", deviceName());
                JSONObject reply = request(host, "POST", "/companion/pair", body, null);
                prefs.edit().putString("host", host).putString("token", reply.getString("token"))
                        .putString("pc", reply.optString("name", hello.optString("name"))).apply();
                lastSent = 0;
            } catch (Http e) {
                error = e.getMessage();
            } catch (Exception e) {
                error = "Can't reach Toasted at " + host + ". Check the address, that Toasted is running, and that Windows Firewall allows it.";
            }
            final String err = error;
            main.post(() -> done.done(err));
        });
    }
}
