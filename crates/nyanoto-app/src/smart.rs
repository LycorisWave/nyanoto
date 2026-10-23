//! Умные плейлисты и перемешивание в окне.

use std::path::PathBuf;
use std::time::Duration;

use nyanoto_library::{Condition, SmartOrder, SmartRules, TextField, Track, shuffle};
use slint::{Model, ModelRc, SharedString, TimerMode, VecModel};

use crate::lang::Lex;
use crate::{App, OpenList, SmartRow, with_app};

/// Перемешивание очереди.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shuffle {
    Off,
    Tracks,
    /// Альбомы в случайном порядке, треки внутри альбома — по порядку.
    Albums,
}

impl Shuffle {
    pub fn code(self) -> &'static str {
        match self {
            Shuffle::Off => "off",
            Shuffle::Tracks => "tracks",
            Shuffle::Albums => "albums",
        }
    }

    pub fn from_code(code: Option<&str>) -> Self {
        match code {
            Some("tracks") => Shuffle::Tracks,
            Some("albums") => Shuffle::Albums,
            _ => Shuffle::Off,
        }
    }

    fn next(self) -> Self {
        match self {
            Shuffle::Off => Shuffle::Tracks,
            Shuffle::Tracks => Shuffle::Albums,
            Shuffle::Albums => Shuffle::Off,
        }
    }
}

/// Порядок очереди с перемешиванием: выбранный трек играет первым.
pub fn shuffled(mut tracks: Vec<Track>, start: usize, mode: Shuffle, seed: u64) -> Vec<Track> {
    if tracks.is_empty() || mode == Shuffle::Off {
        return tracks;
    }
    let start = start.min(tracks.len() - 1);
    match mode {
        Shuffle::Off => tracks,
        Shuffle::Tracks => {
            let first = tracks.remove(start);
            shuffle(&mut tracks, seed);
            tracks.insert(0, first);
            tracks
        }
        Shuffle::Albums => {
            // Альбом — подряд идущие треки одной папки.
            let mut groups: Vec<Vec<Track>> = Vec::new();
            let mut folder: Option<PathBuf> = None;
            let (mut first_group, mut first_offset) = (0, 0);
            for (index, track) in tracks.into_iter().enumerate() {
                let parent = track.path.parent().map(PathBuf::from);
                if groups.is_empty() || parent != folder {
                    groups.push(Vec::new());
                    folder = parent;
                }
                let last = groups.len() - 1;
                if index == start {
                    (first_group, first_offset) = (last, groups[last].len());
                }
                groups[last].push(track);
            }
            // Выбранный трек играет первым, его альбом доигрывается до конца.
            let mut first = groups.remove(first_group);
            first.drain(..first_offset);
            shuffle(&mut groups, seed);
            first.into_iter().chain(groups.into_iter().flatten()).collect()
        }
    }
}

/// Виды условий в порядке списка окна.
const KIND_COUNT: i32 = 16;

fn needs_value(kind: i32) -> bool {
    matches!(kind, 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 14 | 15)
}

pub fn kinds(lex: &Lex) -> Vec<SharedString> {
    [
        lex.k_liked, lex.k_never, lex.k_not_for_days, lex.k_played_at_least, lex.k_added_within,
        lex.k_contains, lex.k_artist, lex.k_album, lex.k_genre, lex.k_composer, lex.k_year,
        lex.k_hires, lex.k_lossless, lex.k_lossy, lex.k_longer, lex.k_shorter,
    ]
    .into_iter()
    .map(SharedString::from)
    .collect()
}

pub fn orders(lex: &Lex) -> Vec<SharedString> {
    [lex.o_library, lex.o_random, lex.o_most, lex.o_recent, lex.o_least]
        .into_iter()
        .map(SharedString::from)
        .collect()
}

pub fn presets(lex: &Lex) -> Vec<SharedString> {
    [lex.p_never, lex.p_long_ago, lex.p_liked, lex.p_often, lex.p_month, lex.p_hires]
        .into_iter()
        .map(SharedString::from)
        .collect()
}

fn order_index(order: SmartOrder) -> i32 {
    match order {
        SmartOrder::Library => 0,
        SmartOrder::Random => 1,
        SmartOrder::MostPlayed => 2,
        SmartOrder::RecentlyAdded => 3,
        SmartOrder::LeastRecentlyPlayed => 4,
    }
}

fn order_at(index: i32) -> SmartOrder {
    match index {
        1 => SmartOrder::Random,
        2 => SmartOrder::MostPlayed,
        3 => SmartOrder::RecentlyAdded,
        4 => SmartOrder::LeastRecentlyPlayed,
        _ => SmartOrder::Library,
    }
}

/// Условие из строки редактора. Неверное число — условия нет.
pub fn condition(kind: i32, value: &str) -> Option<Condition> {
    let number = || value.trim().parse::<u32>().ok();
    let text = |field| Some(Condition::Contains(field, value.trim().to_string()));
    match kind {
        0 => Some(Condition::Liked),
        1 => Some(Condition::NeverPlayed),
        2 => number().map(Condition::NotPlayedForDays),
        3 => number().map(Condition::PlayedAtLeast),
        4 => number().map(Condition::AddedWithinDays),
        5 => text(TextField::Any),
        6 => text(TextField::Artist),
        7 => text(TextField::Album),
        8 => text(TextField::Genre),
        9 => text(TextField::Composer),
        10 => {
            let (from, to) = value.split_once(['-', '–', '—']).unwrap_or((value, value));
            Some(Condition::YearBetween(from.trim().parse().ok()?, to.trim().parse().ok()?))
        }
        11 => Some(Condition::HiRes),
        12 => Some(Condition::Lossless),
        13 => Some(Condition::Lossy),
        14 => number().map(Condition::LongerThanMinutes),
        15 => number().map(Condition::ShorterThanMinutes),
        _ => None,
    }
}

/// Строка редактора для условия.
pub fn row_of(condition: &Condition) -> (i32, String) {
    match condition {
        Condition::Liked => (0, String::new()),
        Condition::NeverPlayed => (1, String::new()),
        Condition::NotPlayedForDays(days) => (2, days.to_string()),
        Condition::PlayedAtLeast(times) => (3, times.to_string()),
        Condition::AddedWithinDays(days) => (4, days.to_string()),
        Condition::Contains(field, text) => {
            let kind = match field {
                TextField::Artist => 6,
                TextField::Album => 7,
                TextField::Genre => 8,
                TextField::Composer => 9,
                TextField::Any | TextField::Title | TextField::Path => 5,
            };
            (kind, text.clone())
        }
        Condition::YearBetween(from, to) if from == to => (10, from.to_string()),
        Condition::YearBetween(from, to) => (10, format!("{from}-{to}")),
        Condition::HiRes => (11, String::new()),
        Condition::Lossless => (12, String::new()),
        Condition::Lossy => (13, String::new()),
        Condition::LongerThanMinutes(minutes) => (14, minutes.to_string()),
        Condition::ShorterThanMinutes(minutes) => (15, minutes.to_string()),
    }
}

/// Заготовка: название и правила.
pub fn preset(index: i32, lex: &Lex) -> Option<(&'static str, SmartRules)> {
    let rules = |conditions: Vec<Condition>, order, limit| SmartRules { all: true, conditions, order, limit };
    Some(match index {
        0 => (lex.p_never, rules(vec![Condition::NeverPlayed], SmartOrder::Random, None)),
        1 => (lex.p_long_ago, rules(vec![Condition::NotPlayedForDays(90)], SmartOrder::LeastRecentlyPlayed, None)),
        2 => (lex.p_liked, rules(vec![Condition::Liked], SmartOrder::Random, None)),
        3 => (lex.p_often, rules(vec![Condition::PlayedAtLeast(5)], SmartOrder::MostPlayed, Some(100))),
        4 => (lex.p_month, rules(vec![Condition::AddedWithinDays(30)], SmartOrder::RecentlyAdded, None)),
        5 => (lex.p_hires, rules(vec![Condition::HiRes], SmartOrder::Library, None)),
        _ => return None,
    })
}

fn smart_row(kind: i32, value: &str) -> SmartRow {
    SmartRow { kind, value: value.into(), needs_value: needs_value(kind) }
}

impl App {
    pub(crate) fn show_shuffle(&self) {
        let lex = self.lex();
        let state = match self.shuffle.get() {
            Shuffle::Off => lex.repeat_off,
            Shuffle::Tracks => lex.shuffle_tracks,
            Shuffle::Albums => lex.shuffle_albums,
        };
        let text = if self.shuffle.get() == Shuffle::Off { lex.shuffle.to_string() } else { format!("{}: {state}", lex.shuffle) };
        self.window.set_shuffle_text(text.into());
        self.window.set_shuffle_on(self.shuffle.get() != Shuffle::Off);
        self.window.set_shuffle_mode(match self.shuffle.get() {
            Shuffle::Off => 0,
            Shuffle::Tracks => 1,
            Shuffle::Albums => 2,
        });
        self.window.set_smart_kinds(ModelRc::from(std::rc::Rc::new(VecModel::from(kinds(lex)))));
        self.window.set_smart_orders(ModelRc::from(std::rc::Rc::new(VecModel::from(orders(lex)))));
        self.window.set_smart_presets(ModelRc::from(std::rc::Rc::new(VecModel::from(presets(lex)))));
    }

    pub(crate) fn cycle_shuffle(&self) {
        let next = self.shuffle.get().next();
        self.shuffle.set(next);
        self.save_setting(crate::SETTING_SHUFFLE, next.code());
        self.apply_shuffle();
        self.show_shuffle();
    }

    /// Перемешивание на лету: несыгранное переставляем, текущий доигрывает, выключили — порядок назад.
    /// Альбомы — в исходном порядке, иначе рассыпятся.
    pub(crate) fn apply_shuffle(&self) {
        let items = self.engine.queue();
        let current = self.engine.current_id().and_then(|id| items.iter().position(|item| item.id == id));
        let played = current.map_or(0, |index| index + 1);
        let mode = self.shuffle.get();
        let original = self.shuffle_original.borrow().clone();
        // Позиции в исходном порядке (повтор одного файла — по очереди вхождений).
        let order_of = |order: &[PathBuf]| -> Vec<u64> {
            let mut taken = vec![false; items.len()];
            let mut ids = Vec::with_capacity(items.len());
            for path in order {
                if let Some(index) = (0..items.len()).find(|index| !taken[*index] && items[*index].path == *path) {
                    taken[index] = true;
                    ids.push(items[index].id);
                }
            }
            ids
        };
        if mode == Shuffle::Off {
            if let Some(original) = original {
                // Сыгранное и текущее — на местах, остальное — как было.
                let mut ids: Vec<u64> = items[..played].iter().map(|item| item.id).collect();
                let rest: Vec<u64> = order_of(&original).into_iter().filter(|id| !ids.contains(id)).collect();
                ids.extend(rest);
                self.engine.reorder(ids);
            }
            *self.shuffle_original.borrow_mut() = None;
            return;
        }
        if items.len() < played + 2 {
            return;
        }
        let original = original.unwrap_or_else(|| items.iter().map(|item| item.path.clone()).collect());
        *self.shuffle_original.borrow_mut() = Some(original.clone());
        // Оставшиеся — в исходном порядке, потом перемешать.
        let ahead: Vec<u64> = order_of(&original).into_iter().filter(|id| items[played..].iter().any(|item| item.id == *id)).collect();
        let queue = self.queue.borrow();
        let tracks: Vec<(u64, Track)> = ahead
            .iter()
            .filter_map(|id| {
                let item = items.iter().find(|item| item.id == *id)?;
                Some((*id, queue.get(&item.path)?.clone()))
            })
            .collect();
        drop(queue);
        let start = (self.next_random() % tracks.len().max(1) as u64) as usize;
        let mixed = shuffled(tracks.iter().map(|(_, track)| track.clone()).collect(), start, mode, self.next_random());
        let mut left: Vec<(u64, Track)> = tracks;
        let mut ids: Vec<u64> = items[..played].iter().map(|item| item.id).collect();
        for track in mixed {
            if let Some(index) = left.iter().position(|(_, candidate)| candidate.path == track.path) {
                ids.push(left.remove(index).0);
            }
        }
        self.engine.reorder(ids);
    }

    /// Вся библиотека вперемешку: треками или альбомами, как выбрано в панели.
    pub(crate) fn shuffle_everything(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let tracks = library.tracks(usize::MAX >> 1, 0).unwrap_or_default();
        if tracks.is_empty() {
            return;
        }
        if self.shuffle.get() == Shuffle::Off {
            self.shuffle.set(Shuffle::Tracks);
            self.save_setting(crate::SETTING_SHUFFLE, Shuffle::Tracks.code());
            self.show_shuffle();
        }
        let start = (self.next_random() % tracks.len() as u64) as usize;
        self.play(tracks, start, crate::QueueKind::List);
    }

    // ───────────────────────── редактор правил ─────────────────────────

    pub(crate) fn open_smart_editor(&self, editing: Option<i64>) {
        let rules = editing
            .and_then(|id| self.library.as_ref()?.smart_rules(id).ok().flatten())
            .unwrap_or(SmartRules { all: true, ..SmartRules::default() });
        let name = editing
            .and_then(|id| self.playlists.borrow().iter().find(|list| list.id == id).map(|list| list.name.clone()))
            .unwrap_or_default();
        self.smart_editing.set(editing);
        self.window.set_smart_name(name.into());
        self.fill_smart_editor(&rules);
        self.window.set_smart_open(true);
    }

    fn fill_smart_editor(&self, rules: &SmartRules) {
        let draft: Vec<(i32, String)> = rules.conditions.iter().map(row_of).collect();
        self.smart_rows
            .set_vec(draft.iter().map(|(kind, value)| smart_row(*kind, value)).collect::<Vec<_>>());
        *self.smart_draft.borrow_mut() = draft;
        let window = &self.window;
        window.set_smart_match(i32::from(!rules.all));
        window.set_smart_order(order_index(rules.order));
        window.set_smart_limit(rules.limit.map(|limit| limit.to_string()).unwrap_or_default().into());
        self.count_smart();
    }

    pub(crate) fn smart_preset(&self, index: i32) {
        let Some((name, rules)) = preset(index, self.lex()) else {
            return;
        };
        if self.window.get_smart_name().is_empty() {
            self.window.set_smart_name(name.into());
        }
        self.fill_smart_editor(&rules);
    }

    pub(crate) fn smart_add_condition(&self) {
        self.smart_draft.borrow_mut().push((0, String::new()));
        self.smart_rows.push(smart_row(0, ""));
        self.count_smart();
    }

    pub(crate) fn smart_remove_condition(&self, index: usize) {
        let mut draft = self.smart_draft.borrow_mut();
        if index < draft.len() {
            draft.remove(index);
            self.smart_rows.remove(index);
        }
        drop(draft);
        self.count_smart();
    }

    pub(crate) fn smart_kind_changed(&self, index: usize, kind: i32) {
        let kind = kind.clamp(0, KIND_COUNT - 1);
        if let Some(entry) = self.smart_draft.borrow_mut().get_mut(index) {
            entry.0 = kind;
            if !needs_value(kind) {
                entry.1.clear();
            }
            if index < self.smart_rows.row_count() {
                self.smart_rows.set_row_data(index, smart_row(kind, &entry.1));
            }
        }
        self.count_smart();
    }

    /// Правка поля значения: строки не перерисовываются, чтобы не сбить курсор.
    pub(crate) fn smart_value_edited(&self, index: usize, text: &str) {
        if let Some(entry) = self.smart_draft.borrow_mut().get_mut(index) {
            entry.1 = text.to_string();
        }
        self.count_smart();
    }

    fn smart_rules_from_editor(&self) -> SmartRules {
        let window = &self.window;
        SmartRules {
            all: window.get_smart_match() == 0,
            conditions: self
                .smart_draft
                .borrow()
                .iter()
                .filter_map(|(kind, value)| condition(*kind, value))
                .collect(),
            order: order_at(window.get_smart_order()),
            limit: window.get_smart_limit().trim().parse().ok().filter(|limit| *limit > 0),
        }
    }

    /// Сколько треков подходит: считается через 0,3 с после последней правки.
    pub(crate) fn count_smart(&self) {
        self.smart_timer.start(TimerMode::SingleShot, Duration::from_millis(300), || {
            with_app(|app| {
                let Some(library) = app.library.as_ref() else {
                    return;
                };
                let rules = app.smart_rules_from_editor();
                let count = library.smart_tracks(&rules, 1).map_or(0, |tracks| tracks.len());
                app.window
                    .set_smart_message(format!("{}: {count}", app.lex().smart_found).into());
            });
        });
    }

    pub(crate) fn save_smart(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let name = self.window.get_smart_name().trim().to_string();
        if name.is_empty() {
            return;
        }
        let rules = self.smart_rules_from_editor();
        let saved = match self.smart_editing.get() {
            Some(id) => library
                .set_smart_rules(id, &rules)
                .and_then(|()| library.rename_playlist(id, &name))
                .map(|()| id),
            None => library.create_smart_playlist(&name, &rules),
        };
        match saved {
            Ok(id) => {
                self.window.set_smart_open(false);
                self.refresh_playlists();
                let index = self.playlists.borrow().iter().position(|list| list.id == id);
                if let Some(index) = index {
                    self.open_playlist(index + 1);
                }
            }
            Err(error) => self.report(error),
        }
    }

    /// Открытый плейлист — умный?
    pub(crate) fn open_smart_id(&self) -> Option<i64> {
        match self.open_list.get() {
            OpenList::Playlist(id) if self.playlists.borrow().iter().any(|list| list.id == id && list.smart) => {
                Some(id)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn track(id: i64, folder: &str) -> Track {
        Track {
            id,
            path: Path::new(folder).join(format!("{id}.flac")),
            title: None,
            artist: None,
            album: None,
            album_artist: None,
            year: None,
            track_no: None,
            disc_no: None,
            duration_ms: None,
            format: None,
            problem: None,
            owner: None,
            folder_artist: None,
            folder_album: None,
            bit_depth: None,
            sample_rate: None,
            channels: None,
            bitrate: None,
            genre: None,
            composer: None,
            conductor: None,
            work: None,
            movement: None,
            movement_no: None,
            lyrics: false,
        }
    }

    #[test]
    fn shuffled_tracks_start_with_the_chosen_one() {
        let tracks: Vec<Track> = (0..20).map(|id| track(id, "a")).collect();
        let order: Vec<i64> = shuffled(tracks, 7, Shuffle::Tracks, 42).iter().map(|track| track.id).collect();
        assert_eq!(order[0], 7);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        assert_ne!(order, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn shuffled_albums_keep_their_inner_order() {
        let mut tracks = Vec::new();
        for (album, folder) in ["a", "b", "c", "d"].iter().enumerate() {
            for number in 0..3 {
                tracks.push(track(album as i64 * 10 + number, folder));
            }
        }
        // Выбран второй трек альбома «b»: он играет первым, альбом доигрывается.
        let order: Vec<i64> = shuffled(tracks, 4, Shuffle::Albums, 9).iter().map(|track| track.id).collect();
        assert_eq!(&order[..2], &[11, 12]);
        assert_eq!(order.len(), 11);
        for chunk in order[2..].chunks(3) {
            assert_eq!(chunk[1], chunk[0] + 1, "альбом идёт по порядку: {order:?}");
            assert_eq!(chunk[2], chunk[0] + 2);
        }
    }

    #[test]
    fn conditions_round_trip_through_rows() {
        for kind in 0..KIND_COUNT {
            let value = match kind {
                10 => "1970-1979",
                5..=9 => "кино",
                _ if needs_value(kind) => "12",
                _ => "",
            };
            let parsed = condition(kind, value).unwrap_or_else(|| panic!("вид {kind}"));
            assert_eq!(row_of(&parsed), (kind, value.to_string()), "вид {kind}");
        }
        assert_eq!(condition(2, "много"), None);
        assert_eq!(condition(10, "1985"), Some(Condition::YearBetween(1985, 1985)));
    }
}
