//! Консольный плеер: проверка движка и библиотеки без интерфейса.

mod library;

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nyanoto_core::{
    Capture, Engine, Event, OpenOptions, OutputTarget, Repeat, State, TrackDecoder, TrackError,
    guard,
};
use nyanoto_library::AUDIO_EXTENSIONS;

const USAGE: &str = "\
nyanoto-cli — консольный плеер Nyanoto

Использование:
  nyanoto-cli play  [--for СЕКУНДЫ] [--null] [--exclusive] ПУТЬ...   воспроизвести файлы и папки
  nyanoto-cli check [-v] ПУТЬ...                       декодировать без звука и проверить
  nyanoto-cli scan  [--db ФАЙЛ] [ПАПКА...]             добавить папки в библиотеку и просканировать
  nyanoto-cli find  [--db ФАЙЛ] СЛОВА...               найти треки в библиотеке
  nyanoto-cli library [--db ФАЙЛ]                      сводка по библиотеке и проблемные файлы
  nyanoto-cli gen-library [--cover-kb N] ПАПКА ЧИСЛО   создать искусственную коллекцию для проверок
  nyanoto-cli setting [--db ФАЙЛ] КЛЮЧ [ЗНАЧЕНИЕ]      прочитать или задать настройку плеера
  nyanoto-cli inspect [--sensitivity low|medium|high] ПУТЬ...  проверить подлинность качества
  nyanoto-cli convert [--wav] [--cd] ИЗ В              перекодировать файл (FLAC по умолчанию; --cd: 16 бит, 44,1 кГц)

  --for СЕКУНДЫ   остановиться через столько секунд
  --null          играть в память, а не на устройство, с максимальной скоростью
  --exclusive     эксклюзивный вывод WASAPI в формате файла (Windows)
  -v              показать сведения о каждом файле, а не только о проблемных
  --db ФАЙЛ       файл базы; без него берётся база в папке данных пользователя

Команды во время воспроизведения (ввод и Enter):
  p        пауза / продолжить
  n, b     следующий / предыдущий трек
  s СЕК    перейти на секунду СЕК текущего трека
  v 0-100  громкость
  r        повтор: выкл / очередь / трек
  a        остановиться после текущего трека
  i        что играет и где
  q        выход";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("play") => play(args.collect()),
        Some("check") => check(args.collect()),
        Some("scan") => library::scan(args.collect()),
        Some("find") => library::find(args.collect()),
        Some("library") => library::info(args.collect()),
        Some("gen-library") => library::generate(args.collect()),
        Some("setting") => library::setting(args.collect()),
        Some("inspect") => inspect(args.collect()),
        Some("convert") => convert(args.collect()),
        _ => {
            println!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Раскрывает папки в отсортированные списки звуковых файлов.
fn collect_files(paths: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths.iter().map(PathBuf::from) {
        if path.is_dir() {
            let start = files.len();
            walk(&path, &mut files);
            files[start..].sort_by_cached_key(|file| file.to_string_lossy().to_lowercase());
        } else {
            files.push(path);
        }
    }
    files
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("не удалось прочитать папку {}", dir.display());
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            walk(&path, files);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                AUDIO_EXTENSIONS.iter().any(|known| known.eq_ignore_ascii_case(extension))
            })
        {
            files.push(path);
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

// ───────────────────────────── play ─────────────────────────────

/// Формат, в котором `--null` принимает звук: как у типичного устройства,
/// чтобы ресемплер работал так же, как при настоящем воспроизведении.
const NULL_RATE: u32 = 48_000;
const NULL_CHANNELS: usize = 2;

fn play(mut args: Vec<String>) -> ExitCode {
    let mut limit = None;
    let mut capture = None;
    let mut exclusive = false;
    loop {
        match args.first().map(String::as_str) {
            Some("--for") => {
                let Some(seconds) = args.get(1).and_then(|value| value.parse::<f64>().ok()) else {
                    eprintln!("после --for нужно число секунд");
                    return ExitCode::from(2);
                };
                limit = Some(Duration::from_secs_f64(seconds.max(0.0)));
                args.drain(..2);
            }
            Some("--null") => {
                capture = Some(Capture::new(NULL_RATE, NULL_CHANNELS));
                args.remove(0);
            }
            Some("--exclusive") => {
                exclusive = true;
                args.remove(0);
            }
            _ => break,
        }
    }
    let files = collect_files(&args);
    if files.is_empty() {
        eprintln!("нечего играть");
        return ExitCode::from(2);
    }
    println!("в очереди: {}", files.len());

    let engine = Engine::new(match &capture {
        Some(capture) => OutputTarget::Capture(capture.clone()),
        None => OutputTarget::Device,
    });
    engine.set_exclusive(exclusive);
    engine.set_queue(files);
    engine.play();
    // Для `--null`: сколько звука дошло до выхода и сколько обещали файлы.
    let mut heard_frames = 0u64;
    let mut promised = Duration::ZERO;

    let (lines_tx, lines) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });

    let started = Instant::now();
    let mut stdin_open = true;
    let mut repeat = Repeat::Off;
    let mut stop_after = false;
    let mut failed = false;
    loop {
        if let Some(capture) = &capture {
            heard_frames += (capture.take().len() / NULL_CHANNELS) as u64;
        }
        while let Ok(event) = engine.events().try_recv() {
            match event {
                Event::Finished => {
                    println!("очередь доиграна");
                    if let Some(capture) = &capture {
                        heard_frames += (capture.take().len() / NULL_CHANNELS) as u64;
                        let heard = heard_frames as f64 / f64::from(NULL_RATE);
                        println!(
                            "звука на выходе: {heard:.3} с, по заголовкам файлов: {:.3} с, за {:.1} с",
                            promised.as_secs_f64(),
                            started.elapsed().as_secs_f64()
                        );
                    }
                    return report(&engine, failed);
                }
                Event::TrackStarted(ref now) => {
                    promised += now.info.duration.unwrap_or_default();
                    print_event(&event);
                }
                Event::OutputFailed(message) => {
                    eprintln!("вывод звука не открылся: {message}");
                    return ExitCode::FAILURE;
                }
                Event::TrackFailed { .. } => {
                    failed = true;
                    print_event(&event);
                }
                other => print_event(&other),
            }
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            print_status(&engine);
            return report(&engine, failed);
        }

        if !stdin_open {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        let line = match lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            // Ввод закрыт (запуск без терминала): просто доигрываем очередь.
            Err(RecvTimeoutError::Disconnected) => {
                stdin_open = false;
                continue;
            }
        };
        let mut words = line.split_whitespace();
        match (words.next(), words.next()) {
            (Some("p"), _) => engine.toggle(),
            (Some("n"), _) => engine.next(),
            (Some("b"), _) => engine.previous(),
            (Some("s"), Some(value)) => match value.parse::<f64>() {
                Ok(seconds) if seconds >= 0.0 => engine.seek(Duration::from_secs_f64(seconds)),
                _ => println!("s СЕК — число секунд от начала трека"),
            },
            (Some("v"), Some(value)) => match value.parse::<f32>() {
                Ok(percent) => engine.set_volume(percent / 100.0),
                Err(_) => println!("v 0-100"),
            },
            (Some("r"), _) => {
                repeat = match repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                };
                engine.set_repeat(repeat);
                println!("повтор: {repeat:?}");
            }
            (Some("a"), _) => {
                stop_after = !stop_after;
                engine.set_stop_after_current(stop_after);
                println!("стоп после текущего: {}", if stop_after { "да" } else { "нет" });
            }
            (Some("i"), _) => print_status(&engine),
            (Some("q"), _) => return report(&engine, failed),
            (None, _) => {}
            _ => println!("{USAGE}"),
        }
        let _ = io::stdout().flush();
    }
}

fn print_event(event: &Event) {
    match event {
        Event::TrackStarted(now) => {
            let info = &now.info;
            let mut details = vec![info.codec.clone()];
            if let Some(rate) = info.sample_rate {
                details.push(format!("{rate} Гц"));
            }
            if let Some(bits) = info.bits_per_sample {
                details.push(format!("{bits} бит"));
            }
            if let Some(channels) = info.channels {
                details.push(format!("{channels} кан."));
            }
            if let Some(duration) = info.duration {
                details.push(clock(duration));
            }
            println!("▶ {}  [{}]", file_name(&now.path), details.join(", "));
        }
        Event::TrackFailed { path, error, .. } => {
            println!("✗ {}: {error}", file_name(path));
        }
        Event::TrackDamaged {
            path,
            decode_errors,
            missing,
            ..
        } => {
            println!("! {}: {}", file_name(path), damage(*decode_errors, *missing));
        }
        Event::StateChanged(state) => println!(
            "{}",
            match state {
                State::Playing => "играет",
                State::Paused => "пауза",
                State::Stopped => "остановлен",
            }
        ),
        Event::OutputOpened {
            device,
            rate,
            channels,
            exclusive_bits,
        } => match exclusive_bits {
            Some(bits) => println!("вывод: {device}, {rate} Гц, {channels} кан., эксклюзивно, {bits} бит"),
            None => println!("вывод: {device}, {rate} Гц, {channels} кан."),
        },
        Event::OutputLost => println!("устройство вывода пропало, открываю заново"),
        Event::PausedForDevice => println!("устройство вывода сменилось — пауза"),
        Event::OutputFailed(message) => println!("вывод звука не открылся: {message}"),
        Event::ExclusiveUnavailable(message) => println!("эксклюзивный режим недоступен, играю в общем: {message}"),
        Event::QueueChanged | Event::Finished => {}
    }
}

fn print_status(engine: &Engine) {
    match (engine.now_playing(), engine.position()) {
        (Some(now), Some(position)) => {
            let total = now.info.duration.map(clock).unwrap_or_else(|| "?".into());
            println!("{}  {} / {total}", file_name(&now.path), clock(position));
        }
        _ => println!("ничего не играет"),
    }
}

fn report(engine: &Engine, failed: bool) -> ExitCode {
    let underruns = engine.underruns();
    println!("заиканий: {underruns}");
    if failed || underruns > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ───────────────────────────── check ─────────────────────────────

/// Описание повреждений трека одной строкой.
fn damage(decode_errors: u64, missing: Option<Duration>) -> String {
    let mut parts = Vec::new();
    if decode_errors > 0 {
        parts.push(format!("повреждённых мест: {decode_errors}, заменены тишиной"));
    }
    if let Some(missing) = missing {
        parts.push(format!("файл обрезан, не хватает {:.1} с", missing.as_secs_f64()));
    }
    parts.join("; ")
}

enum Verdict {
    Ok(String),
    Damaged(String),
    Failed(TrackError),
}

struct Checked {
    verdict: Verdict,
    audio: Duration,
}

fn check(mut args: Vec<String>) -> ExitCode {
    let verbose = args.first().map(String::as_str) == Some("-v");
    if verbose {
        args.remove(0);
    }
    let files = collect_files(&args);
    if files.is_empty() {
        eprintln!("нечего проверять");
        return ExitCode::from(2);
    }

    let started = Instant::now();
    let (mut ok, mut damaged, mut failed, mut panicked) = (0u32, 0u32, 0u32, 0u32);
    let mut audio = Duration::ZERO;
    for file in &files {
        let checked = check_file(file);
        audio += checked.audio;
        match checked.verdict {
            Verdict::Ok(details) => {
                ok += 1;
                if verbose {
                    println!("ЦЕЛ        {}: {details}", file_name(file));
                }
            }
            Verdict::Damaged(what) => {
                damaged += 1;
                println!("ПОВРЕЖДЁН  {}: {what}", file.display());
            }
            Verdict::Failed(error) => {
                if matches!(error, TrackError::Panicked(_)) {
                    panicked += 1;
                } else {
                    failed += 1;
                }
                println!("НЕ ИГРАЕТ  {}: {error}", file.display());
            }
        }
    }

    let elapsed = started.elapsed();
    println!(
        "\nфайлов: {}, целых: {ok}, повреждённых: {damaged}, не играет: {failed}, сбоев декодера: {panicked}",
        files.len()
    );
    println!(
        "звука: {}, время проверки: {:.1} с ({:.0}× быстрее реального времени)",
        clock(audio),
        elapsed.as_secs_f64(),
        audio.as_secs_f64() / elapsed.as_secs_f64().max(1e-9)
    );
    // Повреждённые и неподдерживаемые файлы — дело обычное. Сбой декодера —
    // ошибка в коде, и только он считается провалом проверки.
    if panicked > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Декодирует файл целиком и сверяет контрольную сумму потока, если она есть.
fn check_file(path: &Path) -> Checked {
    let mut audio = Duration::ZERO;
    let result = guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions { verify: true })?;
        let mut frames = 0u64;
        let mut rate = 0u32;
        while let Some(chunk) = decoder.next_chunk()? {
            let channels = chunk.channels.count().max(1);
            frames += (chunk.samples.len() / channels) as u64;
            rate = chunk.rate;
        }
        if rate > 0 {
            audio = Duration::from_secs_f64(frames as f64 / f64::from(rate));
        }
        let (errors, missing) = (decoder.decode_errors(), decoder.missing());
        let checksum = decoder.finalize();
        Ok(if frames == 0 {
            Verdict::Damaged("нет звука".into())
        } else if errors > 0 || missing.is_some() {
            Verdict::Damaged(damage(errors, missing))
        } else if checksum == Some(false) {
            Verdict::Damaged("контрольная сумма потока не сошлась".into())
        } else {
            let info = decoder.info();
            Verdict::Ok(format!(
                "{}, {} Гц, {} бит, {}, {}",
                info.codec,
                rate,
                info.bits_per_sample.map_or("?".into(), |bits| bits.to_string()),
                clock(audio),
                match checksum {
                    Some(_) => "контрольная сумма сошлась",
                    None => "контрольной суммы нет",
                }
            ))
        })
    });
    Checked {
        verdict: result.unwrap_or_else(Verdict::Failed),
        audio,
    }
}

// ───────────────────────────── inspect ─────────────────────────────

/// Проверяет подлинность качества файлов без потерь в несколько потоков.
fn inspect(mut args: Vec<String>) -> ExitCode {
    let mut sensitivity = nyanoto_core::Sensitivity::Low;
    if let Some(at) = args.iter().position(|arg| arg == "--sensitivity") {
        sensitivity = match args.get(at + 1).map(String::as_str) {
            Some("low") => nyanoto_core::Sensitivity::Low,
            Some("medium") => nyanoto_core::Sensitivity::Medium,
            Some("high") => nyanoto_core::Sensitivity::High,
            _ => {
                eprintln!("после --sensitivity нужно low, medium или high");
                return ExitCode::from(2);
            }
        };
        args.drain(at..at + 2);
    }
    let files: Vec<PathBuf> = collect_files(&args)
        .into_iter()
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    ["flac", "wav", "aiff", "aif", "ape", "wv"]
                        .iter()
                        .any(|lossless| lossless.eq_ignore_ascii_case(extension))
                })
        })
        .collect();
    if files.is_empty() {
        eprintln!("нечего проверять: нет файлов без потерь");
        return ExitCode::from(2);
    }
    let started = Instant::now();
    let workers = thread::available_parallelism().map_or(4, |count| count.get()).clamp(1, 8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(file) = files.get(index) else {
                    return;
                };
                let result = nyanoto_core::inspect(file);
                results.lock().unwrap_or_else(|error| error.into_inner()).push((index, result));
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|error| error.into_inner());
    results.sort_by_key(|(index, _)| *index);

    use nyanoto_core::{Sensitivity, Verdict};
    // Подробности печатаются для выбранной чувствительности, итоги — для всех трёх.
    let mut counts = std::collections::BTreeMap::new();
    for (index, result) in &results {
        let file = &files[*index];
        match result {
            Ok(inspection) => {
                for level in Sensitivity::ALL {
                    let verdict = inspection.verdict(level);
                    *counts.entry((format!("{level:?}"), format!("{verdict:?}"))).or_insert(0u32) += 1;
                }
                let verdict = inspection.verdict(sensitivity);
                if !matches!(verdict, Verdict::Genuine | Verdict::Unknown) {
                    println!(
                        "{:<12} срез {} Гц, стена {:.0} дБ, тише музыки на {:.0} дБ  {}",
                        format!("{verdict:?}"),
                        inspection.cutoff_hz.map_or("?".into(), |hz| hz.to_string()),
                        inspection.wall_db,
                        inspection.silence_db,
                        file.display()
                    );
                }
            }
            Err(error) => {
                println!("ОШИБКА       {}: {error}", file.display());
                *counts.entry(("—".into(), "Error".into())).or_insert(0u32) += 1;
            }
        }
    }
    println!("\nфайлов: {}, за {:.1} с", files.len(), started.elapsed().as_secs_f64());
    for ((level, verdict), count) in counts {
        println!("  {level:<6} {verdict}: {count}");
    }
    ExitCode::SUCCESS
}

fn convert(mut args: Vec<String>) -> ExitCode {
    let mut options = nyanoto_core::ConvertOptions { format: nyanoto_core::TargetFormat::Flac, rate: None, bits: None };
    loop {
        match args.first().map(String::as_str) {
            Some("--wav") => options.format = nyanoto_core::TargetFormat::Wav,
            Some("--cd") => {
                options.rate = Some(44_100);
                options.bits = Some(16);
            }
            _ => break,
        }
        args.remove(0);
    }
    let [source, target] = args.as_slice() else {
        eprintln!("нужно: nyanoto-cli convert [--wav] [--cd] ИЗ В");
        return ExitCode::from(2);
    };
    if Path::new(target).exists() {
        eprintln!("{target} уже есть — не перезаписываю");
        return ExitCode::from(2);
    }
    let started = Instant::now();
    match nyanoto_core::convert_file(Path::new(source), Path::new(target), &options, &std::sync::atomic::AtomicBool::new(false)) {
        Ok(_) => {
            println!("готово за {:.2} с", started.elapsed().as_secs_f64());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("не вышло: {error}");
            ExitCode::FAILURE
        }
    }
}
