//! DSD: одни данные в DSF, DFF и WavPack дают один PCM до бита.
//! Плюс амплитуда, фаза и шум синуса, перемотка, обрезка, порядок каналов DFF, порча.

mod common;

use std::time::Duration;

use common::{DSD64, WavpackSource, assert_same, decode, dsd_tones, encode_wavpack, write_dff, write_dsf};
use nyanoto_core::{OpenOptions, TrackDecoder, TrackError};
use nyanoto_testkit::{Rng, Scratch};
use realfft::RealFftPlanner;

const TONE: (f64, f64) = (1_000.0, 0.5);
const OTHER: (f64, f64) = (3_000.0, 0.25);

/// Амплитуда и фаза тона `frequency` в канале `channel` методом наименьших
/// квадратов по кадрам `range`, плюс остаток (всё, что не тон).
fn fit(samples: &[f32], channels: usize, channel: usize, rate: u32, frequency: f64, range: std::ops::Range<usize>) -> (f64, f64) {
    let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for frame in range {
        let phase = std::f64::consts::TAU * frequency * frame as f64 / f64::from(rate);
        let (sin, cos) = phase.sin_cos();
        let y = f64::from(samples[frame * channels + channel]);
        ss += sin * sin;
        sc += sin * cos;
        cc += cos * cos;
        ys += y * sin;
        yc += y * cos;
    }
    let det = ss * cc - sc * sc;
    let a = (ys * cc - yc * sc) / det;
    let b = (yc * ss - ys * sc) / det;
    ((a * a + b * b).sqrt(), b.atan2(a))
}

/// Отношение сигнал/шум в полосе 20 Гц – 20 кГц по спектру с окном
/// Блэкмана–Харриса: бины тона и его соседей — сигнал, остальное — шум.
fn snr_in_band(samples: &[f32], channels: usize, channel: usize, rate: u32, frequency: f64) -> f64 {
    let size = 32_768;
    let start = 4_096;
    let mut frame: Vec<f64> = (0..size)
        .map(|index| {
            let t = index as f64 / (size - 1) as f64 * std::f64::consts::TAU;
            let window = 0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
            f64::from(samples[(start + index) * channels + channel]) * window
        })
        .collect();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(size);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut frame, &mut spectrum).unwrap();
    let bin = f64::from(rate) / size as f64;
    let tone = (frequency / bin).round() as usize;
    let (mut signal, mut noise) = (0.0, 0.0);
    for (index, value) in spectrum.iter().enumerate() {
        let hz = index as f64 * bin;
        let power = value.norm_sqr();
        if index.abs_diff(tone) <= 6 {
            signal += power;
        } else if (20.0..=20_000.0).contains(&hz) {
            noise += power;
        }
    }
    10.0 * (signal / noise).log10()
}

fn stereo_bits(seconds: f64) -> Vec<u8> {
    dsd_tones(DSD64, seconds, &[TONE, OTHER])
}

#[test]
fn dsf_dff_and_wavpack_give_the_same_pcm() {
    let bits = stereo_bits(1.0);
    let scratch = Scratch::new("dsd-same");
    let dsf = decode(&scratch.write("a.dsf", &write_dsf(&bits, 2, DSD64))).unwrap();
    let dff = decode(&scratch.write("a.dff", &write_dff(&bits, &[b"SLFT", b"SRGT"], DSD64))).unwrap();
    let samples: Vec<i32> = bits.iter().map(|byte| i32::from(*byte)).collect();
    let wavpack = encode_wavpack(&WavpackSource {
        samples: &samples,
        channels: 2,
        rate: DSD64,
        bits: 8,
        mask: 0x3,
        float: false,
        hybrid: None,
        dsd: true,
    });
    let wv = decode(&scratch.write("a.wv", &wavpack.wv)).unwrap();
    assert_eq!(dsf.rate, 88_200);
    assert_eq!(dsf.info.sample_rate, Some(88_200));
    assert_eq!(dsf.info.codec, "dsf");
    assert_eq!(dff.info.codec, "dff");
    assert_eq!(wv.info.codec, "wavpack-dsd");
    // Секунда DSD64 — ровно 88 200 кадров PCM.
    assert_eq!(dsf.samples.len(), 88_200 * 2);
    assert_same(&dff.samples, &dsf.samples, "DFF против DSF");
    assert_same(&wv.samples, &dsf.samples, "WavPack против DSF");
    assert_eq!(dsf.errors + dff.errors + wv.errors, 0);
}

#[test]
fn pcm_keeps_level_phase_and_is_clean_in_band() {
    let bits = stereo_bits(1.0);
    let scratch = Scratch::new("dsd-quality");
    let decoded = decode(&scratch.write("q.dsf", &write_dsf(&bits, 2, DSD64))).unwrap();
    let rate = decoded.rate;
    for (channel, (frequency, amplitude)) in [TONE, OTHER].into_iter().enumerate() {
        let (level, phase) = fit(&decoded.samples, 2, channel, rate, frequency, 2_000..80_000);
        let error_db = 20.0 * (level / amplitude).log10();
        assert!(error_db.abs() < 0.01, "канал {channel}: уровень {error_db:+.4} дБ");
        // Фаза в кадрах PCM: задержка фильтров компенсирована до доли кадра.
        let shift = phase / (std::f64::consts::TAU * frequency) * f64::from(rate);
        assert!(shift.abs() < 0.05, "канал {channel}: сдвиг {shift:.3} кадра");
        // Шум задаёт тестовый модулятор (~−80 дБ), конвертер добавляет много меньше.
        let snr = snr_in_band(&decoded.samples, 2, channel, rate, frequency);
        let floor = 20.0 * amplitude.log10() - snr;
        assert!(floor < -75.0, "канал {channel}: шум {floor:.1} дБ от полной шкалы");
    }
}

#[test]
fn dsd_silence_stays_silent() {
    // 0x69 — тишина DSD: поровну единиц и нулей, тоны только на частотах,
    // кратных 352,8 кГц, их должен убрать фильтр.
    let bits = vec![0x69u8; 2 * 352_800 / 2];
    let scratch = Scratch::new("dsd-silence");
    let decoded = decode(&scratch.write("quiet.dsf", &write_dsf(&bits, 2, DSD64))).unwrap();
    let peak = decoded.samples.iter().fold(0f32, |peak, sample| peak.max(sample.abs()));
    let db = 20.0 * f64::from(peak).log10();
    assert!(db < -120.0, "пик тишины {db:.1} дБ");
}

#[test]
fn seek_gives_the_same_samples_as_playing_through() {
    let bits = stereo_bits(1.5);
    let scratch = Scratch::new("dsd-seek");
    let path = scratch.write("s.dsf", &write_dsf(&bits, 2, DSD64));
    let whole = decode(&path).unwrap().samples;
    let mut decoder = TrackDecoder::open(&path, OpenOptions::default()).unwrap();
    for frame in [100_000usize, 3, 50_001, 132_299] {
        let landed = decoder.seek(Duration::from_secs_f64(frame as f64 / 88_200.0)).unwrap();
        assert_eq!((landed.as_secs_f64() * 88_200.0).round() as usize, frame);
        let chunk = decoder.next_chunk().unwrap().unwrap();
        let count = chunk.samples.len().min(400);
        assert_same(&chunk.samples[..count], &whole[frame * 2..frame * 2 + count], "после перемотки");
    }
}

#[test]
fn dff_channels_are_put_in_place() {
    // В файле правый канал записан первым.
    let bits = stereo_bits(0.3);
    let swapped: Vec<u8> = bits.as_chunks::<2>().0.iter().flat_map(|[left, right]| [*right, *left]).collect();
    let scratch = Scratch::new("dsd-order");
    let normal = decode(&scratch.write("n.dff", &write_dff(&bits, &[b"SLFT", b"SRGT"], DSD64))).unwrap();
    let reordered = decode(&scratch.write("r.dff", &write_dff(&swapped, &[b"SRGT", b"SLFT"], DSD64))).unwrap();
    assert_same(&reordered.samples, &normal.samples, "порядок каналов");
}

#[test]
fn dsd128_becomes_176_4_khz() {
    let bits = dsd_tones(DSD64 * 2, 0.5, &[TONE]);
    let scratch = Scratch::new("dsd128");
    let decoded = decode(&scratch.write("128.dsf", &write_dsf(&bits, 1, DSD64 * 2))).unwrap();
    assert_eq!(decoded.rate, 176_400);
    assert_eq!(decoded.samples.len(), 88_200);
    let (level, _) = fit(&decoded.samples, 1, 0, 176_400, TONE.0, 4_000..80_000);
    assert!((20.0 * (level / TONE.1).log10()).abs() < 0.01);
}

#[test]
fn passband_is_35_khz_for_dsd64_and_50_khz_above() {
    // Тон 45 кГц: у DSD64 он за полосой (≥53,2 кГц он подавлен полностью, а
    // 45 кГц — на склоне), у DSD128 — внутри ровной полосы до 50 кГц.
    let scratch = Scratch::new("dsd-band");
    let tone = (45_000.0, 0.25);
    let dsd128 = decode(&scratch.write("128.dsf", &write_dsf(&dsd_tones(DSD64 * 2, 0.5, &[tone]), 1, DSD64 * 2))).unwrap();
    let (level, _) = fit(&dsd128.samples, 1, 0, 176_400, tone.0, 4_000..80_000);
    assert!((20.0 * (level / tone.1).log10()).abs() < 0.05, "DSD128: 45 кГц должны пройти");
    let dsd64 = decode(&scratch.write("64.dsf", &write_dsf(&dsd_tones(DSD64, 0.5, &[tone]), 1, DSD64))).unwrap();
    let (level, _) = fit(&dsd64.samples, 1, 0, 88_200, tone.0, 2_000..40_000);
    assert!(20.0 * (level / tone.1).log10() < -3.0, "DSD64: 45 кГц за полосой");
}

#[test]
fn truncated_dsf_plays_what_is_left() {
    let bits = stereo_bits(1.0);
    let file = write_dsf(&bits, 2, DSD64);
    let scratch = Scratch::new("dsd-cut");
    let whole = decode(&scratch.write("whole.dsf", &file)).unwrap();
    // Обрыв посреди второй группы блоков.
    let cut = decode(&scratch.write("cut.dsf", &file[..92 + 2 * 4096 + 3000])).unwrap();
    let frames = cut.samples.len() / 2;
    assert!(frames > 1_000 && frames < 3_000, "{frames} кадров");
    // Последние кадры перед обрывом затронуты хвостом фильтра, начало — нет.
    let safe = (frames - 100) * 2;
    assert_same(&cut.samples[..safe], &whole.samples[..safe], "до обрыва");
    assert!(cut.missing.is_some());
}

/// DST не наш: уходит в FFmpeg, а без него — «не поддерживается».
#[test]
fn dst_is_not_decoded_as_plain_dsd() {
    let mut file = write_dff(&stereo_bits(0.1), &[b"SLFT", b"SRGT"], DSD64);
    let at = file.windows(5).position(|window| window == b"DSD \x0e").unwrap();
    file[at..at + 4].copy_from_slice(b"DST ");
    let scratch = Scratch::new("dsd-dst");
    if let Ok(decoded) = decode(&scratch.write("dst.dff", &file)) {
        assert_ne!(decoded.info.codec, "dff");
    }
}

#[test]
fn mutated_dsd_never_panics() {
    let bits = stereo_bits(0.2);
    let scratch = Scratch::new("dsd-fuzz");
    let mut rng = Rng(5);
    for (name, original) in [("f.dsf", write_dsf(&bits, 2, DSD64)), ("f.dff", write_dff(&bits, &[b"SLFT", b"SRGT"], DSD64))] {
        for round in 0..100 {
            let mut bytes = original.clone();
            // Портятся заголовки: данные DSD любые допустимы.
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(200.min(bytes.len()));
                bytes[at] = rng.next() as u8;
            }
            if round % 10 == 0 {
                bytes.truncate(rng.below(bytes.len()));
            }
            if let Err(TrackError::Panicked(message)) = decode(&scratch.write(name, &bytes)) {
                panic!("{name}, раунд {round}: паника {message}");
            }
        }
    }
}
