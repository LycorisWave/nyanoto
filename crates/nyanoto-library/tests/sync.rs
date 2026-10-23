//! Связка устройств через общую папку.

use nyanoto_library::{Grouping, Library, ScanProgress, SyncReport, Track};
use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};

fn flac(title: &str) -> Vec<u8> {
    encode_flac_tagged(&signal(4096, 44_100), 44_100, &[("TITLE", title), ("ARTIST", "Кто")], 0).bytes
}

/// Устройство со своей копией музыки. `album` — как называется папка
/// альбома на этом устройстве.
fn device(scratch: &Scratch, name: &str, album: &str) -> Library {
    for title in ["Раз", "Два", "Три"] {
        scratch.write(&format!("{name}/music/Кто/{album}/{title}.flac"), &flac(title));
    }
    let mut library = Library::open(&scratch.path(&format!("{name}/library.db"))).unwrap();
    library.add_root(&scratch.path(&format!("{name}/music"))).unwrap();
    library.scan(&ScanProgress::default()).unwrap();
    library
}

fn track(library: &Library, title: &str) -> Track {
    let artist = &library.artists(Grouping::Folders).unwrap()[0];
    let album = &library.albums(artist).unwrap()[0];
    library
        .album_tracks(album)
        .unwrap()
        .into_iter()
        .find(|track| track.title.as_deref() == Some(title))
        .unwrap()
}

#[test]
fn likes_playlists_and_plays_travel_between_devices() {
    let scratch = Scratch::new("sync");
    let pc = device(&scratch, "pc", "Альбом");
    // На телефоне папка альбома называется иначе: треки узнаются по тегам.
    let phone = device(&scratch, "phone", "Альбом (2001)");

    let (one, two, three) = (track(&pc, "Раз"), track(&pc, "Два"), track(&pc, "Три"));
    pc.set_liked(one.id, true).unwrap();
    let road = pc.create_playlist("Дорога").unwrap();
    pc.add_to_playlist(road, &[one.id, two.id]).unwrap();
    pc.record_play(three.id, 60_000).unwrap();

    let from_pc = pc.sync_export("pc").unwrap();
    let report = phone.sync_import(&from_pc).unwrap();
    assert_eq!(report, SyncReport { likes: 1, unlikes: 0, playlists: 1, plays: 1, unknown: 0 });
    let phone_one = track(&phone, "Раз");
    assert!(phone.liked_ids().unwrap().contains(&phone_one.id));
    let lists = phone.playlists().unwrap();
    assert_eq!((lists[0].name.as_str(), lists[0].tracks), ("Дорога", 2));
    assert_eq!(phone.listen_stats(Grouping::Folders, 5).unwrap().plays, 1);

    // Повторный приём ничего не удваивает.
    assert_eq!(phone.sync_import(&from_pc).unwrap(), SyncReport::default());

    // На телефоне позже сняли лайк — компьютер это принимает.
    let mut from_phone = phone.sync_export("phone").unwrap();
    let key = from_phone.likes.iter().find(|mark| mark.key.tags == "кто|раз").unwrap().key.clone();
    from_phone.likes.clear();
    from_phone.unlikes.push(nyanoto_library::SyncMark { key, at: i64::MAX / 2 });
    let report = pc.sync_import(&from_phone).unwrap();
    assert_eq!(report.unlikes, 1);
    assert!(!pc.liked_ids().unwrap().contains(&one.id));
    // А лайк из старого файла компьютера снятие не перебивает.
    pc.sync_import(&from_pc).unwrap();
    assert!(!pc.liked_ids().unwrap().contains(&one.id));
}
