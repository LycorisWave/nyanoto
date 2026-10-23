//! Теги и свойства файла. Только заголовки: звук не декодируем, обложки мимо памяти.
//! Ошибка разбора не стопорит скан — файл попадает с пометкой.

use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::flac::FlacFile;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag};

/// Разделитель значений многозначного тега в строке для показа.
const JOINER: &str = "; ";

#[derive(Debug, Default)]
pub(crate) struct Tags {
    pub format: Option<String>,
    pub duration_ms: Option<i64>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub bitrate: Option<u32>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub artists: Vec<String>,
    pub album_artists: Vec<String>,
    pub genres: Vec<String>,
    pub composers: Vec<String>,
    pub conductor: Option<String>,
    pub work: Option<String>,
    pub movement: Option<String>,
    pub movement_no: Option<u32>,
    pub movement_total: Option<u32>,
    pub year: Option<u16>,
    pub track_no: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_no: Option<u32>,
    pub disc_total: Option<u32>,
    pub sort_title: Option<String>,
    pub sort_artist: Option<String>,
    pub sort_album: Option<String>,
    pub sort_album_artist: Option<String>,
    pub problem: Option<String>,
    /// Есть ли в тегах текст песни (обычный или синхронный).
    pub lyrics: bool,
}

impl Tags {
    pub fn artist(&self) -> Option<String> {
        joined(&self.artists)
    }

    pub fn album_artist(&self) -> Option<String> {
        joined(&self.album_artists)
    }

    pub fn genre(&self) -> Option<String> {
        joined(&self.genres)
    }

    pub fn composer(&self) -> Option<String> {
        joined(&self.composers)
    }

    /// Первый исполнитель альбома, а без него — трека.
    pub fn owner(&self) -> Option<&str> {
        self.album_artists
            .first()
            .or(self.artists.first())
            .map(String::as_str)
    }
}

impl Tags {
    fn fix_encoding(&mut self) {
        let fix = |value: &mut String| {
            if let Some(fixed) = crate::text::fix_mojibake(value) {
                *value = fixed;
            }
        };
        for value in [
            &mut self.title,
            &mut self.album,
            &mut self.conductor,
            &mut self.work,
            &mut self.movement,
            &mut self.sort_title,
            &mut self.sort_artist,
            &mut self.sort_album,
            &mut self.sort_album_artist,
        ]
        .into_iter()
        .flatten()
        {
            fix(value);
        }
        for list in [&mut self.artists, &mut self.album_artists, &mut self.genres, &mut self.composers] {
            list.iter_mut().for_each(fix);
        }
    }
}

fn joined(values: &[String]) -> Option<String> {
    (!values.is_empty()).then(|| values.join(JOINER))
}

/// Без ошибок: причина — в [`Tags::problem`]. `fix_encoding` чинит кривую кириллицу.
pub(crate) fn read(path: &Path, fix_encoding: bool) -> Tags {
    let outcome = std::panic::catch_unwind(|| parse(path));
    match outcome {
        Ok(Ok(mut tags)) => {
            if fix_encoding {
                tags.fix_encoding();
            }
            tags
        }
        Ok(Err(problem)) => Tags {
            problem: Some(problem),
            ..Tags::default()
        },
        Err(_) => Tags {
            problem: Some("сбой при разборе тегов".into()),
            ..Tags::default()
        },
    }
}

fn parse(path: &Path) -> Result<Tags, String> {
    // Matroska и CAF разбирает Symphonia, TTA и DSD — ядро.
    let mut tags =
        parse_lofty(path).or_else(|problem| crate::probe::probe(path).map(|probed| probed.tags).ok_or(problem))?;
    // lofty врёт про WavPack: DSD — «8 бит», гибрид без .wvc — «без потерь». Правду знает ядро.
    if tags.format.as_deref() == Some("wavpack")
        && let Some(probe) = nyanoto_core::probe(path)
    {
        match probe.format.as_str() {
            "dsd" => {
                tags.format = Some("dsd".into());
                tags.sample_rate = Some(probe.sample_rate);
                tags.bit_depth = Some(1);
            }
            "wavpack-lossy" => tags.format = Some("wavpack-lossy".into()),
            _ => {}
        }
    }
    Ok(tags)
}

fn parse_lofty(path: &Path) -> Result<Tags, String> {
    // Расширение — только подсказка: формат уточняется по содержимому.
    let file = Probe::open(path)
        .map_err(|error| error.to_string())?
        .options(ParseOptions::new().read_cover_art(false))
        .guess_file_type()
        .map_err(|error| error.to_string())?
        .read()
        .map_err(|error| error.to_string())?;

    let properties = file.properties();
    let mut tags = Tags {
        format: Some(format!("{:?}", file.file_type()).to_lowercase()),
        duration_ms: i64::try_from(properties.duration().as_millis()).ok(),
        sample_rate: properties.sample_rate(),
        bit_depth: properties.bit_depth(),
        channels: properties.channels(),
        bitrate: properties.audio_bitrate(),
        ..Tags::default()
    };

    // Основной для формата тег важнее остальных (скажем, ID3v2 важнее ID3v1).
    let ordered = file.primary_tag().into_iter().chain(file.tags());
    for tag in ordered {
        fill(&mut tags, tag);
    }
    Ok(tags)
}

/// Дополняет `tags` значениями из `tag`, не трогая уже заполненное.
fn fill(tags: &mut Tags, tag: &Tag) {
    let text = |key: ItemKey| tag.get_string(key).and_then(clean);
    let list = |key: ItemKey| -> Vec<String> { tag.get_strings(key).filter_map(clean).collect() };

    // LYRICS (Vorbis, MP4) и UNSYNCEDLYRICS / USLT (ID3) — обычный и синхронный текст.
    tags.lyrics = tags.lyrics || text(ItemKey::Lyrics).is_some() || text(ItemKey::UnsyncLyrics).is_some();
    tags.title = tags.title.take().or_else(|| text(ItemKey::TrackTitle));
    tags.album = tags.album.take().or_else(|| text(ItemKey::AlbumTitle));
    if tags.artists.is_empty() {
        // Отдельный тег со списком исполнителей точнее строки для показа.
        tags.artists = list(ItemKey::TrackArtists);
        if tags.artists.is_empty() {
            tags.artists = list(ItemKey::TrackArtist);
        }
    }
    if tags.album_artists.is_empty() {
        tags.album_artists = list(ItemKey::AlbumArtist);
    }
    if tags.genres.is_empty() {
        tags.genres = list(ItemKey::Genre);
    }
    if tags.composers.is_empty() {
        tags.composers = list(ItemKey::Composer);
    }
    tags.conductor = tags.conductor.take().or_else(|| text(ItemKey::Conductor));
    tags.work = tags.work.take().or_else(|| text(ItemKey::Work));
    tags.movement = tags.movement.take().or_else(|| text(ItemKey::Movement));
    // Номер части бывает записан как «2/4».
    let number = |key: ItemKey| {
        tag.get_string(key)
            .and_then(|value| value.split('/').next().and_then(|first| first.trim().parse().ok()))
    };
    tags.movement_no = tags.movement_no.or_else(|| number(ItemKey::MovementNumber));
    tags.movement_total = tags.movement_total.or_else(|| number(ItemKey::MovementTotal));
    tags.year = tags.year.or_else(|| tag.date().map(|date| date.year));
    tags.track_no = tags.track_no.or_else(|| tag.track());
    tags.track_total = tags.track_total.or_else(|| tag.track_total());
    tags.disc_no = tags.disc_no.or_else(|| tag.disk());
    tags.disc_total = tags.disc_total.or_else(|| tag.disk_total());
    tags.sort_title = tags.sort_title.take().or_else(|| text(ItemKey::TrackTitleSortOrder));
    tags.sort_artist = tags.sort_artist.take().or_else(|| text(ItemKey::TrackArtistSortOrder));
    tags.sort_album = tags.sort_album.take().or_else(|| text(ItemKey::AlbumTitleSortOrder));
    tags.sort_album_artist = tags
        .sort_album_artist
        .take()
        .or_else(|| text(ItemKey::AlbumArtistSortOrder));
}

/// Убирает пробелы по краям и отбрасывает пустые значения.
fn clean(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('\0').trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Одно поле тега: ключ в том виде, в каком он записан в файле, и значение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagField {
    pub key: String,
    pub value: String,
}

/// Все текстовые теги файла.
#[derive(Debug, Clone)]
pub struct FileTags {
    pub fields: Vec<TagField>,
    /// Пока только FLAC: незнакомые поля не теряются.
    pub editable: bool,
    /// Вид тега, например «Vorbis comments» или «Id3v2»; пусто, если тегов нет.
    pub kind: String,
}

/// Читает все текстовые теги файла.
pub fn read_fields(path: &Path) -> Result<FileTags, String> {
    std::panic::catch_unwind(|| read_fields_inner(path))
        .unwrap_or_else(|_| Err("сбой при разборе тегов".into()))
}

fn read_fields_inner(path: &Path) -> Result<FileTags, String> {
    let options = ParseOptions::new().read_cover_art(false);
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if let Ok(flac) = FlacFile::read_from(&mut file, options) {
        let fields = flac
            .vorbis_comments()
            .map(|comments| {
                comments
                    .items()
                    .map(|(key, value)| TagField { key: key.to_string(), value: value.to_string() })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(FileTags { fields, editable: true, kind: "Vorbis comments".into() });
    }

    let read = Probe::open(path)
        .map_err(|error| error.to_string())
        .and_then(|probe| probe.options(options).guess_file_type().map_err(|error| error.to_string()))
        .and_then(|probe| probe.read().map_err(|error| error.to_string()));
    let file = match read {
        Ok(file) => file,
        Err(problem) => {
            let probed = crate::probe::probe(path).ok_or(problem)?;
            let kind = if probed.fields.is_empty() { String::new() } else { "Symphonia".into() };
            return Ok(FileTags { fields: probed.fields, editable: false, kind });
        }
    };
    let Some(tag) = file.primary_tag().or(file.first_tag()) else {
        return Ok(FileTags { fields: Vec::new(), editable: false, kind: String::new() });
    };
    let fields = tag
        .items()
        .filter_map(|item| {
            let value = match item.value() {
                ItemValue::Text(text) | ItemValue::Locator(text) => text.clone(),
                ItemValue::Binary(_) => return None,
            };
            let key = item
                .key()
                .map_key(tag.tag_type())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{:?}", item.key()));
            Some(TagField { key, value })
        })
        .collect();
    Ok(FileTags { fields, editable: false, kind: format!("{:?}", tag.tag_type()) })
}

/// Текстовые поля FLAC заменяются на `fields`, обложки и звук не трогаем. Через временную копию.
pub fn write_fields(path: &Path, fields: &[TagField]) -> Result<(), String> {
    for field in fields {
        let key = field.key.trim();
        // Имя поля Vorbis: печатные символы ASCII без знака «=».
        if key.is_empty() || !key.bytes().all(|byte| (0x20..=0x7D).contains(&byte) && byte != b'=') {
            return Err(format!("недопустимое имя тега: «{}»", field.key));
        }
    }
    std::panic::catch_unwind(|| write_fields_inner(path, fields))
        .unwrap_or_else(|_| Err("сбой при записи тегов".into()))
}

fn write_fields_inner(path: &Path, fields: &[TagField]) -> Result<(), String> {
    let mut source = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut flac = FlacFile::read_from(&mut source, ParseOptions::new())
        .map_err(|_| "теги можно править только у FLAC".to_string())?;
    drop(source);

    let mut comments = flac.vorbis_comments().cloned().unwrap_or_default();
    comments.take_items().for_each(drop);
    for field in fields {
        comments.push(field.key.trim().to_uppercase(), field.value.clone());
    }
    flac.set_vorbis_comments(comments);

    let temporary = path.with_extension("nyanoto-tmp");
    let written = std::fs::copy(path, &temporary)
        .map_err(|error| error.to_string())
        .and_then(|_| {
            flac.save_to_path(&temporary, WriteOptions::default())
                .map_err(|error| error.to_string())
        })
        .and_then(|()| std::fs::rename(&temporary, path).map_err(|error| error.to_string()));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Теги и обложки в результат конвертации: FLAC — Vorbis, WAV — ID3v2.
pub fn copy_tags(source: &Path, target: &Path) -> Result<(), String> {
    std::panic::catch_unwind(|| copy_tags_inner(source, target))
        .unwrap_or_else(|_| Err("сбой при переносе тегов".into()))
}

fn copy_tags_inner(source: &Path, target: &Path) -> Result<(), String> {
    use lofty::tag::{TagExt, TagType};
    let tagged = lofty::read_from_path(source).map_err(|error| error.to_string())?;
    let Some(tag) = tagged.primary_tag().or(tagged.first_tag()) else {
        return Ok(());
    };
    let kind = match target.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("flac") => TagType::VorbisComments,
        _ => TagType::Id3v2,
    };
    let mut tag = tag.clone();
    tag.re_map(kind);
    tag.save_to_path(target, WriteOptions::default()).map_err(|error| error.to_string())
}
