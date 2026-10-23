//! Метаданные классики и исправление кодировки тегов.

use nyanoto_library::{Grouping, Library, ScanProgress};
use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};

fn flac(tags: &[(&str, &str)]) -> Vec<u8> {
    encode_flac_tagged(&signal(4096, 44_100), 44_100, tags, 0).bytes
}

/// Строка в Windows-1251, прочитанная как Latin-1: так выглядят старые теги.
fn mojibake(text: &str) -> String {
    text.chars()
        .map(|char| match char {
            'А'..='я' => char::from_u32(u32::from(char) - 0x0410 + 0xC0).unwrap(),
            other => other,
        })
        .collect()
}

#[test]
fn composer_work_and_movements() {
    let scratch = Scratch::new("classical");
    let symphony = [
        ("TITLE", "Allegro con brio"),
        ("COMPOSER", "Бетховен"),
        ("WORK", "Симфония № 5"),
        ("CONDUCTOR", "Караян"),
        ("ALBUM", "Симфонии 5 и 7"),
        ("ARTIST", "Берлинский филармонический"),
    ];
    for (number, movement) in [(2, "Andante con moto"), (1, "Allegro con brio")] {
        let number_text = number.to_string();
        let mut tags = symphony.to_vec();
        tags.push(("MOVEMENTNAME", movement));
        tags.push(("MOVEMENT", &number_text));
        // Номера дорожек нарочно в обратном порядке: части идут по номеру части.
        tags.push(("TRACKNUMBER", if number == 1 { "9" } else { "3" }));
        scratch.write(&format!("music/Караян/Бетховен/{number}.flac"), &flac(&tags));
    }
    scratch.write(
        "music/Другое/Альбом/1.flac",
        &flac(&[("TITLE", "Песня"), ("ARTIST", "Кто-то"), ("ALBUM", "Альбом")]),
    );

    let mut library = Library::open(&scratch.path("db/library.db")).unwrap();
    library.add_root(&scratch.path("music")).unwrap();
    library.scan(&ScanProgress::default()).unwrap();

    let composers = library.artists(Grouping::Composers).unwrap();
    let names: Vec<&str> = composers.iter().map(|artist| artist.name.as_str()).collect();
    assert_eq!(names, ["Бетховен", ""], "без композитора — в конце");
    let works = library.albums(&composers[0]).unwrap();
    assert_eq!(works[0].title.as_deref(), Some("Симфония № 5"));
    let parts = library.album_tracks(&works[0]).unwrap();
    let movements: Vec<_> = parts.iter().map(|track| (track.movement_no, track.movement.clone())).collect();
    assert_eq!(movements, [(Some(1), Some("Allegro con brio".into())), (Some(2), Some("Andante con moto".into()))]);
    assert_eq!(parts[0].conductor.as_deref(), Some("Караян"));

    // Поиск находит по произведению и по дирижёру.
    assert_eq!(library.search("симфония", 10).unwrap().len(), 2);
    assert_eq!(library.search("караян", 10).unwrap().len(), 2);
    // Альбом трека в обзоре по композиторам — его произведение.
    let album = library.album_of(&parts[1], Grouping::Composers).unwrap().unwrap();
    assert_eq!(album.title.as_deref(), Some("Симфония № 5"));
}

#[test]
fn wrong_encoding_is_fixed_only_when_asked() {
    let scratch = Scratch::new("cp1251-tags");
    let title = mojibake("Группа крови");
    let artist = mojibake("Кино");
    scratch.write("music/a/b/1.flac", &flac(&[("TITLE", &title), ("ARTIST", &artist), ("ALBUM", "Björk")]));

    let mut library = Library::open(&scratch.path("db/library.db")).unwrap();
    library.add_root(&scratch.path("music")).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    assert!(library.search("группа", 10).unwrap().is_empty(), "без настройки ничего не исправляется");

    let (count, examples) = library.encoding_preview(10).unwrap();
    assert_eq!(count, 1);
    assert!(examples.contains(&(title.clone(), "Группа крови".to_string())));
    assert!(examples.contains(&(artist.clone(), "Кино".to_string())));

    library.set_fix_encoding(true).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    let found = library.search("группа", 10).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title.as_deref(), Some("Группа крови"));
    assert_eq!(found[0].artist.as_deref(), Some("Кино"));
    assert_eq!(found[0].album.as_deref(), Some("Björk"), "латиница с умлаутом остаётся");
    assert_eq!(library.encoding_preview(10).unwrap().0, 0);

    library.set_fix_encoding(false).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    assert!(library.search("группа", 10).unwrap().is_empty(), "выключение возвращает как в файле");
}
