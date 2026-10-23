//! Сканирование: обход → сверка с базой → теги (несколько потоков) → запись.
//! Очереди ограничены — память не растёт с библиотекой, медленный этап просто тормозит быстрый.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Statement, params, params_from_iter};

use crate::schema::{
    self, FIELD_ALBUM_ARTIST, FIELD_ARTIST, FIELD_COMPOSER, FIELD_GENRE, LibraryError,
};
use crate::tags::{self, Tags};

/// Расширения, по которым сканер отбирает файлы.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "m4b", "mp4", "aac", "ogg", "oga", "opus", "wav", "wave", "aiff", "aif",
    "caf", "mka", "webm", "wv", "ape", "tta", "mpc", "dsf", "dff", "wma", "ac3", "dts",
    // Через FFmpeg (модуль редких форматов).
    "eac3", "ec3", "thd", "mlp", "dtshd", "tak", "shn", "oma", "aa3", "at3", "ra", "rka", "w64", "au",
];

/// Глубже этого сканер не спускается: защита от зацикленных ссылок.
const MAX_DEPTH: usize = 64;

/// Сколько записей уходит в базу одной транзакцией.
const BATCH: usize = 1000;

/// Колонки трека, которые сканер перезаписывает при каждом чтении файла.
const SCANNED_COLUMNS: &[&str] = &[
    "size",
    "mtime",
    "problem",
    "format",
    "duration_ms",
    "sample_rate",
    "bit_depth",
    "channels",
    "bitrate",
    "title",
    "artist",
    "album",
    "album_artist",
    "genre",
    "composer",
    "year",
    "track_no",
    "track_total",
    "disc_no",
    "disc_total",
    "sort_title",
    "sort_artist",
    "sort_album",
    "sort_album_artist",
    "owner",
    "folder_artist",
    "folder_album",
    "conductor",
    "work",
    "movement",
    "movement_no",
    "movement_total",
    "lyrics",
];

/// Биты столбца `lyrics`: `.lrc` рядом с файлом и текст в тегах.
const LYRICS_FILE: i64 = 1;
const LYRICS_TAG: i64 = 2;

/// Ход сканирования. Счётчики можно читать из другого потока, пока оно идёт.
#[derive(Default)]
pub struct ScanProgress {
    /// Найдено звуковых файлов.
    pub found: AtomicU64,
    /// Из них не изменились с прошлого раза и не перечитывались.
    pub unchanged: AtomicU64,
    /// Прочитано тегов.
    pub read: AtomicU64,
    /// Файлов, которые не удалось разобрать.
    pub problems: AtomicU64,
    /// Обход папки закончен: `found − unchanged` — сколько всего тегов читать.
    pub walked: AtomicBool,
    cancel: AtomicBool,
}

impl ScanProgress {
    /// Просит сканер остановиться. Уже прочитанное сохраняется.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub found: u64,
    pub unchanged: u64,
    pub added: u64,
    pub updated: u64,
    /// Файлы, попавшие в библиотеку с пометкой о проблеме.
    pub problems: u64,
    /// Файлы, которых больше нет на месте. Записи о них сохранены.
    pub missing: u64,
    /// Файлы, которые считались пропавшими и вернулись.
    pub revived: u64,
    /// Файлы с именами, которые нельзя записать в базу как текст.
    pub skipped_names: u64,
    /// Папки, которые сейчас недоступны. Их треки не тронуты.
    pub offline_roots: Vec<PathBuf>,
    /// Обход неполный (отмена, нечитаемая папка) — пропавшие не отмечаем.
    pub incomplete: bool,
    pub elapsed: Duration,
}

struct FileEntry {
    rel: String,
    size: i64,
    mtime: i64,
    /// Номер записи, если файл уже есть в базе, но изменился.
    existing: Option<i64>,
    /// Лежит ли рядом `.lrc` с тем же именем.
    lyrics_file: bool,
}

enum Out {
    Read { entry: FileEntry, tags: Box<Tags> },
    Revive(i64),
    /// Файл не менялся, но рядом появился или пропал `.lrc`.
    LyricsFile(i64, bool),
}

struct Walked {
    found: u64,
    complete: bool,
    skipped_names: u64,
}

/// Множество номеров записей, чьи файлы встретились при обходе.
struct Seen(Vec<u64>);

impl Seen {
    fn new(max_id: i64) -> Self {
        Self(vec![0; max_id.max(0) as usize / 64 + 1])
    }

    fn insert(&mut self, id: i64) {
        if let Some(word) = self.0.get_mut(id as usize / 64) {
            *word |= 1 << (id as usize % 64);
        }
    }

    fn contains(&self, id: i64) -> bool {
        self.0
            .get(id as usize / 64)
            .is_some_and(|word| word & (1 << (id as usize % 64)) != 0)
    }
}

pub(crate) fn scan(
    connection: &Connection,
    database: &Path,
    progress: &ScanProgress,
) -> Result<ScanReport, LibraryError> {
    let started = Instant::now();
    let mut report = ScanReport::default();
    let roots: Vec<(i64, String)> = connection
        .prepare("SELECT id, path FROM roots ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;

    for (root_id, path) in roots {
        if progress.is_cancelled() {
            report.incomplete = true;
            break;
        }
        scan_root(connection, database, root_id, Path::new(&path), progress, &mut report)?;
    }
    report.found = progress.found.load(Ordering::Relaxed);
    report.unchanged = progress.unchanged.load(Ordering::Relaxed);
    report.elapsed = started.elapsed();
    Ok(report)
}

fn scan_root(
    connection: &Connection,
    database: &Path,
    root_id: i64,
    root: &Path,
    progress: &ScanProgress,
    report: &mut ScanReport,
) -> Result<(), LibraryError> {
    let offline = |report: &mut ScanReport| -> Result<(), LibraryError> {
        connection.execute("UPDATE roots SET online = 0 WHERE id = ?1", [root_id])?;
        report.offline_roots.push(root.to_path_buf());
        Ok(())
    };
    // Недоступная папка — не повод трогать её треки: диск может вернуться.
    if std::fs::read_dir(root).is_err() {
        return offline(report);
    }

    let (max_id, known): (i64, i64) = connection.query_row(
        "SELECT (SELECT COALESCE(MAX(id), 0) FROM tracks),
                (SELECT COUNT(*) FROM tracks WHERE root_id = ?1 AND missing = 0)",
        [root_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let fix_encoding = crate::user::flag(connection, crate::SETTING_FIX_ENCODING)?;
    let workers = std::thread::available_parallelism()
        .map_or(4, |count| count.get())
        .clamp(2, 8);

    let (found_tx, found_rx) = mpsc::sync_channel::<FileEntry>(2048);
    let (work_tx, work_rx) = mpsc::sync_channel::<FileEntry>(256);
    let (out_tx, out_rx) = mpsc::sync_channel::<Out>(512);
    // Читатели закончили — очередь закрыта, предыдущий этап тоже встаёт.
    let work_rx = Arc::new(Mutex::new(work_rx));

    let (walked, seen) = std::thread::scope(|scope| {
        let walker = scope.spawn(move || walk(root, &found_tx, progress));
        let filter = {
            let out_tx = out_tx.clone();
            scope.spawn(move || {
                filter(database, root_id, max_id, found_rx, &work_tx, &out_tx, progress)
            })
        };
        for _ in 0..workers {
            let (work_rx, out_tx) = (work_rx.clone(), out_tx.clone());
            scope.spawn(move || read_tags(root, fix_encoding, &work_rx, &out_tx, progress));
        }
        drop(work_rx);
        drop(out_tx);

        let written = write(connection, root_id, out_rx, progress, report);
        let walked = walker.join().unwrap_or(Walked {
            found: 0,
            complete: false,
            skipped_names: 0,
        });
        let seen = filter.join().unwrap_or_else(|_| Ok(Seen::new(0)));
        written.and(seen).map(|seen| (walked, seen))
    })?;

    report.skipped_names += walked.skipped_names;
    if !walked.complete || progress.is_cancelled() {
        report.incomplete = true;
        return Ok(());
    }
    // Пусто, хотя музыка была, — диск не примонтирован. Считаем недоступной.
    if walked.found == 0 && known > 0 {
        return offline(report);
    }

    let gone: Vec<i64> = connection
        .prepare("SELECT id FROM tracks WHERE root_id = ?1 AND missing = 0 AND id <= ?2")?
        .query_map((root_id, max_id), |row| row.get(0))?
        .filter_map(Result::ok)
        .filter(|id| !seen.contains(*id))
        .collect();
    for chunk in gone.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(", ");
        connection.execute(
            &format!("UPDATE tracks SET missing = 1 WHERE id IN ({marks})"),
            params_from_iter(chunk),
        )?;
    }
    report.missing += gone.len() as u64;
    connection.execute(
        "UPDATE roots SET online = 1, scanned = ?2 WHERE id = ?1",
        (root_id, unix_now()),
    )?;
    Ok(())
}

/// Обходит папку и отправляет дальше все звуковые файлы.
fn walk(root: &Path, found: &SyncSender<FileEntry>, progress: &ScanProgress) -> Walked {
    progress.walked.store(false, Ordering::Relaxed);
    let walked = walk_tree(root, found, progress);
    progress.walked.store(true, Ordering::Relaxed);
    walked
}

fn walk_tree(root: &Path, found: &SyncSender<FileEntry>, progress: &ScanProgress) -> Walked {
    let mut walked = Walked {
        found: 0,
        complete: true,
        skipped_names: 0,
    };
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    // Ссылки: внутрь папки и по кругу не ходим (иначе дубли и вечный обход), наружу — один раз.
    let real_root = std::fs::canonicalize(root).ok();
    let mut linked: HashSet<PathBuf> = HashSet::new();
    while let Some((dir, depth)) = stack.pop() {
        if progress.is_cancelled() {
            walked.complete = false;
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            walked.complete = false;
            continue;
        };
        // Папка читается целиком: текст `.lrc` может встретиться позже своего трека.
        let mut listed = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => listed.push(entry),
                Err(_) => walked.complete = false,
            }
        }
        let lyric_names: HashSet<std::ffi::OsString> = listed
            .iter()
            .map(|entry| PathBuf::from(entry.file_name()))
            .filter(|name| name.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("lrc")))
            .filter_map(|name| name.file_stem().map(|stem| stem.to_os_string()))
            .collect();
        for entry in listed {
            // Скрытое: корзина флешки, `._01.flac` от macOS.
            if entry.file_name().as_encoded_bytes().starts_with(b".") {
                continue;
            }
            // Корзина и точки восстановления Windows.
            let name = entry.file_name();
            if SYSTEM_FOLDERS.iter().any(|system| name.eq_ignore_ascii_case(system)) {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                walked.complete = false;
                continue;
            };
            let is_dir = if kind.is_symlink() { path.is_dir() } else { kind.is_dir() };
            if is_dir {
                if kind.is_symlink() {
                    let Ok(target) = std::fs::canonicalize(&path) else {
                        walked.complete = false;
                        continue;
                    };
                    let inside = real_root.as_deref().is_some_and(|real_root| target.starts_with(real_root));
                    if inside || !linked.insert(target) {
                        continue;
                    }
                }
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !is_audio(&path) {
                continue;
            }
            let metadata = if kind.is_symlink() {
                std::fs::metadata(&path)
            } else {
                entry.metadata()
            };
            let Ok(metadata) = metadata else {
                walked.complete = false;
                continue;
            };
            let Some(rel) = relative(root, &path) else {
                walked.skipped_names += 1;
                continue;
            };
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| i64::try_from(since.as_nanos()).unwrap_or(i64::MAX));
            walked.found += 1;
            progress.found.fetch_add(1, Ordering::Relaxed);
            let lyrics_file = path.file_stem().is_some_and(|stem| lyric_names.contains(stem));
            let entry = FileEntry {
                rel,
                size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                mtime,
                existing: None,
                lyrics_file,
            };
            if found.send(entry).is_err() {
                walked.complete = false;
                return walked;
            }
        }
    }
    walked
}

pub(crate) fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            AUDIO_EXTENSIONS.iter().any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Служебные папки Windows, которые сканер обходит стороной.
const SYSTEM_FOLDERS: [&str; 3] = ["$RECYCLE.BIN", "RECYCLER", "System Volume Information"];

/// Путь внутри корня через `/`. `None` — имя не текст.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let mut rel = String::new();
    for part in path.strip_prefix(root).ok()?.components() {
        if !rel.is_empty() {
            rel.push('/');
        }
        rel.push_str(part.as_os_str().to_str()?);
    }
    Some(rel)
}

/// Неизменённые с прошлого раза файлы пропускаем.
fn filter(
    database: &Path,
    root_id: i64,
    max_id: i64,
    found: Receiver<FileEntry>,
    work: &SyncSender<FileEntry>,
    out: &SyncSender<Out>,
    progress: &ScanProgress,
) -> Result<Seen, LibraryError> {
    let connection = schema::open_read_only(database)?;
    let mut lookup = connection
        .prepare("SELECT id, size, mtime, missing, problem IS NOT NULL, lyrics FROM tracks WHERE root_id = ?1 AND rel_path = ?2")?;
    let mut seen = Seen::new(max_id);
    for mut entry in found {
        let known: Option<(i64, i64, i64, bool, bool, i64)> = lookup
            .query_row((root_id, &entry.rel), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
            })
            .optional()?;
        if let Some((id, size, mtime, missing, problem, lyrics)) = known {
            seen.insert(id);
            // Не прочитался в прошлый раз — пробуем снова, вдруг уже умеем.
            if size == entry.size && mtime == entry.mtime && !problem {
                progress.unchanged.fetch_add(1, Ordering::Relaxed);
                if missing && out.send(Out::Revive(id)).is_err() {
                    break;
                }
                if (lyrics & LYRICS_FILE != 0) != entry.lyrics_file && out.send(Out::LyricsFile(id, entry.lyrics_file)).is_err() {
                    break;
                }
                continue;
            }
            entry.existing = Some(id);
        }
        if work.send(entry).is_err() {
            break;
        }
    }
    Ok(seen)
}

fn read_tags(
    root: &Path,
    fix_encoding: bool,
    work: &Mutex<Receiver<FileEntry>>,
    out: &SyncSender<Out>,
    progress: &ScanProgress,
) {
    loop {
        let next = work.lock().unwrap_or_else(|error| error.into_inner()).recv();
        let Ok(entry) = next else {
            return;
        };
        let tags = Box::new(tags::read(&root.join(&entry.rel), fix_encoding));
        progress.read.fetch_add(1, Ordering::Relaxed);
        if tags.problem.is_some() {
            progress.problems.fetch_add(1, Ordering::Relaxed);
        }
        if out.send(Out::Read { entry, tags }).is_err() {
            return;
        }
    }
}

/// Записывает результаты в базу пачками.
fn write(
    connection: &Connection,
    root_id: i64,
    results: Receiver<Out>,
    progress: &ScanProgress,
    report: &mut ScanReport,
) -> Result<(), LibraryError> {
    let names = SCANNED_COLUMNS.join(", ");
    let marks: Vec<String> = (0..SCANNED_COLUMNS.len()).map(|index| format!("?{}", index + 4)).collect();
    let updates: Vec<String> = SCANNED_COLUMNS
        .iter()
        .map(|column| format!("{column} = excluded.{column}"))
        .collect();
    let mut upsert = connection.prepare(&format!(
        "INSERT INTO tracks (root_id, rel_path, added, missing, {names})
         VALUES (?1, ?2, ?3, 0, {})
         ON CONFLICT (root_id, rel_path) DO UPDATE SET missing = 0, {}
         RETURNING id",
        marks.join(", "),
        updates.join(", ")
    ))?;
    let now = unix_now();

    connection.execute_batch("BEGIN IMMEDIATE")?;
    let stored = (|| -> Result<(), LibraryError> {
        let mut in_batch = 0;
        for result in results {
            match result {
                Out::Revive(id) => {
                    connection
                        .prepare_cached("UPDATE tracks SET missing = 0 WHERE id = ?1")?
                        .execute([id])?;
                    report.revived += 1;
                }
                Out::LyricsFile(id, on) => {
                    connection
                        .prepare_cached("UPDATE tracks SET lyrics = (lyrics & ?2) | ?3 WHERE id = ?1")?
                        .execute(params![id, !LYRICS_FILE, if on { LYRICS_FILE } else { 0 }])?;
                }
                Out::Read { entry, tags } => {
                    store(connection, &mut upsert, root_id, &entry, &tags, now)?;
                    match entry.existing {
                        Some(_) => report.updated += 1,
                        None => report.added += 1,
                    }
                    report.problems += u64::from(tags.problem.is_some());
                }
            }
            in_batch += 1;
            // Отмена не должна ждать, пока опустеет вся очередь.
            if in_batch >= BATCH || progress.is_cancelled() {
                connection.execute_batch("COMMIT; BEGIN IMMEDIATE")?;
                in_batch = 0;
            }
        }
        Ok(())
    })();
    match stored {
        Ok(()) => Ok(connection.execute_batch("COMMIT")?),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn store(
    connection: &Connection,
    upsert: &mut Statement<'_>,
    root_id: i64,
    entry: &FileEntry,
    tags: &Tags,
    now: i64,
) -> Result<(), LibraryError> {
    let (artist, album_artist) = (tags.artist(), tags.album_artist());
    let (genre, composer) = (tags.genre(), tags.composer());
    // Папки над файлом: «Исполнитель/Альбом/…/файл».
    let folders: Vec<&str> = entry.rel.split('/').collect();
    let folder_artist = (folders.len() >= 2).then(|| folders[0]);
    let folder_album = (folders.len() >= 3).then(|| folders[1]);
    let id: i64 = upsert.query_row(
        params![
            root_id,
            entry.rel,
            now,
            entry.size,
            entry.mtime,
            tags.problem,
            tags.format,
            tags.duration_ms,
            tags.sample_rate,
            tags.bit_depth,
            tags.channels,
            tags.bitrate,
            tags.title,
            artist,
            tags.album,
            album_artist,
            genre,
            composer,
            tags.year,
            tags.track_no,
            tags.track_total,
            tags.disc_no,
            tags.disc_total,
            tags.sort_title,
            tags.sort_artist,
            tags.sort_album,
            tags.sort_album_artist,
            tags.owner(),
            folder_artist,
            folder_album,
            tags.conductor,
            tags.work,
            tags.movement,
            tags.movement_no,
            tags.movement_total,
            if entry.lyrics_file { LYRICS_FILE } else { 0 } | if tags.lyrics { LYRICS_TAG } else { 0 },
        ],
        |row| row.get(0),
    )?;

    if entry.existing.is_some() {
        connection
            .prepare_cached("DELETE FROM track_values WHERE track_id = ?1")?
            .execute([id])?;
        connection
            .prepare_cached("DELETE FROM tracks_fts WHERE rowid = ?1")?
            .execute([id])?;
    }
    let mut insert_value = connection.prepare_cached(
        "INSERT INTO track_values (track_id, field, position, value) VALUES (?1, ?2, ?3, ?4)",
    )?;
    let lists = [
        (FIELD_ARTIST, &tags.artists),
        (FIELD_ALBUM_ARTIST, &tags.album_artists),
        (FIELD_GENRE, &tags.genres),
        (FIELD_COMPOSER, &tags.composers),
    ];
    for (field, values) in lists {
        for (position, value) in values.iter().enumerate() {
            insert_value.execute(params![id, field, position as i64, value])?;
        }
    }
    connection
        .prepare_cached(
            "INSERT INTO tracks_fts (rowid, title, artist, album, album_artist, genre, path, composer, work, conductor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            id,
            tags.title,
            artist,
            tags.album,
            album_artist,
            genre,
            entry.rel,
            composer,
            tags.work,
            tags.conductor
        ])?;
    Ok(())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}
