package top.lycoria.nyanoto;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.os.SystemClock;

/**
 * Служба воспроизведения: держит процесс живым, пока играет музыка, показывает уведомление
 * с кнопками и отдаёт трек экрану блокировки и наушникам (MediaSession). Сам звук — в Rust.
 */
public class PlaybackService extends Service {
    static final String CHANNEL = "playback";
    static final int ID = 1;
    static final String PLAY_PAUSE = "top.lycoria.nyanoto.PLAY_PAUSE";
    static final String NEXT = "top.lycoria.nyanoto.NEXT";
    static final String PREVIOUS = "top.lycoria.nyanoto.PREVIOUS";

    /** Что играет. */
    static final class Now {
        final String title, artist, album, art;
        final int state;
        final long position, length;
        final float rate;
        final boolean canNext, canPrevious;

        Now(String title, String artist, String album, String art, int state, long position, long length,
                float rate, boolean canNext, boolean canPrevious) {
            this.title = title;
            this.artist = artist;
            this.album = album;
            this.art = art;
            this.state = state;
            this.position = position;
            this.length = length;
            this.rate = rate;
            this.canNext = canNext;
            this.canPrevious = canPrevious;
        }
    }

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static MediaSession session;
    private static Now now;
    private static String artPath;
    private static Bitmap artBitmap;
    private static PowerManager.WakeLock wake;
    private static AudioFocusRequest focus;
    // Пауза из-за чужого звука на время (звонок, навигатор): вернём сами.
    private static boolean pausedForFocus;
    private static boolean noisyRegistered;

    private static final BroadcastReceiver NOISY = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            if (AudioManager.ACTION_AUDIO_BECOMING_NOISY.equals(intent.getAction())) {
                Bridge.send(3, 0);
            }
        }
    };

    static void show(final Context context, final Now next) {
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                apply(context, next);
            }
        });
    }

    static void hide(final Context context) {
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                now = null;
                release(context);
                context.stopService(new Intent(context, PlaybackService.class));
                NotificationManager manager = context.getSystemService(NotificationManager.class);
                if (manager != null) {
                    manager.cancel(ID);
                }
                if (session != null) {
                    session.setActive(false);
                    session.release();
                    session = null;
                }
            }
        });
    }

    private static void apply(Context context, Now next) {
        Now previous = now;
        now = next;
        if (next.state == 0) {
            hide(context);
            return;
        }
        MediaSession media = session(context);
        if (artPath == null || !artPath.equals(next.art)) {
            artPath = next.art;
            artBitmap = next.art.isEmpty() ? null : BitmapFactory.decodeFile(next.art);
        }
        media.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, next.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, next.artist)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, next.album)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, next.length)
                .putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, artBitmap)
                .build());
        boolean playing = next.state == 1;
        long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                | PlaybackState.ACTION_STOP | PlaybackState.ACTION_SEEK_TO;
        if (next.canNext) {
            actions |= PlaybackState.ACTION_SKIP_TO_NEXT;
        }
        if (next.canPrevious) {
            actions |= PlaybackState.ACTION_SKIP_TO_PREVIOUS;
        }
        media.setPlaybackState(new PlaybackState.Builder()
                .setActions(actions)
                .setState(playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED, next.position,
                        playing ? next.rate : 0f, SystemClock.elapsedRealtime())
                .build());
        if (playing) {
            hold(context);
            if (previous == null || previous.state != 1) {
                Intent start = new Intent(context, PlaybackService.class);
                if (Build.VERSION.SDK_INT >= 26) {
                    context.startForegroundService(start);
                } else {
                    context.startService(start);
                }
            }
        } else {
            if (!pausedForFocus) {
                release(context);
            }
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) {
            manager.notify(ID, notification(context));
        }
    }

    private static MediaSession session(Context context) {
        if (session == null) {
            session = new MediaSession(context, "nyanoto");
            session.setCallback(new MediaSession.Callback() {
                @Override
                public void onPlay() {
                    Bridge.send(2, 0);
                }

                @Override
                public void onPause() {
                    Bridge.send(3, 0);
                }

                @Override
                public void onStop() {
                    Bridge.send(4, 0);
                }

                @Override
                public void onSkipToNext() {
                    Bridge.send(5, 0);
                }

                @Override
                public void onSkipToPrevious() {
                    Bridge.send(6, 0);
                }

                @Override
                public void onSeekTo(long position) {
                    Bridge.send(7, position);
                }
            });
            Intent open = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
            if (open != null) {
                session.setSessionActivity(PendingIntent.getActivity(context, 0, open, PendingIntent.FLAG_IMMUTABLE));
            }
            session.setActive(true);
        }
        return session;
    }

    private static Notification notification(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null && manager.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel channel = new NotificationChannel(CHANNEL, context.getString(R.string.channel),
                    NotificationManager.IMPORTANCE_LOW);
            channel.setShowBadge(false);
            manager.createNotificationChannel(channel);
        }
        Now shown = now;
        boolean playing = shown != null && shown.state == 1;
        Notification.Builder builder = new Notification.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_ears)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOngoing(playing)
                .setShowWhen(false);
        if (shown != null) {
            builder.setContentTitle(shown.title).setContentText(shown.artist).setLargeIcon(artBitmap);
        }
        Intent open = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (open != null) {
            builder.setContentIntent(PendingIntent.getActivity(context, 0, open, PendingIntent.FLAG_IMMUTABLE));
        }
        builder.addAction(new Notification.Action.Builder(android.R.drawable.ic_media_previous, "previous",
                action(context, PREVIOUS)).build());
        builder.addAction(new Notification.Action.Builder(
                playing ? android.R.drawable.ic_media_pause : android.R.drawable.ic_media_play,
                playing ? "pause" : "play", action(context, PLAY_PAUSE)).build());
        builder.addAction(new Notification.Action.Builder(android.R.drawable.ic_media_next, "next",
                action(context, NEXT)).build());
        Notification.MediaStyle style = new Notification.MediaStyle().setShowActionsInCompactView(0, 1, 2);
        if (session != null) {
            style.setMediaSession(session.getSessionToken());
        }
        builder.setStyle(style);
        return builder.build();
    }

    private static PendingIntent action(Context context, String name) {
        Intent intent = new Intent(context, PlaybackService.class).setAction(name);
        return PendingIntent.getForegroundService(context, name.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE);
    }

    /** Играет: не спать, звук наш, наушники выдернули — пауза. */
    private static void hold(Context context) {
        if (wake == null) {
            PowerManager power = context.getSystemService(PowerManager.class);
            wake = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nyanoto:playback");
            wake.setReferenceCounted(false);
        }
        wake.acquire();
        AudioManager audio = context.getSystemService(AudioManager.class);
        if (focus == null && audio != null) {
            focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build())
                    .setOnAudioFocusChangeListener(new AudioManager.OnAudioFocusChangeListener() {
                        @Override
                        public void onAudioFocusChange(int change) {
                            if (change == AudioManager.AUDIOFOCUS_LOSS) {
                                pausedForFocus = false;
                                Bridge.send(3, 0);
                            } else if (change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
                                pausedForFocus = now != null && now.state == 1;
                                Bridge.send(3, 0);
                            } else if (change == AudioManager.AUDIOFOCUS_GAIN && pausedForFocus) {
                                pausedForFocus = false;
                                Bridge.send(2, 0);
                            }
                        }
                    }, MAIN)
                    .build();
            audio.requestAudioFocus(focus);
        }
        if (!noisyRegistered) {
            context.registerReceiver(NOISY, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
            noisyRegistered = true;
        }
    }

    private static void release(Context context) {
        if (wake != null && wake.isHeld()) {
            wake.release();
        }
        AudioManager audio = context.getSystemService(AudioManager.class);
        if (focus != null && audio != null) {
            audio.abandonAudioFocusRequest(focus);
            focus = null;
        }
        if (noisyRegistered) {
            try {
                context.unregisterReceiver(NOISY);
            } catch (IllegalArgumentException e) {
                // Уже снят.
            }
            noisyRegistered = false;
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Notification shown = notification(this);
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, shown, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(ID, shown);
        }
        String action = intent != null ? intent.getAction() : null;
        if (PLAY_PAUSE.equals(action)) {
            Bridge.send(1, 0);
        } else if (NEXT.equals(action)) {
            Bridge.send(5, 0);
        } else if (PREVIOUS.equals(action)) {
            Bridge.send(6, 0);
        }
        // Пауза: уведомление остаётся, но его можно смахнуть, а процессу разрешено уснуть.
        if (now == null || now.state != 1) {
            stopForeground(STOP_FOREGROUND_DETACH);
        }
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
