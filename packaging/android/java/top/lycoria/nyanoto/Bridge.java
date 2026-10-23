package top.lycoria.nyanoto;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.Settings;

/** Мостик между Rust и Android. Rust зовёт статические методы, сюда же он вешает свои natives. */
public final class Bridge {
    static volatile Activity activity;

    /** Кнопки уведомления и наушников: 1 пауза/игра, 2 игра, 3 пауза, 4 стоп, 5 дальше, 6 назад, 7 перемотка (мс). */
    static native void command(int code, long value);

    /** Приложение снова на экране; доступ к файлам могли дать. */
    static native void resumed(boolean storage);

    /** Жест «Назад»: 0 начали, 1 тянут (progress 0…1), 2 отпустили — назад, 3 передумали. edge 0 — слева. */
    static native void gesture(int phase, float progress, int edge);

    private Bridge() {}

    static void send(int code, long value) {
        try {
            command(code, value);
        } catch (UnsatisfiedLinkError e) {
            // Rust ещё не запустился — команду некому отдать.
        }
    }

    static void backGesture(int phase, float progress, int edge) {
        try {
            gesture(phase, progress, edge);
        } catch (UnsatisfiedLinkError e) {
            // Rust ещё не запустился.
        }
    }

    /** Тёмная тема — светлые значки строки состояния, светлая — тёмные. */
    static void setDarkBars(final boolean dark) {
        final Activity a = activity;
        if (a == null || Build.VERSION.SDK_INT < 30) {
            return;
        }
        a.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                android.view.WindowInsetsController bars = a.getWindow().getInsetsController();
                if (bars != null) {
                    int light = android.view.WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                            | android.view.WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                    bars.setSystemBarsAppearance(dark ? 0 : light, light);
                }
            }
        });
    }

    /** Rust: есть ли куда вернуться внутри приложения. */
    static void setBack(final boolean on) {
        final Activity a = activity;
        if (a instanceof MainActivity) {
            a.runOnUiThread(new Runnable() {
                @Override
                public void run() {
                    ((MainActivity) a).setBack(on);
                }
            });
        }
    }

    static void tellResumed() {
        try {
            resumed(hasStorage());
        } catch (UnsatisfiedLinkError e) {
            // Rust спросит сам, когда запустится.
        }
    }

    static boolean hasStorage() {
        if (Build.VERSION.SDK_INT >= 30) {
            return Environment.isExternalStorageManager();
        }
        Activity a = activity;
        return a != null && a.checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED;
    }

    /** Открыть переключатель «Доступ ко всем файлам» (или обычный запрос на старых Android). */
    static void askStorage() {
        final Activity a = activity;
        if (a == null) {
            return;
        }
        a.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (Build.VERSION.SDK_INT >= 30) {
                    try {
                        a.startActivity(new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                                Uri.parse("package:" + a.getPackageName())));
                    } catch (Exception e) {
                        a.startActivity(new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION));
                    }
                } else {
                    a.requestPermissions(new String[] {Manifest.permission.READ_EXTERNAL_STORAGE}, 2);
                }
            }
        });
    }

    /** «Назад» на главной: уйти в фон, музыка играет дальше. */
    static void home() {
        final Activity a = activity;
        if (a != null) {
            a.runOnUiThread(new Runnable() {
                @Override
                public void run() {
                    a.moveTaskToBack(true);
                }
            });
        }
    }

    /** Что играет: state 0 — ничего, 1 — играет, 2 — пауза. */
    static void update(String title, String artist, String album, String art, int state,
            long position, long length, float rate, boolean canNext, boolean canPrevious) {
        Activity a = activity;
        if (a != null) {
            PlaybackService.show(a.getApplicationContext(),
                    new PlaybackService.Now(title, artist, album, art, state, position, length, rate, canNext, canPrevious));
        }
    }

    static void stop() {
        Activity a = activity;
        if (a != null) {
            PlaybackService.hide(a.getApplicationContext());
        }
    }
}
