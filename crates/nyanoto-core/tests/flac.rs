//! FLAC: целые, битые, обрезанные и случайно испорченные.


use std::path::Path;
use std::time::{Duration, Instant};

use nyanoto_testkit::{FLAC_BLOCK, FLAC_MD5, Flac, Rng, Scratch, encode_flac, signal};
use nyanoto_core::{
    Capture, Engine, Event, OpenOptions, OutputTarget, TrackDecoder, TrackError, TrackInfo, guard,
};

const RATE: u32 = 44_100;

struct Decoded {
    /// Чередующиеся стереосэмплы.
    samples: Vec<f32>,
    errors: u64,
    md5_ok: Option<bool>,
    missing: Option<Duration>,
    info: TrackInfo,
}

fn decode(path: &Path) -> Result<Decoded, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions { verify: true })?;
        let mut samples = Vec::new();
        while let Some(chunk) = decoder.next_chunk()? {
            samples.extend_from_slice(chunk.samples);
            // Повреждённый файл не должен разворачиваться в бесконечный звук.
            assert!(samples.len() < 200_000_000, "декодер не останавливается");
        }
        Ok(Decoded {
            samples,
            errors: decoder.decode_errors(),
            md5_ok: decoder.finalize(),
            missing: decoder.missing(),
            info: decoder.info().clone(),
        })
    })
}

fn as_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|sample| f32::from(*sample) / 32_768.0).collect()
}

/// Десять блоков и неполный одиннадцатый.
fn source() -> (Vec<i16>, Flac) {
    let samples = signal(FLAC_BLOCK * 10 + 1234, RATE);
    let flac = encode_flac(&samples, RATE);
    (samples, flac)
}

/// Портит один байт в середине звуковых данных кадра.
fn damage_frame(flac: &Flac, frame: usize) -> Vec<u8> {
    let mut bytes = flac.bytes.clone();
    let range = &flac.frames[frame];
    bytes[range.start + range.len() / 2] ^= 0x5A;
    bytes
}

#[test]
fn flac_decodes_bit_exact_and_md5_matches() {
    let scratch = Scratch::new("flac-exact");
    let (samples, flac) = source();
    let decoded = decode(&scratch.write("a.flac", &flac.bytes)).unwrap();

    assert_eq!(decoded.samples, as_f32(&samples));
    assert_eq!(decoded.errors, 0);
    assert_eq!(decoded.md5_ok, Some(true));
    assert_eq!(decoded.missing, None);
    assert_eq!(decoded.info.codec, "flac");
    assert_eq!(decoded.info.sample_rate, Some(RATE));
    assert_eq!(decoded.info.bits_per_sample, Some(16));
    assert_eq!(decoded.info.channels, Some(2));
}

#[test]
fn wrong_md5_is_reported() {
    let scratch = Scratch::new("flac-md5");
    let (samples, flac) = source();
    let mut bytes = flac.bytes;
    bytes[FLAC_MD5.start] ^= 0xFF;
    let decoded = decode(&scratch.write("a.flac", &bytes)).unwrap();

    // Звук цел, но о несовпадении суммы узнаём.
    assert_eq!(decoded.samples, as_f32(&samples));
    assert_eq!(decoded.md5_ok, Some(false));
}

#[test]
fn lost_frame_becomes_silence_of_the_same_length() {
    let scratch = Scratch::new("flac-lost-frame");
    let (samples, flac) = source();
    for lost in [0, 4, 9] {
        let decoded = decode(&scratch.write("a.flac", &damage_frame(&flac, lost))).unwrap();
        let expected = as_f32(&samples);
        assert_eq!(decoded.samples.len(), expected.len(), "кадр {lost}");
        assert!(decoded.errors > 0, "кадр {lost}");

        let hole = lost * FLAC_BLOCK * 2..(lost + 1) * FLAC_BLOCK * 2;
        assert!(
            decoded.samples[hole.clone()].iter().all(|sample| *sample == 0.0),
            "кадр {lost}: на месте битого кадра не тишина"
        );
        assert_eq!(decoded.samples[..hole.start], expected[..hole.start], "кадр {lost}");
        assert_eq!(decoded.samples[hole.end..], expected[hole.end..], "кадр {lost}");
    }
}

#[test]
fn truncated_flac_plays_what_is_left_and_reports_the_rest() {
    let scratch = Scratch::new("flac-truncated");
    let (samples, flac) = source();
    // Обрыв посреди шестого кадра: пять кадров целы.
    let cut = flac.frames[5].start + 1000;
    let decoded = decode(&scratch.write("a.flac", &flac.bytes[..cut])).unwrap();

    let whole = 5 * FLAC_BLOCK * 2;
    assert_eq!(decoded.samples[..whole], as_f32(&samples)[..whole]);
    assert_eq!(decoded.samples.len(), whole);
    let missing = decoded.missing.expect("обрыв не замечен");
    let expected = (samples.len() - whole) as f64 / 2.0 / f64::from(RATE);
    assert!((missing.as_secs_f64() - expected).abs() < 1e-6, "{missing:?}");
}

#[test]
fn seek_without_seektable_is_sample_accurate() {
    let scratch = Scratch::new("flac-seek");
    let samples = signal(RATE as usize * 3, RATE);
    let path = scratch.write("a.flac", &encode_flac(&samples, RATE).bytes);

    // Таблицы поиска нет; цели и на границе кадра, и внутри.
    for target in [FLAC_BLOCK * 3, RATE as usize, RATE as usize * 2 + 777] {
        let mut decoder = TrackDecoder::open(&path, OpenOptions::default()).unwrap();
        let wanted = Duration::from_secs_f64(target as f64 / f64::from(RATE));
        let landed = decoder.seek(wanted).unwrap();
        assert!(
            (landed.as_secs_f64() - wanted.as_secs_f64()).abs() < 1e-6,
            "{landed:?} вместо {wanted:?}"
        );
        let mut rest = Vec::new();
        while let Some(chunk) = decoder.next_chunk().unwrap() {
            rest.extend_from_slice(chunk.samples);
        }
        assert_eq!(rest, as_f32(&samples)[target * 2..], "цель {target}");
        assert_eq!(decoder.decode_errors(), 0);
    }
}

#[test]
fn mutated_flac_never_panics() {
    let scratch = Scratch::new("flac-mutated");
    let flac = encode_flac(&signal(FLAC_BLOCK * 3 + 500, RATE), RATE);
    let mut rng = Rng(0x00C0_FFEE_1234_5678);
    let started = Instant::now();

    for round in 0..600 {
        let mut bytes = flac.bytes.clone();
        match round % 6 {
            // Случайные байты где угодно.
            0 => {
                for _ in 0..1 + rng.below(16) {
                    let at = rng.below(bytes.len());
                    bytes[at] = rng.next() as u8;
                }
            }
            // Заголовок файла и STREAMINFO: частота, каналы, разрядность, длина.
            1 => {
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(42);
                    bytes[at] = rng.next() as u8;
                }
            }
            // Заголовки кадров: размер блока, номер кадра, синхрокод.
            2 => {
                let frame = &flac.frames[rng.below(flac.frames.len())];
                let at = frame.start + rng.below(8);
                bytes[at] = rng.next() as u8;
            }
            // Обрыв в случайном месте.
            3 => bytes.truncate(rng.below(bytes.len())),
            // Вырезанный кусок.
            4 => {
                let from = rng.below(bytes.len());
                let to = (from + rng.below(20_000)).min(bytes.len());
                bytes.drain(from..to);
            }
            // Участок, забитый единицами: сплошные ложные синхрокоды.
            _ => {
                let from = rng.below(bytes.len());
                let to = (from + rng.below(5_000)).min(bytes.len());
                bytes[from..to].fill(0xFF);
            }
        }
        if let Err(TrackError::Panicked(message)) = decode(&scratch.write("m.flac", &bytes)) {
            scratch.write("panicked.flac", &bytes);
            panic!("раунд {round}: декодер запаниковал: {message}");
        }
    }
    // Ни одно повреждение не должно подвешивать декодер.
    assert!(started.elapsed() < Duration::from_secs(120), "{:?}", started.elapsed());
}

#[test]
fn damaged_flac_in_queue_is_reported_and_keeps_its_length() {
    let scratch = Scratch::new("flac-queue");
    let (samples, flac) = source();
    let good = scratch.write("good.flac", &flac.bytes);
    let damaged = scratch.write("damaged.flac", &damage_frame(&flac, 3));
    let truncated = scratch.write("truncated.flac", &flac.bytes[..flac.frames[8].start]);

    let capture = Capture::new(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    engine.set_queue(vec![good, damaged, truncated]);
    engine.play();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match engine.events().recv_timeout(left) {
            Ok(Event::Finished) => break,
            Ok(event) => events.push(event),
            Err(_) => panic!("очередь не доиграла; события: {events:?}"),
        }
    }

    let reports: Vec<(String, u64, bool)> = events
        .iter()
        .filter_map(|event| match event {
            Event::TrackDamaged {
                path,
                decode_errors,
                missing,
                ..
            } => Some((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                *decode_errors,
                missing.is_some(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 2, "{events:?}");
    assert_eq!(reports[0].0, "damaged.flac");
    assert!(reports[0].1 > 0 && !reports[0].2);
    assert_eq!(reports[1].0, "truncated.flac");
    assert!(reports[1].2);
    assert!(!events.iter().any(|event| matches!(event, Event::TrackFailed { .. })), "{events:?}");

    // Целый и битый треки звучат полную длину, обрезанный — сколько в нём есть.
    let frames = capture.take().len() / 2;
    assert_eq!(frames, samples.len() / 2 * 2 + 8 * FLAC_BLOCK);
}

#[test]
fn replay_gain_tags_set_the_loudness() {
    use nyanoto_core::Loudness;

    let scratch = Scratch::new("flac-replay-gain");
    let samples = signal(FLAC_BLOCK * 4, RATE);
    // −6,02 дБ — ровно вдвое тише. У альбома −12,04 дБ, вчетверо.
    let tagged = nyanoto_testkit::encode_flac_tagged(
        &samples,
        RATE,
        &[
            ("REPLAYGAIN_TRACK_GAIN", "-6.02 dB"),
            ("REPLAYGAIN_ALBUM_GAIN", "-12.04 dB"),
            ("REPLAYGAIN_TRACK_PEAK", "0.5"),
        ],
        0,
    );
    let path = scratch.write("rg.flac", &tagged.bytes);
    let info = TrackDecoder::open(&path, OpenOptions::default()).unwrap().info().clone();
    assert_eq!(info.replay_gain.track_gain_db, Some(-6.02));
    assert_eq!(info.replay_gain.track_peak, Some(0.5));

    let loudness_of = |loudness: Loudness| {
        let capture = Capture::new(RATE, 2);
        let engine = Engine::new(OutputTarget::Capture(capture.clone()));
        engine.set_loudness(loudness);
        engine.set_queue(vec![path.clone()]);
        engine.play();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(engine.events().recv_timeout(deadline - Instant::now()), Ok(Event::Finished)) {
            assert!(Instant::now() < deadline, "трек не доиграл");
        }
        // Громкость по среднеквадратичному после разгона громкости в начале.
        let played = capture.take();
        let tail = &played[4_000..];
        (tail.iter().map(|sample| sample * sample).sum::<f32>() / tail.len() as f32).sqrt()
    };
    let off = loudness_of(Loudness::Off);
    let track = loudness_of(Loudness::Track);
    let album = loudness_of(Loudness::Album);
    assert!((track / off - 0.5).abs() < 0.01, "по треку: {}", track / off);
    assert!((album / off - 0.25).abs() < 0.01, "по альбому: {}", album / off);

    // Выше пика не поднимаем.
    let loud = nyanoto_core::ReplayGain { track_gain_db: Some(12.0), track_peak: Some(0.8), ..Default::default() };
    assert!((Loudness::Track.factor(&loud) - 1.25).abs() < 1e-6);
    assert_eq!(Loudness::Track.factor(&nyanoto_core::ReplayGain::default()), 1.0);
}

#[test]
fn full_check_finds_wrong_md5_and_truncation() {
    use std::sync::atomic::AtomicBool;

    let scratch = Scratch::new("flac-health");
    let (_, flac) = source();
    let stop = AtomicBool::new(false);

    let whole = scratch.write("whole.flac", &flac.bytes);
    let health = nyanoto_core::verify_file(&whole, &stop).unwrap().unwrap();
    assert_eq!(health.md5, Some(true));
    assert!(!health.is_damaged(), "{health:?}");
    assert!(health.audio_ms > 0);

    let mut wrong = flac.bytes.clone();
    wrong[FLAC_MD5.start] ^= 0xFF;
    let wrong = scratch.write("wrong.flac", &wrong);
    let health = nyanoto_core::verify_file(&wrong, &stop).unwrap().unwrap();
    assert_eq!(health.md5, Some(false));
    assert!(health.is_damaged());

    let cut = flac.frames[1].end;
    let truncated = scratch.write("cut.flac", &flac.bytes[..cut]);
    let health = nyanoto_core::verify_file(&truncated, &stop).unwrap().unwrap();
    assert!(health.missing_ms.is_some(), "{health:?}");
    assert!(health.is_damaged());

    assert_eq!(nyanoto_core::verify_file(&whole, &AtomicBool::new(true)).unwrap(), None);
}
