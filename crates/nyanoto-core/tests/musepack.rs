//! Musepack SV7 и SV8 — с потерями, так что сверяем длину, уровень, фазу, чистоту, перемотку и порчу.
//! Образцы: SV8 — `mpcenc --quality 5`, SV7 — `mppenc --standard` из WAV от `write_sources`.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::decode;
use nyanoto_core::{OpenOptions, TrackDecoder, TrackError};
use nyanoto_testkit::{Rng, Scratch};

const RATE: u32 = 44_100;
const FRAMES: usize = 66_150;
/// Тон и уровень по каналам.
const TONES: [(f64, f64); 2] = [(1_000.0, 0.5), (3_000.0, 0.25)];

fn source() -> Vec<f32> {
    (0..FRAMES)
        .flat_map(|frame| {
            TONES.map(|(frequency, amplitude)| {
                (amplitude * (std::f64::consts::TAU * frequency * frame as f64 / f64::from(RATE)).sin()) as f32
            })
        })
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

#[test]
#[ignore]
fn write_sources() {
    let samples: Vec<i16> = source().iter().map(|sample| (sample * 32_767.0).round() as i16).collect();
    let mut data = Vec::new();
    for sample in &samples {
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
    std::fs::write(fixture("mpc_source.wav"), wav).unwrap();
}

/// Уровень, фаза (в кадрах) и остаток после вычитания тона, по кадрам `range`.
fn fit(samples: &[f32], channel: usize, (frequency, _): (f64, f64), range: std::ops::Range<usize>) -> (f64, f64, f64) {
    let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let omega = std::f64::consts::TAU * frequency / f64::from(RATE);
    for frame in range.clone() {
        let (sin, cos) = (omega * frame as f64).sin_cos();
        let y = f64::from(samples[frame * 2 + channel]);
        ss += sin * sin;
        sc += sin * cos;
        cc += cos * cos;
        ys += y * sin;
        yc += y * cos;
    }
    let det = ss * cc - sc * sc;
    let (a, b) = ((ys * cc - yc * sc) / det, (yc * ss - ys * sc) / det);
    let residual: f64 = range
        .clone()
        .map(|frame| {
            let (sin, cos) = (omega * frame as f64).sin_cos();
            (f64::from(samples[frame * 2 + channel]) - a * sin - b * cos).powi(2)
        })
        .sum::<f64>()
        / range.len() as f64;
    ((a * a + b * b).sqrt(), b.atan2(a) / omega, residual.sqrt())
}

#[test]
fn both_versions_keep_length_level_and_timing() {
    for (name, version) in [("mpc_sv8.mpc", "mpc-sv8"), ("mpc_sv7.mpc", "mpc-sv7")] {
        let decoded = decode(&fixture(name)).unwrap();
        assert_eq!(decoded.info.codec, version);
        assert_eq!(decoded.info.sample_rate, Some(RATE));
        assert_eq!(decoded.info.channels, Some(2));
        assert_eq!(decoded.info.bits_per_sample, None, "{name}: у формата с потерями нет разрядности");
        assert_eq!(decoded.samples.len(), FRAMES * 2, "{name}: точная длина");
        assert_eq!(decoded.errors, 0);
        assert_eq!(decoded.missing, None);
        for (channel, tone) in TONES.into_iter().enumerate() {
            let (level, shift, residual) = fit(&decoded.samples, channel, tone, 4_000..FRAMES - 4_000);
            let error_db = 20.0 * (level / tone.1).log10();
            assert!(error_db.abs() < 0.1, "{name}, канал {channel}: уровень {error_db:+.3} дБ");
            assert!(shift.abs() < 0.05, "{name}, канал {channel}: сдвиг {shift:.3} кадра");
            // Чистоту задаёт кодек. SV8 62/27 дБ, SV7 63/38 — FFmpeg даёт столько же.
            let snr = 20.0 * (tone.1 / std::f64::consts::SQRT_2 / residual).log10();
            let needed = if channel == 0 { 55.0 } else { 20.0 };
            assert!(snr > needed, "{name}, канал {channel}: SNR {snr:.1} дБ");
        }
    }
}

#[test]
fn seek_lands_on_the_sample() {
    for name in ["mpc_sv8.mpc", "mpc_sv7.mpc"] {
        let whole = decode(&fixture(name)).unwrap().samples;
        let mut decoder = TrackDecoder::open(&fixture(name), OpenOptions::default()).unwrap();
        for frame in [40_000usize, 1_153, 12_345, 66_000] {
            let landed = decoder.seek(Duration::from_secs_f64(frame as f64 / f64::from(RATE))).unwrap();
            assert_eq!((landed.as_secs_f64() * f64::from(RATE)).round() as usize, frame);
            let chunk = decoder.next_chunk().unwrap().unwrap();
            let count = chunk.samples.len().min(300);
            let worst = chunk.samples[..count]
                .iter()
                .zip(&whole[frame * 2..frame * 2 + count])
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert!(worst < 1e-4, "{name}, кадр {frame}: расхождение {worst}");
        }
    }
}

#[test]
fn truncated_musepack_plays_what_is_left_and_reports_the_rest() {
    let bytes = std::fs::read(fixture("mpc_sv8.mpc")).unwrap();
    let scratch = Scratch::new("mpc-cut");
    let decoded = decode(&scratch.write("cut.mpc", &bytes[..bytes.len() * 2 / 3])).unwrap();
    let frames = decoded.samples.len() / 2;
    assert!(frames > FRAMES / 3 && frames < FRAMES, "{frames} кадров");
    assert!(decoded.missing.is_some());
}

#[test]
fn mutated_musepack_never_panics() {
    let scratch = Scratch::new("mpc-fuzz");
    let mut rng = Rng(3);
    for name in ["mpc_sv8.mpc", "mpc_sv7.mpc"] {
        let original = std::fs::read(fixture(name)).unwrap();
        for round in 0..150 {
            let mut bytes = original.clone();
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(bytes.len());
                bytes[at] = rng.next() as u8;
            }
            if round % 10 == 0 {
                bytes.truncate(rng.below(bytes.len()));
            }
            if let Err(TrackError::Panicked(message)) = decode(&scratch.write("fuzz.mpc", &bytes)) {
                panic!("{name}, раунд {round}: паника {message}");
            }
        }
    }
}
