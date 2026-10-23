//! Данные пользователя и правка тегов.

use nyanoto_library::{
    Grouping, Library, ScanProgress, TagField, Track, quality, read_fields, write_fields,
};
use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};

fn setup(scratch: &Scratch) -> Library {
    for (index, name) in ["a", "b", "c"].iter().enumerate() {
        let title = format!("Песня {name}");
        let number = (index + 1).to_string();
        let file = encode_flac_tagged(
            &signal(4096, 44_100),
            44_100,
            &[("TITLE", title.as_str()), ("TRACKNUMBER", number.as_str())],
            0,
        );
        scratch.write(&format!("music/Исполнитель/Альбом/0{number}. {name}.flac"), &file.bytes);
    }
    let mut library = Library::open(&scratch.path("data/library.db")).unwrap();
    library.add_root(&scratch.path("music")).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    library
}

fn tracks(library: &Library) -> Vec<Track> {
    let artist = &library.artists(Grouping::Folders).unwrap()[0];
    let album = &library.albums(artist).unwrap()[0];
    library.album_tracks(album).unwrap()
}

#[test]
fn settings_round_trip() {
    let scratch = Scratch::new("user-settings");
    let library = setup(&scratch);
    assert_eq!(library.setting("grouping").unwrap(), None);
    library.set_setting("grouping", "tags").unwrap();
    library.set_setting("grouping", "folders").unwrap();
    assert_eq!(library.setting("grouping").unwrap().as_deref(), Some("folders"));
}

#[test]
fn likes_and_playlists() {
    let scratch = Scratch::new("user-playlists");
    let library = setup(&scratch);
    let tracks = tracks(&library);
    let (a, b, c) = (tracks[0].id, tracks[1].id, tracks[2].id);

    library.set_liked(b, true).unwrap();
    library.set_liked(b, true).unwrap();
    library.set_liked(c, true).unwrap();
    library.set_liked(c, false).unwrap();
    assert_eq!(library.liked_ids().unwrap().into_iter().collect::<Vec<_>>(), [b]);
    assert_eq!(library.liked_tracks().unwrap()[0].id, b);

    let list = library.create_playlist("  Дорога  ").unwrap();
    library.add_to_playlist(list, &[c, a]).unwrap();
    library.add_to_playlist(list, &[c]).unwrap();
    let playlists = library.playlists().unwrap();
    assert_eq!((playlists[0].name.as_str(), playlists[0].tracks), ("Дорога", 3));

    // Трек дважды в плейлисте: убирается именно тот, на чьё место указали.
    let items = library.playlist_tracks(list).unwrap();
    let order: Vec<i64> = items.iter().map(|(_, track)| track.id).collect();
    assert_eq!(order, [c, a, c]);
    library.remove_from_playlist(list, items[0].0).unwrap();
    let order: Vec<i64> = library.playlist_tracks(list).unwrap().iter().map(|(_, track)| track.id).collect();
    assert_eq!(order, [a, c]);

    library.delete_playlist(list).unwrap();
    assert!(library.playlists().unwrap().is_empty());
}

#[test]
fn listening_history_feeds_statistics() {
    let scratch = Scratch::new("user-stats");
    let library = setup(&scratch);
    let tracks = tracks(&library);
    for _ in 0..3 {
        library.record_play(tracks[1].id, 60_000).unwrap();
    }
    library.record_play(tracks[0].id, 30_000).unwrap();

    let stats = library.listen_stats(Grouping::Folders, 10).unwrap();
    assert_eq!((stats.plays, stats.listened_ms), (4, 210_000));
    assert!(stats.since.is_some());
    assert_eq!(stats.top_tracks[0].0.id, tracks[1].id);
    assert_eq!(stats.top_tracks[0].1, 3);
    assert_eq!(stats.top_artists, [("Исполнитель".to_string(), 4, 210_000)]);

    // Альбом прослушан столько раз, сколько его длин набралось по времени.
    let graph = library.listen_graph(Grouping::Folders, 10, 10).unwrap();
    assert_eq!(graph.len(), 1);
    assert_eq!((graph[0].name.as_str(), graph[0].tracks, graph[0].listened_ms), ("Исполнитель", 4, 210_000));
    let length: i64 = tracks.iter().filter_map(|track| track.duration_ms).sum();
    let album = &graph[0].albums[0];
    assert_eq!(album.listened_ms, 210_000);
    assert!((album.plays - 210_000.0 / length as f32).abs() < 0.01, "{} раз", album.plays);
    assert!(graph[0].albums.windows(2).all(|pair| pair[0].listened_ms >= pair[1].listened_ms));
    // Папки: альбома — для обложки, исполнителя — для его фото.
    let artist_folder = graph[0].folder.clone().expect("папка исполнителя");
    assert!(artist_folder.ends_with("Исполнитель"));
    assert!(graph[0].albums.iter().all(|album| album.folder.parent() == Some(artist_folder.as_path())));
}

#[test]
fn quality_label_describes_the_stream() {
    let scratch = Scratch::new("user-quality");
    let library = setup(&scratch);
    let mut track = tracks(&library).remove(0);
    let flac = quality(&track);
    assert_eq!((flac.format, flac.lossless, flac.hires), ("FLAC", true, false));
    assert_eq!((flac.bit_depth, flac.sample_rate, flac.channels), (Some(16), Some(44_100), Some(2)));

    track.bit_depth = Some(24);
    track.sample_rate = Some(96_000);
    assert!(quality(&track).hires);

    track.format = Some("mpeg".into());
    track.bit_depth = None;
    track.sample_rate = Some(44_100);
    track.bitrate = Some(320);
    let mp3 = quality(&track);
    assert_eq!((mp3.format, mp3.lossless, mp3.hires, mp3.bitrate), ("MP3", false, false, Some(320)));

    // DSD: разрядность и частота уже в имени, отдельно не пишутся.
    track.format = Some("dsd".into());
    track.bit_depth = Some(1);
    track.sample_rate = Some(5_644_800);
    let dsd = quality(&track);
    assert_eq!((dsd.format, dsd.lossless, dsd.hires), ("DSD128", true, true));
    assert_eq!((dsd.bit_depth, dsd.sample_rate), (None, None));

    track.format = Some("tta".into());
    track.bit_depth = Some(24);
    track.sample_rate = Some(48_000);
    let tta = quality(&track);
    assert_eq!((tta.format, tta.lossless, tta.hires, tta.bit_depth), ("TTA", true, true, Some(24)));
}

#[test]
fn tag_editing_keeps_cover_audio_and_unknown_fields() {
    let scratch = Scratch::new("user-tags");
    let original = encode_flac_tagged(
        &signal(4096 * 3, 44_100),
        44_100,
        &[
            ("TITLE", "Старое"),
            ("ARTIST", "Первый"),
            ("ARTIST", "Второй"),
            ("GENRE", "Rock"),
            ("QOBUZ URL", "https://example.invalid/album"),
        ],
        100_000,
    );
    let path = scratch.write("music/song.flac", &original.bytes);
    let audio_start = original.frames[0].start;
    let audio = &original.bytes[audio_start..];

    let before = read_fields(&path).unwrap();
    assert!(before.editable);
    assert_eq!(before.fields.len(), 5);

    // Меняем название, убираем жанр, добавляем своё поле.
    let mut fields: Vec<TagField> = before
        .fields
        .iter()
        .filter(|field| field.key != "GENRE")
        .cloned()
        .map(|mut field| {
            if field.key == "TITLE" {
                field.value = "Новое".into();
            }
            field
        })
        .collect();
    fields.push(TagField { key: "mood".into(), value: "спокойное".into() });
    write_fields(&path, &fields).unwrap();

    let after = read_fields(&path).unwrap();
    let pairs: Vec<(&str, &str)> = after
        .fields
        .iter()
        .map(|field| (field.key.as_str(), field.value.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("TITLE", "Новое"),
            ("ARTIST", "Первый"),
            ("ARTIST", "Второй"),
            ("QOBUZ URL", "https://example.invalid/album"),
            ("MOOD", "спокойное"),
        ]
    );

    // Звук не тронут: кадры в конце файла побайтно те же. Обложка на месте.
    let written = std::fs::read(&path).unwrap();
    assert!(written.ends_with(audio));
    assert!(written.windows(64).any(|window| window.iter().all(|byte| *byte == 0xA5)));
    assert!(!scratch.path("music/song.nyanoto-tmp").exists());

    // Недопустимое имя поля отклоняется, файл не меняется.
    let bad = [TagField { key: "A=B".into(), value: "x".into() }];
    assert!(write_fields(&path, &bad).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), written);

    // Не-FLAC пока только для чтения.
    scratch.write("music/song.mp3", b"ID3");
    assert!(write_fields(&scratch.path("music/song.mp3"), &fields).is_err());
}

#[test]
fn authenticity_checks_are_kept_until_the_file_changes() {
    use nyanoto_library::{Authenticity, CheckLevel, CheckRecord, QualityOverride};

    let scratch = Scratch::new("user-authenticity");
    let mut library = setup(&scratch);
    let tracks = tracks(&library);
    assert_eq!(library.tracks_to_check(100).unwrap().len(), 3);
    // Пачками: следующая продолжает после последнего трека прошлой.
    let first = library.tracks_to_check_after(0, 2).unwrap();
    assert_eq!(first.len(), 2);
    let rest = library.tracks_to_check_after(first[1].0, 2).unwrap();
    assert_eq!(rest.len(), 1);
    assert!(rest[0].0 > first[1].0);
    assert!(library.tracks_to_check_after(rest[0].0, 2).unwrap().is_empty());

    use Authenticity::{Genuine, LossyOrigin};
    // Подозрителен только при средней и высокой чувствительности.
    let lossy = CheckRecord { verdicts: [Genuine, LossyOrigin, LossyOrigin], cutoff_hz: Some(19_900), real_bits: None, real_rate: None };
    let fine = CheckRecord { verdicts: [Genuine; 3], cutoff_hz: Some(21_000), real_bits: None, real_rate: None };
    library.store_check(tracks[0].id, &lossy).unwrap();
    library.store_check(tracks[1].id, &fine).unwrap();
    library.store_check(tracks[2].id, &fine).unwrap();
    assert!(library.tracks_to_check(100).unwrap().is_empty());
    assert!(library.suspicious(CheckLevel::Low).unwrap().is_empty());
    let suspicious = library.suspicious(CheckLevel::Medium).unwrap();
    assert_eq!(suspicious.len(), 1);
    assert_eq!(suspicious[&tracks[0].id].verdict, LossyOrigin);
    assert_eq!(suspicious[&tracks[0].id].cutoff_hz, Some(19_900));
    assert_eq!(library.suspicious(CheckLevel::High).unwrap().len(), 1);

    // Решение пользователя по треку хранится и снимается.
    library.set_quality_override(tracks[0].id, Some(&QualityOverride::Label("CD".into()))).unwrap();
    assert_eq!(library.quality_overrides().unwrap()[&tracks[0].id], QualityOverride::Label("CD".into()));
    library.set_quality_override(tracks[0].id, Some(&QualityOverride::Accept)).unwrap();
    assert_eq!(library.quality_overrides().unwrap()[&tracks[0].id], QualityOverride::Accept);
    library.set_quality_override(tracks[0].id, None).unwrap();
    assert!(library.quality_overrides().unwrap().is_empty());

    // Файл заменили: старый вердикт не действует, файл ждёт новой проверки.
    let path = &tracks[0].path;
    let file = encode_flac_tagged(&signal(8192, 44_100), 44_100, &[("TITLE", "Новое")], 0);
    std::fs::write(path, file.bytes).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    assert!(library.suspicious(CheckLevel::High).unwrap().is_empty());
    let pending = library.tracks_to_check(100).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, tracks[0].id);
}

#[test]
fn positions_and_bookmarks_of_long_recordings() {
    let scratch = Scratch::new("user-long");
    let library = setup(&scratch);
    let tracks = tracks(&library);
    let (book, other) = (tracks[0].id, tracks[1].id);

    assert_eq!(library.position(book).unwrap(), None);
    library.save_position(book, 61_000).unwrap();
    library.save_position(book, 3_723_000).unwrap();
    assert_eq!(library.position(book).unwrap(), Some(3_723_000));
    assert_eq!(library.position(other).unwrap(), None);
    library.forget_position(book).unwrap();
    assert_eq!(library.position(book).unwrap(), None);

    let late = library.add_bookmark(book, 90_000, "  Сцена у моря ").unwrap();
    library.add_bookmark(book, 10_000, "Начало").unwrap();
    library.add_bookmark(other, 5_000, "Чужая").unwrap();
    let marks = library.bookmarks(book).unwrap();
    let names: Vec<_> = marks.iter().map(|mark| (mark.position_ms, mark.name.as_str())).collect();
    assert_eq!(names, [(10_000, "Начало"), (90_000, "Сцена у моря")]);
    library.delete_bookmark(late).unwrap();
    assert_eq!(library.bookmarks(book).unwrap().len(), 1);
}

#[test]
fn smart_playlists_follow_their_rules() {
    use nyanoto_library::{Condition, SmartOrder, SmartRules, TextField};

    let scratch = Scratch::new("user-smart");
    let library = setup(&scratch);
    let tracks = tracks(&library);
    let (a, b, c) = (tracks[0].id, tracks[1].id, tracks[2].id);
    library.set_liked(b, true).unwrap();
    for _ in 0..3 {
        library.record_play(c, 60_000).unwrap();
    }
    library.record_play(b, 60_000).unwrap();

    let ids = |rules: &SmartRules| -> Vec<i64> {
        library.smart_tracks(rules, 7).unwrap().iter().map(|track| track.id).collect()
    };
    let rules = |all: bool, conditions: Vec<Condition>| SmartRules { all, conditions, ..SmartRules::default() };

    assert_eq!(ids(&rules(true, vec![Condition::Liked])), [b]);
    assert_eq!(ids(&rules(true, vec![Condition::NeverPlayed])), [a]);
    assert_eq!(ids(&rules(true, vec![Condition::PlayedAtLeast(2)])), [c]);
    // Регистр и кириллица: «ПЕСНЯ A» находит «Песня a».
    assert_eq!(ids(&rules(true, vec![Condition::Contains(TextField::Title, "ПЕСНЯ A".into())])), [a]);
    assert_eq!(ids(&rules(false, vec![Condition::Liked, Condition::NeverPlayed])), [a, b]);
    assert!(ids(&rules(true, vec![Condition::Liked, Condition::NeverPlayed])).is_empty());
    assert_eq!(ids(&rules(true, vec![Condition::NotPlayedForDays(30)])), [a], "слушали сегодня — не давно");

    let mut most = rules(true, Vec::new());
    most.order = SmartOrder::MostPlayed;
    most.limit = Some(2);
    assert_eq!(ids(&most), [c, b]);
    most.order = SmartOrder::Random;
    most.limit = None;
    let mut shuffled = ids(&most);
    shuffled.sort();
    let mut all = vec![a, b, c];
    all.sort();
    assert_eq!(shuffled, all, "случайный порядок не теряет треки");

    let id = library.create_smart_playlist("Любимое", &rules(true, vec![Condition::Liked])).unwrap();
    let stored = library.smart_rules(id).unwrap().unwrap();
    assert_eq!(stored, rules(true, vec![Condition::Liked]));
    let listed = library.playlists().unwrap();
    assert!(listed.iter().any(|playlist| playlist.id == id && playlist.smart));
    let plain = library.create_playlist("Обычный").unwrap();
    assert_eq!(library.smart_rules(plain).unwrap(), None);
}

#[test]
fn tags_and_cover_travel_to_a_converted_file() {
    let scratch = Scratch::new("copy-tags");
    let source = scratch.write(
        "source.flac",
        &encode_flac_tagged(&signal(4096, 44_100), 44_100, &[("TITLE", "Песня"), ("ARTIST", "Кто"), ("TRACKNUMBER", "3")], 2048)
            .bytes,
    );
    // Как будто только что сконвертирован: звук есть, тегов нет.
    let target = scratch.write("target.flac", &nyanoto_testkit::encode_flac(&signal(4096, 44_100), 44_100).bytes);
    nyanoto_library::copy_tags(&source, &target).unwrap();
    let fields = read_fields(&target).unwrap().fields;
    let value = |key: &str| fields.iter().find(|field| field.key.eq_ignore_ascii_case(key)).map(|field| field.value.clone());
    assert_eq!(value("TITLE").as_deref(), Some("Песня"));
    assert_eq!(value("ARTIST").as_deref(), Some("Кто"));
    assert_eq!(value("TRACKNUMBER").as_deref(), Some("3"));
    assert!(std::fs::metadata(&target).unwrap().len() > 2048, "обложка перенесена");
}

#[test]
fn queue_survives_restart_and_lost_tracks() {
    let scratch = Scratch::new("user-queue");
    let library = setup(&scratch);
    let tracks = tracks(&library);
    let ids: Vec<i64> = tracks.iter().map(|track| track.id).collect();
    assert!(library.saved_queue().unwrap().tracks.is_empty());

    // Один трек дважды: очередь — не множество.
    let queue = [ids[2], ids[0], ids[1], ids[0]];
    library.save_queue(&queue, 2, 61_500, "album").unwrap();
    library.save_queue_place(2, 65_000).unwrap();
    drop(library);
    let mut library = Library::open(&scratch.path("data/library.db")).unwrap();
    let saved = library.saved_queue().unwrap();
    let got: Vec<i64> = saved.tracks.iter().map(|track| track.id).collect();
    assert_eq!(got, queue);
    assert_eq!((saved.current, saved.position_ms, saved.kind.as_deref()), (2, 65_000, Some("album")));

    // Файл текущего трека пропал: им становится следующий, с начала.
    std::fs::remove_file(&tracks[1].path).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    let saved = library.saved_queue().unwrap();
    let got: Vec<i64> = saved.tracks.iter().map(|track| track.id).collect();
    assert_eq!(got, [ids[2], ids[0], ids[0]]);
    assert_eq!((saved.current, saved.position_ms), (2, 0));
}

#[test]
fn history_keeps_order_without_repeats_in_a_row() {
    let scratch = Scratch::new("user-history");
    let library = setup(&scratch);
    let tracks = tracks(&library);
    let (a, b) = (tracks[0].id, tracks[1].id);
    for id in [a, b, b, a] {
        library.add_history(id).unwrap();
    }
    let got: Vec<i64> = library.history(10).unwrap().iter().map(|(track, _)| track.id).collect();
    assert_eq!(got, [a, b, a], "повтор подряд — одна запись");
    assert_eq!(library.history(2).unwrap().len(), 2);
    // Предел: старые записи уходят.
    for round in 0..(nyanoto_library::HISTORY_LIMIT + 10) {
        library.add_history(if round % 2 == 0 { a } else { b }).unwrap();
    }
    assert_eq!(library.history(10_000).unwrap().len() as i64, nyanoto_library::HISTORY_LIMIT);
    library.clear_history().unwrap();
    assert!(library.history(10).unwrap().is_empty());
}

/// Одна папка по папкам, другая по тегам — обзор сводит обе.
#[test]
fn each_root_has_its_own_grouping() {
    let scratch = Scratch::new("user-per-root");
    let tagged = |artist: &str, album: &str, title: &str| {
        encode_flac_tagged(
            &signal(4096, 44_100),
            44_100,
            &[("ARTIST", artist), ("ALBUM", album), ("TITLE", title)],
            0,
        )
        .bytes
    };
    // По папкам: имена папок; теги здесь другие и не должны быть видны.
    scratch.write("folders/Папка-исполнитель/Папка-альбом/01.flac", &tagged("Тег А", "Тег-альбом А", "Раз"));
    // По тегам: папки свалены как попало, а теги в порядке.
    scratch.write("tags/свалка/x.flac", &tagged("Тег Б", "Тег-альбом Б", "Два"));
    let mut library = Library::open(&scratch.path("data/library.db")).unwrap();
    let folders = library.add_root(&scratch.path("folders")).unwrap();
    let tags = library.add_root(&scratch.path("tags")).unwrap();
    library.set_root_grouping(folders, Grouping::Folders).unwrap();
    library.set_root_grouping(tags, Grouping::Tags).unwrap();
    library.scan(&ScanProgress::default()).unwrap();

    let roots = library.roots().unwrap();
    assert_eq!(roots.iter().map(|root| root.grouping).collect::<Vec<_>>(), [Grouping::Folders, Grouping::Tags]);
    let artists = library.artists(Grouping::PerRoot).unwrap();
    let names: Vec<&str> = artists.iter().map(|artist| artist.name.as_str()).collect();
    assert_eq!(names, ["Папка-исполнитель", "Тег Б"]);
    let by_tags = library.albums(&artists[1]).unwrap();
    assert_eq!(by_tags[0].title.as_deref(), Some("Тег-альбом Б"));
    assert_eq!(by_tags[0].grouping, Grouping::Tags, "альбом раскладывается, как его папка");
    assert_eq!(library.album_tracks(&by_tags[0]).unwrap().len(), 1);
    let by_folders = library.albums(&artists[0]).unwrap();
    assert_eq!(by_folders[0].title.as_deref(), Some("Папка-альбом"));

    // Поменяли раскладку папки — обзор следует за ней.
    library.set_root_grouping(folders, Grouping::Tags).unwrap();
    let names: Vec<String> = library.artists(Grouping::PerRoot).unwrap().into_iter().map(|artist| artist.name).collect();
    assert_eq!(names, ["Тег А", "Тег Б"]);
}
