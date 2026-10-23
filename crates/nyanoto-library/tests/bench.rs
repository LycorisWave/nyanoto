//! Замер того, что окно делает в своём потоке, на большой базе:
//! `NYANOTO_BENCH_DB=путь cargo test --release -p nyanoto-library --test bench -- --ignored --nocapture`.

use std::time::Instant;

use nyanoto_library::{Grouping, Library};

fn time<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = work();
    println!("{name:<40} {:>8.1} мс", started.elapsed().as_secs_f64() * 1000.0);
    result
}

#[test]
#[ignore]
fn ui_thread_operations() {
    let Ok(path) = std::env::var("NYANOTO_BENCH_DB") else {
        eprintln!("нет NYANOTO_BENCH_DB");
        return;
    };
    let library = time("открыть базу", || Library::open(std::path::Path::new(&path)).unwrap());
    time("сводка (stats)", || library.stats().unwrap());
    let artists = time("исполнители по папкам", || library.artists(Grouping::Folders).unwrap());
    time("исполнители по тегам", || library.artists(Grouping::Tags).unwrap());
    time("фильтр исполнителей «испалнитель 01»", || {
        let matcher = nyanoto_library::TextMatcher::new("испалнитель 01");
        artists.iter().filter(|artist| matcher.matches(&artist.name)).count()
    });
    time("фильтр исполнителей «zhanulka»", || {
        let matcher = nyanoto_library::TextMatcher::new("zhanulka");
        artists.iter().filter(|artist| matcher.matches(&artist.name)).count()
    });
    let artist = &artists[artists.len() / 2];
    let albums = time("альбомы исполнителя", || library.albums(artist).unwrap());
    time("треки альбома", || library.album_tracks(&albums[0]).unwrap());
    time("поиск «трек 01234»", || library.search("трек 01234", 300).unwrap());
    time("поиск с ранжированием «исполн 0123»", || library.search_ranked("исполн 0123", 300).unwrap());
    time("поиск с опечаткой «испалнитель»", || library.search_ranked("испалнитель", 300).unwrap());
    time("…ещё раз (словарь уже в памяти)", || library.search_ranked("испалнитель", 300).unwrap());
    time("поиск с опечаткой «трэк»", || library.search_ranked("трэк", 300).unwrap());
    time("поиск слова из всех треков «трек»", || library.search_ranked("трек", 300).unwrap());
    time("поиск в другой раскладке «nhtr»", || library.search_ranked("nhtr", 300).unwrap());
    time("лайки", || library.liked_ids().unwrap());
    time("плейлисты", || library.playlists().unwrap());
    time("статистика", || library.listen_stats(Grouping::Folders, 20).unwrap());
    time("карта прослушиваний", || library.listen_graph(Grouping::Folders, 30, 10).unwrap());
    time("сохранённая очередь", || library.saved_queue().unwrap());
    time("история", || library.history(500).unwrap());
    let tracks = time("все треки альбома ×100 (очередь)", || {
        albums.iter().cycle().take(100).map(|album| library.album_tracks(album).unwrap().len()).sum::<usize>()
    });
    println!("треков: {tracks}");
}
