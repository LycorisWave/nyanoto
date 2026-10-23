//! Конвертация альбома или плейлиста в FLAC/WAV. Папки как в библиотеке, ничего не перезаписываем.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nyanoto_core::{ConvertOptions, TargetFormat, convert_file};
use nyanoto_library::{Track, copy_tags};

use crate::{App, in_ui};

pub const SETTING_CONVERT: &str = "convert";
const SETTING_CONVERT_FOLDER: &str = "convert_folder";

fn options(format: i32, quality: i32) -> ConvertOptions {
    let format = if format == 1 { TargetFormat::Wav } else { TargetFormat::Flac };
    match quality {
        1 => ConvertOptions { format, rate: Some(44_100), bits: Some(16) },
        2 => ConvertOptions { format, rate: Some(48_000), bits: Some(24) },
        _ => ConvertOptions { format, rate: None, bits: None },
    }
}

/// Куда ляжет результат: путь трека внутри его корневой папки, но в `folder`.
pub fn target_path(track: &Path, roots: &[PathBuf], folder: &Path, extension: &str) -> PathBuf {
    let inside = roots
        .iter()
        .filter_map(|root| track.strip_prefix(root).ok())
        .min_by_key(|inside| inside.components().count())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let parent = track.parent().and_then(Path::file_name).map(PathBuf::from).unwrap_or_default();
            parent.join(track.file_name().unwrap_or_default())
        });
    folder.join(inside).with_extension(extension)
}

impl App {
    pub(crate) fn open_conversion(&self, tracks: Vec<Track>) {
        if tracks.is_empty() {
            return;
        }
        let lex = self.lex();
        let folder = self
            .library
            .as_ref()
            .and_then(|library| library.setting(SETTING_CONVERT_FOLDER).ok().flatten())
            .unwrap_or_default();
        self.window.set_convert_count(format!("{}: {}", lex.convert_count, tracks.len()).into());
        self.window.set_convert_folder(folder.into());
        if !self.converting.get() {
            self.window.set_convert_message(Default::default());
        }
        *self.convert_tracks.borrow_mut() = tracks;
        self.window.set_convert_open(true);
    }

    pub(crate) fn choose_convert_folder(&self) {
        self.choose(crate::choose::Want::Folder, "", |app, folder| {
            let text = folder.to_string_lossy().into_owned();
            app.save_setting(SETTING_CONVERT_FOLDER, &text);
            app.window.set_convert_folder(text.into());
        });
    }

    pub(crate) fn start_conversion(&self) {
        let folder = PathBuf::from(self.window.get_convert_folder().as_str());
        if self.converting.get() || folder.as_os_str().is_empty() {
            return;
        }
        let tracks = self.convert_tracks.borrow().clone();
        let roots: Vec<PathBuf> = self
            .library
            .as_ref()
            .and_then(|library| library.roots().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|root| root.path)
            .collect();
        let options = options(self.window.get_convert_format(), self.window.get_convert_quality());
        let extension = if options.format == TargetFormat::Wav { "wav" } else { "flac" };
        let mut seen = std::collections::HashSet::new();
        // «a.flac» и «a.wav» из одной папки дают один и тот же результат: берётся первый.
        let jobs: Vec<(PathBuf, PathBuf)> = tracks
            .iter()
            .map(|track| (track.path.clone(), target_path(&track.path, &roots, &folder, extension)))
            .filter(|(_, target)| seen.insert(target.clone()))
            .collect();
        let total = jobs.len();
        self.converting.set(true);
        self.window.set_convert_busy(true);
        let stop = Arc::new(AtomicBool::new(false));
        *self.stop_convert.borrow_mut() = stop.clone();
        let lex = self.lex();
        let _ = std::thread::Builder::new().name("nyanoto-convert".into()).spawn(move || {
            let workers = std::thread::available_parallelism()
                .map_or(2, |count| count.get())
                .saturating_sub(1)
                .max(1);
            let next = AtomicUsize::new(0);
            let (done, skipped) = (AtomicUsize::new(0), AtomicUsize::new(0));
            let failed = std::sync::Mutex::new(Vec::<String>::new());
            std::thread::scope(|scope| {
                for _ in 0..workers.min(total) {
                    scope.spawn(|| {
                        while !stop.load(Ordering::Relaxed) {
                            let Some((source, target)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                                return;
                            };
                            if target.exists() || target == source {
                                skipped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            let made = target
                                .parent()
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .map_err(|error| error.to_string())
                                .and_then(|()| {
                                    convert_file(source, target, &options, &stop).map_err(|error| crate::rows::track_error(&error, lex))
                                });
                            match made {
                                Ok(true) => {
                                    if let Err(error) = copy_tags(source, target) {
                                        failed.lock().unwrap_or_else(|poison| poison.into_inner()).push(format!(
                                            "{}: {error}",
                                            crate::rows::file_name(source)
                                        ));
                                    }
                                    let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                                    let passed = finished + skipped.load(Ordering::Relaxed);
                                    in_ui(move |app| {
                                        let lex = app.lex();
                                        app.window.set_convert_message(
                                            format!("{} {passed} {} {total}", lex.convert_running, lex.of).into(),
                                        );
                                    });
                                }
                                Ok(false) => return,
                                Err(error) => failed
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner())
                                    .push(format!("{}: {error}", crate::rows::file_name(source))),
                            }
                        }
                    });
                }
            });
            let (done, skipped) = (done.into_inner(), skipped.into_inner());
            let failed = failed.into_inner().unwrap_or_default();
            in_ui(move |app| {
                let lex = app.lex();
                app.converting.set(false);
                app.window.set_convert_busy(false);
                let mut text = format!("{}: {done} {} {total}", lex.convert_done, lex.of);
                if skipped > 0 {
                    text.push_str(&format!("; {}: {skipped}", lex.convert_skipped));
                }
                if !failed.is_empty() {
                    text.push_str(&format!("; {}: {}", lex.convert_failed, failed.join("; ")));
                }
                app.window.set_convert_message(text.clone().into());
                app.window.set_status(text.into());
            });
        });
    }

    pub(crate) fn stop_conversion(&self) {
        self.stop_convert.borrow().store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_repeat_the_library_layout() {
        let roots = vec![PathBuf::from("D:/music"), PathBuf::from("D:/music/inner")];
        let track = Path::new("D:/music/Artist/Album/CD 2/01. Song.flac");
        assert_eq!(
            target_path(track, &roots, Path::new("E:/out"), "wav"),
            PathBuf::from("E:/out/Artist/Album/CD 2/01. Song.wav")
        );
        let stray = Path::new("C:/elsewhere/Album/02. B.mp3");
        assert_eq!(
            target_path(stray, &roots, Path::new("E:/out"), "flac"),
            PathBuf::from("E:/out/Album/02. B.flac")
        );
    }
}
