//! Поиск с поправкой на раскладку и опечатки на настоящем индексе.

use nyanoto_library::{Library, ScanProgress};
use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};

fn library(scratch: &Scratch, titles: &[(&str, &str)]) -> Library {
    for (index, (artist, title)) in titles.iter().enumerate() {
        let file = encode_flac_tagged(
            &signal(4096, 44_100),
            44_100,
            &[("TITLE", title), ("ARTIST", artist)],
            0,
        );
        scratch.write(&format!("music/{index:02}.flac"), &file.bytes);
    }
    let mut library = Library::open(&scratch.path("data/library.db")).unwrap();
    library.add_root(&scratch.path("music")).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    library
}

fn titles(library: &Library, query: &str) -> Vec<String> {
    library
        .search(query, 50)
        .unwrap()
        .into_iter()
        .map(|track| track.title.unwrap_or_default())
        .collect()
}

#[test]
fn search_forgives_layout_and_typos_but_keeps_exact_first() {
    let scratch = Scratch::new("search-fuzzy");
    let library = library(
        &scratch,
        &[
            ("Кино", "Привет"),
            ("Кино", "Привет3"),
            ("Blink-182", "All the Small Things"),
            ("AC/DC", "Thunderstruck"),
            ("Radiohead", "Creep"),
            ("2 + 2 = 5", "Radiohead"),
        ],
    );

    // Неверная раскладка в обе стороны.
    assert!(titles(&library, "ghbdtn").contains(&"Привет".to_string()));
    assert!(titles(&library, "кфвшщруфв").contains(&"Creep".to_string()));

    // Пропущенная и лишняя буква.
    assert!(titles(&library, "првет").contains(&"Привет".to_string()));
    assert!(titles(&library, "пирвет").contains(&"Привет".to_string()));

    // «Привет3» — настоящее название: точное совпадение первым, похожее следом.
    let found = titles(&library, "привет3");
    assert_eq!(found.first().map(String::as_str), Some("Привет3"));
    assert!(found.contains(&"Привет".to_string()));

    // Знаки и цифры внутри названий работают и что-то значат.
    assert_eq!(titles(&library, "ac/dc"), ["Thunderstruck"]);
    assert_eq!(titles(&library, "blink-182"), ["All the Small Things"]);
    assert!(titles(&library, "blink 183").is_empty());
    assert!(titles(&library, "2 + 2 = 5").contains(&"Radiohead".to_string()));

    // Совсем другое слово не находит ничего.
    assert!(titles(&library, "пока").is_empty());

    // После нового сканирования словарь обновляется.
    let file = encode_flac_tagged(&signal(4096, 44_100), 44_100, &[("TITLE", "Звезда")], 0);
    scratch.write("music/new.flac", &file.bytes);
    let mut library = library;
    library.scan(&ScanProgress::default()).unwrap();
    library.forget_search_cache();
    assert!(titles(&library, "звзда").contains(&"Звезда".to_string()));
}
