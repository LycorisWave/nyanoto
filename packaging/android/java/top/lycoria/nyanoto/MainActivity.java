package top.lycoria.nyanoto;

import android.Manifest;
import android.app.NativeActivity;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.window.BackEvent;
import android.window.OnBackAnimationCallback;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;

/** Окно плеера: всё рисует Rust (Slint), здесь только разрешения и возврат в приложение. */
public class MainActivity extends NativeActivity {
    // Свой «Назад»: пока в приложении есть куда возвращаться — ведём жест сами (интерфейс
    // приоткрывается за пальцем), на главной — отдаём системе (предпросмотр рабочего стола).
    private OnBackInvokedDispatcher quiet;
    private OnBackInvokedCallback back;
    private boolean backOn;

    /**
     * Slint вешает свой обработчик «Назад» сюда — тогда система не показывает предпросмотр.
     * Отдаём прокси: всё уходит в настоящий диспетчер окна, кроме обработчика Slint.
     */
    @Override
    public OnBackInvokedDispatcher getOnBackInvokedDispatcher() {
        if (Build.VERSION.SDK_INT < 33) {
            return super.getOnBackInvokedDispatcher();
        }
        if (quiet == null) {
            final OnBackInvokedDispatcher real = super.getOnBackInvokedDispatcher();
            final ClassLoader ours = MainActivity.class.getClassLoader();
            quiet = (OnBackInvokedDispatcher) java.lang.reflect.Proxy.newProxyInstance(ours,
                    new Class<?>[] {OnBackInvokedDispatcher.class}, new java.lang.reflect.InvocationHandler() {
                        @Override
                        public Object invoke(Object proxy, java.lang.reflect.Method method, Object[] args)
                                throws Throwable {
                            if (args != null && method.getName().equals("registerOnBackInvokedCallback")) {
                                Object callback = args[args.length - 1];
                                if (callback != null && callback.getClass().getName().contains("Slint")) {
                                    return null;
                                }
                            }
                            try {
                                return method.invoke(real, args);
                            } catch (java.lang.reflect.InvocationTargetException e) {
                                throw e.getCause();
                            }
                        }
                    });
        }
        return quiet;
    }

    /** Есть ли куда вернуться внутри приложения. Только из потока окна Android. */
    void setBack(boolean on) {
        if (Build.VERSION.SDK_INT < 33 || on == backOn) {
            return;
        }
        if (back == null) {
            back = Build.VERSION.SDK_INT >= 34 ? animated() : new OnBackInvokedCallback() {
                @Override
                public void onBackInvoked() {
                    Bridge.backGesture(2, 0f, 0);
                }
            };
        }
        OnBackInvokedDispatcher real = super.getOnBackInvokedDispatcher();
        if (on) {
            real.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, back);
        } else {
            real.unregisterOnBackInvokedCallback(back);
        }
        backOn = on;
    }

    private OnBackInvokedCallback animated() {
        return new OnBackAnimationCallback() {
            private float startX;

            @Override
            public void onBackStarted(BackEvent event) {
                startX = event.getTouchX();
                Bridge.backGesture(0, 0f, event.getSwipeEdge());
            }

            @Override
            public void onBackProgressed(BackEvent event) {
                // Бывает, система не считает прогресс (0) — тогда по пальцу: треть ширины — до конца.
                float progress = event.getProgress();
                if (progress <= 0f) {
                    float width = Math.max(1, getWindow().getDecorView().getWidth());
                    progress = Math.min(1f, Math.abs(event.getTouchX() - startX) / (width / 3f));
                }
                Bridge.backGesture(1, progress, event.getSwipeEdge());
            }

            @Override
            public void onBackCancelled() {
                Bridge.backGesture(3, 0f, 0);
            }

            @Override
            public void onBackInvoked() {
                Bridge.backGesture(2, 0f, 0);
            }
        };
    }

    @Override
    protected void onCreate(Bundle state) {
        Bridge.activity = this;
        super.onCreate(state);
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        Bridge.tellResumed();
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(code, permissions, results);
        Bridge.tellResumed();
    }

    @Override
    protected void onDestroy() {
        if (Bridge.activity == this) {
            Bridge.activity = null;
        }
        super.onDestroy();
    }
}
