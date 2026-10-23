//! Данные пользователя: настройки, лайки, плейлисты, история. Только в базе, не в файлах.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, params};

use crate::browse::{album_folder, artist_folder};
use crate::{Grouping, Library, LibraryError, TRACK_COLUMNS, Track};

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub tracks: u32,
    /// Умный плейлист: треки по правилам, `tracks` у него не считается.
    pub smart: bool,
}

/// Исполнитель для карты: сколько треков и времени, альбомы по убыванию.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphArtist {
    pub name: String,
    /// Прослушанных треков (каждое прослушивание).
    pub tracks: u32,
    pub listened_ms: u64,
    pub albums: Vec<GraphAlbum>,
    /// Папка исполнителя над альбомами — там может быть фото.
    pub folder: Option<PathBuf>,
}

/// Альбом на карте прослушиваний.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphAlbum {
    /// `None` — треки без альбома.
    pub title: Option<String>,
    /// Прослушано «целиком»: время / длина альбома (одна песня 50 минут подряд — один раз).
    pub plays: f32,
    pub listened_ms: u64,
    /// Папка альбома: в ней ищется обложка.
    pub folder: PathBuf,
}

/// Сохранённая очередь воспроизведения.
#[derive(Debug, Clone, Default)]
pub struct SavedQueue {
    pub tracks: Vec<Track>,
    /// Номер трека в `tracks`, на котором остановились.
    pub current: usize,
    pub position_ms: u64,
    /// Что это была за очередь (решает приложение: альбом, плейлист…).
    pub kind: Option<String>,
}

/// Сводка истории прослушиваний.
#[derive(Debug, Clone, Default)]
pub struct ListenStats {
    pub plays: u64,
    pub listened_ms: u64,
    /// Когда записано первое прослушивание, секунды Unix.
    pub since: Option<i64>,
    /// Самые слушаемые треки и сколько раз.
    pub top_tracks: Vec<(Track, u32)>,
    /// Самые слушаемые по времени: треков и мс.
    pub top_artists: Vec<(String, u32, u64)>,
}

/// Время / длина альбома. Без длины — ноль.
pub(crate) fn album_plays(listened_ms: u64, length_ms: i64) -> f32 {
    if length_ms <= 0 {
        return 0.0;
    }
    listened_ms as f32 / length_ms as f32
}

/// Сколько записей хранит журнал «что играло».
pub const HISTORY_LIMIT: i64 = 500;

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// Включена ли настройка-переключатель (значение «1»).
pub(crate) fn flag(connection: &rusqlite::Connection, key: &str) -> Result<bool, LibraryError> {
    let value: Option<String> = connection
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0))
        .optional()?;
    Ok(value.as_deref() == Some("1"))
}

impl Library {
    pub fn setting(&self, key: &str) -> Result<Option<String>, LibraryError> {
        Ok(self
            .connection
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), LibraryError> {
        self.connection.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    // ───────────────────────────── лайки ─────────────────────────────

    pub fn liked_ids(&self) -> Result<HashSet<i64>, LibraryError> {
        let mut statement = self.connection.prepare_cached("SELECT track_id FROM favorites")?;
        let ids = statement.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
        Ok(ids)
    }

    pub fn set_liked(&self, track: i64, liked: bool) -> Result<(), LibraryError> {
        // Когда сняли лайк — запоминаем: связка устройств переносит и это.
        if liked {
            self.connection.execute(
                "INSERT OR IGNORE INTO favorites (track_id, added) SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
                (track, now()),
            )?;
            self.connection.execute("DELETE FROM unliked WHERE track_id = ?1", [track])?;
        } else {
            self.connection.execute("DELETE FROM favorites WHERE track_id = ?1", [track])?;
            self.connection.execute(
                "INSERT INTO unliked (track_id, at) SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)
                 ON CONFLICT (track_id) DO UPDATE SET at = excluded.at",
                (track, now()),
            )?;
        }
        Ok(())
    }

    /// Лайкнутые треки, последние сверху.
    pub fn liked_tracks(&self) -> Result<Vec<Track>, LibraryError> {
        self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM favorites f
                 JOIN tracks t ON t.id = f.track_id JOIN roots r ON r.id = t.root_id
                 WHERE t.missing = 0 ORDER BY f.added DESC, t.rel_path"
            ),
            [],
        )
    }

    // ─────────────────────────── плейлисты ───────────────────────────

    pub fn playlists(&self) -> Result<Vec<Playlist>, LibraryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT p.id, p.name, COUNT(i.track_id), p.rules IS NOT NULL FROM playlists p
             LEFT JOIN playlist_items i ON i.playlist_id = p.id
             GROUP BY p.id ORDER BY p.name COLLATE NOCASE, p.id",
        )?;
        let playlists = statement
            .query_map([], |row| {
                Ok(Playlist { id: row.get(0)?, name: row.get(1)?, tracks: row.get(2)?, smart: row.get(3)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(playlists)
    }

    pub fn create_playlist(&self, name: &str) -> Result<i64, LibraryError> {
        self.connection.execute(
            "INSERT INTO playlists (name, created, updated) VALUES (?1, ?2, ?2)",
            (name.trim(), now()),
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn rename_playlist(&self, playlist: i64, name: &str) -> Result<(), LibraryError> {
        self.connection
            .execute("UPDATE playlists SET name = ?2, updated = ?3 WHERE id = ?1", (playlist, name.trim(), now()))?;
        Ok(())
    }

    pub fn delete_playlist(&self, playlist: i64) -> Result<(), LibraryError> {
        self.connection.execute("DELETE FROM playlists WHERE id = ?1", [playlist])?;
        Ok(())
    }

    /// В конец плейлиста. Повторы можно.
    pub fn add_to_playlist(&self, playlist: i64, tracks: &[i64]) -> Result<(), LibraryError> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut next: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?1",
            [playlist],
            |row| row.get(0),
        )?;
        {
            let mut insert = transaction.prepare_cached(
                "INSERT INTO playlist_items (playlist_id, position, track_id)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?3)",
            )?;
            for track in tracks {
                next += insert.execute(params![playlist, next, track])? as i64;
            }
        }
        transaction.execute("UPDATE playlists SET updated = ?2 WHERE id = ?1", (playlist, now()))?;
        transaction.commit()?;
        Ok(())
    }

    /// Треки с местом в плейлисте — по месту и убираем, даже дубли.
    pub fn playlist_tracks(&self, playlist: i64) -> Result<Vec<(i64, Track)>, LibraryError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS}, i.position FROM playlist_items i
             JOIN tracks t ON t.id = i.track_id JOIN roots r ON r.id = t.root_id
             WHERE i.playlist_id = ?1 AND t.missing = 0 ORDER BY i.position"
        ))?;
        let items = statement
            .query_map([playlist], |row| Ok((row.get(crate::TRACK_COLUMN_COUNT)?, crate::track_from(row)?)))?
            .collect::<Result<_, _>>()?;
        Ok(items)
    }

    pub fn remove_from_playlist(&self, playlist: i64, position: i64) -> Result<(), LibraryError> {
        self.connection.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND position = ?2",
            [playlist, position],
        )?;
        self.connection
            .execute("UPDATE playlists SET updated = ?2 WHERE id = ?1", (playlist, now()))?;
        Ok(())
    }

    // ─────────────────────── очередь воспроизведения ───────────────────────

    /// Запоминает очередь: треки по порядку, номер текущего и позицию в нём.
    pub fn save_queue(&self, tracks: &[i64], current: usize, position_ms: u64, kind: &str) -> Result<(), LibraryError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM queue_items", [])?;
        {
            let mut insert = transaction.prepare_cached(
                "INSERT INTO queue_items (position, track_id) SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?2)",
            )?;
            for (position, track) in tracks.iter().enumerate() {
                insert.execute((position as i64, track))?;
            }
        }
        let mut set = transaction.prepare_cached("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)")?;
        set.execute(("queue_current", current.to_string()))?;
        set.execute(("queue_position_ms", position_ms.to_string()))?;
        set.execute(("queue_kind", kind))?;
        drop(set);
        transaction.commit()?;
        Ok(())
    }

    /// Где остановились, без самой очереди: номер трека и позиция в нём.
    pub fn save_queue_place(&self, current: usize, position_ms: u64) -> Result<(), LibraryError> {
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut set = transaction.prepare_cached("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)")?;
            set.execute(("queue_current", current.to_string()))?;
            set.execute(("queue_position_ms", position_ms.to_string()))?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Сохранённая очередь без пропавших. Пропал текущий — следующий, с нуля.
    pub fn saved_queue(&self) -> Result<SavedQueue, LibraryError> {
        let current: Option<i64> = self.setting("queue_current")?.and_then(|value| value.parse().ok());
        let position_ms: u64 = self.setting("queue_position_ms")?.and_then(|value| value.parse().ok()).unwrap_or(0);
        let kind = self.setting("queue_kind")?;
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS}, q.position FROM queue_items q
             JOIN tracks t ON t.id = q.track_id JOIN roots r ON r.id = t.root_id
             WHERE t.missing = 0 ORDER BY q.position"
        ))?;
        let items: Vec<(i64, Track)> = statement
            .query_map([], |row| Ok((row.get(crate::TRACK_COLUMN_COUNT)?, crate::track_from(row)?)))?
            .collect::<Result<_, _>>()?;
        let wanted = current.unwrap_or(0);
        let index = items.iter().position(|(position, _)| *position >= wanted);
        let (current, position_ms) = match index {
            Some(index) if items[index].0 == wanted => (index, position_ms),
            Some(index) => (index, 0),
            None => (0, 0),
        };
        Ok(SavedQueue { tracks: items.into_iter().map(|(_, track)| track).collect(), current, position_ms, kind })
    }

    // ─────────────────────── журнал «что играло» ───────────────────────

    /// Трек начал играть. Тот же подряд — одна запись. Храним [`HISTORY_LIMIT`].
    pub fn add_history(&self, track: i64) -> Result<(), LibraryError> {
        let transaction = self.connection.unchecked_transaction()?;
        let last: Option<(i64, i64)> = transaction
            .query_row("SELECT id, track_id FROM history ORDER BY id DESC LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        match last {
            Some((id, last_track)) if last_track == track => {
                transaction.execute("UPDATE history SET at = ?2 WHERE id = ?1", (id, now()))?;
            }
            _ => {
                transaction.execute(
                    "INSERT INTO history (track_id, at) SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
                    (track, now()),
                )?;
                transaction.execute(
                    "DELETE FROM history WHERE id <= (SELECT id FROM history ORDER BY id DESC LIMIT 1 OFFSET ?1)",
                    [HISTORY_LIMIT],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Что играло, новое первым (секунды Unix). Пропавшие не показываем.
    pub fn history(&self, limit: usize) -> Result<Vec<(Track, i64)>, LibraryError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {TRACK_COLUMNS}, h.at FROM history h
             JOIN tracks t ON t.id = h.track_id JOIN roots r ON r.id = t.root_id
             WHERE t.missing = 0 ORDER BY h.id DESC LIMIT ?1"
        ))?;
        let rows = statement
            .query_map([limit as i64], |row| Ok((crate::track_from(row)?, row.get(crate::TRACK_COLUMN_COUNT)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn clear_history(&self) -> Result<(), LibraryError> {
        self.connection.execute("DELETE FROM history", [])?;
        Ok(())
    }

    // ─────────────────────── история прослушиваний ───────────────────────

    /// Записывает прослушивание. Вызывается, только если статистика включена.
    pub fn record_play(&self, track: i64, listened_ms: u64) -> Result<(), LibraryError> {
        self.record_play_as(track, listened_ms, 0)
    }

    /// Карта: `artists` исполнителей и до `albums` альбомов у каждого.
    /// CROSS JOIN держит порядок от `plays`, иначе SQLite идёт от всех треков.
    pub fn listen_graph(&self, grouping: Grouping, artists: usize, albums: usize) -> Result<Vec<GraphArtist>, LibraryError> {
        let (owner, album) = (grouping.owner(), grouping.album());
        // Длина альбома — всех его треков в библиотеке, а не только слушанных.
        let mut statement = self.connection.prepare(&format!(
            "WITH listened AS (
                 SELECT {owner} AS group_owner, {album} AS group_album, COUNT(*) AS times,
                        SUM(p.listened_ms) AS listened, MIN(r.path || '/' || t.rel_path) AS first
                 FROM plays p
                 CROSS JOIN tracks t ON t.id = p.track_id
                 JOIN roots r ON r.id = t.root_id
                 GROUP BY group_owner, group_album
             ), lengths AS (
                 SELECT {owner} AS group_owner, {album} AS group_album, SUM(t.duration_ms) AS length
                 FROM tracks t WHERE t.missing = 0
                 GROUP BY group_owner, group_album
             )
             SELECT l.group_owner, l.group_album, l.times, COALESCE(l.listened, 0), l.first, COALESCE(n.length, 0)
             FROM listened l
             LEFT JOIN lengths n ON n.group_owner IS l.group_owner AND n.group_album IS l.group_album"
        ))?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut found: Vec<GraphArtist> = Vec::new();
        for row in rows {
            let (name, title, tracks, listened, first, length) = row?;
            let listened_ms = listened.max(0) as u64;
            let album = GraphAlbum {
                folder: album_folder(grouping, Path::new(&first), title.as_deref()),
                title,
                plays: album_plays(listened_ms, length),
                listened_ms,
            };
            match found.iter_mut().find(|artist| artist.name == name) {
                Some(artist) => {
                    artist.tracks += tracks;
                    artist.listened_ms += listened_ms;
                    artist.albums.push(album);
                }
                None => found.push(GraphArtist { name, tracks, listened_ms, albums: vec![album], folder: None }),
            }
        }
        found.sort_by(|a, b| b.listened_ms.cmp(&a.listened_ms).then_with(|| a.name.cmp(&b.name)));
        found.truncate(artists);
        for artist in &mut found {
            artist.folder = artist.albums.iter().find_map(|album| artist_folder(&artist.name, &album.folder));
            artist.albums.sort_by(|a, b| b.listened_ms.cmp(&a.listened_ms).then_with(|| a.title.cmp(&b.title)));
            artist.albums.truncate(albums);
        }
        Ok(found)
    }

    pub fn listen_stats(&self, grouping: Grouping, top: usize) -> Result<ListenStats, LibraryError> {
        let (plays, listened_ms, since): (i64, i64, Option<i64>) = self.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(listened_ms), 0), MIN(at) FROM plays",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let mut statement = self.connection.prepare(&format!(
            "SELECT {TRACK_COLUMNS}, COUNT(*) AS times FROM plays p
             CROSS JOIN tracks t ON t.id = p.track_id JOIN roots r ON r.id = t.root_id
             GROUP BY t.id ORDER BY times DESC, MAX(p.at) DESC LIMIT ?1"
        ))?;
        let top_tracks = statement
            .query_map([top as i64], |row| Ok((crate::track_from(row)?, row.get(crate::TRACK_COLUMN_COUNT)?)))?
            .collect::<Result<_, _>>()?;

        let mut statement = self.connection.prepare(&format!(
            "SELECT {} AS group_owner, COUNT(*) AS times, COALESCE(SUM(p.listened_ms), 0) AS listened FROM plays p
             CROSS JOIN tracks t ON t.id = p.track_id
             GROUP BY group_owner ORDER BY listened DESC, times DESC LIMIT ?1",
            grouping.owner()
        ))?;
        let top_artists = statement
            .query_map([top as i64], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)?.max(0) as u64)))?
            .collect::<Result<_, _>>()?;

        Ok(ListenStats {
            plays: plays.max(0) as u64,
            listened_ms: listened_ms.max(0) as u64,
            since,
            top_tracks,
            top_artists,
        })
    }
}
