//! Nyanoto на телефоне: NativeActivity грузит эту библиотеку и зовёт `android_main`.
//! Всё остальное — тот же `nyanoto-app`, только с мобильной разметкой.

#![cfg(target_os = "android")]

#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    stderr_to_logcat();
    // База и настройки — во внутренней папке приложения.
    if let Some(dir) = app.internal_data_path() {
        // SAFETY: других потоков ещё нет.
        unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
    }
    if let Err(error) = slint::android::init(app) {
        eprintln!("nyanoto: окно не создалось: {error}");
        return;
    }
    if let Err(error) = nyanoto_app::run() {
        eprintln!("nyanoto: {error}");
    }
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(priority: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
}

/// Ошибки и паники (stderr) — в logcat под тегом `nyanoto`, иначе их не видно.
fn stderr_to_logcat() {
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: обычные pipe/dup2; конец для чтения отдаём потоку.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 || libc::dup2(fds[1], 2) < 0 {
            return;
        }
    }
    let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let _ = std::thread::Builder::new().name("nyanoto-log".into()).spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if let Ok(text) = std::ffi::CString::new(line) {
                // 6 — ANDROID_LOG_ERROR.
                unsafe { __android_log_write(6, c"nyanoto".as_ptr(), text.as_ptr()) };
            }
        }
    });
}
