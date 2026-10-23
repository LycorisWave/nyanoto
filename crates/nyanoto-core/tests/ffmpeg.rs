//! Редкие форматы через FFmpeg 7. Нет FFmpeg — тесты ничего не проверяют и говорят почему.
//! Образцы — системным FFmpeg 7.1 из WAV от `write_sources` (mlp, ac3 96k, wmav2 64k).

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{assert_same, decode};
use nyanoto_core::{FfmpegStatus, OpenOptions, TrackDecoder, ffmpeg_status};

const RATE: u32 = 48_000;
const FRAMES: usize = 48_000;
const TONES: [(f64, f64); 2] = [(1_000.0, 0.5), (3_000.0, 0.25)];

fn source() -> Vec<i16> {
    (0..FRAMES)
        .flat_map(|frame| {
            TONES.map(|(frequency, amplitude)| {
                ((std::f64::consts::TAU * frequency * frame as f64 / f64::from(RATE)).sin() * amplitude * 32_767.0).round()
                    as i16
            })
        })
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Открывает файл через FFmpeg; `None` и пояснение, если FFmpeg нет.
fn ffmpeg_ready(name: &str) -> bool {
    let opened = TrackDecoder::open(&fixture(name), OpenOptions::default());
    match ffmpeg_status() {
        FfmpegStatus::Loaded { .. } => {
            opened.unwrap();
            true
        }
        status => {
            eprintln!("FFmpeg недоступен ({status:?}) — тест пропущен");
            false
        }
    }
}

#[test]
#[ignore]
fn write_sources() {
    let mut data = Vec::new();
    for sample in source() {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    for value in [1u16, 2] {
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes());
    for value in [4u16, 16] {
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(fixture("ff_source.wav"), wav).unwrap();
}

/// Уровень тона по наименьшим квадратам.
fn level(samples: &[f32], channel: usize, frequency: f64, range: std::ops::Range<usize>) -> f64 {
    let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for frame in range {
        let (sin, cos) = (std::f64::consts::TAU * frequency * frame as f64 / f64::from(RATE)).sin_cos();
        let y = f64::from(samples[frame * 2 + channel]);
        ss += sin * sin;
        sc += sin * cos;
        cc += cos * cos;
        ys += y * sin;
        yc += y * cos;
    }
    let det = ss * cc - sc * sc;
    let (a, b) = ((ys * cc - yc * sc) / det, (yc * ss - ys * sc) / det);
    (a * a + b * b).sqrt()
}

#[test]
fn mlp_is_bit_exact_and_seeks_to_the_sample() {
    if !ffmpeg_ready("ff.mlp") {
        return;
    }
    let want: Vec<f32> = source().iter().map(|sample| f32::from(*sample) / 32_768.0).collect();
    let decoded = decode(&fixture("ff.mlp")).unwrap();
    assert_eq!(decoded.info.codec, "mlp");
    assert_eq!(decoded.errors, 0);
    // MLP добивает конец до своего блока тишиной: сверяем звук исходника.
    assert_same(&decoded.samples[..want.len()], &want, "MLP");
    let mut decoder = TrackDecoder::open(&fixture("ff.mlp"), OpenOptions::default()).unwrap();
    for frame in [30_000usize, 7, 41_234] {
        let landed = decoder.seek(Duration::from_secs_f64(frame as f64 / f64::from(RATE))).unwrap();
        assert_eq!((landed.as_secs_f64() * f64::from(RATE)).round() as usize, frame);
        let chunk = decoder.next_chunk().unwrap().unwrap();
        let count = chunk.samples.len().min(200);
        assert_same(&chunk.samples[..count], &want[frame * 2..frame * 2 + count], "после перемотки");
    }
}

#[test]
fn lossy_formats_keep_their_level() {
    for (name, codec) in [("ff.ac3", "ac3"), ("ff.wma", "wmav2")] {
        if !ffmpeg_ready(name) {
            return;
        }
        let decoded = decode(&fixture(name)).unwrap();
        assert_eq!(decoded.info.codec, codec);
        assert_eq!(decoded.errors, 0, "{name}");
        assert!(decoded.samples.len() >= FRAMES * 2 * 9 / 10, "{name}: звук почти весь");
        for (channel, (frequency, amplitude)) in TONES.into_iter().enumerate() {
            let error_db = 20.0 * (level(&decoded.samples, channel, frequency, 8_000..40_000) / amplitude).log10();
            assert!(error_db.abs() < 0.5, "{name}, канал {channel}: уровень {error_db:+.2} дБ");
        }
    }
}
