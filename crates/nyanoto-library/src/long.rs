//! Длинные записи: место, на котором остановились, и закладки.

use rusqlite::{OptionalExtension, params};

use crate::{Library, LibraryError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub id: i64,
    pub position_ms: i64,
    pub name: String,
}

impl Library {
    /// Запоминает, где остановились в треке.
    pub fn save_position(&self, track: i64, position_ms: i64) -> Result<(), LibraryError> {
        self.connection
            .prepare_cached(
                "INSERT INTO positions (track_id, position_ms, updated) VALUES (?1, ?2, unixepoch())
                 ON CONFLICT (track_id) DO UPDATE SET position_ms = excluded.position_ms,
                                                      updated = excluded.updated",
            )?
            .execute(params![track, position_ms.max(0)])?;
        Ok(())
    }

    pub fn position(&self, track: i64) -> Result<Option<i64>, LibraryError> {
        Ok(self
            .connection
            .prepare_cached("SELECT position_ms FROM positions WHERE track_id = ?1")?
            .query_row([track], |row| row.get(0))
            .optional()?)
    }

    /// Трек дослушан: место больше не нужно.
    pub fn forget_position(&self, track: i64) -> Result<(), LibraryError> {
        self.connection
            .prepare_cached("DELETE FROM positions WHERE track_id = ?1")?
            .execute([track])?;
        Ok(())
    }

    pub fn bookmarks(&self, track: i64) -> Result<Vec<Bookmark>, LibraryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, position_ms, name FROM bookmarks WHERE track_id = ?1 ORDER BY position_ms, id",
        )?;
        let bookmarks = statement
            .query_map([track], |row| {
                Ok(Bookmark { id: row.get(0)?, position_ms: row.get(1)?, name: row.get(2)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(bookmarks)
    }

    pub fn add_bookmark(&self, track: i64, position_ms: i64, name: &str) -> Result<i64, LibraryError> {
        self.connection
            .prepare_cached(
                "INSERT INTO bookmarks (track_id, position_ms, name, created) VALUES (?1, ?2, ?3, unixepoch())",
            )?
            .execute(params![track, position_ms.max(0), name.trim()])?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn delete_bookmark(&self, id: i64) -> Result<(), LibraryError> {
        self.connection
            .prepare_cached("DELETE FROM bookmarks WHERE id = ?1")?
            .execute([id])?;
        Ok(())
    }
}
