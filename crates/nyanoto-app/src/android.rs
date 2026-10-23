//! Телефон: мостик к Java (`top.lycoria.nyanoto.Bridge`) — уведомление и экран блокировки,
//! доступ к файлам. Java зовёт нас через `nyanoto-android` (команды, возврат в приложение).

use std::ffi::c_void;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::{JNIEnv, JavaVM, NativeMethod};

use crate::system_media::Now;

const BRIDGE: &str = "top.lycoria.nyanoto.Bridge";

static CLASS: OnceLock<GlobalRef> = OnceLock::new();

/// Зовёт `work` с JNI и классом `Bridge`. Класс ищем загрузчиком активности:
/// из нашего потока `FindClass` видит только системные классы.
fn with_bridge<R>(work: impl FnOnce(&mut JNIEnv, &JClass) -> jni::errors::Result<R>) -> Option<R> {
    let context = ndk_context::android_context();
    // SAFETY: указатель на JavaVM даёт android-activity, он живёт всё время процесса.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    if CLASS.get().is_none() {
        let class = load(&mut env, context.context()).ok()?;
        let _ = CLASS.set(class);
    }
    let class: &JClass = CLASS.get()?.as_obj().into();
    let result = work(&mut env, class);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result.ok()
}

fn load(env: &mut JNIEnv, activity: *mut c_void) -> jni::errors::Result<GlobalRef> {
    // SAFETY: глобальная ссылка на активность из ndk-context, удалять её не нам.
    let activity = unsafe { JObject::from_raw(activity.cast()) };
    let loader = env.call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
    let name = env.new_string(BRIDGE)?;
    let class: JClass = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?.l()?.into();
    // NativeActivity грузит нас мимо System.loadLibrary — свои natives вешаем сами.
    env.register_native_methods(
        &class,
        &[
            NativeMethod { name: "command".into(), sig: "(IJ)V".into(), fn_ptr: native_command as *mut c_void },
            NativeMethod { name: "resumed".into(), sig: "(Z)V".into(), fn_ptr: native_resumed as *mut c_void },
            NativeMethod { name: "gesture".into(), sig: "(IFI)V".into(), fn_ptr: native_gesture as *mut c_void },
        ],
    )?;
    env.new_global_ref(class)
}

extern "system" fn native_command(_env: JNIEnv, _class: JClass, code: jint, value: jlong) {
    command(code, value);
}

extern "system" fn native_resumed(_env: JNIEnv, _class: JClass, storage: jboolean) {
    resumed(storage != 0);
}

/// Жест «Назад» из Java: 0 начали, 1 тянут, 2 отпустили — назад, 3 передумали.
extern "system" fn native_gesture(_env: JNIEnv, _class: JClass, phase: jint, progress: jfloat, edge: jint) {
    let _ = slint::invoke_from_event_loop(move || {
        crate::with_app(|app| {
            app.window.set_back_edge(edge);
            match phase {
                0 | 1 => app.window.set_back_progress(progress.clamp(0.0, 1.0)),
                2 => {
                    app.window.set_back_progress(0.0);
                    app.window.invoke_back_commit();
                }
                _ => app.window.set_back_progress(0.0),
            }
        })
    });
}

/// Что играет — в уведомление и на экран блокировки; ничего — убрать.
pub(crate) fn show(now: &Now) {
    with_bridge(|env, class| {
        let title = env.new_string(&now.title)?;
        let artist = env.new_string(&now.artist)?;
        let album = env.new_string(&now.album)?;
        let art = env.new_string(now.art.as_ref().map(|art| art.to_string_lossy().into_owned()).unwrap_or_default())?;
        let state = match now.playback {
            crate::system_media::Playback::Playing => 1,
            crate::system_media::Playback::Paused => 2,
            crate::system_media::Playback::Stopped => 0,
        };
        env.call_static_method(
            class,
            "update",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;IJJFZZ)V",
            &[
                JValue::Object(&title),
                JValue::Object(&artist),
                JValue::Object(&album),
                JValue::Object(&art),
                JValue::Int(state),
                JValue::Long((now.position * 1000.0) as i64),
                JValue::Long((now.length * 1000.0) as i64),
                JValue::Float(now.rate as f32),
                JValue::Bool(now.can_next.into()),
                JValue::Bool(now.can_previous.into()),
            ],
        )?;
        Ok(())
    });
}

/// Плеер закрывается: уведомление и служба больше не нужны.
pub(crate) fn hide() {
    with_bridge(|env, class| env.call_static_method(class, "stop", "()V", &[]).map(drop));
}

/// Есть ли доступ ко всем файлам.
pub(crate) fn has_storage() -> bool {
    with_bridge(|env, class| env.call_static_method(class, "hasStorage", "()Z", &[])?.z()).unwrap_or(false)
}

/// Открыть системный переключатель «Доступ ко всем файлам».
pub(crate) fn ask_storage() {
    with_bridge(|env, class| env.call_static_method(class, "askStorage", "()V", &[]).map(drop));
}

/// Есть ли куда вернуться внутри приложения: нет — жест «Назад» ведёт система.
pub(crate) fn set_back(on: bool) {
    with_bridge(|env, class| env.call_static_method(class, "setBack", "(Z)V", &[JValue::Bool(on.into())]).map(drop));
}

/// Тёмная тема — светлые значки строки состояния и навигации, светлая — тёмные.
pub(crate) fn set_dark_bars(dark: bool) {
    with_bridge(|env, class| env.call_static_method(class, "setDarkBars", "(Z)V", &[JValue::Bool(dark.into())]).map(drop));
}

/// Уйти на главный экран, не закрываясь.
pub(crate) fn leave() {
    with_bridge(|env, class| env.call_static_method(class, "home", "()V", &[]).map(drop));
}

/// Кнопка в уведомлении, на экране блокировки или на наушниках.
fn command(code: i32, value: i64) {
    crate::system_media::android_command(code, value);
}

/// Приложение снова на экране; доступ к файлам могли только что дать.
fn resumed(storage: bool) {
    let _ = slint::invoke_from_event_loop(move || crate::with_app(|app| app.storage_changed(storage)));
}

/// Где обычно лежит музыка на телефоне.
const PHONE_MUSIC: &str = "/storage/emulated/0/Music";

impl crate::App {
    /// Доступ к файлам дали или забрали. Дали — сканируем; папок нет — предлагаем «Музыку».
    pub(crate) fn storage_changed(&self, granted: bool) {
        let was_denied = self.window.get_storage_denied();
        self.window.set_storage_denied(!granted);
        if !granted || !was_denied {
            return;
        }
        let no_roots = self.library.as_ref().and_then(|library| library.roots().ok()).is_some_and(|roots| roots.is_empty());
        let music = std::path::Path::new(PHONE_MUSIC);
        if no_roots && music.is_dir() {
            self.window.set_folder_choice_path(PHONE_MUSIC.into());
            self.choosing_root.replace(Some(music.to_path_buf()));
            self.window.set_folder_choice_open(true);
        } else {
            self.start_scan();
        }
    }
}
