//! Библиотека: сканер, теги, база и поиск.
//! Файлы только читаем. Пропавший файл или диск записи не стирает.

mod authenticity;
mod browse;
mod daily;
mod health;
mod long;
mod probe;
mod scan;
mod search;
mod smart;
mod sync;
mod schema;
mod tags;
mod text;
mod user;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Row};

pub use authenticity::{Authenticity, Check, CheckLevel, CheckRecord, QualityOverride};
pub use browse::{Album, Artist, Grouping, artist_folder, natural_cmp};
pub use daily::{
    DAILY_LENGTHS, DailyOptions, DailyReadiness, DailyThreshold, PLAY_CHOSEN, PLAY_DAILY, PLAY_SLEEPY, TrackFeel,
    daily_day, daily_order, genre_aggression, tempo_gap,
};
pub use health::{AlbumGap, Damage, HealthRecord, HealthReport};
pub use long::Bookmark;
pub use scan::{AUDIO_EXTENSIONS, ScanProgress, ScanReport};
pub use search::{TextMatcher, text_matches};
pub use smart::{Condition, SmartOrder, SmartRules, TextField, shuffle};
pub use sync::{SyncFile, SyncMark, SyncReport, TrackKey};
pub use schema::LibraryError;
pub use tags::{FileTags, TagField, copy_tags, read_fields, write_fields};
pub use text::{cp1251, decode_text, fix_mojibake};

/// Настройка: исправлять при сканировании русский текст в чужой кодировке.
pub const SETTING_FIX_ENCODING: &str = "tags_cp1251";
pub use user::{GraphAlbum, GraphArtist, HISTORY_LIMIT, ListenStats, Playlist, SavedQueue};

/// Корневая папка библиотеки.
#[derive(Debug, Clone)]
pub struct Root {
    pub id: i64,
    pub path: PathBuf,
    /// Была ли папка доступна при последнем сканировании.
    pub online: bool,
    /// Как раскладывать её музыку: по папкам, по тегам или по композиторам.
    pub grouping: Grouping,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u16>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub duration_ms: Option<i64>,
    pub format: Option<String>,
    /// Почему файл не удалось разобрать.
    pub problem: Option<String>,
    /// Основной исполнитель по тегам: под ним трек стоит в обзоре по тегам.
    pub owner: Option<String>,
    /// Папка исполнителя и папка альбома, в которых лежит файл.
    pub folder_artist: Option<String>,
    pub folder_album: Option<String>,
    pub bit_depth: Option<u8>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// Битрейт звука, кбит/с.
    pub bitrate: Option<u32>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub conductor: Option<String>,
    /// Произведение, часть и её номер — для классики.
    pub work: Option<String>,
    pub movement: Option<String>,
    pub movement_no: Option<u32>,
    /// Есть ли у трека текст песни: `.lrc` рядом или текст в тегах.
    pub lyrics: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub tracks: u64,
    pub missing: u64,
    pub problems: u64,
    pub duration_ms: u64,
}

/// Сколько столбцов в `TRACK_COLUMNS`: запросы с добавочными столбцами читают их с этого номера.
pub(crate) const TRACK_COLUMN_COUNT: usize = 27;

const TRACK_COLUMNS: &str = "t.id, r.path, t.rel_path, t.title, t.artist, t.album, \
     t.album_artist, t.year, t.track_no, t.duration_ms, t.format, t.problem, t.disc_no, t.owner, \
     t.folder_artist, t.folder_album, t.bit_depth, t.sample_rate, t.channels, t.bitrate, \
     t.genre, t.composer, t.conductor, t.work, t.movement, t.movement_no, t.lyrics";

fn track_from(row: &Row<'_>) -> rusqlite::Result<Track> {
    let root: String = row.get(1)?;
    let rel: String = row.get(2)?;
    Ok(Track {
        id: row.get(0)?,
        path: Path::new(&root).join(rel),
        title: row.get(3)?,
        artist: row.get(4)?,
        album: row.get(5)?,
        album_artist: row.get(6)?,
        year: row.get(7)?,
        track_no: row.get(8)?,
        disc_no: row.get(12)?,
        duration_ms: row.get(9)?,
        format: row.get(10)?,
        problem: row.get(11)?,
        owner: row.get(13)?,
        folder_artist: row.get(14)?,
        folder_album: row.get(15)?,
        bit_depth: row.get(16)?,
        sample_rate: row.get(17)?,
        channels: row.get(18)?,
        bitrate: row.get(19)?,
        genre: row.get(20)?,
        composer: row.get(21)?,
        conductor: row.get(22)?,
        work: row.get(23)?,
        movement: row.get(24)?,
        movement_no: row.get(25)?,
        lyrics: row.get::<_, i64>(26)? != 0,
    })
}

pub struct Library {
    connection: Connection,
    path: PathBuf,
    /// Словарь поиска: читается при первом неточном поиске.
    vocabulary: std::cell::RefCell<Option<search::Vocabulary>>,
}

impl Library {
    /// Отмечает, есть ли рядом `.lrc` (сохранили в редакторе или нашли при игре).
    pub fn set_lyrics_file(&self, track: i64, on: bool) -> Result<(), LibraryError> {
        self.connection.execute(
            "UPDATE tracks SET lyrics = (lyrics & ~1) | ?2 WHERE id = ?1",
            (track, i64::from(on)),
        )?;
        Ok(())
    }

    /// Открывает базу, создавая её при первом запуске.
    pub fn open(path: &Path) -> Result<Self, LibraryError> {
        Ok(Self {
            connection: schema::open(path)?,
            path: path.to_path_buf(),
            vocabulary: std::cell::RefCell::default(),
        })
    }

    /// Добавляет папку музыки. Одна музыка дважды не попадает:
    /// - папка внутри добавленной — [`LibraryError::InsideRoot`];
    /// - папка снаружи забирает треки вложенных вместе с лайками и статистикой.
    ///
    /// Имя не в UTF-8 — [`LibraryError::FolderName`]: пути в базе текстом.
    pub fn add_root(&self, path: &Path) -> Result<i64, LibraryError> {
        let absolute = clean(&std::path::absolute(path)?);
        let Some(text) = absolute.to_str() else {
            return Err(LibraryError::FolderName(absolute));
        };
        let roots = self.roots()?;
        if let Some(same) = roots.iter().find(|root| same_path(&root.path, &absolute)) {
            return Ok(same.id);
        }
        if let Some(outer) = roots.iter().find(|root| inside(&absolute, &root.path)) {
            return Err(LibraryError::InsideRoot(outer.path.clone()));
        }
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("INSERT INTO roots (path) VALUES (?1)", [text])?;
        let id = transaction.last_insert_rowid();
        let mut merged = false;
        for root in roots.iter().filter(|root| inside(&root.path, &absolute)) {
            let Some(prefix) = root.path.strip_prefix(&absolute).ok().and_then(slash_path) else {
                continue;
            };
            // Путь сменился — треки перечитает следующее сканирование.
            transaction.execute(
                "UPDATE tracks SET root_id = ?1, rel_path = ?2 || '/' || rel_path, mtime = -1 WHERE root_id = ?3",
                (id, &prefix, root.id),
            )?;
            transaction.execute("DELETE FROM roots WHERE id = ?1", [root.id])?;
            merged = true;
        }
        if merged {
            // Исполнитель и альбом по папкам считаются от новой папки музыки.
            transaction.execute(
                "UPDATE tracks SET
                     folder_artist = CASE WHEN instr(rel_path, '/') > 0
                         THEN substr(rel_path, 1, instr(rel_path, '/') - 1) END,
                     folder_album = CASE
                         WHEN instr(substr(rel_path, instr(rel_path, '/') + 1), '/') > 0
                          AND instr(rel_path, '/') > 0
                         THEN substr(substr(rel_path, instr(rel_path, '/') + 1), 1,
                                     instr(substr(rel_path, instr(rel_path, '/') + 1), '/') - 1) END
                 WHERE root_id = ?1",
                [id],
            )?;
        }
        transaction.commit()?;
        Ok(id)
    }

    /// Убирает папку с треками, лайками и историей. Файлы не трогает.
    pub fn remove_root(&self, id: i64) -> Result<(), LibraryError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM tracks_fts WHERE rowid IN (SELECT id FROM tracks WHERE root_id = ?1)", [id])?;
        transaction.execute("DELETE FROM track_values WHERE track_id IN (SELECT id FROM tracks WHERE root_id = ?1)", [id])?;
        transaction.execute("DELETE FROM roots WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Сколько треков в папке музыки — чтобы предупредить перед удалением.
    pub fn root_tracks(&self, id: i64) -> Result<u64, LibraryError> {
        let count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM tracks WHERE root_id = ?1 AND missing = 0", [id], |row| row.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Раскладка музыки этой папки.
    pub fn set_root_grouping(&self, root: i64, grouping: Grouping) -> Result<(), LibraryError> {
        self.connection.execute("UPDATE roots SET grouping = ?2 WHERE id = ?1", (root, grouping.code()))?;
        Ok(())
    }

    /// Без модуля классики раскладки по композиторам нет: такие папки — по папкам.
    pub fn drop_composer_roots(&self) -> Result<(), LibraryError> {
        self.connection.execute("UPDATE roots SET grouping = 'folders' WHERE grouping = 'composers'", [])?;
        Ok(())
    }

    pub fn roots(&self) -> Result<Vec<Root>, LibraryError> {
        let mut statement = self
            .connection
            .prepare("SELECT id, path, online, grouping FROM roots ORDER BY id")?;
        let roots = statement
            .query_map([], |row| {
                Ok(Root {
                    id: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    online: row.get(2)?,
                    grouping: Grouping::from_code(row.get::<_, Option<String>>(3)?.as_deref()),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(roots)
    }

    /// Сканирует все корневые папки. Неизменившиеся файлы не перечитываются.
    pub fn scan(&mut self, progress: &ScanProgress) -> Result<ScanReport, LibraryError> {
        scan::scan(&self.connection, &self.path, progress)
    }

    pub fn stats(&self) -> Result<Stats, LibraryError> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FILTER (WHERE missing = 0),
                    COUNT(*) FILTER (WHERE missing = 1),
                    COUNT(*) FILTER (WHERE missing = 0 AND problem IS NOT NULL),
                    COALESCE(SUM(duration_ms) FILTER (WHERE missing = 0), 0)
             FROM tracks",
            [],
            |row| {
                // SQLite хранит целые со знаком; счётчики отрицательными не бывают.
                let count = |index: usize| row.get::<_, i64>(index).map(|value| value.max(0) as u64);
                Ok(Stats {
                    tracks: count(0)?,
                    missing: count(1)?,
                    problems: count(2)?,
                    duration_ms: count(3)?,
                })
            },
        )?)
    }

    /// Окно из всех треков по «исполнитель, альбом, диск, номер».
    pub fn tracks(&self, limit: usize, offset: usize) -> Result<Vec<Track>, LibraryError> {
        self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks t JOIN roots r ON r.id = t.root_id
                 WHERE t.missing = 0
                 ORDER BY t.album_artist, t.album, t.disc_no, t.track_no, t.rel_path
                 LIMIT ?1 OFFSET ?2"
            ),
            (limit as i64, offset as i64),
        )
    }

    /// Файлы, которые не удалось разобрать, с причиной.
    pub fn problems(&self, limit: usize) -> Result<Vec<Track>, LibraryError> {
        self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks t JOIN roots r ON r.id = t.root_id
                 WHERE t.missing = 0 AND t.problem IS NOT NULL
                 ORDER BY t.rel_path LIMIT ?1"
            ),
            [limit as i64],
        )
    }

    /// Сколько треков изменит исправление кодировки и примеры «было → станет».
    pub fn encoding_preview(&self, examples: usize) -> Result<(usize, Vec<(String, String)>), LibraryError> {
        let mut statement = self.connection.prepare(
            "SELECT title, artist, album, album_artist, genre, composer FROM tracks WHERE missing = 0",
        )?;
        let mut rows = statement.query([])?;
        let (mut count, mut found) = (0, Vec::new());
        while let Some(row) = rows.next()? {
            let mut changed = false;
            for index in 0..6 {
                let Some(value) = row.get::<_, Option<String>>(index)? else {
                    continue;
                };
                if let Some(fixed) = text::fix_mojibake(&value) {
                    changed = true;
                    if found.len() < examples && !found.iter().any(|(was, _)| *was == value) {
                        found.push((value, fixed));
                    }
                }
            }
            count += usize::from(changed);
        }
        Ok((count, found))
    }

    /// Всё перечитается при следующем сканировании.
    pub fn set_fix_encoding(&self, on: bool) -> Result<(), LibraryError> {
        self.set_setting(SETTING_FIX_ENCODING, if on { "1" } else { "0" })?;
        self.connection.execute("UPDATE tracks SET mtime = -1", [])?;
        Ok(())
    }

    /// Все значения многозначного тега у трека: по строке на исполнителя.
    pub fn artists_of(&self, track: i64) -> Result<Vec<String>, LibraryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT value FROM track_values WHERE track_id = ?1 AND field = ?2 ORDER BY position",
        )?;
        let values = statement
            .query_map((track, schema::FIELD_ARTIST), |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(values)
    }

    pub(crate) fn query(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Track>, LibraryError> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let tracks = statement.query_map(params, track_from)?.collect::<Result<_, _>>()?;
        Ok(tracks)
    }
}

pub const DATA_FOLDER: &str = "nyanoto";
/// Старая папка, переезжает при первом запуске.
const OLD_DATA_FOLDER: &str = "nekori";

/// Одна настройка до окна (чем его рисовать): только чтение, схему не трогаем.
pub fn peek_setting(database: &Path, key: &str) -> Option<String> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(database, flags).ok()?;
    connection.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0)).ok()
}

/// База в папке данных; старая `nekori` переезжает целиком.
pub fn default_database() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    }?;
    let folder = base.join(DATA_FOLDER);
    let old = base.join(OLD_DATA_FOLDER);
    if !folder.exists() && old.join("library.db").is_file() {
        // Не вышло — начнём с чистой базы, старая останется как была.
        let _ = std::fs::rename(&old, &folder);
    }
    Some(folder.join("library.db"))
}

/// Качество звука: формат, разрядность, частота, каналы, битрейт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quality {
    /// Название формата для показа: FLAC, MP3, WAV…
    pub format: &'static str,
    pub lossless: bool,
    /// Без потерь и больше 16 бит или 48 кГц.
    pub hires: bool,
    /// DSD (DSF, DFF, DST): однобитный звук, свой уровень качества.
    pub dsd: bool,
    /// Разрядность; у форматов с потерями её нет.
    pub bit_depth: Option<u8>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// Битрейт, кбит/с.
    pub bitrate: Option<u32>,
}

/// Во сколько раз частота DSD выше DSD64 (64 × 44,1 или 48 кГц); 0 — не DSD.
fn dsd_multiple(rate: u32) -> u32 {
    [44_100u32, 48_000].into_iter().find(|base| rate.is_multiple_of(base * 64)).map_or(0, |base| rate / (base * 64))
}

pub fn quality(track: &Track) -> Quality {
    let format = track.format.as_deref().unwrap_or_default();
    let name = match format {
        "flac" => "FLAC",
        "mpeg" => "MP3",
        "wav" => "WAV",
        "aiff" => "AIFF",
        "mp4" => "M4A",
        "vorbis" => "OGG Vorbis",
        "opus" => "Opus",
        "ape" => "APE",
        "wavpack" => "WavPack",
        "wavpack-lossy" => "WavPack Hybrid",
        "mpc" => "Musepack",
        "tta" => "TTA",
        // DSD64, DSD128…: кратность 44,1 (или 48) кГц × 64.
        "dsd" => match track.sample_rate.map(dsd_multiple) {
            Some(1) => "DSD64",
            Some(2) => "DSD128",
            Some(4) => "DSD256",
            Some(8) => "DSD512",
            _ => "DSD",
        },
        "aac" => "AAC",
        "alac" => "ALAC",
        "pcm" => "PCM",
        "speex" => "Speex",
        // Через FFmpeg: имя кодека FFmpeg.
        "wmav1" | "wmav2" => "WMA",
        "wmapro" => "WMA Pro",
        "wmalossless" => "WMA Lossless",
        "wmavoice" => "WMA Voice",
        "ac3" => "AC3",
        "eac3" => "E-AC3",
        "dts" => "DTS",
        "dts-hd-ma" => "DTS-HD MA",
        "dts-hd-hra" => "DTS-HD HRA",
        "truehd" => "TrueHD",
        "mlp" => "MLP",
        "tak" => "TAK",
        "dst" => "DSD (DST)",
        "atrac1" | "atrac3" | "atrac3p" | "atrac9" => "ATRAC",
        "atrac3al" | "atrac3pal" => "ATRAC Lossless",
        "shorten" => "Shorten",
        "mp4als" => "MPEG-4 ALS",
        "ralf" => "RealAudio Lossless",
        "cook" | "sipr" | "ra_144" | "ra_288" => "RealAudio",
        "mp1" => "MP1",
        "mp2" => "MP2",
        "mp3" => "MP3",
        "amr_nb" | "amr_wb" => "AMR",
        "osq" => "OSQ",
        "bonk" => "Bonk",
        "rka" => "RK Audio",
        "wavarc" => "WavArc",
        "qoa" => "QOA",
        "hca" => "HCA",
        "sbc" => "SBC",
        "aptx" | "aptx_hd" => "aptX",
        name if name.starts_with("pcm_") => "PCM",
        name if name.starts_with("adpcm_") => "ADPCM",
        _ => "?",
    };
    // У M4A без потерь (ALAC) есть разрядность, у AAC — нет.
    let dsd = matches!(format, "dsd" | "dst");
    let lossless = matches!(
        format,
        "flac" | "wav" | "aiff" | "ape" | "wavpack" | "alac" | "pcm" | "tta" | "dsd" | "dst" | "wmalossless"
            | "dts-hd-ma" | "truehd" | "mlp" | "tak" | "atrac3al" | "atrac3pal" | "shorten" | "mp4als" | "ralf"
            | "osq" | "bonk" | "rka" | "wavarc"
    ) || format.starts_with("pcm_")
        || (format == "mp4" && track.bit_depth.is_some());
    let hires = lossless
        && (track.bit_depth.is_some_and(|bits| bits > 16)
            || track.sample_rate.is_some_and(|rate| rate > 48_000));

    Quality {
        format: name,
        lossless,
        hires,
        dsd,
        // У DSD разрядность (1 бит) и частота в мегагерцах уже сказаны именем «DSD64».
        bit_depth: track.bit_depth.filter(|_| lossless && !dsd),
        sample_rate: track.sample_rate.filter(|_| !dsd),
        channels: track.channels,
        bitrate: track.bitrate.filter(|bitrate| *bitrate > 0),
    }
}

/// Версия встроенной SQLite: для «О приложении».
pub fn sqlite_version() -> &'static str {
    rusqlite::version()
}

/// Путь без `.` и `..` и без лишних разделителей в конце.
fn clean(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            part => clean.push(part.as_os_str()),
        }
    }
    clean
}

/// Путь с разделителем `/`, как `rel_path` в базе; `None`, если он не текст.
fn slash_path(path: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = path.components().map(|part| part.as_os_str().to_str()).collect();
    parts.map(|parts| parts.join("/")).filter(|path| !path.is_empty())
}

/// Сравнение путей: в Windows без учёта регистра, как в её файловой системе.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// `path` лежит внутри `folder` (и не совпадает с ней).
fn inside(path: &Path, folder: &Path) -> bool {
    if same_path(path, folder) {
        return false;
    }
    if cfg!(windows) {
        let (path, folder) = (path.to_string_lossy().to_lowercase(), folder.to_string_lossy().to_lowercase());
        Path::new(&path).starts_with(Path::new(&folder))
    } else {
        path.starts_with(folder)
    }
}
