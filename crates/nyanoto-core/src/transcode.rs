//! Конвертация в FLAC или WAV. Без пересчёта — бит в бит, с пересчётом — треугольный дизеринг.
//! Через временный файл.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::convert::{Converter, Speed};
use crate::flac_writer::FlacWriter;
use crate::source::{OpenOptions, TrackDecoder, TrackError, guard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetFormat {
    Flac,
    Wav,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvertOptions {
    pub format: TargetFormat,
    /// Частота результата; `None` — как у файла.
    pub rate: Option<u32>,
    /// Разрядность результата (16 или 24); `None` — как у файла, но не больше 24.
    pub bits: Option<u32>,
}

enum Sink {
    Flac(Box<FlacWriter<BufWriter<File>>>),
    Wav { out: BufWriter<File>, bytes: usize, data: u64 },
}

impl Sink {
    fn write(&mut self, samples: &[i32]) -> std::io::Result<()> {
        match self {
            Sink::Flac(writer) => writer.write(samples),
            Sink::Wav { out, bytes, data } => {
                let mut raw = Vec::with_capacity(samples.len() * *bytes);
                for sample in samples {
                    raw.extend_from_slice(&sample.to_le_bytes()[..*bytes]);
                }
                *data += raw.len() as u64;
                out.write_all(&raw)
            }
        }
    }

    fn finish(self) -> std::io::Result<()> {
        match self {
            Sink::Flac(writer) => (*writer).finish().map(drop),
            Sink::Wav { mut out, data, .. } => {
                let data = u32::try_from(data).map_err(|_| std::io::Error::other("WAV больше 4 ГБ"))?;
                out.seek(SeekFrom::Start(4))?;
                out.write_all(&(36 + data).to_le_bytes())?;
                out.seek(SeekFrom::Start(40))?;
                out.write_all(&data.to_le_bytes())?;
                out.flush()
            }
        }
    }
}

fn wav_header(out: &mut impl Write, rate: u32, channels: usize, bits: u32) -> std::io::Result<()> {
    let block = channels as u32 * bits.div_ceil(8);
    out.write_all(b"RIFF")?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&(channels as u16).to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * block).to_le_bytes())?;
    out.write_all(&(block as u16).to_le_bytes())?;
    out.write_all(&(bits as u16).to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&0u32.to_le_bytes())
}

/// `f32` в целое `bits`; `noise` — дизеринг в младших разрядах.
fn quantize(sample: f32, bits: u32, noise: f64) -> i32 {
    let scale = f64::from(1u32 << (bits - 1));
    let max = scale - 1.0;
    (f64::from(sample) * scale + noise).round().clamp(-scale, max) as i32
}

/// Простой генератор для дизеринга.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Треугольное распределение от −1 до 1 младшего разряда.
    fn triangular(&mut self) -> f64 {
        self.next() - self.next()
    }
}

/// Конвертирует `source` в `target`. `false` — работу прервали, файла нет.
pub fn convert_file(source: &Path, target: &Path, options: &ConvertOptions, stop: &AtomicBool) -> Result<bool, TrackError> {
    let partial = target.with_extension("nyanoto-part");
    let result = guard(|| convert_inner(source, &partial, options, stop));
    match result {
        Ok(true) => {
            std::fs::rename(&partial, target).map_err(TrackError::Write)?;
            Ok(true)
        }
        other => {
            let _ = std::fs::remove_file(&partial);
            other
        }
    }
}

fn convert_inner(source: &Path, partial: &Path, options: &ConvertOptions, stop: &AtomicBool) -> Result<bool, TrackError> {
    let io = TrackError::Write;
    let mut decoder = TrackDecoder::open(source, OpenOptions::default())?;
    let info = decoder.info().clone();
    let source_rate = info.sample_rate.unwrap_or(44_100);
    // У форматов с потерями разрядности нет: для них хватает 16 бит.
    let source_bits = info.bits_per_sample.unwrap_or(16);
    let rate = options.rate.unwrap_or(source_rate);
    let bits = options.bits.unwrap_or(source_bits.clamp(16, 24)).clamp(16, 24);
    let channels = info.channels.unwrap_or(2).clamp(1, 8);
    // Точно — только если ничего не пересчитываем. Lossy дробный — тоже с дизерингом.
    let dither = rate != source_rate || bits < source_bits || info.bits_per_sample.is_none();

    let file = BufWriter::new(File::create(partial).map_err(io)?);
    let mut sink = match options.format {
        TargetFormat::Flac => Sink::Flac(Box::new(FlacWriter::new(file, rate, channels, bits).map_err(io)?)),
        TargetFormat::Wav => {
            let mut out = file;
            wav_header(&mut out, rate, channels, bits).map_err(io)?;
            Sink::Wav { out, bytes: bits.div_ceil(8) as usize, data: 0 }
        }
    };
    let mut converter: Option<Converter> = None;
    let mut converted = Vec::new();
    let mut ints = Vec::new();
    let mut noise = Noise(0x2545_F491_4F6C_DD1D);
    let mut emit = |samples: &[f32], sink: &mut Sink| -> Result<(), TrackError> {
        ints.clear();
        ints.extend(samples.iter().map(|sample| {
            let jitter = if dither { noise.triangular() } else { 0.0 };
            quantize(*sample, bits, jitter)
        }));
        sink.write(&ints).map_err(io)
    };
    while let Some(chunk) = decoder.next_chunk()? {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if chunk.channels.count() != channels {
            return Err(TrackError::Unsupported("число каналов меняется посреди файла".into()));
        }
        if rate == chunk.rate {
            emit(chunk.samples, &mut sink)?;
            continue;
        }
        if converter.is_none() {
            converter = Some(Converter::new(chunk.rate, chunk.channels, rate, channels, Speed::NORMAL)?);
        }
        converted.clear();
        if let Some(converter) = converter.as_mut() {
            converter.process(chunk.samples, &mut converted)?;
        }
        emit(&converted, &mut sink)?;
    }
    if let Some(mut converter) = converter {
        converted.clear();
        converter.flush(&mut converted)?;
        emit(&converted, &mut sink)?;
    }
    sink.finish().map_err(io)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_is_exact_without_dither() {
        for value in [-32_768i32, -1, 0, 1, 30_000, 32_767] {
            assert_eq!(quantize(value as f32 / 32_768.0, 16, 0.0), value);
        }
        for value in [-8_388_608i32, 0, 5_000_001, 8_388_607] {
            assert_eq!(quantize(value as f32 / 8_388_608.0, 24, 0.0), value);
        }
        assert_eq!(quantize(1.5, 16, 0.0), 32_767, "перегрузка обрезается");
    }

    #[test]
    fn triangular_noise_stays_within_one_step() {
        let mut noise = Noise(42);
        let values: Vec<f64> = (0..10_000).map(|_| noise.triangular()).collect();
        assert!(values.iter().all(|value| value.abs() < 1.0));
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        assert!(mean.abs() < 0.02);
    }
}
