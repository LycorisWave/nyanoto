//! Сквозные проверки движка: звук в память и сравнение с файлами.


use std::f32::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nyanoto_testkit::Scratch;
use nyanoto_core::{Capture, EqBand, Engine, Equalizer, Event, FilterKind, OutputTarget, Speed, State};

const RATE: u32 = 44_100;
const TONE: f32 = 441.0;
const AMPLITUDE: f32 = 0.5;

/// Значение непрерывного синуса в кадре `frame`.
fn tone(frame: u64, rate: u32) -> f32 {
    (frame as f32 * TONE * TAU / rate as f32).sin() * AMPLITUDE
}

/// Пишет 16-битный стереофайл WAV с кадрами синуса `first..first + frames`.
fn write_wav(path: &Path, first: u64, frames: u64) {
    let data_len = (frames * 4) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for frame in first..first + frames {
        let sample = (tone(frame, RATE) * 32767.0).round() as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, bytes).unwrap();
}

/// Режет один непрерывный синус на файлы заданной длины.
fn album(scratch: &Scratch, lengths: &[u64]) -> Vec<PathBuf> {
    let mut first = 0;
    lengths
        .iter()
        .enumerate()
        .map(|(index, &frames)| {
            let path = scratch.path(&format!("{:02}.wav", index + 1));
            write_wav(&path, first, frames);
            first += frames;
            path
        })
        .collect()
}

/// Ждёт конца очереди и возвращает все события, пришедшие до него.
fn wait_finished(engine: &Engine) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match engine.events().recv_timeout(left) {
            Ok(Event::Finished) => return events,
            Ok(event) => events.push(event),
            Err(_) => panic!("очередь не доиграла за 30 секунд; события: {events:?}"),
        }
    }
}

fn wait_for(engine: &Engine, mut wanted: impl FnMut(&Event) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match engine.events().recv_timeout(left) {
            Ok(event) if wanted(&event) => return,
            Ok(_) => {}
            Err(_) => panic!("нужное событие не пришло за 10 секунд"),
        }
    }
}

fn started_names(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::TrackStarted(now) => now.path.file_name(),
            _ => None,
        })
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// Левый канал захваченного звука.
fn left(samples: &[f32]) -> Vec<f32> {
    samples.iter().step_by(2).copied().collect()
}

/// Наибольший скачок между соседями — щелчок на стыке виден сразу.
fn worst_jump(signal: &[f32]) -> f32 {
    signal
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

fn max_sine_step(rate: u32) -> f32 {
    TAU * TONE / rate as f32 * AMPLITUDE
}

#[test]
fn album_plays_without_gaps() {
    let scratch = Scratch::new("gapless");
    let lengths = [30_000, 17_011, 44_100, 9_999];
    let files = album(&scratch, &lengths);
    let total: u64 = lengths.iter().sum();

    let capture = Capture::new(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    engine.set_queue(files);
    engine.play();
    let events = wait_finished(&engine);

    assert_eq!(started_names(&events), ["01.wav", "02.wav", "03.wav", "04.wav"]);
    assert_eq!(engine.state(), State::Stopped);

    // Ни одного лишнего и ни одного потерянного кадра на трёх стыках.
    let signal = left(&capture.take());
    assert_eq!(signal.len() as u64, total);
    // Первые миллисекунды громкость нарастает, сравниваем после них.
    for (frame, sample) in signal.iter().enumerate().skip(2_000) {
        let expected = tone(frame as u64, RATE);
        assert!(
            (sample - expected).abs() < 1e-3,
            "кадр {frame}: {sample} вместо {expected}"
        );
    }
}

#[test]
fn album_plays_without_gaps_through_resampler() {
    let scratch = Scratch::new("gapless-resampled");
    let lengths = [30_000, 17_011, 44_100, 9_999];
    let files = album(&scratch, &lengths);
    let total: u64 = lengths.iter().sum();
    let out_rate = 48_000;

    let capture = Capture::new(out_rate, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    engine.set_queue(files);
    engine.play();
    let events = wait_finished(&engine);
    assert_eq!(started_names(&events).len(), 4);

    let signal = left(&capture.take());
    let expected = (total * u64::from(out_rate)).div_ceil(u64::from(RATE));
    assert_eq!(signal.len() as u64, expected);

    // Совпадает с посчитанным сразу, стыки тоже.
    let body = &signal[2_000..signal.len() - 2_000];
    for (offset, sample) in body.iter().enumerate() {
        let frame = (offset + 2_000) as u64;
        let wanted = tone(frame, out_rate);
        assert!(
            (sample - wanted).abs() < 0.01,
            "кадр {frame}: {sample} вместо {wanted}"
        );
    }
    assert!(worst_jump(body) < max_sine_step(out_rate) * 1.2);
}

#[test]
fn broken_files_are_skipped_not_fatal() {
    let scratch = Scratch::new("broken");
    let good = album(&scratch, &[20_000, 20_000]);

    let garbage = scratch.path("garbage.flac");
    let noise: Vec<u8> = (0..50_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    fs::write(&garbage, noise).unwrap();

    // Заголовок настоящего FLAC, после которого идёт мусор.
    let truncated = scratch.path("truncated.flac");
    let mut bytes = b"fLaC".to_vec();
    bytes.extend_from_slice(&[0x80, 0x00, 0x00, 0x22]);
    bytes.extend_from_slice(&[0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(&[0x0A, 0xC4, 0x42, 0xF0, 0x00, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(&[0u8; 16]);
    bytes.extend_from_slice(&[0xFF; 300]);
    fs::write(&truncated, bytes).unwrap();

    let empty = scratch.path("empty.mp3");
    fs::write(&empty, b"").unwrap();
    let missing = scratch.path("missing.flac");

    let capture = Capture::new(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    engine.set_queue(vec![
        garbage,
        good[0].clone(),
        truncated,
        empty,
        missing,
        good[1].clone(),
    ]);
    engine.play();
    let events = wait_finished(&engine);

    assert_eq!(started_names(&events), ["01.wav", "02.wav"]);
    let failures = events
        .iter()
        .filter(|event| matches!(event, Event::TrackFailed { .. }))
        .count();
    assert_eq!(failures, 4, "{events:?}");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            Event::TrackFailed { error: nyanoto_core::TrackError::Panicked(_), .. }
        )),
        "декодер запаниковал: {events:?}"
    );

    // Два целых трека прозвучали подряд, без дыры на месте битых.
    assert_eq!(left(&capture.take()).len(), 40_000);
}

#[test]
fn queue_of_only_broken_files_finishes() {
    let scratch = Scratch::new("all-broken");
    let files: Vec<PathBuf> = (0..3)
        .map(|index| {
            let path = scratch.path(&format!("{index}.flac"));
            fs::write(&path, b"not audio at all").unwrap();
            path
        })
        .collect();

    let engine = Engine::new(OutputTarget::Capture(Capture::new(RATE, 2)));
    engine.set_queue(files);
    engine.set_repeat(nyanoto_core::Repeat::All);
    engine.play();
    // Повтор очереди из одних битых файлов не должен крутиться вечно.
    let events = wait_finished(&engine);
    assert!(started_names(&events).is_empty());
}

#[test]
fn seek_lands_on_the_requested_sample() {
    let scratch = Scratch::new("seek");
    let files = album(&scratch, &[RATE as u64 * 4]);

    // Захват в темпе воспроизведения: иначе трек доиграет раньше, чем дойдёт пауза.
    let capture = Capture::paced(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    engine.set_queue(files);
    engine.play();
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(_)));
    engine.pause();
    wait_for(&engine, |event| matches!(event, Event::StateChanged(State::Paused)));

    // Перемотка на паузе: позиция меняется сразу, звук не идёт.
    engine.seek(Duration::from_secs(3));
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.position().is_none_or(|position| position < Duration::from_secs(3)) {
        assert!(Instant::now() < deadline, "позиция не обновилась: {:?}", engine.position());
        std::thread::sleep(Duration::from_millis(5));
    }
    let position = engine.position().unwrap();
    assert!(position < Duration::from_millis(3_050), "{position:?}");

    capture.take();
    engine.play();
    wait_finished(&engine);

    // После перемотки ровно секунда, и с того самого кадра.
    let signal = left(&capture.take());
    assert_eq!(signal.len(), RATE as usize);
    for (offset, sample) in signal.iter().enumerate().skip(2_000) {
        let wanted = tone(RATE as u64 * 3 + offset as u64, RATE);
        assert!((sample - wanted).abs() < 1e-3, "кадр {offset}: {sample} вместо {wanted}");
    }
}

#[test]
fn seek_right_after_play_is_not_lost() {
    let scratch = Scratch::new("seek-at-start");
    let files = album(&scratch, &[RATE as u64 * 4]);

    let capture = Capture::new(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    // Перемотка уходит раньше, чем трек успел зазвучать.
    engine.play_list(files, 0);
    engine.seek(Duration::from_secs(3));
    let events = wait_finished(&engine);
    assert_eq!(started_names(&events), ["01.wav"], "трек объявлен один раз");

    let signal = left(&capture.take());
    assert_eq!(signal.len(), RATE as usize, "осталась ровно последняя секунда");
    for (offset, sample) in signal.iter().enumerate().skip(2_000) {
        let wanted = tone(RATE as u64 * 3 + offset as u64, RATE);
        assert!((sample - wanted).abs() < 1e-3, "кадр {offset}: {sample} вместо {wanted}");
    }
}

#[test]
fn next_previous_and_stop_after_current() {
    let scratch = Scratch::new("navigation");
    let files = album(&scratch, &[RATE as u64 * 2, RATE as u64 * 2, RATE as u64 * 2]);

    let engine = Engine::new(OutputTarget::Capture(Capture::paced(RATE, 2)));
    engine.set_queue(files);
    engine.play();
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(_)));
    engine.pause();
    wait_for(&engine, |event| matches!(event, Event::StateChanged(State::Paused)));

    let name = |engine: &Engine| {
        engine
            .now_playing()
            .map(|now| now.path.file_name().unwrap().to_string_lossy().into_owned())
    };

    engine.next();
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(_)));
    assert_eq!(name(&engine).as_deref(), Some("02.wav"));
    assert_eq!(engine.state(), State::Paused);

    engine.previous();
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(_)));
    assert_eq!(name(&engine).as_deref(), Some("01.wav"));

    // «Стоп после текущего»: первый трек доигрывает, второй не начинается.
    engine.set_stop_after_current(true);
    engine.play();
    let events = wait_finished(&engine);
    assert!(started_names(&events).is_empty(), "{events:?}");
    assert_eq!(engine.state(), State::Stopped);
}

#[test]
fn slowed_album_keeps_every_track_and_its_length() {
    let scratch = Scratch::new("speed-length");
    let lengths = [RATE as u64, RATE as u64 / 2 + 333];
    let files = album(&scratch, &lengths);

    for (speed, keep_pitch) in [(0.5, true), (2.0, false)] {
        let capture = Capture::new(RATE, 2);
        let engine = Engine::new(OutputTarget::Capture(capture.clone()));
        engine.set_speed(Speed { factor: speed, keep_pitch });
        engine.play_list(files.clone(), 0);
        let events = wait_finished(&engine);
        assert_eq!(started_names(&events), ["01.wav", "02.wav"], "скорость {speed}");

        // Каждый трек растянут целиком и отдельно: длина складывается по трекам.
        let expected: u64 = lengths
            .iter()
            .map(|&frames| if keep_pitch { (frames as f64 / speed).round() as u64 } else { frames.div_ceil(2) })
            .sum();
        let got = capture.take().len() as u64 / 2;
        assert_eq!(got, expected, "скорость {speed}");
    }
}

#[test]
fn position_runs_at_the_chosen_speed() {
    let scratch = Scratch::new("speed-position");
    let files = album(&scratch, &[RATE as u64 * 4]);
    let engine = Engine::new(OutputTarget::Capture(Capture::paced(RATE, 2)));
    engine.set_speed(Speed { factor: 0.5, keep_pitch: true });
    engine.play_list(files, 0);
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(_)));
    assert!((engine.speed() - 0.5).abs() < 1e-9);

    // За секунду настоящего времени на половинной скорости проходит полсекунды трека.
    let started = Instant::now();
    let from = engine.position().unwrap().as_secs_f64();
    std::thread::sleep(Duration::from_millis(1000));
    let passed = engine.position().unwrap().as_secs_f64() - from;
    let real = started.elapsed().as_secs_f64();
    assert!((passed - real * 0.5).abs() < 0.1, "за {real:.2} с прошло {passed:.2} с трека");

    // Смена скорости на ходу продолжает с того же места.
    let before = engine.position().unwrap().as_secs_f64();
    engine.set_speed(Speed { factor: 1.5, keep_pitch: false });
    let deadline = Instant::now() + Duration::from_secs(5);
    while (engine.speed() - 1.5).abs() > 0.01 {
        assert!(Instant::now() < deadline, "скорость не сменилась");
        std::thread::sleep(Duration::from_millis(5));
    }
    let after = engine.position().unwrap().as_secs_f64();
    assert!((after - before).abs() < 0.2, "позиция прыгнула: {before:.2} → {after:.2}");
}

#[test]
fn equalizer_changes_the_level_of_its_band() {
    let scratch = Scratch::new("equalizer");
    let files = album(&scratch, &[RATE as u64 * 2]);
    let capture = Capture::new(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    engine.set_equalizer(Equalizer {
        enabled: true,
        preamp_db: 0.0,
        bands: vec![EqBand { kind: FilterKind::Peaking, freq: TONE, gain_db: -6.0, q: 1.0 }],
    });
    engine.play_list(files, 0);
    wait_finished(&engine);

    let signal = left(&capture.take());
    let settled = &signal[RATE as usize / 2..];
    let peak = settled.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let gain_db = 20.0 * (peak / AMPLITUDE).log10();
    assert!((gain_db + 6.0).abs() < 0.3, "{gain_db:.2} дБ вместо −6");
}

#[test]
fn crossfade_overlaps_different_albums_but_not_one_album() {
    let scratch = Scratch::new("crossfade");
    let seconds = RATE as u64 * 2;
    let file = |name: &str| {
        let path = scratch.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_wav(&path, 0, seconds);
        path
    };
    let other_albums = vec![file("a/01.wav"), file("b/01.wav")];
    let one_album = vec![file("c/01.wav"), file("c/02.wav")];

    for (files, overlap) in [(other_albums, RATE as u64 / 2), (one_album, 0)] {
        let capture = Capture::new(RATE, 2);
        let engine = Engine::new(OutputTarget::Capture(capture.clone()));
        engine.set_crossfade(Some(Duration::from_millis(500)));
        engine.play_list(files, 0);
        let events = wait_finished(&engine);
        assert_eq!(started_names(&events), ["01.wav", if overlap > 0 { "01.wav" } else { "02.wav" }]);
        let frames = capture.take().len() as u64 / 2;
        assert_eq!(frames, 2 * seconds - overlap, "наложение {overlap} кадров");
    }
}

#[test]
fn position_belongs_to_the_sounding_track() {
    let scratch = Scratch::new("position-owner");
    let files = album(&scratch, &[RATE as u64, RATE as u64]);

    let engine = Engine::new(OutputTarget::Capture(Capture::paced(RATE, 2)));
    engine.play_list(files.clone(), 0);
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(now) if now.path == files[0]));
    assert!(engine.position_in(&files[0]).is_some());
    assert_eq!(engine.position_in(&files[1]), None, "второй трек ещё не звучит");
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(now) if now.path == files[1]));
    assert_eq!(engine.position_in(&files[0]), None, "позиция второго трека не приписывается первому");
    assert!(engine.position_in(&files[1]).is_some());
    engine.stop();
}

#[test]
fn cued_queue_waits_silently_and_starts_where_it_was() {
    let scratch = Scratch::new("cue");
    let files = album(&scratch, &[RATE as u64 * 2, RATE as u64 * 3, RATE as u64 * 2]);
    let capture = Capture::new(RATE, 2);
    let engine = Engine::new(OutputTarget::Capture(capture.clone()));
    let changed = |event: &Event| matches!(event, Event::QueueChanged);

    // Так очередь восстанавливается после перезапуска: второй трек, с 2 с.
    engine.cue(files, 1, Duration::from_secs(2));
    wait_for(&engine, changed);
    let queue = engine.queue();
    assert_eq!(engine.current_id(), Some(queue[1].id));
    assert_eq!(engine.state(), State::Stopped);

    // Без игры «следующий» и перемотка только двигают подготовленный трек.
    engine.next();
    wait_for(&engine, changed);
    assert_eq!(engine.current_id(), Some(queue[2].id));
    engine.previous();
    wait_for(&engine, changed);
    assert_eq!(engine.current_id(), Some(queue[1].id));
    engine.seek(Duration::from_secs(2));
    // Первый трек — в конец очереди.
    engine.move_item(queue[0].id, 2);
    wait_for(&engine, changed);
    let order: Vec<u64> = engine.queue().iter().map(|item| item.id).collect();
    assert_eq!(order, [queue[1].id, queue[2].id, queue[0].id]);
    assert_eq!(engine.state(), State::Stopped);
    assert!(capture.take().is_empty(), "до «играть» звука нет");

    engine.play();
    let events = wait_finished(&engine);
    assert_eq!(started_names(&events), ["02.wav", "03.wav", "01.wav"]);
    let signal = left(&capture.take());
    assert_eq!(signal.len(), RATE as usize * 5, "секунда второго трека и два трека целиком");
    for (offset, sample) in signal.iter().enumerate().take(RATE as usize).skip(2_000) {
        let wanted = tone(RATE as u64 * 4 + offset as u64, RATE);
        assert!((sample - wanted).abs() < 1e-3, "кадр {offset}: {sample} вместо {wanted}");
    }
}

#[test]
fn play_now_keeps_the_queue() {
    let scratch = Scratch::new("play-now");
    let files = album(&scratch, &[RATE as u64 / 2, RATE as u64 / 2, RATE as u64 / 2]);
    // В темпе воспроизведения: иначе первый трек доиграет раньше, чем дойдёт команда.
    let engine = Engine::new(OutputTarget::Capture(Capture::paced(RATE, 2)));
    engine.set_queue(files[..2].to_vec());
    engine.play();
    wait_for(&engine, |event| matches!(event, Event::TrackStarted(_)));
    // Третий файл — «сейчас»: встаёт за первым и играет, второй остаётся после него.
    engine.play_now(vec![files[2].clone()]);
    let events = wait_finished(&engine);
    assert_eq!(started_names(&events), ["03.wav", "02.wav"]);
    let order: Vec<String> =
        engine.queue().iter().map(|item| item.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(order, ["01.wav", "03.wav", "02.wav"]);
}
