//! Сканер на настоящих файлах во временной папке.

use std::fs;

use nyanoto_library::{Grouping, Library, LibraryError, ScanProgress, ScanReport};
use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};

fn flac(tags: &[(&str, &str)]) -> Vec<u8> {
    encode_flac_tagged(&signal(4096, 44_100), 44_100, tags, 0).bytes
}

/// Три файла: с полными тегами, с двумя исполнителями и вовсе без тегов.
fn music(scratch: &Scratch) {
    scratch.write(
        "music/Кино/Группа крови/01.flac",
        &flac(&[
            ("TITLE", "Группа крови"),
            ("ARTIST", "Кино"),
            ("ALBUMARTIST", "Кино"),
            ("ALBUM", "Группа крови"),
            ("TRACKNUMBER", "1"),
            ("DATE", "1988"),
            ("GENRE", "Rock"),
            ("GENRE", "Post-punk"),
        ]),
    );
    scratch.write(
        "music/duet.flac",
        &flac(&[("TITLE", "Дуэт"), ("ARTIST", "Первый"), ("ARTIST", "Второй")]),
    );
    scratch.write("music/untagged.flac", &flac(&[]));
}

fn library(scratch: &Scratch) -> Library {
    let library = Library::open(&scratch.path("data/library.db")).unwrap();
    library.add_root(&scratch.path("music")).unwrap();
    library
}

fn scan(library: &mut Library) -> (ScanReport, u64) {
    let progress = ScanProgress::default();
    let report = library.scan(&progress).unwrap();
    (report, progress.read.load(std::sync::atomic::Ordering::Relaxed))
}

#[test]
fn scan_reads_tags_and_search_finds_them() {
    let scratch = Scratch::new("lib-tags");
    music(&scratch);
    let mut library = library(&scratch);
    let (report, _) = scan(&mut library);

    assert_eq!((report.found, report.added, report.problems), (3, 3, 0), "{report:?}");
    assert!(!report.incomplete);
    assert_eq!(library.stats().unwrap().tracks, 3);

    // Начало слова, другой регистр, кириллица.
    let found = library.search("груп", 10).unwrap();
    assert_eq!(found.len(), 1);
    let track = &found[0];
    assert_eq!(track.title.as_deref(), Some("Группа крови"));
    assert_eq!(track.artist.as_deref(), Some("Кино"));
    assert_eq!(track.year, Some(1988));
    assert_eq!(track.track_no, Some(1));
    assert_eq!(track.format.as_deref(), Some("flac"));
    assert_eq!(track.duration_ms, Some(92));
    assert!(track.path.ends_with("Кино/Группа крови/01.flac"));

    // Два исполнителя остаются двумя значениями, а не одной строкой.
    let duet = &library.search("ВТОРОЙ", 10).unwrap()[0];
    assert_eq!(duet.artist.as_deref(), Some("Первый; Второй"));
    assert_eq!(library.artists_of(duet.id).unwrap(), ["Первый", "Второй"]);

    // Файл без тегов находится по имени, поиск по жанру и по двум словам работает.
    assert_eq!(library.search("untagged", 10).unwrap().len(), 1);
    assert_eq!(library.search("post-punk", 10).unwrap().len(), 1);
    assert_eq!(library.search("кино кров", 10).unwrap().len(), 1);
    assert!(library.search("нетакого", 10).unwrap().is_empty());
    // Спецсимволы запроса не ломают поиск.
    assert!(library.search("\" OR * (", 10).is_ok());
}

#[test]
fn rescan_reads_only_what_changed() {
    let scratch = Scratch::new("lib-rescan");
    music(&scratch);
    let mut library = library(&scratch);
    scan(&mut library);

    let (report, read) = scan(&mut library);
    assert_eq!((report.added, report.updated, report.unchanged), (0, 0, 3), "{report:?}");
    assert_eq!(read, 0, "неизменившиеся файлы не должны перечитываться");

    scratch.write("music/duet.flac", &flac(&[("TITLE", "Новое название"), ("ARTIST", "Соло")]));
    let (report, read) = scan(&mut library);
    assert_eq!((report.added, report.updated, report.unchanged), (0, 1, 2), "{report:?}");
    assert_eq!(read, 1);

    let track = &library.search("новое", 10).unwrap()[0];
    assert_eq!(library.artists_of(track.id).unwrap(), ["Соло"]);
    assert!(library.search("дуэт", 10).unwrap().is_empty());
    assert!(library.search("второй", 10).unwrap().is_empty());
    assert_eq!(library.stats().unwrap().tracks, 3);
}

#[test]
fn missing_files_are_kept_and_come_back() {
    let scratch = Scratch::new("lib-missing");
    music(&scratch);
    let mut library = library(&scratch);
    scan(&mut library);
    let id = library.search("дуэт", 10).unwrap()[0].id;

    let (here, away) = (scratch.path("music/duet.flac"), scratch.path("duet.away"));
    fs::rename(&here, &away).unwrap();
    let (report, _) = scan(&mut library);
    assert_eq!(report.missing, 1, "{report:?}");
    let stats = library.stats().unwrap();
    assert_eq!((stats.tracks, stats.missing), (2, 1));
    assert!(library.search("дуэт", 10).unwrap().is_empty());

    // Файл вернулся нетронутым: запись та же, теги не перечитываются.
    fs::rename(&away, &here).unwrap();
    let (report, read) = scan(&mut library);
    assert_eq!((report.revived, report.added, read), (1, 0, 0), "{report:?}");
    assert_eq!(library.search("дуэт", 10).unwrap()[0].id, id);
    assert_eq!(library.stats().unwrap().missing, 0);
}

#[test]
fn offline_root_keeps_its_tracks() {
    let scratch = Scratch::new("lib-offline");
    music(&scratch);
    let mut library = library(&scratch);
    scan(&mut library);

    // Диск отключили: папки нет вовсе.
    let (here, away) = (scratch.path("music"), scratch.path("music.away"));
    fs::rename(&here, &away).unwrap();
    let (report, _) = scan(&mut library);
    assert_eq!(report.offline_roots, std::slice::from_ref(&here));
    assert_eq!(report.missing, 0);
    assert!(!library.roots().unwrap()[0].online);
    let stats = library.stats().unwrap();
    assert_eq!((stats.tracks, stats.missing), (3, 0));
    assert_eq!(library.search("кино", 10).unwrap().len(), 1);

    // Точка монтирования без диска: папка есть, но пуста.
    fs::create_dir(&here).unwrap();
    let (report, _) = scan(&mut library);
    assert_eq!(report.offline_roots.len(), 1, "{report:?}");
    assert_eq!(library.stats().unwrap().tracks, 3);

    // Диск вернулся: всё на месте, ничего не перечитано.
    fs::remove_dir(&here).unwrap();
    fs::rename(&away, &here).unwrap();
    let (report, read) = scan(&mut library);
    assert_eq!((report.unchanged, report.missing, read), (3, 0, 0), "{report:?}");
    assert!(library.roots().unwrap()[0].online);
}

#[test]
fn broken_files_are_recorded_not_fatal() {
    let scratch = Scratch::new("lib-broken");
    music(&scratch);
    let noise: Vec<u8> = (0..40_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
    scratch.write("music/garbage.flac", &noise);
    scratch.write("music/empty.mp3", b"");
    // FLAC с чужим расширением разбирается по содержимому.
    scratch.write("music/disguised.mp3", &flac(&[("TITLE", "Замаскированный")]));
    // Обложка на три мегабайта не мешает прочитать теги.
    let big = encode_flac_tagged(&signal(4096, 44_100), 44_100, &[("TITLE", "С обложкой")], 3 << 20);
    scratch.write("music/cover.flac", &big.bytes);
    // Не звуковой файл сканер не трогает.
    scratch.write("music/notes.txt", b"hello");

    let mut library = library(&scratch);
    let (report, _) = scan(&mut library);
    assert_eq!((report.found, report.added, report.problems), (7, 7, 2), "{report:?}");

    let problems = library.problems(10).unwrap();
    let names: Vec<_> = problems
        .iter()
        .map(|track| track.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["empty.mp3", "garbage.flac"]);
    assert!(problems.iter().all(|track| track.problem.is_some()));

    let disguised = &library.search("замаскированный", 10).unwrap()[0];
    assert_eq!(disguised.format.as_deref(), Some("flac"));
    assert_eq!(library.search("обложкой", 10).unwrap().len(), 1);
    assert_eq!(library.stats().unwrap().problems, 2);

    // Непрочитанные — снова каждый раз, остальные пропускаются.
    let (report, _) = scan(&mut library);
    assert_eq!((report.unchanged, report.updated, report.problems), (5, 2, 2), "{report:?}");
}

#[test]
fn cancelled_scan_does_not_mark_files_missing() {
    let scratch = Scratch::new("lib-cancel");
    music(&scratch);
    let mut library = library(&scratch);
    scan(&mut library);

    let progress = ScanProgress::default();
    progress.cancel();
    let report = library.scan(&progress).unwrap();
    assert!(report.incomplete);
    assert_eq!(report.missing, 0);
    let stats = library.stats().unwrap();
    assert_eq!((stats.tracks, stats.missing), (3, 0));
}

#[test]
fn browse_by_tags_goes_artist_album_track() {
    let scratch = Scratch::new("lib-browse");
    let track = |title: &str, owner: &str, album: &str, year: &str, disc: &str, number: &str| {
        flac(&[
            ("TITLE", title),
            ("ARTIST", owner),
            ("ALBUMARTIST", owner),
            ("ALBUM", album),
            ("DATE", year),
            ("DISCNUMBER", disc),
            ("TRACKNUMBER", number),
        ])
    };
    // Разный регистр в тегах одного исполнителя, два альбома, двухдисковый альбом.
    scratch.write("music/a/1.flac", &track("Звезда", "КИНО", "Звезда по имени Солнце", "1989", "1", "2"));
    scratch.write("music/a/2.flac", &track("Пачка", "Кино", "Звезда по имени Солнце", "1989", "1", "1"));
    scratch.write("music/b/1.flac", &track("Кукушка", "Кино", "Чёрный альбом", "1990", "2", "1"));
    scratch.write("music/b/2.flac", &track("Красно-жёлтые дни", "Кино", "Чёрный альбом", "1990", "1", "3"));
    scratch.write("music/c/1.flac", &track("Creep", "Radiohead", "Pablo Honey", "1993", "1", "2"));
    // Совместный альбом стоит под первым исполнителем, а не под «Кино; Гость».
    scratch.write(
        "music/d/1.flac",
        &flac(&[
            ("TITLE", "Вместе"),
            ("ARTIST", "Кино"),
            ("ARTIST", "Гость"),
            ("ALBUMARTIST", "Кино"),
            ("ALBUMARTIST", "Гость"),
            ("ALBUM", "Совместный"),
            ("DATE", "1991"),
        ]),
    );
    scratch.write("music/untagged.flac", &flac(&[]));

    let mut library = library(&scratch);
    scan(&mut library);

    let artists = library.artists(Grouping::Tags).unwrap();
    let names: Vec<&str> = artists.iter().map(|artist| artist.name.as_str()).collect();
    assert_eq!(names, ["Radiohead", "КИНО", ""]);
    let kino = &artists[1];
    assert_eq!((kino.albums, kino.tracks, kino.names.len()), (3, 5, 2));

    let albums = library.albums(kino).unwrap();
    let titles: Vec<_> = albums.iter().map(|album| album.title.as_deref().unwrap()).collect();
    assert_eq!(titles, ["Звезда по имени Солнце", "Чёрный альбом", "Совместный"]);
    assert_eq!((albums[0].year, albums[0].tracks), (Some(1989), 2));
    assert!(albums[0].folder.ends_with("music/a"));

    // Внутри альбома порядок по диску и номеру, а не по имени файла.
    let tracks = library.album_tracks(&albums[1]).unwrap();
    let order: Vec<_> = tracks.iter().map(|track| track.title.as_deref().unwrap()).collect();
    assert_eq!(order, ["Красно-жёлтые дни", "Кукушка"]);
    assert_eq!(tracks[1].disc_no, Some(2));

    // От найденного трека можно вернуться к его альбому.
    let found = &library.search("кукушка", 1).unwrap()[0];
    let album = library.album_of(found, Grouping::Tags).unwrap().unwrap();
    assert_eq!(album.title.as_deref(), Some("Чёрный альбом"));

    // Треки без тегов собраны под пустым исполнителем и без альбома.
    let unknown = library.albums(&artists[2]).unwrap();
    assert_eq!((unknown.len(), unknown[0].title.is_none()), (1, true));
    assert_eq!(library.album_tracks(&unknown[0]).unwrap().len(), 1);
}

#[test]
fn browse_by_folders_ignores_tags() {
    let scratch = Scratch::new("lib-folders");
    // Теги нарочно врут: структуру задают папки.
    let file = flac(&[("TITLE", "Чужое"), ("ARTIST", "Чужой"), ("ALBUM", "Чужой альбом")]);
    for name in [
        "music/Кино/Группа крови (1988) [UPC1]/02. Кино - Закрой за мной дверь.flac",
        "music/Кино/Группа крови (1988) [UPC1]/10. Кино - Спокойная ночь.flac",
        "music/Кино/Группа крови (1988) [UPC1]/1. Кино - Группа крови.flac",
        "music/КИНО/Звезда/CD 2/01. a.flac",
        "music/КИНО/Звезда/CD 1/01. b.flac",
        "music/Кино/без альбома.flac",
        "music/в корне.flac",
    ] {
        scratch.write(name, &file);
    }
    let mut library = library(&scratch);
    scan(&mut library);

    let artists = library.artists(Grouping::Folders).unwrap();
    assert_eq!(artists.len(), 2);
    let kino = &artists[0];
    assert_eq!(kino.name.to_lowercase(), "кино");
    // На Windows «КИНО» и «Кино» — одна и та же папка, на Linux — две.
    assert_eq!((kino.albums, kino.tracks), (3, 6));
    assert_eq!(artists[1].name, "");

    let albums = library.albums(kino).unwrap();
    let titles: Vec<_> = albums.iter().map(|album| album.title.as_deref()).collect();
    assert_eq!(titles, [Some("Группа крови (1988) [UPC1]"), Some("Звезда"), None]);
    // Папка альбома — сама папка «Звезда», а не CD 1 внутри неё.
    let folder = &albums[1].folder;
    assert!(folder.ends_with("Звезда"), "{folder:?}");
    assert_eq!(folder.parent().unwrap().file_name().unwrap().to_string_lossy().to_lowercase(), "кино");

    let names = |album| -> Vec<String> {
        library
            .album_tracks(album)
            .unwrap()
            .iter()
            .map(|track| track.path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        names(&albums[0]),
        ["1. Кино - Группа крови", "02. Кино - Закрой за мной дверь", "10. Кино - Спокойная ночь"]
    );
    assert_eq!(names(&albums[1]), ["01. b", "01. a"]);
    assert_eq!(names(&albums[2]), ["без альбома"]);

    let in_cd2 = library
        .album_tracks(&albums[1])
        .unwrap()
        .into_iter()
        .find(|track| track.path.ends_with("CD 2/01. a.flac"))
        .unwrap();
    let album = library.album_of(&in_cd2, Grouping::Folders).unwrap().unwrap();
    assert_eq!(album.title.as_deref(), Some("Звезда"));
}

/// Ссылки по кругу и внутрь не размножают треки, наружу — один раз.
#[cfg(unix)]
#[test]
fn folder_links_do_not_loop_or_duplicate() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new("lib-links");
    music(&scratch);
    scratch.write("outside/Сторонний/01.flac", &flac(&[("TITLE", "Снаружи")]));
    symlink(scratch.path("music"), scratch.path("music/Кино/loop")).unwrap();
    symlink(scratch.path("music/Кино"), scratch.path("music/again")).unwrap();
    symlink(scratch.path("outside"), scratch.path("music/linked")).unwrap();
    symlink(scratch.path("outside"), scratch.path("music/linked-twice")).unwrap();
    let mut library = library(&scratch);
    let (report, _) = scan(&mut library);

    assert_eq!((report.found, report.added), (4, 4), "{report:?}");
    assert!(!report.incomplete);
}

/// Скрытые папки и файлы (корзина флешки, служебные копии macOS) не музыка.
#[test]
fn hidden_entries_are_skipped() {
    let scratch = Scratch::new("lib-hidden");
    music(&scratch);
    scratch.write("music/.Trash-1000/files/old.flac", &flac(&[]));
    scratch.write("music/Кино/Группа крови/._01.flac", b"\0\x05\x16\x07 resource fork");
    let mut library = library(&scratch);
    let (report, _) = scan(&mut library);

    assert_eq!((report.found, report.added, report.problems), (3, 3, 0), "{report:?}");
}

#[test]
fn several_folders_never_duplicate_music() {
    let scratch = Scratch::new("lib-roots");
    scratch.write("music/A/Альбом/01.flac", &flac(&[("TITLE", "Первая")]));
    scratch.write("music/B/Альбом/02.flac", &flac(&[("TITLE", "Вторая")]));
    // Корзина Windows на флешке: туда не заглядываем.
    scratch.write("music/B/$RECYCLE.BIN/S-1-5/03.flac", &flac(&[("TITLE", "Удалённая")]));
    let mut library = Library::open(&scratch.path("data/library.db")).unwrap();
    library.add_root(&scratch.path("music/A")).unwrap();
    library.add_root(&scratch.path("music/B")).unwrap();
    // Та же папка ещё раз — та же запись, а не вторая.
    let again = library.add_root(&scratch.path("music/B/")).unwrap();
    assert_eq!(library.roots().unwrap().len(), 2);
    assert_eq!(library.roots().unwrap()[1].id, again);
    scan(&mut library);
    assert_eq!(library.stats().unwrap().tracks, 2, "корзина не сканируется");
    let first = library.search("Первая", 10).unwrap()[0].clone();
    library.set_liked(first.id, true).unwrap();

    // Папка внутри добавленной не добавляется: её музыка уже в библиотеке.
    match library.add_root(&scratch.path("music/A/Альбом")) {
        Err(LibraryError::InsideRoot(outer)) => assert_eq!(outer, scratch.path("music/A")),
        other => panic!("{other:?}"),
    }

    // Общая папка забирает обе: записи те же, лайк на месте.
    library.add_root(&scratch.path("music")).unwrap();
    let roots = library.roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, scratch.path("music"));
    let moved = library.search("Первая", 10).unwrap()[0].clone();
    assert_eq!(moved.id, first.id);
    assert_eq!(moved.path, first.path);
    assert_eq!(moved.folder_artist.as_deref(), Some("A"));
    assert_eq!(moved.folder_album.as_deref(), Some("Альбом"));
    assert!(library.liked_ids().unwrap().contains(&first.id));
    // Пересканирование не задваивает и перечитывает перенесённые.
    let (report, read) = scan(&mut library);
    assert_eq!((report.added, read), (0, 2), "{report:?}");
    assert_eq!(library.search("B", 10).unwrap().len(), 1);
    let (report, read) = scan(&mut library);
    assert_eq!((report.added, read), (0, 0), "{report:?}");
    assert_eq!(library.stats().unwrap().tracks, 2);

    // Убрали папку — треки ушли, файлы остались. Старые номера ошибок не дают.
    library.remove_root(roots[0].id).unwrap();
    library.save_queue(&[first.id], 0, 1000, "list").unwrap();
    library.add_history(first.id).unwrap();
    library.record_play(first.id, 1000).unwrap();
    library.set_liked(first.id, true).unwrap();
    assert!(library.saved_queue().unwrap().tracks.is_empty());
    assert!(library.history(10).unwrap().is_empty());
    assert_eq!(library.stats().unwrap().tracks, 0);
    assert!(library.search("Первая", 10).unwrap().is_empty());
    assert!(scratch.path("music/A/Альбом/01.flac").exists());
}

/// Значок текста: `.lrc` рядом и текст в тегах.
#[test]
fn scan_notices_lyrics_next_to_file_and_in_tags() {
    let scratch = Scratch::new("lib-lyrics");
    scratch.write("music/a.flac", &flac(&[("TITLE", "С файлом")]));
    scratch.write("music/b.flac", &flac(&[("TITLE", "В тегах"), ("LYRICS", "[00:01.00]строка")]));
    scratch.write("music/c.flac", &flac(&[("TITLE", "Без текста")]));
    scratch.write("music/a.lrc", "[00:01.00]первая строка".as_bytes());
    let mut library = library(&scratch);
    scan(&mut library);
    let lyrics = |library: &Library, title: &str| library.search(title, 5).unwrap()[0].lyrics;
    assert!(lyrics(&library, "файлом"));
    assert!(lyrics(&library, "тегах"));
    assert!(!lyrics(&library, "Без"));

    // Текст убрали, а к третьему добавили — сами треки не менялись.
    fs::remove_file(scratch.path("music/a.lrc")).unwrap();
    scratch.write("music/c.lrc", "[00:02.00]вторая".as_bytes());
    scan(&mut library);
    assert!(!lyrics(&library, "файлом"));
    assert!(lyrics(&library, "Без"));
    assert!(lyrics(&library, "тегах"));
}
