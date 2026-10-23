//! TTA, WavPack, APE, Musepack, DSD — бит в бит с исходником.
//! TTA-образцы сделал FFmpeg из WAV от `write_sources`:
//! `cargo test -p nyanoto-core --test formats write_sources -- --ignored`, потом `ffmpeg -i x.wav -c:a tta x.tta`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nyanoto_core::{OpenOptions, TrackDecoder, TrackError, TrackInfo, guard};
use nyanoto_testkit::{Rng, Scratch};

/// Исходный сигнал образца: целые сэмплы, каналы подряд.
struct Source {
    name: &'static str,
    rate: u32,
    channels: usize,
    bits: u32,
    frames: usize,
}

const TTA_SOURCES: &[Source] = &[
    Source { name: "tta16_stereo", rate: 8_000, channels: 2, bits: 16, frames: 20_000 },
    Source { name: "tta24_mono", rate: 8_000, channels: 1, bits: 24, frames: 9_600 },
    Source { name: "tta8_3ch", rate: 8_000, channels: 3, bits: 8, frames: 8_800 },
];

impl Source {
    /// Треугольники и чуть шума, только целые — повторяется до бита везде.
    fn samples(&self) -> Vec<i32> {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let amplitude = 1i64 << (self.bits - 2);
        let noise = 1i64 << (self.bits / 3);
        let mut samples = Vec::with_capacity(self.frames * self.channels);
        for frame in 0..self.frames as i64 {
            for channel in 0..self.channels as i64 {
                let period = 40 + 13 * channel;
                let phase = frame % period;
                let triangle = (2 * phase - period).abs() * 2 * amplitude / period - amplitude;
                let hiss = rng.below(2 * noise as usize + 1) as i64 - noise;
                samples.push((triangle + hiss) as i32);
            }
        }
        samples
    }

    fn as_f32(&self) -> Vec<f32> {
        let scale = 1.0 / (1u32 << (self.bits - 1)) as f32;
        self.samples().iter().map(|sample| *sample as f32 * scale).collect()
    }

    fn fixture(&self, extension: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(format!("{}.{extension}", self.name))
    }

    /// WAV с этим сигналом — исходник для кодировщиков.
    fn wav(&self) -> Vec<u8> {
        let bytes = (self.bits / 8) as usize;
        let mut data = Vec::new();
        for sample in self.samples() {
            // В WAV 8-битный звук беззнаковый.
            let value = if self.bits == 8 { sample + 128 } else { sample };
            data.extend_from_slice(&value.to_le_bytes()[..bytes]);
        }
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&(self.channels as u16).to_le_bytes());
        wav.extend_from_slice(&self.rate.to_le_bytes());
        wav.extend_from_slice(&(self.rate * (self.channels * bytes) as u32).to_le_bytes());
        wav.extend_from_slice(&((self.channels * bytes) as u16).to_le_bytes());
        wav.extend_from_slice(&(self.bits as u16).to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        wav
    }
}

struct Decoded {
    samples: Vec<f32>,
    errors: u64,
    info: TrackInfo,
    missing: Option<Duration>,
}

fn decode(path: &Path) -> Result<Decoded, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions { verify: true })?;
        let mut samples = Vec::new();
        while let Some(chunk) = decoder.next_chunk()? {
            samples.extend_from_slice(chunk.samples);
            assert!(samples.len() < 50_000_000, "декодер не останавливается");
        }
        Ok(Decoded { samples, errors: decoder.decode_errors(), info: decoder.info().clone(), missing: decoder.missing() })
    })
}

/// Первое расхождение, чтобы по ошибке было видно, где именно.
fn assert_same(got: &[f32], want: &[f32], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: длина");
    if let Some(index) = got.iter().zip(want).position(|(a, b)| a != b) {
        panic!("{what}: сэмпл {index}: {} вместо {}", got[index], want[index]);
    }
}

/// Сверка двух файлов бит в бит:
/// `NYANOTO_A=a.flac NYANOTO_B=b.tta cargo test -p nyanoto-core --test formats same_audio -- --ignored --nocapture`
#[test]
#[ignore]
fn same_audio() {
    let a = decode(Path::new(&std::env::var("NYANOTO_A").unwrap())).unwrap();
    let b = decode(Path::new(&std::env::var("NYANOTO_B").unwrap())).unwrap();
    println!("A: {:?}\nB: {:?}", a.info, b.info);
    println!("A: {} сэмплов, ошибок {}; B: {} сэмплов, ошибок {}", a.samples.len(), a.errors, b.samples.len(), b.errors);
    assert_same(&b.samples, &a.samples, "B против A");
    println!("совпадает бит в бит");
}

/// Выгрузка `NYANOTO_A` в `NYANOTO_OUT` (f32 LE, каналы подряд).
#[test]
#[ignore]
fn dump_audio() {
    let decoded = decode(Path::new(&std::env::var("NYANOTO_A").unwrap())).unwrap();
    let bytes: Vec<u8> = decoded.samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
    std::fs::write(std::env::var("NYANOTO_OUT").unwrap(), bytes).unwrap();
    println!("{:?}, {} сэмплов", decoded.info, decoded.samples.len());
}

#[test]
#[ignore]
fn write_sources() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    for source in TTA_SOURCES {
        std::fs::write(dir.join(format!("{}.wav", source.name)), source.wav()).unwrap();
    }
}

#[test]
fn tta_decodes_bit_exact() {
    for source in TTA_SOURCES {
        let decoded = decode(&source.fixture("tta")).unwrap();
        assert_same(&decoded.samples, &source.as_f32(), source.name);
        assert_eq!(decoded.errors, 0);
        assert_eq!(decoded.missing, None);
        assert_eq!(decoded.info.codec, "tta");
        assert_eq!(decoded.info.sample_rate, Some(source.rate));
        assert_eq!(decoded.info.channels, Some(source.channels));
        assert_eq!(decoded.info.bits_per_sample, Some(source.bits));
        let seconds = source.frames as f64 / f64::from(source.rate);
        assert!((decoded.info.duration.unwrap().as_secs_f64() - seconds).abs() < 1e-6);
    }
}

#[test]
fn tta_seek_lands_on_the_sample() {
    let source = &TTA_SOURCES[0];
    let want = source.as_f32();
    let mut decoder = TrackDecoder::open(&source.fixture("tta"), OpenOptions::default()).unwrap();
    // Во второй кадр TTA (кадр — 8359 сэмплов при 8 кГц), потом назад в первый.
    for frame in [12_345usize, 100, 19_999] {
        let landed = decoder.seek(Duration::from_secs_f64(frame as f64 / 8_000.0)).unwrap();
        assert_eq!((landed.as_secs_f64() * 8_000.0).round() as usize, frame);
        let chunk = decoder.next_chunk().unwrap().unwrap();
        let count = chunk.samples.len().min(64);
        assert_same(&chunk.samples[..count], &want[frame * 2..frame * 2 + count], "после перемотки");
    }
}

#[test]
fn damaged_tta_frame_becomes_silence_of_the_same_length() {
    let source = &TTA_SOURCES[0];
    let mut bytes = std::fs::read(source.fixture("tta")).unwrap();
    // Середина второго кадра: заголовок 22 + таблица 16 байт.
    let size = |index: usize| u32::from_le_bytes(bytes[22 + index * 4..26 + index * 4].try_into().unwrap()) as usize;
    let second = 22 + 16 + size(0);
    let middle = second + size(1) / 2;
    bytes[middle] ^= 0x5A;
    let scratch = Scratch::new("tta-damage");
    let decoded = decode(&scratch.write("damaged.tta", &bytes)).unwrap();
    let want = source.as_f32();
    assert_eq!(decoded.errors, 1);
    assert_eq!(decoded.samples.len(), want.len());
    let frame = 8_000 * 256 / 245 * 2;
    assert_same(&decoded.samples[..frame], &want[..frame], "первый кадр");
    assert!(decoded.samples[frame..2 * frame].iter().all(|sample| *sample == 0.0));
    assert_same(&decoded.samples[2 * frame..], &want[2 * frame..], "третий кадр");
}

#[test]
fn truncated_tta_plays_what_is_left_and_reports_the_rest() {
    let source = &TTA_SOURCES[0];
    let bytes = std::fs::read(source.fixture("tta")).unwrap();
    let scratch = Scratch::new("tta-cut");
    let decoded = decode(&scratch.write("cut.tta", &bytes[..bytes.len() - 100])).unwrap();
    // Третий кадр обрезан: звук кончается на двух целых, остальное — «не хватает».
    let frame = 8_000 * 256 / 245;
    assert_same(&decoded.samples, &source.as_f32()[..2 * frame * 2], "целые кадры");
    let missing = decoded.missing.expect("обрезка замечена");
    assert_eq!((missing.as_secs_f64() * 8_000.0).round() as usize, source.frames - 2 * frame);
}

#[test]
fn mutated_tta_never_panics() {
    let source = &TTA_SOURCES[2];
    let original = std::fs::read(source.fixture("tta")).unwrap();
    let scratch = Scratch::new("tta-fuzz");
    let mut rng = Rng(7);
    for round in 0..200 {
        let mut bytes = original.clone();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(bytes.len());
            bytes[at] = rng.next() as u8;
        }
        if round % 10 == 0 {
            bytes.truncate(rng.below(bytes.len()));
        }
        let path = scratch.write("fuzz.tta", &bytes);
        // Ошибка — можно, паника и бесконечный звук — нельзя.
        if let Err(TrackError::Panicked(message)) = decode(&path) {
            panic!("раунд {round}: паника {message}");
        }
    }
}
