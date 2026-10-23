//! Здоровье: битые файлы, дубликаты, пропуски, пропавшие.
//! Битые находит фоновая проверка, остальное — по базе без чтения файлов.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::{Library, LibraryError, TRACK_COLUMN_COUNT, TRACK_COLUMNS, Track, natural_cmp};

/// Итог полной проверки одного файла.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthRecord {
    pub md5: Option<bool>,
    pub decode_errors: u64,
    pub missing_ms: Option<u64>,
    pub audio_ms: u64,
    /// Файл не открылся или декодер сломался на нём.
    pub error: Option<String>,
}

/// Что не так с файлом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    /// Сумма MD5 не сошлась: звук не тот, что был записан.
    Md5,
    /// Сколько мест не декодировалось.
    DecodeErrors(u64),
    /// Сколько звука не хватает в конце, мс.
    Truncated(u64),
    Silent,
    /// Не открылся, с причиной.
    Unreadable(String),
}

/// Альбом, в котором не хватает треков по номерам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumGap {
    pub folder: PathBuf,
    /// Недостающие номера: «3» или «2.05» — диск и трек.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HealthReport {
    pub damaged: Vec<(Track, Damage)>,
    pub duplicates: Vec<Vec<Track>>,
    pub gaps: Vec<AlbumGap>,
    pub missing: Vec<Track>,
    /// Сколько файлов уже проверено полностью и сколько всего.
    pub checked: u64,
    pub total: u64,
}

impl Library {
    /// Файлы без свежей полной проверки.
    pub fn tracks_to_verify(&self, limit: usize) -> Result<Vec<(i64, PathBuf)>, LibraryError> {
        self.tracks_to_verify_after(0, limit)
    }

    /// Пачками: каждая продолжает с места прошлой.
    pub fn tracks_to_verify_after(&self, after: i64, limit: usize) -> Result<Vec<(i64, PathBuf)>, LibraryError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT t.id, r.path, t.rel_path FROM tracks t
             JOIN roots r ON r.id = t.root_id
             LEFT JOIN health h ON h.track_id = t.id
             WHERE t.id > ?2 AND t.missing = 0 AND t.problem IS NULL
               AND (h.track_id IS NULL OR h.size != t.size OR h.mtime != t.mtime)
             ORDER BY t.id LIMIT ?1",
        )?;
        let tracks = statement
            .query_map([limit as i64, after], |row| {
                let root: String = row.get(1)?;
                let rel: String = row.get(2)?;
                Ok((row.get(0)?, Path::new(&root).join(rel)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(tracks)
    }

    pub fn store_health(&self, track: i64, record: &HealthRecord) -> Result<(), LibraryError> {
        self.connection
            .prepare_cached(
                "INSERT INTO health (track_id, size, mtime, md5, decode_errors, missing_ms, audio_ms, error)
                 SELECT id, size, mtime, ?2, ?3, ?4, ?5, ?6 FROM tracks WHERE id = ?1
                 ON CONFLICT (track_id) DO UPDATE SET
                     size = excluded.size, mtime = excluded.mtime, md5 = excluded.md5,
                     decode_errors = excluded.decode_errors, missing_ms = excluded.missing_ms,
                     audio_ms = excluded.audio_ms, error = excluded.error",
            )?
            .execute(params![
                track,
                record.md5,
                record.decode_errors as i64,
                record.missing_ms.map(|ms| ms as i64),
                record.audio_ms as i64,
                record.error
            ])?;
        Ok(())
    }

    /// Сколько файлов проверено полностью и сколько всего можно проверить.
    pub fn health_progress(&self) -> Result<(u64, u64), LibraryError> {
        Ok(self.connection.query_row(
            "SELECT COUNT(h.track_id), COUNT(*) FROM tracks t
             LEFT JOIN health h ON h.track_id = t.id AND h.size = t.size AND h.mtime = t.mtime
             WHERE t.missing = 0 AND t.problem IS NULL",
            [],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, i64>(1)?.max(0) as u64)),
        )?)
    }

    pub fn health_report(&self) -> Result<HealthReport, LibraryError> {
        let (checked, total) = self.health_progress()?;
        let mut report = HealthReport { checked, total, ..HealthReport::default() };

        // Повреждённые: по свежим полным проверкам и по ошибкам сканирования.
        let base = TRACK_COLUMN_COUNT;
        let mut statement = self.connection.prepare(&format!(
            "SELECT {TRACK_COLUMNS}, h.md5, h.decode_errors, h.missing_ms, h.audio_ms, h.error
             FROM tracks t JOIN roots r ON r.id = t.root_id
             JOIN health h ON h.track_id = t.id AND h.size = t.size AND h.mtime = t.mtime
             WHERE t.missing = 0
               AND (h.md5 = 0 OR h.decode_errors > 0 OR h.missing_ms IS NOT NULL
                    OR h.audio_ms = 0 OR h.error IS NOT NULL)
             ORDER BY t.rel_path"
        ))?;
        let rows = statement.query_map([], |row| {
            let track = crate::track_from(row)?;
            let md5: Option<bool> = row.get(base)?;
            let errors: i64 = row.get(base + 1)?;
            let missing: Option<i64> = row.get(base + 2)?;
            let error: Option<String> = row.get(base + 4)?;
            let damage = if let Some(error) = error {
                Damage::Unreadable(error)
            } else if md5 == Some(false) {
                Damage::Md5
            } else if let Some(missing) = missing {
                Damage::Truncated(missing.max(0) as u64)
            } else if errors > 0 {
                Damage::DecodeErrors(errors as u64)
            } else {
                Damage::Silent
            };
            Ok((track, damage))
        })?;
        for row in rows {
            report.damaged.push(row?);
        }
        for track in self.problems(10_000)? {
            let problem = track.problem.clone().unwrap_or_default();
            report.damaged.push((track, Damage::Unreadable(problem)));
        }

        let tracks = self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks t JOIN roots r ON r.id = t.root_id
                 WHERE t.missing = 0 ORDER BY t.rel_path"
            ),
            [],
        )?;
        report.duplicates = duplicates(&tracks);
        report.gaps = gaps(&tracks);
        report.missing = self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks t JOIN roots r ON r.id = t.root_id
                 WHERE t.missing = 1 ORDER BY t.rel_path LIMIT 10000"
            ),
            [],
        )?;
        Ok(report)
    }
}

/// Имя для сравнения: тег или файл без номера, буквы и цифры.
fn title_key(track: &Track) -> String {
    let stem = track.path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let raw = match &track.title {
        Some(title) => title.clone(),
        None => stem
            .trim_start_matches(|char: char| char.is_ascii_digit() || " .-_".contains(char))
            .to_string(),
    };
    normalize(&raw)
}

fn normalize(text: &str) -> String {
    text.chars().filter(|char| char.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Одна песня одного исполнителя, длина ±3 с. Из одной папки не считаем («Intro» на двух дисках).
fn duplicates(tracks: &[Track]) -> Vec<Vec<Track>> {
    let mut groups: HashMap<(String, String), Vec<&Track>> = HashMap::new();
    for track in tracks {
        let title = title_key(track);
        if title.len() < 2 {
            continue;
        }
        let artist = normalize(
            track.artist.as_deref().or(track.owner.as_deref()).or(track.folder_artist.as_deref()).unwrap_or(""),
        );
        groups.entry((artist, title)).or_default().push(track);
    }
    let mut found: Vec<Vec<Track>> = Vec::new();
    for group in groups.into_values().filter(|group| group.len() > 1) {
        let mut used = vec![false; group.len()];
        for first in 0..group.len() {
            if used[first] {
                continue;
            }
            let mut same = vec![group[first].clone()];
            for other in first + 1..group.len() {
                let close = match (group[first].duration_ms, group[other].duration_ms) {
                    (Some(a), Some(b)) => (a - b).abs() <= 3_000,
                    _ => false,
                };
                let elsewhere = group[first].path.parent() != group[other].path.parent();
                if !used[other] && close && elsewhere {
                    used[other] = true;
                    same.push(group[other].clone());
                }
            }
            if same.len() > 1 {
                found.push(same);
            }
        }
    }
    found.sort_by(|a, b| natural_cmp(&a[0].path.to_string_lossy(), &b[0].path.to_string_lossy()));
    found
}

/// Номер трека: из тега, а без него — из начала имени файла.
fn number_of(track: &Track) -> Option<u32> {
    track.track_no.or_else(|| {
        let stem = track.path.file_stem()?.to_string_lossy().into_owned();
        let digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok().filter(|number| *number > 0 && *number < 1000)
    })
}

/// Пропуски номеров в каждой папке альбома: так видно недокачанное.
fn gaps(tracks: &[Track]) -> Vec<AlbumGap> {
    // Папка альбома → диск → номера. Папки «CD 01» относятся к альбому выше.
    let mut albums: BTreeMap<PathBuf, BTreeMap<u32, Vec<u32>>> = BTreeMap::new();
    for track in tracks {
        let Some(number) = number_of(track) else {
            continue;
        };
        let Some(folder) = track.path.parent() else {
            continue;
        };
        albums
            .entry(folder.to_path_buf())
            .or_default()
            .entry(track.disc_no.unwrap_or(1))
            .or_default()
            .push(number);
    }
    let mut found = Vec::new();
    for (folder, discs) in albums {
        let several = discs.len() > 1;
        let mut missing = Vec::new();
        for (disc, numbers) in discs {
            let highest = numbers.iter().copied().max().unwrap_or(0);
            // Один трек с номером 7 — скорее сингл из сборника, а не пропуск.
            if numbers.len() < 2 {
                continue;
            }
            for number in 1..=highest {
                if !numbers.contains(&number) {
                    missing.push(if several { format!("{disc}.{number:02}") } else { number.to_string() });
                }
            }
        }
        if !missing.is_empty() {
            found.push(AlbumGap { folder, missing });
        }
    }
    found
}
