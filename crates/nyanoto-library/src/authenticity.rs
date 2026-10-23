//! Результаты проверки подлинности и решения пользователя.
//! Храним вердикты всех чувствительностей — смена ничего не пересчитывает.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::{Library, LibraryError};

/// Вердикт проверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authenticity {
    Genuine,
    /// Вероятно, перекодирован из файла с потерями.
    LossyOrigin,
    /// Вероятно, Hi-Res, пересчитанный из CD.
    Upsampled,
    /// Вероятно, 24 бит, дополненные из 16.
    PaddedBits,
    /// Судить не по чему.
    Unknown,
}

impl Authenticity {
    fn code(self) -> i64 {
        match self {
            Authenticity::Genuine => 0,
            Authenticity::LossyOrigin => 1,
            Authenticity::Upsampled => 2,
            Authenticity::PaddedBits => 3,
            Authenticity::Unknown => 4,
        }
    }

    fn from_code(code: i64) -> Self {
        match code {
            0 => Authenticity::Genuine,
            1 => Authenticity::LossyOrigin,
            2 => Authenticity::Upsampled,
            3 => Authenticity::PaddedBits,
            _ => Authenticity::Unknown,
        }
    }
}

/// Чувствительность проверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckLevel {
    #[default]
    Low,
    Medium,
    High,
}

impl CheckLevel {
    /// Колонка с вердиктом этой чувствительности.
    fn column(self) -> &'static str {
        match self {
            CheckLevel::Low => "verdict",
            CheckLevel::Medium => "verdict_medium",
            CheckLevel::High => "verdict_high",
        }
    }
}

/// Что проверка сказала о файле при одной чувствительности.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub verdict: Authenticity,
    /// Где кончается полезный сигнал, Гц.
    pub cutoff_hz: Option<u32>,
    /// Сколько бит занято на самом деле.
    pub real_bits: Option<u32>,
    /// Из какой частоты, вероятно, пересчитан файл.
    pub real_rate: Option<u32>,
}

/// Вердикты низкой, средней и высокой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckRecord {
    pub verdicts: [Authenticity; 3],
    pub cutoff_hz: Option<u32>,
    pub real_bits: Option<u32>,
    pub real_rate: Option<u32>,
}

/// Решение пользователя по подозрительному треку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityOverride {
    /// Оставить как есть: показывать то, что заявляет файл.
    Accept,
    /// Показывать то, что определила проверка.
    Detected,
    /// Показывать подпись, которую выбрал пользователь.
    Label(String),
}

/// Условие «файл без потерь» на языке SQL: проверять есть смысл только их.
const LOSSLESS: &str = "(t.format IN ('flac', 'wav', 'aiff', 'ape', 'wavpack')
                         OR (t.format = 'mp4' AND t.bit_depth IS NOT NULL))";

impl Library {
    /// Без потерь и не проверены, изменились или проверены старой версией.
    pub fn tracks_to_check(&self, limit: usize) -> Result<Vec<(i64, PathBuf)>, LibraryError> {
        self.tracks_to_check_after(0, limit)
    }

    /// То же с трека после `after`: каждая пачка продолжает с места прошлой.
    pub fn tracks_to_check_after(&self, after: i64, limit: usize) -> Result<Vec<(i64, PathBuf)>, LibraryError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT t.id, r.path, t.rel_path FROM tracks t
             JOIN roots r ON r.id = t.root_id
             LEFT JOIN inspections i ON i.track_id = t.id
             WHERE t.id > ?2 AND t.missing = 0 AND {LOSSLESS}
               AND (i.track_id IS NULL OR i.size != t.size OR i.mtime != t.mtime
                    OR i.verdict_high IS NULL)
             ORDER BY t.id LIMIT ?1"
        ))?;
        let tracks = statement
            .query_map([limit as i64, after], |row| {
                let root: String = row.get(1)?;
                let rel: String = row.get(2)?;
                Ok((row.get(0)?, Path::new(&root).join(rel)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(tracks)
    }

    /// С размером и временем файла — изменился, проверим снова.
    pub fn store_check(&self, track: i64, record: &CheckRecord) -> Result<(), LibraryError> {
        let [low, medium, high] = record.verdicts;
        self.connection.execute(
            "INSERT INTO inspections
                 (track_id, size, mtime, verdict, verdict_medium, verdict_high, cutoff_hz, real_bits, real_rate)
             SELECT id, size, mtime, ?2, ?3, ?4, ?5, ?6, ?7 FROM tracks WHERE id = ?1
             ON CONFLICT (track_id) DO UPDATE SET
                 size = excluded.size, mtime = excluded.mtime, verdict = excluded.verdict,
                 verdict_medium = excluded.verdict_medium, verdict_high = excluded.verdict_high,
                 cutoff_hz = excluded.cutoff_hz, real_bits = excluded.real_bits,
                 real_rate = excluded.real_rate",
            params![
                track,
                low.code(),
                medium.code(),
                high.code(),
                record.cutoff_hz,
                record.real_bits,
                record.real_rate
            ],
        )?;
        Ok(())
    }

    /// Треки, которые проверка сочла подозрительными при этой чувствительности.
    pub fn suspicious(&self, level: CheckLevel) -> Result<HashMap<i64, Check>, LibraryError> {
        let column = level.column();
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT i.track_id, i.{column}, i.cutoff_hz, i.real_bits, i.real_rate
             FROM inspections i JOIN tracks t ON t.id = i.track_id
             WHERE i.{column} IN (1, 2, 3) AND i.size = t.size AND i.mtime = t.mtime"
        ))?;
        let checks = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    Check {
                        verdict: Authenticity::from_code(row.get(1)?),
                        cutoff_hz: row.get(2)?,
                        real_bits: row.get(3)?,
                        real_rate: row.get(4)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(checks)
    }

    pub fn quality_overrides(&self) -> Result<HashMap<i64, QualityOverride>, LibraryError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT track_id, mode, label FROM quality_overrides")?;
        let overrides = statement
            .query_map([], |row| {
                let mode: i64 = row.get(1)?;
                let label: Option<String> = row.get(2)?;
                let choice = match (mode, label) {
                    (1, _) => QualityOverride::Detected,
                    (2, Some(label)) => QualityOverride::Label(label),
                    _ => QualityOverride::Accept,
                };
                Ok((row.get(0)?, choice))
            })?
            .collect::<Result<_, _>>()?;
        Ok(overrides)
    }

    /// Сохраняет решение пользователя по треку; `None` возвращает проверку.
    pub fn set_quality_override(
        &self,
        track: i64,
        choice: Option<&QualityOverride>,
    ) -> Result<(), LibraryError> {
        match choice {
            None => {
                self.connection
                    .execute("DELETE FROM quality_overrides WHERE track_id = ?1", [track])?;
            }
            Some(choice) => {
                let (mode, label) = match choice {
                    QualityOverride::Accept => (0, None),
                    QualityOverride::Detected => (1, None),
                    QualityOverride::Label(label) => (2, Some(label.as_str())),
                };
                self.connection.execute(
                    "INSERT INTO quality_overrides (track_id, mode, label) VALUES (?1, ?2, ?3)
                     ON CONFLICT (track_id) DO UPDATE SET mode = excluded.mode, label = excluded.label",
                    params![track, mode, label],
                )?;
            }
        }
        Ok(())
    }
}
