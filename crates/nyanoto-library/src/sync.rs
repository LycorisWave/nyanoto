//! Связка устройств через папку, которую синхронизирует сам пользователь. Сети не касаемся.
//! Каждый пишет свой файл и читает чужие: лайки (последнее действие), плейлисты (свежее), история.
//! Треки — по пути, а не вышло — по исполнителю и названию.

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Library, LibraryError};

/// Как узнать трек на другом устройстве.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackKey {
    /// Путь внутри корневой папки, через «/».
    pub rel: String,
    /// «исполнитель|название» в нижнем регистре; пусто, если тегов нет.
    pub tags: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMark {
    pub key: TrackKey,
    /// Когда, секунды Unix.
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPlaylist {
    pub name: String,
    pub updated: i64,
    /// Правила умного плейлиста в JSON; у обычного — `None`.
    pub rules: Option<String>,
    pub items: Vec<TrackKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPlay {
    pub key: TrackKey,
    pub at: i64,
    pub ms: i64,
    /// Отметки для плейлиста дня. В старых файлах их нет.
    #[serde(default)]
    pub flags: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFile {
    pub version: u32,
    pub device: String,
    pub written: i64,
    pub likes: Vec<SyncMark>,
    pub unlikes: Vec<SyncMark>,
    pub playlists: Vec<SyncPlaylist>,
    pub plays: Vec<SyncPlay>,
    /// Пропуски — тоже часть вкуса.
    #[serde(default)]
    pub skips: Vec<SyncPlay>,
}

/// Что принято из чужого файла.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub likes: usize,
    pub unlikes: usize,
    pub playlists: usize,
    pub plays: usize,
    /// Треков из чужого файла, которых здесь нет.
    pub unknown: usize,
}

fn tag_key(artist: Option<&str>, title: Option<&str>) -> String {
    match (artist, title) {
        (Some(artist), Some(title)) => format!("{}|{}", artist.trim().to_lowercase(), title.trim().to_lowercase()),
        _ => String::new(),
    }
}

/// Все треки базы с их ключами: по пути и по тегам.
struct Index {
    keys: HashMap<i64, TrackKey>,
    by_rel: HashMap<String, i64>,
    by_tags: HashMap<String, i64>,
}

impl Index {
    fn build(library: &Library) -> Result<Self, LibraryError> {
        let mut statement = library
            .connection
            .prepare("SELECT id, rel_path, artist, title FROM tracks WHERE missing = 0 ORDER BY id")?;
        let mut index = Index { keys: HashMap::new(), by_rel: HashMap::new(), by_tags: HashMap::new() };
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (id, rel, artist, title) = row?;
            let tags = tag_key(artist.as_deref(), title.as_deref());
            index.by_rel.entry(rel.clone()).or_insert(id);
            if !tags.is_empty() {
                index.by_tags.entry(tags.clone()).or_insert(id);
            }
            index.keys.insert(id, TrackKey { rel, tags });
        }
        Ok(index)
    }

    fn find(&self, key: &TrackKey) -> Option<i64> {
        self.by_rel
            .get(&key.rel)
            .or_else(|| (!key.tags.is_empty()).then(|| self.by_tags.get(&key.tags)).flatten())
            .copied()
    }
}

impl Library {
    /// Всё, что этот плеер отдаёт другим устройствам.
    pub fn sync_export(&self, device: &str) -> Result<SyncFile, LibraryError> {
        let index = Index::build(self)?;
        let key = |id: i64| index.keys.get(&id).cloned();
        let marks = |sql: &str| -> Result<Vec<SyncMark>, LibraryError> {
            let mut statement = self.connection.prepare(sql)?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
            let mut marks = Vec::new();
            for row in rows {
                let (id, at) = row?;
                if let Some(key) = key(id) {
                    marks.push(SyncMark { key, at });
                }
            }
            Ok(marks)
        };
        let likes = marks("SELECT track_id, added FROM favorites")?;
        let unlikes = marks("SELECT track_id, at FROM unliked")?;

        let mut playlists = Vec::new();
        let lists: Vec<(i64, String, i64, Option<String>)> = self
            .connection
            .prepare("SELECT id, name, COALESCE(updated, created), rules FROM playlists ORDER BY id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .collect::<Result<_, _>>()?;
        for (id, name, updated, rules) in lists {
            let items: Vec<i64> = self
                .connection
                .prepare_cached("SELECT track_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position")?
                .query_map([id], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            playlists.push(SyncPlaylist { name, updated, rules, items: items.into_iter().filter_map(key).collect() });
        }

        let events = |sql: &str| -> Result<Vec<SyncPlay>, LibraryError> {
            let mut statement = self.connection.prepare(sql)?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
            })?;
            let mut events = Vec::new();
            for row in rows {
                let (id, at, ms, flags) = row?;
                if let Some(key) = key(id) {
                    events.push(SyncPlay { key, at, ms, flags: flags as u8 });
                }
            }
            Ok(events)
        };
        let plays = events("SELECT track_id, at, listened_ms, flags FROM plays ORDER BY at")?;
        let skips = events("SELECT track_id, at, listened_ms, flags FROM skips ORDER BY at")?;
        let written = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64);
        Ok(SyncFile { version: 1, device: device.to_string(), written, likes, unlikes, playlists, plays, skips })
    }

    /// Принимает файл другого устройства.
    pub fn sync_import(&self, file: &SyncFile) -> Result<SyncReport, LibraryError> {
        let index = Index::build(self)?;
        let mut report = SyncReport::default();
        let mut unknown: HashSet<&TrackKey> = HashSet::new();
        let transaction = self.connection.unchecked_transaction()?;

        // Когда трек здесь лайкнули или сняли лайк последний раз.
        let local = |id: i64| -> Result<(Option<i64>, Option<i64>), LibraryError> {
            let liked: Option<i64> = transaction
                .prepare_cached("SELECT added FROM favorites WHERE track_id = ?1")?
                .query_row([id], |row| row.get(0))
                .optional()?;
            let unliked: Option<i64> = transaction
                .prepare_cached("SELECT at FROM unliked WHERE track_id = ?1")?
                .query_row([id], |row| row.get(0))
                .optional()?;
            Ok((liked, unliked))
        };
        for mark in &file.likes {
            let Some(id) = index.find(&mark.key) else {
                unknown.insert(&mark.key);
                continue;
            };
            let (liked, unliked) = local(id)?;
            if liked.is_none() && unliked.is_none_or(|at| at < mark.at) {
                transaction.execute("INSERT OR IGNORE INTO favorites (track_id, added) VALUES (?1, ?2)", (id, mark.at))?;
                transaction.execute("DELETE FROM unliked WHERE track_id = ?1", [id])?;
                report.likes += 1;
            }
        }
        for mark in &file.unlikes {
            let Some(id) = index.find(&mark.key) else {
                unknown.insert(&mark.key);
                continue;
            };
            let (liked, _) = local(id)?;
            if liked.is_some_and(|at| at < mark.at) {
                transaction.execute("DELETE FROM favorites WHERE track_id = ?1", [id])?;
                transaction.execute(
                    "INSERT INTO unliked (track_id, at) VALUES (?1, ?2)
                     ON CONFLICT (track_id) DO UPDATE SET at = excluded.at",
                    (id, mark.at),
                )?;
                report.unlikes += 1;
            }
        }

        for playlist in &file.playlists {
            let existing: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT id, COALESCE(updated, created) FROM playlists WHERE name = ?1",
                    [&playlist.name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let id = match existing {
                Some((_, updated)) if updated >= playlist.updated => continue,
                Some((id, _)) => id,
                None => {
                    transaction.execute(
                        "INSERT INTO playlists (name, created, updated) VALUES (?1, ?2, ?2)",
                        (&playlist.name, playlist.updated),
                    )?;
                    transaction.last_insert_rowid()
                }
            };
            transaction.execute(
                "UPDATE playlists SET rules = ?2, updated = ?3 WHERE id = ?1",
                params![id, playlist.rules, playlist.updated],
            )?;
            transaction.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
            let mut position = 0i64;
            for key in &playlist.items {
                match index.find(key) {
                    Some(track) => {
                        transaction.execute(
                            "INSERT INTO playlist_items (playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
                            (id, position, track),
                        )?;
                        position += 1;
                    }
                    None => {
                        unknown.insert(key);
                    }
                }
            }
            report.playlists += 1;
        }

        for play in &file.plays {
            let Some(id) = index.find(&play.key) else {
                unknown.insert(&play.key);
                continue;
            };
            let added = transaction.execute(
                "INSERT INTO plays (track_id, at, listened_ms, flags)
                 SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS
                     (SELECT 1 FROM plays WHERE track_id = ?1 AND at = ?2)",
                (id, play.at, play.ms, i64::from(play.flags)),
            )?;
            report.plays += added;
        }
        for skip in &file.skips {
            let Some(id) = index.find(&skip.key) else {
                continue;
            };
            transaction.execute(
                "INSERT INTO skips (track_id, at, listened_ms, flags)
                 SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS
                     (SELECT 1 FROM skips WHERE track_id = ?1 AND at = ?2)",
                (id, skip.at, skip.ms, i64::from(skip.flags)),
            )?;
        }
        transaction.commit()?;
        report.unknown = unknown.len();
        Ok(report)
    }
}
