//! Поиск в своём потоке: на большой базе это десятки мс, в окне набор бы тормозил.
//! Берём только последний запрос, ответ с номером — устаревший окно не покажет.

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};

use nyanoto_library::{Library, Track, natural_cmp};

enum Job {
    Find { id: u64, text: String },
    /// Библиотека поменялась (сканирование): словарь поиска перечитать.
    Forget,
}

/// Ответ: треки — сначала найденные точно, за ними с поправками; внутри
/// каждой группы — по папкам, как в обзоре.
pub struct Found {
    pub id: u64,
    pub tracks: Vec<Track>,
}

pub struct Searcher {
    jobs: Sender<Job>,
}

impl Searcher {
    pub fn new(database: Option<PathBuf>, done: impl Fn(Found) + Send + 'static) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let _ = std::thread::Builder::new().name("nyanoto-search".into()).spawn(move || {
            let Some(library) = database.and_then(|path| Library::open(&path).ok()) else {
                return;
            };
            while let Ok(first) = inbox.recv() {
                // Пока искали, пользователь мог набрать ещё: нужен только последний запрос.
                let mut latest = None;
                for job in std::iter::once(first).chain(inbox.try_iter()) {
                    match job {
                        Job::Forget => library.forget_search_cache(),
                        Job::Find { id, text } => latest = Some((id, text)),
                    }
                }
                let Some((id, text)) = latest else {
                    continue;
                };
                let (tracks, exact) = library.search_ranked(&text, 300).unwrap_or_default();
                let exact = exact.min(tracks.len());
                let (mut first, mut rest) = (tracks[..exact].to_vec(), tracks[exact..].to_vec());
                first.sort_by(|a, b| natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy()));
                rest.sort_by(|a, b| natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy()));
                first.extend(rest);
                done(Found { id, tracks: first });
            }
        });
        Self { jobs }
    }

    pub fn find(&self, id: u64, text: &str) {
        let _ = self.jobs.send(Job::Find { id, text: text.to_string() });
    }

    pub fn forget(&self) {
        let _ = self.jobs.send(Job::Forget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyanoto_library::ScanProgress;
    use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};
    use std::time::Duration;

    #[test]
    fn answers_the_latest_query_with_exact_matches_first() {
        let scratch = Scratch::new("searcher");
        for (name, title) in [("01", "Бабочки"), ("02", "Бабочка ночная"), ("03", "Другое")] {
            let file = encode_flac_tagged(&signal(4096, 44_100), 44_100, &[("TITLE", title), ("ARTIST", "Zhanulka")], 0);
            scratch.write(&format!("music/Zhanulka/Альбом/{name}.flac"), &file.bytes);
        }
        let database = scratch.path("data/library.db");
        let mut library = Library::open(&database).unwrap();
        library.add_root(&scratch.path("music")).unwrap();
        library.scan(&ScanProgress::default()).unwrap();

        let (sender, answers) = mpsc::channel();
        let searcher = Searcher::new(Some(database), move |found| {
            let _ = sender.send(found);
        });
        // Набор подряд: ответ нужен на последний запрос, промежуточные можно пропустить.
        searcher.find(1, "б");
        searcher.find(2, "баб");
        searcher.find(3, "бабочки");
        let mut last = None;
        while let Ok(found) = answers.recv_timeout(Duration::from_secs(10)) {
            let done = found.id == 3;
            last = Some(found);
            if done {
                break;
            }
        }
        let found = last.expect("ответ пришёл");
        assert_eq!(found.id, 3);
        let titles: Vec<_> = found.tracks.iter().map(|track| track.title.clone().unwrap_or_default()).collect();
        assert_eq!(titles.first().map(String::as_str), Some("Бабочки"), "{titles:?}");
        // С опечаткой — через словарь; после «забыть» словарь читается заново.
        searcher.forget();
        searcher.find(4, "бабчки");
        let found = answers.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(found.id, 4);
        assert!(found.tracks.iter().any(|track| track.title.as_deref() == Some("Бабочки")));
    }
}
