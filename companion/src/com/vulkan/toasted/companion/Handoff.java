package com.vulkan.toasted.companion;

import android.content.ComponentName;
import android.media.MediaMetadata;
import android.media.browse.MediaBrowser;
import android.media.session.MediaController;
import android.media.session.PlaybackState;
import android.net.Uri;
import android.os.Handler;
import android.os.SystemClock;
import android.util.Log;

import org.json.JSONObject;

import java.util.List;

/**
 * The handoff to ARC (claude/roon-arc-handoff-recipe.md). The listener taps the Living Queue playlist
 * in ARC; its first row is the silent "Living Queue" track, and playing it makes ARC fetch the playlist
 * again (about 4.4 s later). Then: list the playlist through ARC's Android Auto service until the song
 * playing at home is in it, play that row with a pause right behind it, and once ARC is on the song,
 * seek to the second and play.
 */
class Handoff {
    static final String TAG = Companion.TAG;
    static final String SERVICE = "com.roon.androidauto.roon_android_auto_native.RoonMediaLibraryService";

    final Companion c;
    final Handler main;
    MediaBrowser browser;
    MediaController aa;
    boolean running;
    long began;
    String playlist;
    /** The song to land on, its second, and when that second was true (it moves on while home plays). */
    String wantTitle;
    long wantMs;
    long wantAt;
    boolean wantMoving;
    String awaited;
    Runnable whenAwaited;
    volatile String line = "";
    /** How the standing connection to ARC's Android Auto service is doing, for the screen. */
    volatile String link = "Not connected yet";
    final java.util.ArrayList<Runnable> whenLinked = new java.util.ArrayList<>();
    boolean linking;

    Handoff(Companion c) {
        this.c = c;
        this.main = c.main;
    }

    void say(String s) {
        Log.i(TAG, "handoff +" + (SystemClock.elapsedRealtime() - began) + " ms: " + s);
        line = s;
        c.changed();
    }

    static boolean isSilent(MediaMetadata m) {
        if (m == null) return false;
        String title = m.getString(MediaMetadata.METADATA_KEY_TITLE);
        String artist = m.getString(MediaMetadata.METADATA_KEY_ARTIST);
        return title != null && title.trim().equalsIgnoreCase("Living Queue") && artist != null && artist.toLowerCase().contains("toasted");
    }

    static boolean isTrack(MediaBrowser.MediaItem item) {
        return item.isPlayable() && item.getMediaId() != null && item.getMediaId().startsWith("roon://track/");
    }

    static boolean silentRow(MediaBrowser.MediaItem item) {
        CharSequence t = item.getDescription().getTitle();
        CharSequence s = item.getDescription().getSubtitle();
        return t != null && t.toString().trim().equalsIgnoreCase("Living Queue") && (s == null || s.toString().toLowerCase().contains("toasted"));
    }

    /** ARC has started the silent track: run once. */
    void start(JSONObject leader, long leaderAt) {
        if (running) return;
        running = true;
        began = SystemClock.elapsedRealtime();
        wantTitle = null;
        if (leader != null && !leader.optString("title").isEmpty()) {
            wantTitle = leader.optString("title");
            wantMs = Math.round(leader.optDouble("seconds", 0) * 1000);
            wantAt = leaderAt;
            wantMoving = "playing".equals(leader.optString("state"));
            say("the silent track is on; carrying on from " + leader.optString("zone") + ": " + wantTitle + " at " + Companion.clock(wantMs));
        } else {
            say("the silent track is on; Toasted can't be asked, so the playlist's first song, from its start");
        }
        connect(() -> findPlaylist(() -> watch()));
    }

    /**
     * Keeps a connection to ARC's Android Auto service open (made while the screen is open and when the
     * listener starts), so a handoff doesn't depend on connecting from the background at that moment.
     */
    void keepLinked() {
        if (browser != null && browser.isConnected() && aa != null) return;
        linkNow(null);
    }

    void stop(String why) {
        say(why);
        running = false;
        awaited = null;
        whenAwaited = null;
    }

    void connect(Runnable then) {
        if (browser != null && browser.isConnected() && aa != null) {
            say("ARC's Android Auto service: connected already");
            then.run();
            return;
        }
        say("connecting to ARC's Android Auto service");
        linkNow(then);
    }

    void linkNow(Runnable then) {
        if (then != null) whenLinked.add(then);
        if (linking) return;
        linking = true;
        link = "Connecting...";
        c.changed();
        final long asked = SystemClock.elapsedRealtime();
        final boolean[] answered = {false};
        main.postDelayed(() -> {
            if (!answered[0]) {
                answered[0] = true;
                linking = false;
                try {
                    browser.disconnect();
                } catch (Exception ignored) {
                }
                browser = null;
                aa = null;
                link = "No answer (tried " + new java.text.SimpleDateFormat("HH:mm:ss").format(new java.util.Date()) + ")";
                Log.w(TAG, "ARC's Android Auto service didn't answer within 5 s");
                whenLinked.clear();
                if (running) stop("ARC's Android Auto service didn't answer within 5 s; nothing done");
                c.changed();
            }
        }, 5000);
        browser = new MediaBrowser(c.app, new ComponentName(Companion.ARC, SERVICE), new MediaBrowser.ConnectionCallback() {
            @Override
            public void onConnected() {
                if (answered[0]) return;
                answered[0] = true;
                linking = false;
                link = "Connected (" + (SystemClock.elapsedRealtime() - asked) + " ms)";
                Log.i(TAG, "connected to ARC's Android Auto service in " + (SystemClock.elapsedRealtime() - asked) + " ms, root " + browser.getRoot());
                aa = new MediaController(c.app, browser.getSessionToken());
                aa.registerCallback(new MediaController.Callback() {
                    @Override
                    public void onMetadataChanged(MediaMetadata m) {
                        String t = m == null ? null : m.getString(MediaMetadata.METADATA_KEY_TITLE);
                        if (awaited != null && t != null && awaited.equalsIgnoreCase(t.trim())) {
                            Runnable r = whenAwaited;
                            awaited = null;
                            whenAwaited = null;
                            if (r != null) r.run();
                        }
                    }
                }, main);
                java.util.ArrayList<Runnable> todo = new java.util.ArrayList<>(whenLinked);
                whenLinked.clear();
                for (Runnable r : todo) r.run();
                c.changed();
            }

            @Override
            public void onConnectionFailed() {
                if (answered[0]) return;
                answered[0] = true;
                linking = false;
                browser = null;
                aa = null;
                whenLinked.clear();
                link = "Turned away";
                if (running) stop("ARC's Android Auto service turned the companion away; nothing done");
                c.changed();
            }

            @Override
            public void onConnectionSuspended() {
                browser = null;
                aa = null;
                link = "Dropped (ARC closed?)";
                c.changed();
            }
        }, null);
        browser.connect();
    }

    /** The children of `id`; null on an error, or when nothing comes within 4 s. */
    void children(String id, java.util.function.Consumer<List<MediaBrowser.MediaItem>> then) {
        final boolean[] done = {false};
        final MediaBrowser b = browser;
        if (b == null) {
            then.accept(null);
            return;
        }
        main.postDelayed(() -> {
            if (done[0]) return;
            done[0] = true;
            Log.w(TAG, "no answer listing " + id);
            try {
                b.unsubscribe(id);
            } catch (Exception ignored) {
            }
            then.accept(null);
        }, 4000);
        b.subscribe(id, new MediaBrowser.SubscriptionCallback() {
            @Override
            public void onChildrenLoaded(String parentId, List<MediaBrowser.MediaItem> list) {
                if (done[0]) return;
                done[0] = true;
                b.unsubscribe(parentId);
                then.accept(list);
            }

            @Override
            public void onError(String parentId) {
                if (done[0]) return;
                done[0] = true;
                then.accept(null);
            }
        });
    }

    /**
     * The playlist ARC is playing from: named in the playing row's id ("...?from=playlist:40:1:<guid>&row=...")
     * when the service has it, else found by name among the library's playlists.
     */
    void findPlaylist(Runnable then) {
        MediaMetadata m = aa.getMetadata();
        String id = m == null ? null : m.getString(MediaMetadata.METADATA_KEY_MEDIA_ID);
        Log.i(TAG, "playing row's id: " + id);
        if (id != null && id.contains("from=")) {
            String from = Uri.decode(id.substring(id.indexOf("from=") + 5).split("&")[0]);
            if (from.startsWith("playlist:")) {
                playlist = "roon://playlist/" + from.substring("playlist:".length());
                c.prefs.edit().putString("playlist", playlist).apply();
                say("the playlist, from the playing row: " + playlist);
                then.run();
                return;
            }
        }
        String known = c.prefs.getString("playlist", "");
        if (!known.isEmpty()) {
            playlist = known;
            say("the playlist, as found before: " + playlist);
            then.run();
            return;
        }
        children("roon://library/playlists", list -> {
            if (list != null) {
                for (MediaBrowser.MediaItem item : list) {
                    CharSequence t = item.getDescription().getTitle();
                    if (t != null && t.toString().trim().equalsIgnoreCase("Living Queue") && item.getMediaId() != null) {
                        playlist = item.getMediaId().split("\\?")[0];
                        c.prefs.edit().putString("playlist", playlist).apply();
                        say("the playlist, found by name: " + playlist);
                        then.run();
                        return;
                    }
                }
            }
            stop("couldn't find the Living Queue playlist in ARC; nothing done");
        });
    }

    /** Lists the playlist every 0.7 s until ARC's fresh copy holds the wanted song. */
    void watch() {
        if (!running) return;
        children(playlist, list -> {
            if (!running) return;
            long since = SystemClock.elapsedRealtime() - began;
            MediaBrowser.MediaItem found = null;
            MediaBrowser.MediaItem first = null;
            int tracks = 0;
            if (list != null) {
                for (MediaBrowser.MediaItem item : list) {
                    if (!isTrack(item) || silentRow(item)) continue;
                    tracks++;
                    if (first == null) first = item;
                    CharSequence t = item.getDescription().getTitle();
                    if (wantTitle != null && t != null && wantTitle.equalsIgnoreCase(t.toString().trim())) {
                        found = item;
                        break;
                    }
                }
            }
            if (found != null) {
                land(found, true);
            } else if (wantTitle == null && first != null && since > 6000) {
                // (no word from Toasted: the playlist's first song once ARC's copy has surely been fetched again)
                land(first, false);
            } else if (since > 15000) {
                if (first != null) {
                    say("\"" + wantTitle + "\" never showed in ARC's copy of the playlist; its first song instead");
                    land(first, false);
                } else {
                    stop("the playlist had nothing to play; nothing done");
                }
            } else {
                say("waiting for ARC's fresh copy of the playlist (" + (list == null ? "no answer" : tracks + " songs, \"" + wantTitle + "\" not in it yet") + ")");
                main.postDelayed(this::watch, 700);
            }
        });
    }

    void land(MediaBrowser.MediaItem row, boolean atSecond) {
        final String title = String.valueOf(row.getDescription().getTitle()).trim();
        final MediaController.TransportControls go = aa.getTransportControls();
        final boolean[] landed = {false};
        awaited = title;
        whenAwaited = () -> {
            if (landed[0]) return;
            landed[0] = true;
            long ms = 0;
            if (atSecond) {
                ms = wantMs + (wantMoving ? SystemClock.elapsedRealtime() - wantAt : 0);
                go.seekTo(Math.max(0, ms));
            }
            go.play();
            stop("on " + title + (atSecond ? " at " + Companion.clock(ms) : ", from the start"));
            c.sendSoon();
        };
        say("playing " + title + " (with a pause right behind it)");
        go.playFromMediaId(row.getMediaId(), row.getDescription().getExtras());
        go.pause();
        main.postDelayed(() -> {
            if (!landed[0]) {
                landed[0] = true;
                awaited = null;
                whenAwaited = null;
                PlaybackState s = aa.getPlaybackState();
                go.play();
                stop("ARC never reported " + title + " within 5 s; let it play (state " + (s == null ? "?" : s.getState()) + ")");
            }
        }, 5000);
    }
}
