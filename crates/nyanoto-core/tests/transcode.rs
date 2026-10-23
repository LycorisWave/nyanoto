//! Конвертация: FLAC и WAV без потерь, понижение частоты и разрядности.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use nyanoto_core::{ConvertOptions, OpenOptions, TargetFormat, TrackDecoder, convert_file};
use nyanoto_testkit::{Scratch, encode_flac, signal};

/// Все сэмплы файла, проверка MD5, частота и разрядность.
fn decode(path: &Path) -> (Vec<f32>, Option<bool>, u32, u32) {
    let mut decoder = TrackDecoder::open(path, OpenOptions { verify: true }).unwrap();
    let info = decoder.info().clone();
    let mut samples = Vec::new();
    while let Some(chunk) = decoder.next_chunk().unwrap() {
        samples.extend_from_slice(chunk.samples);
    }
    assert_eq!(decoder.decode_errors(), 0);
    (samples, decoder.finalize(), info.sample_rate.unwrap(), info.bits_per_sample.unwrap_or(0))
}

/// 24-битный WAV: синус с шумом, чтобы были задействованы все разряды.
fn wav24(path: &Path, frames: usize, rate: u32) {
    let mut bytes = Vec::new();
    let data = (frames * 6) as u32;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 6).to_le_bytes());
    bytes.extend_from_slice(&6u16.to_le_bytes());
    bytes.extend_from_slice(&24u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    let mut seed = 0x1234_5678u32;
    for frame in 0..frames {
        let tone = (frame as f64 * 997.0 * std::f64::consts::TAU / f64::from(rate)).sin() * 6_000_000.0;
        for channel in 0..2 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 12) as i32 % 20_000 - 10_000;
            let value = (tone as i32 * if channel == 0 { 1 } else { -1 }) + noise;
            bytes.extend_from_slice(&value.to_le_bytes()[..3]);
        }
    }
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn flac_and_wav_round_trip_bit_exact() {
    let scratch = Scratch::new("transcode-exact");
    let stop = AtomicBool::new(false);
    let as_is = |format| ConvertOptions { format, rate: None, bits: None };

    // 16 бит: FLAC → FLAC и FLAC → WAV.
    let source = scratch.write("source16.flac", &encode_flac(&signal(44_100 * 3 + 777, 44_100), 44_100).bytes);
    let (original, _, _, _) = decode(&source);
    for (name, format) in [("out16.flac", TargetFormat::Flac), ("out16.wav", TargetFormat::Wav)] {
        let target = scratch.path(name);
        assert!(convert_file(&source, &target, &as_is(format), &stop).unwrap());
        let (samples, md5, rate, bits) = decode(&target);
        assert_eq!((rate, bits), (44_100, 16), "{name}");
        assert_eq!(samples, original, "{name}: звук не тот");
        if format == TargetFormat::Flac {
            assert_eq!(md5, Some(true), "сумма MD5 в STREAMINFO верна");
        }
    }

    // 24 бит: WAV → FLAC, и FLAC выходит заметно меньше WAV.
    let wav = scratch.path("source24.wav");
    wav24(&wav, 48_000 * 2 + 123, 48_000);
    let (original, _, _, _) = decode(&wav);
    let flac = scratch.path("out24.flac");
    assert!(convert_file(&wav, &flac, &as_is(TargetFormat::Flac), &stop).unwrap());
    let (samples, md5, rate, bits) = decode(&flac);
    assert_eq!((rate, bits, md5), (48_000, 24, Some(true)));
    assert_eq!(samples, original);
    let (wav_size, flac_size) = (std::fs::metadata(&wav).unwrap().len(), std::fs::metadata(&flac).unwrap().len());
    assert!(flac_size * 10 < wav_size * 9, "сжатие: {flac_size} против {wav_size}");
    assert!(!scratch.path("out24.nyanoto-part").exists(), "временный файл убран");
}

#[test]
fn downsampling_to_cd_quality() {
    let scratch = Scratch::new("transcode-cd");
    let wav = scratch.path("hires.wav");
    wav24(&wav, 48_000 * 2, 48_000);
    let target = scratch.path("cd.flac");
    let options = ConvertOptions { format: TargetFormat::Flac, rate: Some(44_100), bits: Some(16) };
    assert!(convert_file(&wav, &target, &options, &AtomicBool::new(false)).unwrap());
    let (samples, md5, rate, bits) = decode(&target);
    assert_eq!((rate, bits, md5), (44_100, 16, Some(true)));
    assert_eq!(samples.len(), 44_100 * 2 * 2, "две секунды стерео");

    // Прерванная конвертация не оставляет файлов.
    let stopped = scratch.path("stopped.flac");
    assert!(!convert_file(&wav, &stopped, &options, &AtomicBool::new(true)).unwrap());
    assert!(!stopped.exists() && !scratch.path("stopped.nyanoto-part").exists());
}
