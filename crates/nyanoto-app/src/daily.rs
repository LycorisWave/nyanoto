//! Плейлист дня в окне: когда собирать, разбор звука для порядка, карточка и настройки.
//! Собирается раз в день (с 5 утра) в тихом фоне; на энергосбережении новые треки не разбираем.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use nyanoto_library::{DAILY_LENGTHS, DailyOptions, DailyReadiness, DailyThreshold, Library, Track, TrackFeel, daily_day, daily_order};
use slint::{Image, Model, ModelRc, SharedString, VecModel};

use crate::lang::{Lex, Noun};
use crate::lyrics::Lyrics;
use crate::{App, OpenList, PAGE_PLAYLIST, PAGE_PLAYLISTS, QueueKind, clock, in_ui, load, lyrics, quiet, rows, stage, wavebar};

pub const SETTING_DAILY: &str = "daily";
pub const SETTING_DAILY_LENGTH: &str = "daily_length";
pub const SETTING_DAILY_FRESH: &str = "daily_fresh";
pub const SETTING_DAILY_THRESHOLD: &str = "daily_threshold";
/// «0» — предложение в «Плейлистах» закрыли («Не сейчас»).
pub const SETTING_DAILY_OFFER: &str = "daily_offer";

/// Длина и доля нового по умолчанию.
const DEFAULT_LENGTH: usize = 25;
const DEFAULT_FRESH: f32 = 0.3;
/// Слова в минуту — подсказка, только когда тексты есть хотя бы у пятой части
/// библиотеки и не меньше чем у 100 треков: иначе по ним нечего сравнивать.
const WORDS_SHARE: f32 = 0.2;
const WORDS_MIN: u64 = 100;
/// Обложек на карточке (мозаика 2 × 2) и из скольких альбомов их искать:
/// у части альбомов обложки нет.
const COVERS: usize = 4;
const COVER_FOLDERS: usize = 8;
/// Готовность пересчитывается не чаще раза в пять минут.
const READINESS_FOR: std::time::Duration = std::time::Duration::from_secs(300);
const COVER_SIDE: u32 = 96;

/// Что показывает карточка в «Плейлистах».
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum Card {
    Hidden = 0,
    Offer = 1,
    Building = 2,
    Ready = 3,
    Waiting = 4,
}

#[derive(Default)]
pub struct State {
    on: Cell<bool>,
    dismissed: Cell<bool>,
    length: Cell<usize>,
    fresh: Cell<f32>,
    threshold: Cell<DailyThreshold>,
    readiness: Cell<Option<DailyReadiness>>,
    readiness_at: Cell<Option<Instant>>,
    /// Альбомы, обложки которых запрошены для карточки.
    cover_folders: RefCell<Vec<PathBuf>>,
    /// День собранного плейлиста, который показан.
    day: Cell<Option<i64>>,
    pub tracks: RefCell<Vec<Track>>,
    created: Cell<i64>,
    building: Cell<bool>,
    stop: Arc<AtomicBool>,
    covers: Rc<VecModel<Image>>,
    /// Очередь играет из плейлиста дня: такие прослушивания — выбор
    /// алгоритма, а не человека.
    pub queue: Cell<bool>,
    /// Подсказать о перемешивании, когда трек начнётся (начало трека
    /// очищает строку состояния).
    hint: Cell<bool>,
}

impl State {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn today() -> i64 {
    let now = clock::now();
    daily_day(now, clock::offset_at(now))
}

fn threshold_code(threshold: DailyThreshold) -> &'static str {
    match threshold {
        DailyThreshold::Early => "early",
        DailyThreshold::Usual => "usual",
        DailyThreshold::Strict => "strict",
    }
}

/// Слов в минуту. Меньше 20 слов — это не текст, а «(инструментал)».
pub fn words_per_minute(lyrics: &Lyrics, duration: f32) -> Option<f32> {
    let words: usize = lyrics
        .lines
        .iter()
        .map(|line| line.text.split_whitespace().filter(|word| word.chars().any(char::is_alphabetic)).count())
        .sum();
    if words < 20 {
        return None;
    }
    let times: Vec<f32> = lyrics.lines.iter().filter_map(|line| line.at.map(|at| at.as_secs_f32())).collect();
    let span = match (lyrics.synced, times.first(), times.last()) {
        (true, Some(first), Some(last)) if last > first => last - first + 3.0,
        _ => duration,
    };
    (span >= 30.0).then(|| words as f32 / (span / 60.0))
}

/// Темп, напряжение и слова. Разбор — из кэша сцен или заново, если `analyse`; второе — был ли разбор.
fn track_feel(track: &Track, cache: Option<&Path>, analyse: bool, words: bool, stop: &AtomicBool) -> (TrackFeel, bool) {
    let file = cache.and_then(|cache| Some(cache.join(wavebar::cache_key(&track.path, "nkf")?)));
    let cached = file.as_ref().and_then(|file| stage::feel::from_bytes(&std::fs::read(file).ok()?));
    let mut decoded = false;
    let feel = cached.or_else(|| {
        if !analyse || stop.load(Ordering::Relaxed) {
            return None;
        }
        decoded = true;
        let feel = nyanoto_core::feel(&track.path, stop).ok()??;
        if let (Some(file), Some(cache)) = (&file, cache)
            && std::fs::create_dir_all(cache).is_ok()
        {
            let _ = std::fs::write(file, stage::feel::to_bytes(&feel));
        }
        Some(feel)
    });
    let mood = feel.map(|feel| stage::feel::Mood::from_feel(&feel));
    let duration = track.duration_ms.unwrap_or(0) as f32 / 1000.0;
    let words = (words && track.lyrics).then(|| lyrics::load(&track.path)).flatten().and_then(|text| words_per_minute(&text, duration));
    let feel = TrackFeel {
        tempo: mood.as_ref().and_then(|mood| mood.bpm()),
        energy: mood.as_ref().filter(|mood| mood.measured()).map(|mood| mood.song),
        words,
    };
    (feel, decoded)
}

/// Сбор в фоне: отбор, разбор звука, порядок, запись в базу.
fn build(database: &Path, day: i64, options: DailyOptions, rebuild: bool, stop: &AtomicBool) -> Result<Vec<Track>, String> {
    let library = Library::open(database).map_err(|error| error.to_string())?;
    let picks = library.daily_pick(day, &options, rebuild).map_err(|error| error.to_string())?;
    if picks.is_empty() {
        return Ok(picks);
    }
    let (with, total) = library.lyrics_share().unwrap_or((0, 0));
    let words = total > 0 && with >= WORDS_MIN && with as f32 / total as f32 >= WORDS_SHARE;
    let analyse = load::power_saving() != Some(true);
    let cache = database.parent().map(|dir| dir.join("feel"));
    let mut feels = Vec::with_capacity(picks.len());
    for track in &picks {
        let started = Instant::now();
        let (feel, decoded) = track_feel(track, cache.as_deref(), analyse, words, stop);
        feels.push(feel);
        if decoded {
            quiet::rest(started.elapsed(), stop);
        }
    }
    if stop.load(Ordering::Relaxed) {
        return Err(String::new());
    }
    let order = daily_order(&picks, &feels);
    // Отладка (только отладочная сборка): порядок с темпом и напряжением в файл.
    if cfg!(debug_assertions)
        && let Some(file) = std::env::var_os("NYANOTO_DAILY_DEBUG")
    {
        let lines: Vec<String> = order
            .iter()
            .map(|index| {
                let (track, feel) = (&picks[*index], &feels[*index]);
                format!(
                    "{:>6.1} bpm  energy {:.2}  genre {:.2}  words {:>5.0}  {} — {} [{}]",
                    feel.tempo.unwrap_or(0.0),
                    feel.energy.unwrap_or(-1.0),
                    track.genre.as_deref().and_then(nyanoto_library::genre_aggression).unwrap_or(-1.0),
                    feel.words.unwrap_or(0.0),
                    track.artist.as_deref().unwrap_or("?"),
                    track.title.as_deref().unwrap_or("?"),
                    track.genre.as_deref().unwrap_or(""),
                )
            })
            .collect();
        let _ = std::fs::write(file, lines.join("\n"));
    }
    let ordered: Vec<Track> = order.into_iter().map(|index| picks[index].clone()).collect();
    let ids: Vec<i64> = ordered.iter().map(|track| track.id).collect();
    library.save_daily(day, &ids).map_err(|error| error.to_string())?;
    Ok(ordered)
}

/// «Пока рано: нужно ещё 120 треков в библиотеке, 40 прослушиваний и 5 дней статистики».
fn need_text(readiness: &DailyReadiness, threshold: DailyThreshold, lex: &Lex) -> String {
    let mut parts = Vec::new();
    if readiness.tracks < threshold.tracks() {
        let left = (threshold.tracks() - readiness.tracks) as u32;
        parts.push(lex.daily_need_tracks.replace("{}", &lex.count(left, Noun::Track)));
    }
    if readiness.plays < threshold.plays() {
        parts.push(lex.count((threshold.plays() - readiness.plays) as u32, Noun::Play));
    }
    if readiness.days < threshold.days() {
        let left = (threshold.days() - readiness.days) as u32;
        parts.push(lex.daily_need_days.replace("{}", &lex.count(left, Noun::Day)));
    }
    let list = match parts.len() {
        0 => String::new(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap_or_default();
            format!("{}{}{last}", parts.join(", "), lex.daily_and)
        }
    };
    lex.daily_need.replace("{}", &list)
}

impl App {
    pub(crate) fn load_daily_settings(&self, setting: &dyn Fn(&str) -> Option<String>) {
        let daily = &self.daily;
        daily.on.set(setting(SETTING_DAILY).as_deref() == Some("1"));
        daily.dismissed.set(setting(SETTING_DAILY_OFFER).as_deref() == Some("0"));
        let length = setting(SETTING_DAILY_LENGTH).and_then(|value| value.parse().ok()).filter(|length| DAILY_LENGTHS.contains(length));
        daily.length.set(length.unwrap_or(DEFAULT_LENGTH));
        let fresh = setting(SETTING_DAILY_FRESH).and_then(|value| value.parse::<f32>().ok()).map(|percent| percent / 100.0);
        daily.fresh.set(fresh.unwrap_or(DEFAULT_FRESH).clamp(0.0, 0.6));
        let threshold = setting(SETTING_DAILY_THRESHOLD)
            .and_then(|code| DailyThreshold::ALL.into_iter().find(|threshold| threshold_code(*threshold) == code));
        daily.threshold.set(threshold.unwrap_or_default());
        let window = &self.window;
        window.set_daily_covers(ModelRc::from(daily.covers.clone()));
        window.set_daily_enabled(daily.on.get());
        window.set_daily_length_index(DAILY_LENGTHS.iter().position(|length| *length == daily.length.get()).unwrap_or(1) as i32);
        window.set_daily_fresh((daily.fresh.get() * 100.0).round());
        window.set_daily_threshold_index(DailyThreshold::ALL.iter().position(|known| *known == daily.threshold.get()).unwrap_or(1) as i32);
    }

    /// Хватает ли статистики (и включена ли она).
    fn daily_available(&self) -> bool {
        self.statistics.get() && self.daily.readiness.get().is_some_and(|readiness| readiness.ready(self.daily.threshold.get()))
    }

    /// Проверяет, что плейлист на сегодня есть: читает собранный или
    /// собирает новый. Вызывается при запуске, на странице «Плейлисты» и в
    /// настройках.
    pub(crate) fn daily_check(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        if self.daily.readiness_at.get().is_none_or(|at| at.elapsed() > READINESS_FOR) {
            self.daily.readiness.set(library.daily_readiness().ok());
            self.daily.readiness_at.set(Some(Instant::now()));
        }
        let day = today();
        if self.daily.on.get() && self.daily.day.get() != Some(day) && !self.daily.building.get() {
            match library.daily_list(day) {
                Ok(Some((tracks, created))) if !tracks.is_empty() => {
                    self.daily.day.set(Some(day));
                    self.daily.created.set(created);
                    *self.daily.tracks.borrow_mut() = tracks;
                }
                _ if self.daily_available() => self.daily_build(false, false),
                _ => {}
            }
        }
        self.show_daily();
    }

    /// Начался трек: если наступил новый день — новый плейлист (без лишних
    /// запросов в остальное время).
    pub(crate) fn daily_tick(&self) {
        if self.daily.on.get() && self.daily.day.get().is_some_and(|day| day != today()) {
            self.daily_check();
        }
    }

    /// `force` — без порогов, недостающее добирается случайными треками.
    fn daily_build(&self, rebuild: bool, force: bool) {
        let Some(database) = self.database.clone() else {
            return;
        };
        if self.daily.building.replace(true) {
            return;
        }
        let day = today();
        let seed = if rebuild { clock::now() as u64 ^ 0x5DEE_CE66 } else { day as u64 };
        let options = DailyOptions { length: self.daily.length.get(), fresh: self.daily.fresh.get(), seed, force };
        let stop = self.daily.stop.clone();
        stop.store(false, Ordering::Relaxed);
        self.window.set_status(self.lex().daily_building.into());
        self.show_daily();
        let spawned = std::thread::Builder::new().name("nyanoto-daily".into()).spawn(move || {
            quiet::enter_background();
            let result = build(&database, day, options, rebuild, &stop);
            in_ui(move |app| app.daily_built(day, result));
        });
        if spawned.is_err() {
            self.daily.building.set(false);
            self.show_daily();
        }
    }

    fn daily_built(&self, day: i64, result: Result<Vec<Track>, String>) {
        self.daily.building.set(false);
        if self.window.get_status() == self.lex().daily_building {
            self.window.set_status(SharedString::new());
        }
        match result {
            Ok(tracks) => {
                self.daily.day.set(Some(day));
                self.daily.created.set(clock::now());
                *self.daily.tracks.borrow_mut() = tracks;
            }
            Err(error) if !error.is_empty() => self.window.set_status(error.into()),
            Err(_) => {}
        }
        self.show_daily();
        if self.window.get_page() == PAGE_PLAYLIST && self.open_list.get() == OpenList::Daily {
            self.load_open_playlist();
        }
    }

    /// Карточка в «Плейлистах» и строка состояния в настройках.
    pub(crate) fn show_daily(&self) {
        let lex = self.lex();
        let daily = &self.daily;
        let window = &self.window;
        let available = self.daily_available();
        let tracks = daily.tracks.borrow();
        let card = if daily.building.get() {
            Card::Building
        } else if daily.on.get() && daily.day.get().is_some() && !tracks.is_empty() {
            Card::Ready
        } else if daily.on.get() {
            Card::Waiting
        } else if available && !daily.dismissed.get() {
            Card::Offer
        } else {
            Card::Hidden
        };
        let status = if !self.statistics.get() {
            lex.daily_no_stats.to_string()
        } else {
            match daily.readiness.get() {
                Some(readiness) if !readiness.ready(daily.threshold.get()) => need_text(&readiness, daily.threshold.get(), lex),
                Some(_) => lex.daily_ready.to_string(),
                None => String::new(),
            }
        };
        let detail = match card {
            Card::Ready => {
                let length: u64 = tracks.iter().filter_map(|track| track.duration_ms).map(|ms| ms.max(0) as u64).sum();
                let built = lex.daily_built.replace("{}", &clock::time_of_day(daily.created.get()));
                format!(
                    "{} · {} · {built}",
                    lex.count(tracks.len() as u32, Noun::Track),
                    rows::duration_words(length, lex)
                )
            }
            Card::Building => lex.daily_building.to_string(),
            Card::Waiting if daily.day.get().is_some() => lex.daily_empty.to_string(),
            Card::Waiting => status.clone(),
            Card::Offer => lex.daily_offer_detail.to_string(),
            Card::Hidden => String::new(),
        };
        window.set_daily_state(card as i32);
        window.set_daily_detail(detail.into());
        window.set_daily_status(SharedString::from(status));
        window.set_daily_available(available || daily.on.get() || self.advanced.get());
        window.set_daily_enabled(daily.on.get());

        // Обложки — первые четыре найденные среди восьми альбомов плейлиста.
        let mut folders: Vec<PathBuf> = Vec::new();
        if card == Card::Ready {
            for track in tracks.iter() {
                if let Some(folder) = track.path.parent()
                    && !folders.iter().any(|known| known == folder)
                {
                    folders.push(folder.to_path_buf());
                    if folders.len() == COVER_FOLDERS {
                        break;
                    }
                }
            }
        }
        drop(tracks);
        if *daily.cover_folders.borrow() == folders {
            return;
        }
        daily.covers.set_vec(Vec::new());
        for folder in &folders {
            self.show_cover(folder, COVER_SIDE, |app, image| {
                if app.daily.covers.row_count() < COVERS {
                    app.daily.covers.push(image);
                }
            });
        }
        *daily.cover_folders.borrow_mut() = folders;
    }

    pub(crate) fn open_daily(&self) {
        self.navigated();
        self.open_list.set(OpenList::Daily);
        self.load_open_playlist();
        let window = &self.window;
        window.set_playlist_deletable(false);
        window.set_crumb(self.lex().daily_title.into());
        window.set_page(PAGE_PLAYLIST);
    }

    pub(crate) fn play_daily(&self) {
        let tracks = self.daily.tracks.borrow().clone();
        self.play(tracks, 0, QueueKind::Playlist);
        self.daily.queue.set(true);
        self.daily_shuffle_hint();
    }

    /// Перемешивание включено — волна не сохранится. Решает пользователь, мы только подсказываем.
    pub(crate) fn daily_shuffle_hint(&self) {
        if self.shuffle.get() != crate::smart::Shuffle::Off {
            self.daily.hint.set(true);
        }
    }

    /// Трек начался: отложенная подсказка о перемешивании.
    pub(crate) fn daily_hint_on_start(&self) {
        if self.daily.hint.replace(false) {
            self.window.set_status(self.lex().daily_shuffled.into());
        }
    }

    /// «Собрать заново». Если статистики по-прежнему мало, плейлист мог
    /// появиться только принудительно — тогда и заново так же.
    pub(crate) fn daily_rebuild(&self) {
        self.daily_build(true, !self.daily_available());
    }

    /// «Собрать сейчас» (режим продвинутого пользователя): без порогов. Если
    /// плейлист дня выключен — включается: собранный должен где-то появиться.
    pub(crate) fn daily_force(&self) {
        if !self.daily.on.get() {
            self.daily.on.set(true);
            self.save_flag(SETTING_DAILY, true);
        }
        let rebuild = self.daily.day.get() == Some(today());
        self.daily_build(rebuild, true);
    }

    pub(crate) fn daily_set_enabled(&self, on: bool) {
        self.daily.on.set(on);
        self.save_flag(SETTING_DAILY, on);
        if !on {
            // Выключили сами — карточка «включить?» больше не предлагает.
            self.daily.stop();
            self.daily.dismissed.set(true);
            self.save_setting(SETTING_DAILY_OFFER, "0");
        }
        self.daily_check();
    }

    /// «Включить» на карточке-предложении.
    pub(crate) fn daily_accept(&self) {
        self.daily_set_enabled(true);
    }

    /// «Не сейчас»: карточка больше не предлагает (включить можно в настройках).
    pub(crate) fn daily_dismiss(&self) {
        self.daily.dismissed.set(true);
        self.save_setting(SETTING_DAILY_OFFER, "0");
        self.show_daily();
    }

    pub(crate) fn daily_set_length(&self, index: usize) {
        let length = DAILY_LENGTHS.get(index).copied().unwrap_or(DEFAULT_LENGTH);
        self.daily.length.set(length);
        self.save_setting(SETTING_DAILY_LENGTH, &length.to_string());
    }

    pub(crate) fn daily_set_fresh(&self, percent: f32, done: bool) {
        let percent = percent.round().clamp(0.0, 60.0);
        self.daily.fresh.set(percent / 100.0);
        if done {
            self.save_setting(SETTING_DAILY_FRESH, &format!("{percent}"));
        }
    }

    pub(crate) fn daily_set_threshold(&self, index: usize) {
        let threshold = DailyThreshold::ALL.get(index).copied().unwrap_or_default();
        self.daily.threshold.set(threshold);
        self.save_setting(SETTING_DAILY_THRESHOLD, threshold_code(threshold));
        self.daily_check();
    }

    /// Страница «Плейлисты» открылась.
    pub(crate) fn daily_page_shown(&self) {
        if self.window.get_page() == PAGE_PLAYLISTS {
            self.daily_check();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_per_minute_counts_sung_words() {
        let synced = lyrics::parse(
            "[00:20.00]раз два три четыре пять\n[00:25.00]шесть семь восемь девять десять\n[00:30.00]one two three four five\n[00:35.00]six seven eight nine ten\n[00:50.00]ещё слово",
        );
        // 22 слова за 33 секунды (с 0:20 до 0:50 и ещё 3 на последнюю строку) — 40 в минуту.
        let rate = words_per_minute(&synced, 240.0).unwrap();
        assert!((rate - 40.0).abs() < 0.5, "{rate}");
        let short = lyrics::parse("(инструментал)");
        assert_eq!(words_per_minute(&short, 200.0), None);
        let plain = lyrics::parse(&"слово ".repeat(200));
        assert!((words_per_minute(&plain, 120.0).unwrap() - 100.0).abs() < 0.5);
    }

    #[test]
    fn need_text_lists_what_is_missing() {
        let lex = crate::lang::Lang::Ru.lex();
        let readiness = DailyReadiness { tracks: 380, plays: 149, days: 20 };
        assert_eq!(
            need_text(&readiness, DailyThreshold::Usual, lex),
            "Пока рано: нужно ещё 120 треков в библиотеке и 1 прослушивание."
        );
    }
}
