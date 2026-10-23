//! Обзор: исполнитель → альбом → трек.
//! По папкам: первая — исполнитель, вторая — альбом, глубже — тот же альбом. По тегам — по тегам.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params_from_iter};

use crate::{Library, LibraryError, TRACK_COLUMNS, Track};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    /// Исполнитель и альбом — папки, подписи — имена папок.
    #[default]
    Folders,
    /// Исполнитель и альбом — теги.
    Tags,
    /// Для классики: композитор → произведение (а без него — альбом) → части.
    Composers,
    /// У каждой папки музыки — своя раскладка.
    PerRoot,
}

/// Как разложена папка музыки, из которой трек, на языке SQL.
const ROOT_GROUPING: &str = "(SELECT grouping FROM roots WHERE id = t.root_id)";

impl Grouping {
    /// Под каким исполнителем трек стоит в обзоре, на языке SQL.
    pub(crate) fn owner(self) -> &'static str {
        match self {
            Grouping::Folders => "COALESCE(t.folder_artist, '')",
            Grouping::Tags => "COALESCE(t.owner, '')",
            Grouping::Composers => "COALESCE(t.composer, '')",
            Grouping::PerRoot => {
                "CASE (SELECT grouping FROM roots WHERE id = t.root_id)
                     WHEN 'tags' THEN COALESCE(t.owner, '')
                     WHEN 'composers' THEN COALESCE(t.composer, '')
                     ELSE COALESCE(t.folder_artist, '') END"
            }
        }
    }

    /// К какому альбому относится трек, на языке SQL.
    pub(crate) fn album(self) -> &'static str {
        match self {
            Grouping::Folders => "t.folder_album",
            Grouping::Tags => "t.album",
            Grouping::Composers => "COALESCE(t.work, t.album)",
            Grouping::PerRoot => {
                "CASE (SELECT grouping FROM roots WHERE id = t.root_id)
                     WHEN 'tags' THEN t.album
                     WHEN 'composers' THEN COALESCE(t.work, t.album)
                     ELSE t.folder_album END"
            }
        }
    }

    /// Как записано у папки музыки.
    pub fn code(self) -> &'static str {
        match self {
            Grouping::Folders | Grouping::PerRoot => "folders",
            Grouping::Tags => "tags",
            Grouping::Composers => "composers",
        }
    }

    /// Из записи у папки; нет записи — по папкам.
    pub fn from_code(code: Option<&str>) -> Self {
        match code {
            Some("tags") => Grouping::Tags,
            Some("composers") => Grouping::Composers,
            _ => Grouping::Folders,
        }
    }
}

/// «КИНО» и «Кино» — один исполнитель.
#[derive(Debug, Clone)]
pub struct Artist {
    pub grouping: Grouping,
    /// Имя для показа; пустое у файлов без исполнителя.
    pub name: String,
    /// Все написания.
    pub names: Vec<String>,
    pub albums: u32,
    pub tracks: u32,
}

#[derive(Debug, Clone)]
pub struct Album {
    pub grouping: Grouping,
    /// Написания исполнителя, под которыми альбом найден.
    pub owners: Vec<String>,
    /// `None`, если у файлов нет папки альбома (или тега альбома).
    pub title: Option<String>,
    pub year: Option<u16>,
    pub tracks: u32,
    pub duration_ms: i64,
    /// Папка альбома: там обычно лежит обложка.
    pub folder: PathBuf,
}

/// Список `?N` для подстановки значений в `IN (...)`, начиная с номера `first`.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Как в проводнике: без регистра, «2» раньше «10».
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(digit) = chars.next_if(char::is_ascii_digit) {
                        digits.push(digit);
                    }
                    digits
                };
                let (x, y) = (take(&mut left), take(&mut right));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                left.next();
                right.next();
            }
        }
    }
}

/// Пустые имена (файлы без папки, треки без тега) идут в конце списка.
fn names_cmp(a: &str, b: &str) -> Ordering {
    a.is_empty().cmp(&b.is_empty()).then_with(|| natural_cmp(a, b))
}

impl Library {
    /// Все исполнители библиотеки по алфавиту.
    pub fn artists(&self, grouping: Grouping) -> Result<Vec<Artist>, LibraryError> {
        let owner = grouping.owner();
        let album = grouping.album();
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {owner} AS group_owner, COUNT(DISTINCT COALESCE({album}, '')), COUNT(*)
             FROM tracks t WHERE t.missing = 0 GROUP BY group_owner"
        ))?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, u32>(2)?))
        })?;

        let mut merged: Vec<Artist> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for row in rows {
            let (name, albums, tracks) = row?;
            match index.get(&name.to_lowercase()) {
                Some(&at) => {
                    let artist = &mut merged[at];
                    artist.names.push(name);
                    artist.albums += albums;
                    artist.tracks += tracks;
                }
                None => {
                    index.insert(name.to_lowercase(), merged.len());
                    let names = vec![name.clone()];
                    merged.push(Artist { grouping, name, names, albums, tracks });
                }
            }
        }
        // У написаний одного исполнителя могут быть общие альбомы: пересчитываем.
        for artist in merged.iter_mut().filter(|artist| artist.names.len() > 1) {
            artist.albums = self.connection.query_row(
                &format!(
                    "SELECT COUNT(DISTINCT COALESCE({album}, '')) FROM tracks t
                     WHERE t.missing = 0 AND {owner} IN ({})",
                    placeholders(1, artist.names.len())
                ),
                params_from_iter(&artist.names),
                |row| row.get(0),
            )?;
        }
        merged.sort_by(|a, b| names_cmp(&a.name, &b.name));
        Ok(merged)
    }

    /// Альбомы исполнителя: по папкам — как в проводнике, по тегам — по году.
    pub fn albums(&self, artist: &Artist) -> Result<Vec<Album>, LibraryError> {
        let grouping = artist.grouping;
        let (owner, album) = (grouping.owner(), grouping.album());
        let mut statement = self.connection.prepare(&format!(
            "SELECT {album} AS group_album, MIN(t.year), COUNT(*), COALESCE(SUM(t.duration_ms), 0),
                    MIN(r.path || '/' || t.rel_path), MIN(r.grouping)
             FROM tracks t JOIN roots r ON r.id = t.root_id
             WHERE t.missing = 0 AND {owner} IN ({})
             GROUP BY group_album",
            placeholders(1, artist.names.len())
        ))?;
        let mut albums: Vec<Album> = statement
            .query_map(params_from_iter(&artist.names), |row| {
                let title: Option<String> = row.get(0)?;
                let first: String = row.get(4)?;
                // У каждой папки своё — альбом раскладывается, как его папка.
                let grouping = match grouping {
                    Grouping::PerRoot => Grouping::from_code(row.get::<_, Option<String>>(5)?.as_deref()),
                    other => other,
                };
                Ok(Album {
                    grouping,
                    owners: artist.names.clone(),
                    folder: album_folder(grouping, Path::new(&first), title.as_deref()),
                    title,
                    year: row.get(1)?,
                    tracks: row.get(2)?,
                    duration_ms: row.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        // По папкам — по имени, иначе — по году.
        if albums.iter().all(|album| album.grouping == Grouping::Folders) {
            albums.sort_by(|a, b| names_cmp(a.title.as_deref().unwrap_or(""), b.title.as_deref().unwrap_or("")));
        } else {
            albums.sort_by(|a, b| {
                (a.year.is_none(), a.year)
                    .cmp(&(b.year.is_none(), b.year))
                    .then_with(|| names_cmp(a.title.as_deref().unwrap_or(""), b.title.as_deref().unwrap_or("")))
            });
        }
        Ok(albums)
    }

    /// По папкам — по пути, по тегам — по диску и номеру.
    pub fn album_tracks(&self, album: &Album) -> Result<Vec<Track>, LibraryError> {
        let grouping = album.grouping;
        let order = match grouping {
            Grouping::Folders | Grouping::PerRoot => "t.rel_path",
            Grouping::Tags => "t.disc_no, t.track_no, t.rel_path",
            Grouping::Composers => "t.album, t.disc_no, t.movement_no, t.track_no, t.rel_path",
        };
        let sql = format!(
            "SELECT {TRACK_COLUMNS} FROM tracks t JOIN roots r ON r.id = t.root_id
             WHERE t.missing = 0 AND {} IS ?1 AND {} IN ({})
             ORDER BY {order}",
            grouping.album(),
            grouping.owner(),
            placeholders(2, album.owners.len())
        );
        let mut values = vec![Value::from(album.title.clone())];
        values.extend(album.owners.iter().cloned().map(Value::from));
        let mut tracks = self.query(&sql, params_from_iter(values))?;
        if grouping == Grouping::Folders {
            tracks.sort_by(|a, b| natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy()));
        }
        Ok(tracks)
    }

    /// Альбом, к которому относится трек.
    pub fn album_of(&self, track: &Track, grouping: Grouping) -> Result<Option<Album>, LibraryError> {
        // У каждой папки своё — подписи трека берутся так, как разложена его папка.
        let fields = match grouping {
            Grouping::PerRoot => self.track_grouping(track.id)?,
            other => other,
        };
        let (name, title) = match fields {
            Grouping::PerRoot => (&track.folder_artist, &track.folder_album),
            Grouping::Folders => (&track.folder_artist, &track.folder_album),
            Grouping::Tags => (&track.owner, &track.album),
            Grouping::Composers => (&track.composer, if track.work.is_some() { &track.work } else { &track.album }),
        };
        let name = name.clone().unwrap_or_default();
        let owner = Artist {
            grouping,
            name: name.clone(),
            names: vec![name],
            albums: 0,
            tracks: 0,
        };
        Ok(self.albums(&owner)?.into_iter().find(|album| &album.title == title))
    }

    /// Как разложена папка музыки, из которой трек.
    pub fn track_grouping(&self, track: i64) -> Result<Grouping, LibraryError> {
        let code: Option<String> = self
            .connection
            .query_row(&format!("SELECT {ROOT_GROUPING} FROM tracks t WHERE t.id = ?1"), [track], |row| row.get(0))
            .optional()?
            .flatten();
        Ok(Grouping::from_code(code.as_deref()))
    }
}

/// По папкам — предок с именем альбома (треки бывают в CD 01), по тегам — папка трека.
pub(crate) fn album_folder(grouping: Grouping, first_track: &Path, title: Option<&str>) -> PathBuf {
    let parent = first_track.parent().unwrap_or(first_track);
    if matches!(grouping, Grouping::Folders | Grouping::PerRoot)
        && let Some(title) = title
        && let Some(folder) = parent
            .ancestors()
            .find(|folder| folder.file_name().is_some_and(|name| name.to_string_lossy() == title))
    {
        return folder.to_path_buf();
    }
    parent.to_path_buf()
}

/// Ближайшая папка с именем исполнителя — сам альбом или выше.
pub fn artist_folder(name: &str, album_folder: &Path) -> Option<PathBuf> {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return None;
    }
    album_folder
        .ancestors()
        .find(|folder| folder.file_name().is_some_and(|folder| folder.to_string_lossy().to_lowercase() == name))
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::{artist_folder, natural_cmp};
    use std::path::Path;

    #[test]
    fn artist_folder_is_the_nearest_folder_with_the_name() {
        let album = Path::new("/music/Кино/Группа крови/CD 1");
        assert_eq!(artist_folder("кино", album).as_deref(), Some(Path::new("/music/Кино")));
        assert_eq!(artist_folder("Аквариум", album), None);
        assert_eq!(artist_folder("", album), None);
    }
    use std::cmp::Ordering;

    #[test]
    fn natural_order_matches_explorer() {
        let mut names = vec!["10. Десять", "2. два", "1. Один", "CD 10", "cd 2", "Альбом", "album"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["1. Один", "2. два", "10. Десять", "album", "cd 2", "CD 10", "Альбом"]);
        assert_eq!(natural_cmp("track 007", "track 7"), "track 007".cmp("track 7"));
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
    }
}
