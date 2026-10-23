//! Огибающая громкости трека для волны в редакторе текстов: пик и
//! среднеквадратичное значение на каждые 5 мс.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::source::{OpenOptions, TrackDecoder, TrackError, guard};

/// Сколько столбиков огибающей на секунду записи.
pub const PEAKS_PER_SECOND: u32 = 200;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Peaks {
    /// Пик модуля по всем каналам, 0…1.
    pub peak: Vec<f32>,
    /// Среднеквадратичное по всем каналам, 0…1.
    pub rms: Vec<f32>,
}

impl Peaks {
    /// Длина записи в миллисекундах по числу столбиков.
    pub fn duration_ms(&self) -> i64 {
        self.peak.len() as i64 * 1000 / i64::from(PEAKS_PER_SECOND)
    }
}

/// Читает файл целиком и считает огибающую. `stop` прерывает работу, если
/// огибающая больше не нужна; тогда возвращается `None`.
pub fn peaks(path: &Path, stop: &AtomicBool) -> Result<Option<Peaks>, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions::default())?;
        let mut result = Peaks::default();
        // Текущий столбик: сколько кадров набрано, пик и сумма квадратов.
        let (mut frames, mut peak, mut squares, mut samples) = (0u32, 0f32, 0f64, 0u64);
        let mut bin = 0u32;
        while let Some(chunk) = decoder.next_chunk()? {
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let channels = chunk.channels.count().max(1);
            bin = (chunk.rate / PEAKS_PER_SECOND).max(1);
            for frame in chunk.samples.chunks_exact(channels) {
                for &sample in frame {
                    peak = peak.max(sample.abs());
                    squares += f64::from(sample) * f64::from(sample);
                }
                samples += channels as u64;
                frames += 1;
                if frames == bin {
                    result.peak.push(peak.min(1.0));
                    result.rms.push(((squares / samples as f64).sqrt() as f32).min(1.0));
                    (frames, peak, squares, samples) = (0, 0.0, 0.0, 0);
                }
            }
        }
        if frames > bin / 2 {
            result.peak.push(peak.min(1.0));
            result.rms.push(((squares / samples.max(1) as f64).sqrt() as f32).min(1.0));
        }
        Ok(Some(result))
    })
}
