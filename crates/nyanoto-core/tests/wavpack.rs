//! WavPack от libwavpack: бит в бит 8–32 бит и float, 5.1, гибрид с .wvc и без, MD5, порча, перемотка.

mod common;

use std::time::Duration;

use common::{WavpackSource, assert_same, decode, encode_wavpack};
use nyanoto_core::{OpenOptions, TrackDecoder};
use nyanoto_testkit::{Rng, Scratch};
use symphonia::core::audio::Channels;

const RATE: u32 = 44_100;

/// Синусы разной частоты в каналах и шум, целые `bits` бит. Как в WAV,
/// неполные байты прижаты к старшим битам: 20 бит лежат в 24 с нулями внизу.
fn signal(frames: usize, channels: usize, bits: u32) -> Vec<i32> {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let amplitude = (1u64 << (bits - 1)) as f64 * 0.6;
    let noise = 1i64 << (bits / 2);
    let shift = bits.div_ceil(8) * 8 - bits;
    (0..frames)
        .flat_map(|frame| (0..channels).map(move |channel| (frame, channel)))
        .map(|(frame, channel)| {
            let phase = frame as f64 * (220.0 + 110.0 * channel as f64) * std::f64::consts::TAU / f64::from(RATE);
            let hiss = rng.below(2 * noise as usize + 1) as i64 - noise;
            ((phase.sin() * amplitude) as i32 + hiss as i32) << shift
        })
        .collect()
}

fn as_f32(samples: &[i32], bits: u32) -> Vec<f32> {
    let scale = 1.0 / (1u64 << (bits.div_ceil(8) * 8 - 1)) as f32;
    samples.iter().map(|sample| *sample as f32 * scale).collect()
}

fn source(samples: &[i32], channels: usize, bits: u32) -> WavpackSource<'_> {
    WavpackSource { samples, channels, rate: RATE, bits, mask: 0, float: false, hybrid: None, dsd: false }
}

#[test]
fn integers_decode_bit_exact_and_md5_matches() {
    let scratch = Scratch::new("wv-ints");
    for (bits, channels) in [(16, 2), (24, 1), (8, 2), (32, 2), (20, 2)] {
        let samples = signal(30_000, channels, bits);
        let file = encode_wavpack(&source(&samples, channels, bits));
        let path = scratch.write(&format!("{bits}.wv"), &file.wv);
        let decoded = decode(&path).unwrap();
        assert_same(&decoded.samples, &as_f32(&samples, bits), &format!("{bits} бит"));
        assert_eq!(decoded.errors, 0);
        assert_eq!(decoded.checksum, Some(true), "{bits} бит: MD5");
        assert_eq!(decoded.info.codec, "wavpack");
        assert_eq!(decoded.info.bits_per_sample, Some(bits));
        assert_eq!(decoded.info.channels, Some(channels));
        assert_eq!(decoded.info.sample_rate, Some(RATE));
    }
}

#[test]
fn floats_decode_bit_exact() {
    let samples: Vec<i32> = signal(20_000, 2, 24)
        .iter()
        .map(|sample| (*sample as f32 / 8_388_608.0 * 2.0).to_bits() as i32)
        .collect();
    let file = encode_wavpack(&WavpackSource { float: true, ..source(&samples, 2, 32) });
    let scratch = Scratch::new("wv-float");
    let decoded = decode(&scratch.write("float.wv", &file.wv)).unwrap();
    let want: Vec<f32> = samples.iter().map(|bits| f32::from_bits(*bits as u32)).collect();
    // Значения больше единицы тоже доходят как есть: плеер их не обрезает.
    assert!(want.iter().any(|sample| sample.abs() > 1.0));
    assert_same(&decoded.samples, &want, "float");
    assert_eq!(decoded.checksum, Some(true));
    assert_eq!(decoded.info.bits_per_sample, Some(32));
}

#[test]
fn surround_keeps_its_layout() {
    // 5.1 с боковыми каналами вместо тыловых.
    let mask = 0x60f;
    let samples = signal(10_000, 6, 16);
    let file = encode_wavpack(&WavpackSource { mask, ..source(&samples, 6, 16) });
    let scratch = Scratch::new("wv-51");
    let path = scratch.write("51.wv", &file.wv);
    let mut decoder = TrackDecoder::open(&path, OpenOptions::default()).unwrap();
    let chunk = decoder.next_chunk().unwrap().unwrap();
    assert_eq!(chunk.channels, Channels::from_bits_truncate(mask));
    let decoded = decode(&path).unwrap();
    assert_same(&decoded.samples, &as_f32(&samples, 16), "5.1");
}

#[test]
fn hybrid_is_lossless_with_its_correction_file_and_close_without_it() {
    let samples = signal(40_000, 2, 16);
    let file = encode_wavpack(&WavpackSource { hybrid: Some(3.0), ..source(&samples, 2, 16) });
    let scratch = Scratch::new("wv-hybrid");
    let path = scratch.write("hybrid.wv", &file.wv);
    let correction = scratch.write("hybrid.wvc", file.wvc.as_ref().unwrap());
    let want = as_f32(&samples, 16);

    let lossless = decode(&path).unwrap();
    assert_same(&lossless.samples, &want, "с .wvc");
    assert_eq!(lossless.checksum, Some(true));
    // Библиотека подписывает файл по тому, что о нём говорит ядро.
    assert_eq!(nyanoto_core::probe(&path).unwrap().format, "wavpack");

    std::fs::remove_file(correction).unwrap();
    let lossy = decode(&path).unwrap();
    assert_eq!(lossy.samples.len(), want.len());
    assert_ne!(lossy.samples, want, "без .wvc звук с потерями");
    let signal: f64 = want.iter().map(|x| f64::from(*x).powi(2)).sum();
    let noise: f64 = lossy.samples.iter().zip(&want).map(|(a, b)| f64::from(a - b).powi(2)).sum();
    let snr = 10.0 * (signal / noise).log10();
    assert!(snr > 15.0, "SNR {snr:.1} дБ");
    assert_eq!(lossy.errors, 0);
    let probe = nyanoto_core::probe(&path).unwrap();
    assert_eq!((probe.format.as_str(), probe.lossless), ("wavpack-lossy", Some(false)));
}

/// Битый блок второго моноканала libwavpack глушит молча — ловит наша проверка блоков.
#[test]
fn damaged_block_is_found() {
    let samples = signal(100_000, 2, 16);
    let mut file = encode_wavpack(&source(&samples, 2, 16)).wv;
    let middle = file.len() / 2;
    file[middle] ^= 0x5A;
    let scratch = Scratch::new("wv-damage");
    let decoded = decode(&scratch.write("damaged.wv", &file)).unwrap();
    assert_eq!(decoded.samples.len(), samples.len(), "длина та же");
    assert!(decoded.errors > 0, "повреждение замечено");
    assert_eq!(decoded.checksum, Some(false));
}

#[test]
fn seek_lands_on_the_sample() {
    let samples = signal(200_000, 2, 16);
    let file = encode_wavpack(&source(&samples, 2, 16));
    let scratch = Scratch::new("wv-seek");
    let path = scratch.write("seek.wv", &file.wv);
    let want = as_f32(&samples, 16);
    let mut decoder = TrackDecoder::open(&path, OpenOptions::default()).unwrap();
    for frame in [150_001usize, 7, 99_999, 199_999] {
        let landed = decoder.seek(Duration::from_secs_f64(frame as f64 / f64::from(RATE))).unwrap();
        assert_eq!((landed.as_secs_f64() * f64::from(RATE)).round() as usize, frame);
        let chunk = decoder.next_chunk().unwrap().unwrap();
        let count = chunk.samples.len().min(200);
        assert_same(&chunk.samples[..count], &want[frame * 2..frame * 2 + count], "после перемотки");
    }
}

#[test]
fn replay_gain_is_read_from_the_ape_tag() {
    let samples = signal(5_000, 2, 16);
    let mut file = encode_wavpack(&source(&samples, 2, 16)).wv;
    let items = [("REPLAYGAIN_TRACK_GAIN", "-4.50 dB"), ("REPLAYGAIN_TRACK_PEAK", "0.912")];
    let mut body = Vec::new();
    for (key, value) in items {
        body.extend_from_slice(&(value.len() as u32).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(key.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
    }
    file.extend_from_slice(&body);
    file.extend_from_slice(b"APETAGEX");
    file.extend_from_slice(&2000u32.to_le_bytes());
    file.extend_from_slice(&((body.len() + 32) as u32).to_le_bytes());
    file.extend_from_slice(&(items.len() as u32).to_le_bytes());
    file.extend_from_slice(&[0; 12]);
    let scratch = Scratch::new("wv-tags");
    let decoded = decode(&scratch.write("tagged.wv", &file)).unwrap();
    assert_eq!(decoded.info.replay_gain.track_gain_db, Some(-4.5));
    assert_eq!(decoded.info.replay_gain.track_peak, Some(0.912));
    assert_same(&decoded.samples, &as_f32(&samples, 16), "с тегом");
}

#[test]
fn mutated_wavpack_never_panics() {
    let samples = signal(20_000, 2, 16);
    let original = encode_wavpack(&source(&samples, 2, 16)).wv;
    let scratch = Scratch::new("wv-fuzz");
    let mut rng = Rng(11);
    for round in 0..200 {
        let mut bytes = original.clone();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(bytes.len());
            bytes[at] = rng.next() as u8;
        }
        if round % 10 == 0 {
            bytes.truncate(rng.below(bytes.len()));
        }
        if let Err(nyanoto_core::TrackError::Panicked(message)) = decode(&scratch.write("fuzz.wv", &bytes)) {
            panic!("раунд {round}: паника {message}");
        }
    }
}
