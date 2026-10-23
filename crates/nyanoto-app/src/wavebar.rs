//! Перемотка формой волны. Огибающая — раз в тихом фоне, кэш на диске (~4 КБ на трек).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use nyanoto_core::Peaks;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::{App, in_ui, quiet};

/// Столбиков огибающей на трек, сколько бы он ни длился.
const BINS: usize = 2000;
const WIDTH: u32 = 1600;
const HEIGHT: u32 = 32;

/// Огибающая для полосы: пик и среднее по столбикам, 0…255.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub peak: Vec<u8>,
    pub rms: Vec<u8>,
}

impl Envelope {
    /// Сжимает огибающую 200 столбиков в секунду до `BINS` на весь трек.
    pub fn from_peaks(peaks: &Peaks) -> Self {
        let total = peaks.peak.len();
        let (mut peak, mut rms) = (Vec::with_capacity(BINS), Vec::with_capacity(BINS));
        if total == 0 {
            return Self { peak, rms };
        }
        for bin in 0..BINS {
            let from = bin * total / BINS;
            let to = ((bin + 1) * total / BINS).max(from + 1).min(total);
            let level = |values: &[f32]| (values[from..to].iter().copied().fold(0.0f32, f32::max) * 255.0).round() as u8;
            peak.push(level(&peaks.peak));
            rms.push(level(&peaks.rms));
        }
        Self { peak, rms }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = b"NKW1".to_vec();
        bytes.extend_from_slice(&(self.peak.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.peak);
        bytes.extend_from_slice(&self.rms);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(b"NKW1")?;
        let count = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let data = rest.get(4..)?;
        (data.len() == count * 2).then(|| Self { peak: data[..count].to_vec(), rms: data[count..].to_vec() })
    }

    /// Картинка-маска: пик полупрозрачный, среднее плотное.
    pub fn render(&self) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(WIDTH, HEIGHT);
        let count = self.peak.len();
        if count == 0 {
            return buffer;
        }
        let pixels = buffer.make_mut_slice();
        for column in 0..WIDTH as usize {
            let from = column * count / WIDTH as usize;
            let to = ((column + 1) * count / WIDTH as usize).max(from + 1).min(count);
            let level = |values: &[u8]| values[from..to].iter().copied().max().unwrap_or(0);
            for (value, alpha) in [(level(&self.peak), 110u8), (level(&self.rms), 255u8)] {
                let half = (u32::from(value) * HEIGHT / 2 / 255).max(1);
                for row in HEIGHT / 2 - half.min(HEIGHT / 2)..(HEIGHT / 2 + half).min(HEIGHT) {
                    pixels[row as usize * WIDTH as usize + column] = Rgba8Pixel { r: 255, g: 255, b: 255, a: alpha };
                }
            }
        }
        buffer
    }
}

/// Ключ кэша: путь, размер и время изменения файла. FNV-1a, чтобы ключ не
/// менялся от версии к версии Rust.
pub(crate) fn cache_key(path: &Path, extension: &str) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let text = format!("{}|{}|{modified}", path.display(), metadata.len());
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Some(format!("{hash:016x}.{extension}"))
}

/// Огибающая из кэша или посчитанная заново (и положенная в кэш).
fn envelope(path: &Path, cache: &Path, stop: &AtomicBool) -> Option<Envelope> {
    let file = cache.join(cache_key(path, "nkw")?);
    if let Ok(bytes) = std::fs::read(&file)
        && let Some(envelope) = Envelope::from_bytes(&bytes)
    {
        return Some(envelope);
    }
    let peaks = nyanoto_core::peaks(path, stop).ok()??;
    let envelope = Envelope::from_peaks(&peaks);
    if std::fs::create_dir_all(cache).is_ok() {
        let _ = std::fs::write(&file, envelope.to_bytes());
    }
    Some(envelope)
}

impl App {
    /// Папка кэша волн: рядом с базой.
    fn wave_cache(&self) -> Option<PathBuf> {
        Some(self.database.as_ref()?.parent()?.join("waves"))
    }

    /// Новый трек: волна из кэша или в тихом фоне.
    pub(crate) fn load_wavebar(&self) {
        self.wavebar_stop.borrow().store(true, std::sync::atomic::Ordering::Relaxed);
        self.window.set_wavebar_ready(false);
        if !self.wavebar_on.get() {
            return;
        }
        let (Some(path), Some(cache)) = (self.current.borrow().as_ref().map(|track| track.path.clone()), self.wave_cache())
        else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        *self.wavebar_stop.borrow_mut() = stop.clone();
        let _ = std::thread::Builder::new().name("nyanoto-wavebar".into()).spawn(move || {
            quiet::enter_background();
            let found = envelope(&path, &cache, &stop);
            in_ui(move |app| {
                let current = app.current.borrow().as_ref().map(|track| track.path.clone());
                if current.as_deref() != Some(path.as_path()) {
                    return;
                }
                if let Some(envelope) = found {
                    app.window.set_wavebar(Image::from_rgba8(envelope.render()));
                    app.window.set_wavebar_ready(true);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_keeps_its_shape_and_survives_the_cache() {
        let peaks = Peaks {
            peak: (0..4000).map(|bin| if bin < 2000 { 0.0 } else { 1.0 }).collect(),
            rms: (0..4000).map(|bin| if bin < 2000 { 0.0 } else { 0.5 }).collect(),
        };
        let envelope = Envelope::from_peaks(&peaks);
        assert_eq!(envelope.peak.len(), BINS);
        assert_eq!((envelope.peak[10], envelope.peak[1990]), (0, 255));
        assert_eq!(envelope.rms[1990], 128);
        assert_eq!(Envelope::from_bytes(&envelope.to_bytes()), Some(envelope.clone()));
        assert_eq!(Envelope::from_bytes(b"NKW1\x05\x00\x00\x00ab"), None);

        let image = envelope.render();
        let alpha = |column: u32, row: u32| image.as_slice()[(row * WIDTH + column) as usize].a;
        assert_eq!(alpha(100, 2), 0, "тихая половина почти пустая");
        assert_eq!(alpha(1500, HEIGHT / 2), 255);
        assert_eq!(alpha(1500, 1), 110, "у края только пик");
    }
}
