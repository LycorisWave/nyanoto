//! Здоровье библиотеки в окне: проверка файлов в фоне и страница с итогами.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nyanoto_library::{Damage, HealthRecord, Library, Track};
use slint::{ModelRc, VecModel};

use crate::rows::file_name;
use crate::{App, HealthRow, in_ui, quiet};

/// Сколько файлов фоновая проверка берёт за один заход.
const BATCH: usize = 32;
/// Сколько строк показывать в одном разделе.
const SECTION_LIMIT: usize = 500;

/// Полная проверка одного файла для базы.
fn record_of(path: &Path, stop: &AtomicBool, lex: &'static crate::lang::Lex) -> Option<HealthRecord> {
    match nyanoto_core::verify_file(path, stop) {
        Ok(Some(health)) => Some(HealthRecord {
            md5: health.md5,
            decode_errors: health.decode_errors,
            missing_ms: health.missing_ms,
            audio_ms: health.audio_ms,
            error: None,
        }),
        Ok(None) => None,
        Err(error) => Some(HealthRecord { error: Some(crate::rows::track_error(&error, lex)), ..HealthRecord::default() }),
    }
}

impl App {
    /// Тихая проверка в фоне: файлы, которые ещё не проверялись или
    /// изменились. Прерванная проверка продолжается с того же места.
    pub(crate) fn start_health_checks(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        if !self.health_on.get() || self.health_urgent.get() || self.health_checking.replace(true) {
            return;
        }
        let stop = self.stop_health.clone();
        stop.store(false, Ordering::Relaxed);
        let lex = self.lex();
        let spawned = std::thread::Builder::new().name("nyanoto-health".into()).spawn(move || {
            quiet::enter_background();
            let Ok(library) = Library::open(&database) else {
                in_ui(|app| app.health_checking.set(false));
                return;
            };
            let (mut checked, mut after) = (0usize, 0i64);
            while !stop.load(Ordering::Relaxed) {
                let batch = library.tracks_to_verify_after(after, BATCH).unwrap_or_default();
                let Some(last) = batch.last().map(|(id, _)| *id) else {
                    break;
                };
                after = last;
                for (id, path) in batch {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let started = std::time::Instant::now();
                    if let Some(record) = record_of(&path, &stop, lex) {
                        let _ = library.store_health(id, &record);
                        checked += 1;
                    }
                    quiet::rest(started.elapsed(), &stop);
                }
                in_ui(|app| app.health_changed());
            }
            in_ui(move |app| {
                app.health_checking.set(false);
                if checked > 0 {
                    app.health_changed();
                }
            });
        });
        if spawned.is_err() {
            self.health_checking.set(false);
        }
    }

    /// Немедленная проверка на всех ядрах, кроме одного.
    pub(crate) fn health_now(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        if !self.health_on.get() || self.health_urgent.replace(true) {
            return;
        }
        self.stop_health.store(true, Ordering::Relaxed);
        let stop = self.stop_health_urgent.clone();
        stop.store(false, Ordering::Relaxed);
        self.window.set_health_busy(true);
        let lex = self.lex();
        let spawned = std::thread::Builder::new().name("nyanoto-health-now".into()).spawn(move || {
            let pending = Library::open(&database)
                .and_then(|library| library.tracks_to_verify(usize::MAX))
                .unwrap_or_default();
            let workers = std::thread::available_parallelism()
                .map_or(2, |count| count.get())
                .saturating_sub(1)
                .max(1);
            let next = AtomicUsize::new(0);
            let (results, received) = std::sync::mpsc::channel::<(i64, HealthRecord)>();
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    let results = results.clone();
                    let (next, pending, stop) = (&next, &pending, &stop);
                    scope.spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            let Some((id, path)) = pending.get(next.fetch_add(1, Ordering::Relaxed)) else {
                                return;
                            };
                            if let Some(record) = record_of(path, stop, lex)
                                && results.send((*id, record)).is_err()
                            {
                                return;
                            }
                        }
                    });
                }
                drop(results);
                let Ok(library) = Library::open(&database) else {
                    return;
                };
                for (done, (id, record)) in received.into_iter().enumerate() {
                    let _ = library.store_health(id, &record);
                    if (done + 1).is_multiple_of(25) {
                        in_ui(|app| app.health_changed());
                    }
                }
            });
            in_ui(|app| {
                app.health_urgent.set(false);
                app.window.set_health_busy(false);
                app.health_changed();
            });
        });
        if spawned.is_err() {
            self.health_urgent.set(false);
            self.window.set_health_busy(false);
        }
    }

    pub(crate) fn stop_health_checks(&self) {
        self.stop_health.store(true, Ordering::Relaxed);
        self.stop_health_urgent.store(true, Ordering::Relaxed);
    }

    /// Проверка продвинулась: обновить страницу, если она открыта.
    fn health_changed(&self) {
        if self.window.get_page() == crate::PAGE_HEALTH {
            self.show_health();
        }
    }

    /// Страница «Здоровье»: разделы с находками.
    pub(crate) fn show_health(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let lex = self.lex();
        let report = match library.health_report() {
            Ok(report) => report,
            Err(error) => {
                self.report(error);
                return;
            }
        };
        let mut rows: Vec<HealthRow> = Vec::new();
        let mut tracks: Vec<Option<Track>> = Vec::new();
        let heading = |rows: &mut Vec<HealthRow>, tracks: &mut Vec<Option<Track>>, text: String| {
            rows.push(HealthRow { heading: true, text: text.into(), detail: Default::default() });
            tracks.push(None);
        };
        let folder_of = |track: &Track| {
            track.path.parent().map(|folder| folder.display().to_string()).unwrap_or_default()
        };

        heading(&mut rows, &mut tracks, format!("{} ({})", lex.damaged_files, report.damaged.len()));
        for (track, damage) in report.damaged.iter().take(SECTION_LIMIT) {
            let what = match damage {
                Damage::Md5 => lex.damage_md5.to_string(),
                Damage::DecodeErrors(count) => format!("{}: {count}", lex.damage_errors),
                Damage::Truncated(ms) => format!("{} {}", lex.damage_truncated, crate::rows::clock(*ms as f32 / 1000.0)),
                Damage::Silent => lex.damage_silent.to_string(),
                Damage::Unreadable(why) => format!("{}: {why}", lex.damage_unreadable),
            };
            rows.push(HealthRow {
                heading: false,
                text: file_name(&track.path).into(),
                detail: format!("{what} · {}", folder_of(track)).into(),
            });
            tracks.push(Some(track.clone()));
        }

        let copies: usize = report.duplicates.iter().map(Vec::len).sum();
        heading(&mut rows, &mut tracks, format!("{} ({copies})", lex.duplicates));
        for group in report.duplicates.iter().take(SECTION_LIMIT) {
            for track in group {
                rows.push(HealthRow {
                    heading: false,
                    text: file_name(&track.path).into(),
                    detail: format!("{} {} · {}", lex.copies, group.len(), folder_of(track)).into(),
                });
                tracks.push(Some(track.clone()));
            }
        }

        heading(&mut rows, &mut tracks, format!("{} ({})", lex.gaps, report.gaps.len()));
        for gap in report.gaps.iter().take(SECTION_LIMIT) {
            rows.push(HealthRow {
                heading: false,
                text: gap.folder.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default().into(),
                detail: format!("{}: {} · {}", lex.gap_missing, gap.missing.join(", "), gap.folder.display()).into(),
            });
            tracks.push(None);
        }

        heading(&mut rows, &mut tracks, format!("{} ({})", lex.missing_files, report.missing.len()));
        for track in report.missing.iter().take(SECTION_LIMIT) {
            rows.push(HealthRow {
                heading: false,
                text: file_name(&track.path).into(),
                detail: track.path.display().to_string().into(),
            });
            tracks.push(None);
        }

        heading(&mut rows, &mut tracks, lex.fake_lossless.to_string());
        let fake = if self.authenticity.get() {
            format!("{}: {}", lex.fake_found, self.suspicious.borrow().len())
        } else {
            lex.fake_off.to_string()
        };
        rows.push(HealthRow { heading: false, text: fake.into(), detail: Default::default() });
        tracks.push(None);

        let state = if self.health_urgent.get() || self.health_checking.get() { lex.health_running } else { "" };
        self.window.set_health_progress(
            format!("{} {} {} {} {state}", lex.health_checked, report.checked, lex.of, report.total).into(),
        );
        self.window.set_health_rows(ModelRc::from(std::rc::Rc::new(VecModel::from(rows))));
        *self.health_tracks.borrow_mut() = tracks;
    }

    /// Клик по строке: сведения о треке, если строка про трек.
    pub(crate) fn open_health_row(&self, index: usize) {
        let track = self.health_tracks.borrow().get(index).cloned().flatten();
        if let Some(track) = track {
            self.open_info(track);
        }
    }
}
