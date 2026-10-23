//! Здоровье библиотеки: дубликаты, пропуски, повреждения, пропавшие файлы.

use nyanoto_library::{Damage, HealthRecord, Library, ScanProgress};
use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};

fn flac(frames: usize, tags: &[(&str, &str)]) -> Vec<u8> {
    encode_flac_tagged(&signal(frames, 44_100), 44_100, tags, 0).bytes
}

#[test]
fn report_finds_what_is_wrong() {
    let scratch = Scratch::new("health");
    // Альбом с пропуском третьего трека.
    for number in [1, 2, 4] {
        let number_text = number.to_string();
        let title = format!("Песня {number}");
        scratch.write(
            &format!("music/Кто/Альбом/0{number}. {title}.flac"),
            &flac(4096 * 2, &[("TITLE", &title), ("ARTIST", "Кто"), ("TRACKNUMBER", &number_text)]),
        );
    }
    // Та же песня в сборнике — дубликат; в той же папке дубликатом не считается.
    scratch.write(
        "music/Сборник/Лучшее/07. Кто - Песня 1.flac",
        &flac(4096 * 2, &[("TITLE", "песня 1!"), ("ARTIST", "КТО"), ("TRACKNUMBER", "7")]),
    );
    scratch.write("music/Кто/Пропадёт/01. x.flac", &flac(4096, &[("TITLE", "Икс")]));

    let mut library = Library::open(&scratch.path("db/library.db")).unwrap();
    library.add_root(&scratch.path("music")).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    std::fs::remove_file(scratch.path("music/Кто/Пропадёт/01. x.flac")).unwrap();
    library.scan(&ScanProgress::default()).unwrap();

    let pending = library.tracks_to_verify(100).unwrap();
    assert_eq!(pending.len(), 4);
    let (first, second) = (pending[0].0, pending[1].0);
    library.store_health(first, &HealthRecord { md5: Some(false), audio_ms: 100, ..HealthRecord::default() }).unwrap();
    library.store_health(second, &HealthRecord { decode_errors: 3, audio_ms: 100, ..HealthRecord::default() }).unwrap();
    assert_eq!(library.tracks_to_verify(100).unwrap().len(), 2);
    let after_first = library.tracks_to_verify_after(pending[2].0, 100).unwrap();
    assert_eq!(after_first.len(), 1, "продолжение — только после указанного трека");
    assert_eq!(library.health_progress().unwrap(), (2, 4));

    let report = library.health_report().unwrap();
    let damages: Vec<&Damage> = report.damaged.iter().map(|(_, damage)| damage).collect();
    // Номера треков раздаёт параллельное сканирование, поэтому порядок любой.
    assert_eq!(damages.len(), 2);
    assert!(damages.contains(&&Damage::Md5) && damages.contains(&&Damage::DecodeErrors(3)), "{damages:?}");
    assert_eq!(report.duplicates.len(), 1, "{:?}", report.duplicates);
    assert_eq!(report.duplicates[0].len(), 2);
    assert_eq!(report.gaps.len(), 1);
    assert!(report.gaps[0].folder.ends_with("Альбом"));
    assert_eq!(report.gaps[0].missing, ["3"]);
    assert_eq!(report.missing.len(), 1);
}
