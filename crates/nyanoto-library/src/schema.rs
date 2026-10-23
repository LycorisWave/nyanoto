//! Схема базы и её открытие.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

/// Версия схемы. Хранится в `PRAGMA user_version`.
const VERSION: i32 = 21;

/// Элемент `n` переводит базу из версии `n + 1` в `n + 2`.
const MIGRATIONS: &[&str] = &[
    // 2: владелец трека — основной исполнитель альбома («A; B» → A).
    "ALTER TABLE tracks ADD COLUMN owner TEXT;
     UPDATE tracks SET owner = COALESCE(
         (SELECT value FROM track_values v
          WHERE v.track_id = tracks.id AND v.field = 1 AND v.position = 0),
         (SELECT value FROM track_values v
          WHERE v.track_id = tracks.id AND v.field = 0 AND v.position = 0));
     CREATE INDEX tracks_by_owner ON tracks (owner, album, disc_no, track_no);",
    // 3: обзор по папкам: исполнитель / альбом / всё глубже — тот же альбом.
    "ALTER TABLE tracks ADD COLUMN folder_artist TEXT;
     ALTER TABLE tracks ADD COLUMN folder_album TEXT;
     UPDATE tracks SET
         folder_artist = CASE WHEN instr(rel_path, '/') > 0
             THEN substr(rel_path, 1, instr(rel_path, '/') - 1) END,
         folder_album = CASE
             WHEN instr(substr(rel_path, instr(rel_path, '/') + 1), '/') > 0
              AND instr(rel_path, '/') > 0
             THEN substr(substr(rel_path, instr(rel_path, '/') + 1), 1,
                         instr(substr(rel_path, instr(rel_path, '/') + 1), '/') - 1) END;
     CREATE INDEX tracks_by_folder ON tracks (folder_artist, folder_album);",
    // 4: данные пользователя. Записи треков не удаляются — лайки переживают отключённый диск.
    "CREATE TABLE settings (
         key   TEXT PRIMARY KEY,
         value TEXT NOT NULL
     ) WITHOUT ROWID;
     CREATE TABLE favorites (
         track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
         added    INTEGER NOT NULL
     );
     CREATE TABLE playlists (
         id      INTEGER PRIMARY KEY,
         name    TEXT NOT NULL,
         created INTEGER NOT NULL
     );
     CREATE TABLE playlist_items (
         playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
         position    INTEGER NOT NULL,
         track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
         PRIMARY KEY (playlist_id, position)
     ) WITHOUT ROWID;
     CREATE TABLE plays (
         id          INTEGER PRIMARY KEY,
         track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
         at          INTEGER NOT NULL,
         listened_ms INTEGER NOT NULL
     );
     CREATE INDEX plays_by_track ON plays (track_id);",
    // 5: словарь полнотекстового индекса для поиска с опечатками.
    "CREATE VIRTUAL TABLE tracks_vocab USING fts5vocab (tracks_fts, row);",
    // 6: проверка подлинности качества и решения пользователя по ней.
    "CREATE TABLE inspections (
         track_id  INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
         size      INTEGER NOT NULL,
         mtime     INTEGER NOT NULL,
         verdict   INTEGER NOT NULL,
         cutoff_hz INTEGER,
         real_bits INTEGER,
         real_rate INTEGER
     );
     CREATE TABLE quality_overrides (
         track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
         mode     INTEGER NOT NULL,
         label    TEXT
     );",
    // 7: вердикты средней и высокой чувствительности; старые файлы проверятся заново.
    "ALTER TABLE inspections ADD COLUMN verdict_medium INTEGER;
     ALTER TABLE inspections ADD COLUMN verdict_high INTEGER;",
    // 8: классика — композитор, произведение, дирижёр. Всё перечитается.
    "ALTER TABLE tracks ADD COLUMN conductor TEXT;
     ALTER TABLE tracks ADD COLUMN work TEXT;
     ALTER TABLE tracks ADD COLUMN movement TEXT;
     ALTER TABLE tracks ADD COLUMN movement_no INTEGER;
     ALTER TABLE tracks ADD COLUMN movement_total INTEGER;
     DROP TABLE tracks_vocab;
     DROP TABLE tracks_fts;
     CREATE VIRTUAL TABLE tracks_fts USING fts5 (
         title, artist, album, album_artist, genre, path, composer, work, conductor,
         tokenize = 'unicode61 remove_diacritics 2',
         prefix = '2 3'
     );
     INSERT INTO tracks_fts (rowid, title, artist, album, album_artist, genre, path, composer)
         SELECT id, title, artist, album, album_artist, genre, rel_path, composer FROM tracks;
     CREATE VIRTUAL TABLE tracks_vocab USING fts5vocab (tracks_fts, row);
     UPDATE tracks SET mtime = -1;",
    // 9: длинные записи — где остановились и закладки.
    "CREATE TABLE positions (
         track_id    INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
         position_ms INTEGER NOT NULL,
         updated     INTEGER NOT NULL
     );
     CREATE TABLE bookmarks (
         id          INTEGER PRIMARY KEY,
         track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
         position_ms INTEGER NOT NULL,
         name        TEXT NOT NULL,
         created     INTEGER NOT NULL
     );
     CREATE INDEX bookmarks_by_track ON bookmarks (track_id, position_ms);",
    // 10: умные плейлисты — правила в JSON; у обычных плейлистов NULL.
    "ALTER TABLE playlists ADD COLUMN rules TEXT;",
    // 11: здоровье библиотеки — итог полной проверки каждого файла.
    "CREATE TABLE health (
         track_id      INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
         size          INTEGER NOT NULL,
         mtime         INTEGER NOT NULL,
         md5           INTEGER,
         decode_errors INTEGER NOT NULL,
         missing_ms    INTEGER,
         audio_ms      INTEGER NOT NULL,
         error         TEXT
     );",
    // 12: связка устройств — когда сняли лайк и когда менялся плейлист.
    "CREATE TABLE unliked (
         track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
         at       INTEGER NOT NULL
     );
     ALTER TABLE playlists ADD COLUMN updated INTEGER;
     UPDATE playlists SET updated = created;",
    // 13: DSD внутри WavPack lofty видит как «8 бит» — перечитать.
    "UPDATE tracks SET mtime = -1 WHERE format = 'wavpack' AND bit_depth = 8;",
    // 14: очередь переживает перезапуск.
    "CREATE TABLE queue_items (
         position INTEGER PRIMARY KEY,
         track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE
     );",
    // 15: гибридный WavPack без коррекции — не «без потерь», перечитать.
    "UPDATE tracks SET mtime = -1 WHERE format = 'wavpack';",
    // 16: журнал «что играло» — не статистика, ведётся всегда, последние 500.
    "CREATE TABLE history (
         id       INTEGER PRIMARY KEY,
         track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
         at       INTEGER NOT NULL
     );",
    // 17: индексы по тем же COALESCE, что в запросах, — иначе полный просмотр (50 мс).
    "DROP INDEX IF EXISTS tracks_by_owner;
     DROP INDEX IF EXISTS tracks_by_folder;
     CREATE INDEX tracks_by_folder_group ON tracks (COALESCE(folder_artist, ''), folder_album, missing);
     CREATE INDEX tracks_by_tag_group ON tracks (COALESCE(owner, ''), album, missing);
     CREATE INDEX tracks_by_composer_group ON tracks (COALESCE(composer, ''), COALESCE(work, album), missing);",
    // 18: база на четверть меньше: лишний индекс и копия текста в FTS не нужны.
    "DROP INDEX IF EXISTS track_values_by_value;
     CREATE VIRTUAL TABLE tracks_fts_slim USING fts5 (
         title, artist, album, album_artist, genre, path, composer, work, conductor,
         content = '', contentless_delete = 1,
         tokenize = 'unicode61 remove_diacritics 2',
         prefix = '2 3'
     );
     INSERT INTO tracks_fts_slim (rowid, title, artist, album, album_artist, genre, path, composer, work, conductor)
         SELECT rowid, title, artist, album, album_artist, genre, path, composer, work, conductor FROM tracks_fts;
     DROP TABLE tracks_vocab;
     DROP TABLE tracks_fts;
     ALTER TABLE tracks_fts_slim RENAME TO tracks_fts;
     CREATE VIRTUAL TABLE tracks_vocab USING fts5vocab (tracks_fts, row);",
    // 19: текст песни: бит 1 — `.lrc` рядом, бит 2 — в тегах.
    "ALTER TABLE tracks ADD COLUMN lyrics INTEGER NOT NULL DEFAULT 0;
     UPDATE tracks SET mtime = -1;",
    // 20: плейлист дня — отметки прослушиваний, пропуски, прошлые плейлисты.
    "ALTER TABLE plays ADD COLUMN flags INTEGER NOT NULL DEFAULT 0;
     CREATE INDEX plays_by_time ON plays (at);
     CREATE TABLE skips (
         id          INTEGER PRIMARY KEY,
         track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
         at          INTEGER NOT NULL,
         listened_ms INTEGER NOT NULL,
         flags       INTEGER NOT NULL DEFAULT 0
     );
     CREATE INDEX skips_by_track ON skips (track_id);
     CREATE INDEX skips_by_time ON skips (at);
     CREATE TABLE daily (
         day     INTEGER PRIMARY KEY,
         created INTEGER NOT NULL,
         tracks  TEXT NOT NULL
     );",
    // 21: своя раскладка у каждой папки; общий выбор переходит ко всем.
    "ALTER TABLE roots ADD COLUMN grouping TEXT;
     UPDATE roots SET grouping = (SELECT value FROM settings WHERE key = 'grouping');",
];

/// После этого перехода — VACUUM, иначе место не вернётся.
const SHRINKS_AT: i32 = 18;

/// Номера полей в таблице многозначных тегов.
pub(crate) const FIELD_ARTIST: i64 = 0;
pub(crate) const FIELD_ALBUM_ARTIST: i64 = 1;
pub(crate) const FIELD_GENRE: i64 = 2;
pub(crate) const FIELD_COMPOSER: i64 = 3;

const SCHEMA: &str = "
CREATE TABLE roots (
    id      INTEGER PRIMARY KEY,
    path    TEXT NOT NULL UNIQUE,
    -- Была ли папка доступна при последнем сканировании.
    online  INTEGER NOT NULL DEFAULT 1,
    -- Время последнего полного сканирования, секунды Unix.
    scanned INTEGER
);

CREATE TABLE tracks (
    id           INTEGER PRIMARY KEY,
    root_id      INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
    -- Путь внутри корневой папки, разделитель всегда '/'.
    rel_path     TEXT NOT NULL,
    size         INTEGER NOT NULL,
    mtime        INTEGER NOT NULL,
    -- Файла не оказалось на месте при сканировании доступной папки.
    -- Запись не удаляется: файл может вернуться.
    missing      INTEGER NOT NULL DEFAULT 0,
    -- Почему файл не удалось разобрать; NULL, если всё в порядке.
    problem      TEXT,
    format       TEXT,
    duration_ms  INTEGER,
    sample_rate  INTEGER,
    bit_depth    INTEGER,
    channels     INTEGER,
    bitrate      INTEGER,
    title        TEXT,
    artist       TEXT,
    album        TEXT,
    album_artist TEXT,
    genre        TEXT,
    composer     TEXT,
    year         INTEGER,
    track_no     INTEGER,
    track_total  INTEGER,
    disc_no      INTEGER,
    disc_total   INTEGER,
    sort_title        TEXT,
    sort_artist       TEXT,
    sort_album        TEXT,
    sort_album_artist TEXT,
    added        INTEGER NOT NULL,
    UNIQUE (root_id, rel_path)
);

CREATE INDEX tracks_by_album ON tracks (album_artist, album, disc_no, track_no);

-- Многозначные теги: каждый исполнитель и каждый жанр отдельной строкой.
CREATE TABLE track_values (
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    field    INTEGER NOT NULL,
    position INTEGER NOT NULL,
    value    TEXT NOT NULL,
    PRIMARY KEY (track_id, field, position)
) WITHOUT ROWID;

CREATE INDEX track_values_by_value ON track_values (field, value);

CREATE VIRTUAL TABLE tracks_fts USING fts5 (
    title, artist, album, album_artist, genre, path,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);
";

#[derive(Debug)]
pub enum LibraryError {
    Database(rusqlite::Error),
    Io(std::io::Error),
    /// База создана более новой версией плеера.
    NewerSchema(i32),
    /// Папка уже в библиотеке: она внутри этой папки музыки.
    InsideRoot(std::path::PathBuf),
    /// Имя папки нельзя записать текстом (не UTF-8).
    FolderName(std::path::PathBuf),
}

impl std::fmt::Display for LibraryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibraryError::Database(error) => write!(f, "ошибка базы: {error}"),
            LibraryError::Io(error) => write!(f, "ошибка чтения: {error}"),
            LibraryError::NewerSchema(version) => {
                write!(f, "база создана более новой версией плеера (схема {version})")
            }
            LibraryError::InsideRoot(folder) => write!(f, "папка уже в библиотеке: она внутри {}", folder.display()),
            LibraryError::FolderName(folder) => {
                write!(f, "в имени папки {} есть символы не в UTF-8 — переименуйте её", folder.display())
            }
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<rusqlite::Error> for LibraryError {
    fn from(error: rusqlite::Error) -> Self {
        LibraryError::Database(error)
    }
}

impl From<std::io::Error> for LibraryError {
    fn from(error: std::io::Error) -> Self {
        LibraryError::Io(error)
    }
}

/// Открывает базу для записи, создавая файл и схему при первом запуске.
pub(crate) fn open(path: &Path) -> Result<Connection, LibraryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    // WAL позволяет читать базу, пока сканер в неё пишет.
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(10))?;

    let mut version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > VERSION {
        return Err(LibraryError::NewerSchema(version));
    }
    if version == 0 {
        connection.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = 1; COMMIT;"))?;
        version = 1;
    }
    // Каждый переход — своя транзакция.
    let before = version;
    for (index, step) in MIGRATIONS.iter().enumerate().skip(version as usize - 1) {
        let target = index + 2;
        connection.execute_batch(&format!("BEGIN; {step} PRAGMA user_version = {target}; COMMIT;"))?;
    }
    // Новая база пустая — сжимать нечего; старая сжимается один раз.
    if before > 1 && before < SHRINKS_AT {
        connection.execute_batch("VACUUM;")?;
    }
    Ok(connection)
}

/// Открывает уже существующую базу только для чтения.
pub(crate) fn open_read_only(path: &Path) -> Result<Connection, LibraryError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(connection)
}
