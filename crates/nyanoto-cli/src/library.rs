//! Команды для работы с библиотекой: сканирование, поиск, сводка и
//! генератор искусственной коллекции для стресс-прогонов.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nyanoto_library::{Library, ScanProgress, Track, default_database};
use nyanoto_testkit::{encode_flac_tagged, signal};

/// Достаёт `--db ФАЙЛ` из аргументов и открывает базу.
fn open(args: &mut Vec<String>) -> Option<Library> {
    let path = match args.iter().position(|arg| arg == "--db") {
        Some(at) if at + 1 < args.len() => {
            let path = PathBuf::from(args.remove(at + 1));
            args.remove(at);
            path
        }
        Some(_) => {
            eprintln!("после --db нужен путь к файлу базы");
            return None;
        }
        None => match default_database() {
            Some(path) => path,
            None => {
                eprintln!("не удалось определить папку данных, укажите --db ФАЙЛ");
                return None;
            }
        },
    };
    match Library::open(&path) {
        Ok(library) => Some(library),
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            None
        }
    }
}

fn describe(track: &Track) -> String {
    let name = track.path.file_name().map(|name| name.to_string_lossy().into_owned());
    let title = track.title.clone().or(name).unwrap_or_default();
    let artist = track.artist.as_deref().unwrap_or("—");
    let album = track.album.as_deref().unwrap_or("—");
    format!("{artist} — {title}  [{album}]")
}

pub fn scan(mut args: Vec<String>) -> ExitCode {
    let Some(mut library) = open(&mut args) else {
        return ExitCode::from(2);
    };
    for folder in &args {
        if let Err(error) = library.add_root(Path::new(folder)) {
            eprintln!("{folder}: {error}");
            return ExitCode::FAILURE;
        }
    }

    let progress = ScanProgress::default();
    let result = std::thread::scope(|scope| {
        let scanning = scope.spawn(|| library.scan(&progress));
        // Пока идёт сканирование, показываем счётчики.
        while !scanning.is_finished() {
            eprint!(
                "\rнайдено {}, прочитано {}, без изменений {}   ",
                progress.found.load(Ordering::Relaxed),
                progress.read.load(Ordering::Relaxed),
                progress.unchanged.load(Ordering::Relaxed),
            );
            let _ = std::io::stderr().flush();
            std::thread::sleep(Duration::from_millis(250));
        }
        eprintln!();
        scanning.join()
    });
    let report = match result {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => {
            eprintln!("сканирование не удалось: {error}");
            return ExitCode::FAILURE;
        }
        Err(_) => {
            eprintln!("сканер аварийно завершился");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "найдено: {}, новых: {}, обновлено: {}, без изменений: {}",
        report.found, report.added, report.updated, report.unchanged
    );
    println!(
        "с проблемами: {}, пропало: {}, вернулось: {}",
        report.problems, report.missing, report.revived
    );
    let seconds = report.elapsed.as_secs_f64();
    println!(
        "время: {seconds:.2} с ({:.0} файлов в секунду)",
        report.found as f64 / seconds.max(1e-9)
    );
    for root in &report.offline_roots {
        println!("недоступна, треки сохранены: {}", root.display());
    }
    if report.skipped_names > 0 {
        println!("пропущено из-за имени файла: {}", report.skipped_names);
    }
    if report.incomplete {
        println!("обход неполный: пропавшие файлы не отмечались");
    }
    ExitCode::SUCCESS
}

pub fn find(mut args: Vec<String>) -> ExitCode {
    let Some(library) = open(&mut args) else {
        return ExitCode::from(2);
    };
    let query = args.join(" ");
    let started = Instant::now();
    let found = match library.search(&query, 20) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("поиск не удался: {error}");
            return ExitCode::FAILURE;
        }
    };
    let took = started.elapsed();
    for track in &found {
        println!("{}", describe(track));
    }
    println!("показано: {}, время поиска: {:.2} мс", found.len(), took.as_secs_f64() * 1000.0);
    ExitCode::SUCCESS
}

pub fn info(mut args: Vec<String>) -> ExitCode {
    let Some(library) = open(&mut args) else {
        return ExitCode::from(2);
    };
    let report = || -> Result<(), nyanoto_library::LibraryError> {
        let stats = library.stats()?;
        let hours = stats.duration_ms as f64 / 3_600_000.0;
        println!("треков: {}, звука: {hours:.1} ч", stats.tracks);
        println!("пропавших файлов: {}, с проблемами: {}", stats.missing, stats.problems);
        for root in library.roots()? {
            let state = if root.online { "доступна" } else { "недоступна" };
            println!("папка ({state}): {}", root.path.display());
        }
        for track in library.problems(20)? {
            let problem = track.problem.as_deref().unwrap_or_default();
            println!("проблема: {}: {problem}", track.path.display());
        }
        Ok(())
    };
    match report() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Создаёт искусственную коллекцию: крошечные настоящие FLAC с тегами,
/// разложенные по папкам исполнителей и альбомов.
pub fn generate(mut args: Vec<String>) -> ExitCode {
    let mut cover_bytes = 0usize;
    if let Some(at) = args.iter().position(|arg| arg == "--cover-kb") {
        match args.get(at + 1).and_then(|value| value.parse::<usize>().ok()) {
            Some(kilobytes) => cover_bytes = kilobytes * 1024,
            None => {
                eprintln!("после --cover-kb нужен размер обложки в килобайтах");
                return ExitCode::from(2);
            }
        }
        args.drain(at..at + 2);
    }
    let (Some(folder), Some(count)) =
        (args.first(), args.get(1).and_then(|value| value.parse::<usize>().ok()))
    else {
        eprintln!("нужны папка и число файлов");
        return ExitCode::from(2);
    };

    const TRACKS_PER_ALBUM: usize = 10;
    const ALBUMS_PER_ARTIST: usize = 5;
    const GENRES: [&str; 6] = ["Рок", "Электроника", "Джаз", "Классика", "Хип-хоп", "Эмбиент"];
    let samples = signal(64, 44_100);
    let started = Instant::now();
    for index in 0..count {
        let album = index / TRACKS_PER_ALBUM;
        let artist = album / ALBUMS_PER_ARTIST;
        let number = index % TRACKS_PER_ALBUM + 1;
        let (artist_name, album_name) = (format!("Исполнитель {artist:05}"), format!("Альбом {album:06}"));
        let title = format!("Трек {index:07}");
        let (number_text, year) = (number.to_string(), (1970 + album % 55).to_string());
        let tags = [
            ("TITLE", title.as_str()),
            ("ARTIST", artist_name.as_str()),
            ("ALBUMARTIST", artist_name.as_str()),
            ("ALBUM", album_name.as_str()),
            ("TRACKNUMBER", number_text.as_str()),
            ("DATE", year.as_str()),
            ("GENRE", GENRES[artist % GENRES.len()]),
        ];
        let dir = Path::new(folder).join(&artist_name).join(&album_name);
        let file = dir.join(format!("{number:02} - {title}.flac"));
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&file, encode_flac_tagged(&samples, 44_100, &tags, cover_bytes).bytes));
        if let Err(error) = written {
            eprintln!("{}: {error}", file.display());
            return ExitCode::FAILURE;
        }
    }
    println!("создано файлов: {count}, за {:.1} с", started.elapsed().as_secs_f64());
    ExitCode::SUCCESS
}

/// Читает или задаёт настройку плеера: `setting КЛЮЧ` или `setting КЛЮЧ ЗНАЧЕНИЕ`.
pub fn setting(mut args: Vec<String>) -> ExitCode {
    let Some(library) = open(&mut args) else {
        return ExitCode::from(2);
    };
    let result = match args.as_slice() {
        [key] => library.setting(key).map(|value| println!("{}", value.unwrap_or_default())),
        [key, value] => library.set_setting(key, value),
        _ => {
            eprintln!("нужен ключ и, если задаёте, значение");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
