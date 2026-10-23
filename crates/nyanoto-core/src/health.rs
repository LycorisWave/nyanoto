//! Проверка целости файла: декодирование целиком и сверка MD5 у FLAC.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::source::{OpenOptions, TrackDecoder, TrackError, guard};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileHealth {
    /// Совпала ли сумма MD5; `None`, если формат её не хранит.
    pub md5: Option<bool>,
    /// Сколько мест не декодировалось и заменено тишиной.
    pub decode_errors: u64,
    /// Сколько звука не хватает в конце, мс: файл обрезан.
    pub missing_ms: Option<u64>,
    /// Сколько звука в файле, мс.
    pub audio_ms: u64,
}

impl FileHealth {
    pub fn is_damaged(&self) -> bool {
        self.md5 == Some(false) || self.decode_errors > 0 || self.missing_ms.is_some() || self.audio_ms == 0
    }
}

/// Декодирует файл целиком. `stop` прерывает проверку, тогда `None`.
pub fn verify_file(path: &Path, stop: &AtomicBool) -> Result<Option<FileHealth>, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions { verify: true })?;
        let (mut frames, mut rate) = (0u64, 0u32);
        while let Some(chunk) = decoder.next_chunk()? {
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            frames += (chunk.samples.len() / chunk.channels.count().max(1)) as u64;
            rate = chunk.rate;
        }
        let audio_ms = if rate > 0 { frames * 1000 / u64::from(rate) } else { 0 };
        let decode_errors = decoder.decode_errors();
        let missing_ms = decoder.missing().map(|missing| missing.as_millis() as u64);
        let md5 = decoder.finalize();
        Ok(Some(FileHealth { md5, decode_errors, missing_ms, audio_ms }))
    })
}
