//! Умные плейлисты: треки по правилам. Собираются при каждом открытии.
//! Отбор в коде, а не в SQL — так регистр кириллицы сравнивается правильно.

use serde::{Deserialize, Serialize};

use crate::{Library, LibraryError, TRACK_COLUMN_COUNT, TRACK_COLUMNS, Track, quality};

/// Где искать текст в условии «содержит».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextField {
    Any,
    Title,
    Artist,
    Album,
    Genre,
    Composer,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Condition {
    Liked,
    NeverPlayed,
    /// Не слушали столько дней (или не слушали никогда).
    NotPlayedForDays(u32),
    PlayedAtLeast(u32),
    AddedWithinDays(u32),
    Contains(TextField, String),
    YearBetween(u16, u16),
    HiRes,
    Lossless,
    Lossy,
    LongerThanMinutes(u32),
    ShorterThanMinutes(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SmartOrder {
    /// Как в обзоре: исполнитель, альбом, номер.
    #[default]
    Library,
    Random,
    MostPlayed,
    RecentlyAdded,
    /// Сначала то, что слушали давнее всего.
    LeastRecentlyPlayed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SmartRules {
    /// Все условия сразу или хотя бы одно.
    pub all: bool,
    pub conditions: Vec<Condition>,
    pub order: SmartOrder,
    /// Не больше стольких треков; `None` — без ограничения.
    pub limit: Option<u32>,
}

/// Трек вместе с тем, что о нём знает история.
struct Candidate {
    track: Track,
    plays: u32,
    last_played: Option<i64>,
    liked: bool,
    added: i64,
}

impl Condition {
    fn matches(&self, item: &Candidate, now: i64) -> bool {
        const DAY: i64 = 86_400;
        let track = &item.track;
        match self {
            Condition::Liked => item.liked,
            Condition::NeverPlayed => item.plays == 0,
            Condition::NotPlayedForDays(days) => {
                item.last_played.is_none_or(|at| now - at >= i64::from(*days) * DAY)
            }
            Condition::PlayedAtLeast(times) => item.plays >= *times,
            Condition::AddedWithinDays(days) => now - item.added <= i64::from(*days) * DAY,
            Condition::Contains(field, needle) => {
                let needle = needle.trim().to_lowercase();
                if needle.is_empty() {
                    return true;
                }
                let path = track.path.to_string_lossy();
                let values: Vec<Option<&str>> = match field {
                    TextField::Title => vec![track.title.as_deref()],
                    TextField::Artist => vec![track.artist.as_deref(), track.album_artist.as_deref()],
                    TextField::Album => vec![track.album.as_deref(), track.work.as_deref()],
                    TextField::Genre => vec![track.genre.as_deref()],
                    TextField::Composer => vec![track.composer.as_deref()],
                    TextField::Path => vec![Some(&path)],
                    TextField::Any => vec![
                        track.title.as_deref(),
                        track.artist.as_deref(),
                        track.album_artist.as_deref(),
                        track.album.as_deref(),
                        track.genre.as_deref(),
                        track.composer.as_deref(),
                        track.work.as_deref(),
                        Some(&path),
                    ],
                };
                values.into_iter().flatten().any(|value| value.to_lowercase().contains(&needle))
            }
            Condition::YearBetween(from, to) => track.year.is_some_and(|year| (*from..=*to).contains(&year)),
            Condition::HiRes => quality(track).hires,
            Condition::Lossless => quality(track).lossless,
            Condition::Lossy => !quality(track).lossless,
            Condition::LongerThanMinutes(minutes) => {
                track.duration_ms.is_some_and(|ms| ms > i64::from(*minutes) * 60_000)
            }
            Condition::ShorterThanMinutes(minutes) => {
                track.duration_ms.is_some_and(|ms| ms < i64::from(*minutes) * 60_000)
            }
        }
    }
}

impl SmartRules {
    fn matches(&self, item: &Candidate, now: i64) -> bool {
        if self.conditions.is_empty() {
            return true;
        }
        if self.all {
            self.conditions.iter().all(|condition| condition.matches(item, now))
        } else {
            self.conditions.iter().any(|condition| condition.matches(item, now))
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

impl Library {
    pub fn create_smart_playlist(&self, name: &str, rules: &SmartRules) -> Result<i64, LibraryError> {
        let id = self.create_playlist(name)?;
        self.set_smart_rules(id, rules)?;
        Ok(id)
    }

    pub fn set_smart_rules(&self, playlist: i64, rules: &SmartRules) -> Result<(), LibraryError> {
        self.connection
            .execute(
                "UPDATE playlists SET rules = ?2, updated = unixepoch() WHERE id = ?1",
                (playlist, rules.to_json()),
            )?;
        Ok(())
    }

    /// Правила плейлиста; `None` у обычного плейлиста.
    pub fn smart_rules(&self, playlist: i64) -> Result<Option<SmartRules>, LibraryError> {
        let text: Option<String> = self
            .connection
            .query_row("SELECT rules FROM playlists WHERE id = ?1", [playlist], |row| row.get(0))?;
        Ok(text.as_deref().and_then(SmartRules::from_json))
    }

    /// Треки по правилам. `seed` перемешивает случайный порядок.
    pub fn smart_tracks(&self, rules: &SmartRules, seed: u64) -> Result<Vec<Track>, LibraryError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64);
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS}, COALESCE(p.plays, 0), p.last_played,
                    t.id IN (SELECT track_id FROM favorites), t.added
             FROM tracks t JOIN roots r ON r.id = t.root_id
             LEFT JOIN (SELECT track_id, COUNT(*) AS plays, MAX(at) AS last_played
                        FROM plays GROUP BY track_id) p ON p.track_id = t.id
             WHERE t.missing = 0
             ORDER BY t.album_artist, t.album, t.disc_no, t.track_no, t.rel_path"
        ))?;
        let base = TRACK_COLUMN_COUNT;
        let mut chosen: Vec<Candidate> = statement
            .query_map([], |row| {
                Ok(Candidate {
                    track: crate::track_from(row)?,
                    plays: row.get(base)?,
                    last_played: row.get(base + 1)?,
                    liked: row.get(base + 2)?,
                    added: row.get(base + 3)?,
                })
            })?
            .filter_map(Result::ok)
            .filter(|item| rules.matches(item, now))
            .collect();

        match rules.order {
            SmartOrder::Library => {}
            SmartOrder::Random => shuffle(&mut chosen, seed),
            SmartOrder::MostPlayed => chosen.sort_by_key(|item| std::cmp::Reverse(item.plays)),
            SmartOrder::RecentlyAdded => chosen.sort_by_key(|item| std::cmp::Reverse(item.added)),
            SmartOrder::LeastRecentlyPlayed => chosen.sort_by_key(|item| item.last_played.unwrap_or(i64::MIN)),
        }
        if let Some(limit) = rules.limit {
            chosen.truncate(limit as usize);
        }
        Ok(chosen.into_iter().map(|item| item.track).collect())
    }
}

/// Перемешивание Фишера — Йетса на простом генераторе xorshift.
pub fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed | 1;
    for index in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(index, (state % (index as u64 + 1)) as usize);
    }
}
