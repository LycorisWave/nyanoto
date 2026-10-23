//! Теги своих форматов: ReplayGain и всё для библиотеки, если lofty не знает (TTA, DSF, DFF).
//! APEv2 в конце файла, ID3v2 — перед TTA и внутри DSF/DFF.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use symphonia::core::meta::{MetadataBuilder, StandardTagKey, Tag, Value};

use crate::source::ReplayGain;

/// Больше не читаем — там картинки.
const MAX_TAG: u64 = 1 << 20;

/// Длина тега ID3v2 вместе с заголовком по его первым десяти байтам.
pub(super) fn id3v2_size(head: &[u8; 10]) -> Option<u64> {
    if &head[..3] != b"ID3" || head[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    let body = head[6..10].iter().fold(0u64, |size, byte| (size << 7) | u64::from(*byte));
    // Флаг footer: ещё десять байт после тега.
    let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + body + footer)
}

/// ReplayGain из тега APEv2 в конце файла и тега ID3v2 по смещению `id3v2`.
pub(super) fn replay_gain(file: &mut File, id3v2: Option<u64>) -> ReplayGain {
    let mut gain = ReplayGain::default();
    gain.read(&all(file, id3v2));
    gain
}

/// Все текстовые теги: APEv2 в конце файла, затем ID3v2 по смещению `id3v2`.
pub(super) fn all(file: &mut File, id3v2: Option<u64>) -> Vec<Tag> {
    let mut found = ape(file);
    if let Some(offset) = id3v2 {
        found.extend(read_id3v2(file, offset));
    }
    found
}

/// Стандартный смысл поля APEv2 по его имени (регистр не важен).
fn ape_key(key: &str) -> Option<StandardTagKey> {
    use StandardTagKey as K;
    Some(match key.to_ascii_lowercase().as_str() {
        "title" => K::TrackTitle,
        "artist" => K::Artist,
        "album" => K::Album,
        "album artist" | "albumartist" => K::AlbumArtist,
        "year" | "date" => K::Date,
        "track" | "tracknumber" => K::TrackNumber,
        "disc" | "disk" | "discnumber" => K::DiscNumber,
        "genre" => K::Genre,
        "composer" => K::Composer,
        "conductor" => K::Conductor,
        "comment" => K::Comment,
        "lyrics" | "unsyncedlyrics" => K::Lyrics,
        "titlesort" => K::SortTrackTitle,
        "artistsort" => K::SortArtist,
        "albumsort" => K::SortAlbum,
        "albumartistsort" => K::SortAlbumArtist,
        "replaygain_track_gain" => K::ReplayGainTrackGain,
        "replaygain_track_peak" => K::ReplayGainTrackPeak,
        "replaygain_album_gain" => K::ReplayGainAlbumGain,
        "replaygain_album_peak" => K::ReplayGainAlbumPeak,
        _ => return None,
    })
}

/// Текстовые поля тега APEv2 в конце файла (перед ним может стоять ID3v1).
pub(super) fn ape(file: &mut File) -> Vec<Tag> {
    read_ape(file).unwrap_or_default()
}

fn read_ape(file: &mut File) -> Option<Vec<Tag>> {
    let length = file.seek(SeekFrom::End(0)).ok()?;
    for footer_at in [length.checked_sub(32), length.checked_sub(32 + 128)].into_iter().flatten() {
        let mut footer = [0u8; 32];
        file.seek(SeekFrom::Start(footer_at)).ok()?;
        file.read_exact(&mut footer).ok()?;
        if &footer[..8] != b"APETAGEX" {
            continue;
        }
        let word = |at: usize| u32::from_le_bytes([footer[at], footer[at + 1], footer[at + 2], footer[at + 3]]);
        // Размер считает поля и подвал, но не заголовок.
        let size = u64::from(word(12));
        let count = word(16);
        if !(32..=MAX_TAG).contains(&size) || size > footer_at + 32 {
            return None;
        }
        let mut items = vec![0u8; (size - 32) as usize];
        file.seek(SeekFrom::Start(footer_at + 32 - size)).ok()?;
        file.read_exact(&mut items).ok()?;
        return Some(ape_items(&items, count));
    }
    None
}

/// Разбирает поля тега APEv2: длина значения, флаги, имя до нуля, значение.
fn ape_items(mut data: &[u8], count: u32) -> Vec<Tag> {
    let mut tags = Vec::new();
    for _ in 0..count {
        if data.len() < 9 {
            break;
        }
        let length = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let flags = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let Some(end) = data[8..].iter().position(|byte| *byte == 0) else {
            break;
        };
        let key = String::from_utf8_lossy(&data[8..8 + end]).into_owned();
        let value_at = 8 + end + 1;
        let Some(value) = data.get(value_at..value_at + length) else {
            break;
        };
        // Тип 0 — UTF-8, двоичное мимо. Значения через ноль — каждое отдельно.
        if (flags >> 1) & 3 == 0 {
            let text = String::from_utf8_lossy(value);
            for part in text.split('\0').filter(|part| !part.is_empty()) {
                tags.push(Tag::new(ape_key(&key), &key, Value::String(part.to_string())));
            }
        }
        data = &data[value_at + length..];
    }
    tags
}

fn read_id3v2(file: &mut File, offset: u64) -> Vec<Tag> {
    let mut head = [0u8; 10];
    let bytes = file
        .seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(&mut head))
        .ok()
        .and_then(|_| id3v2_size(&head))
        .filter(|size| *size <= MAX_TAG)
        .and_then(|size| {
            let mut bytes = vec![0u8; size as usize];
            bytes[..10].copy_from_slice(&head);
            file.read_exact(&mut bytes[10..]).ok().map(|_| bytes)
        });
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut builder = MetadataBuilder::new();
    let mut reader = symphonia::core::io::BufReader::new(&bytes);
    if symphonia_metadata::id3v2::read_id3v2(&mut reader, &mut builder).is_err() {
        return Vec::new();
    }
    builder.metadata().tags().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ape_tag(items: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (key, value) in items {
            body.extend_from_slice(&(value.len() as u32).to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(key.as_bytes());
            body.push(0);
            body.extend_from_slice(value.as_bytes());
        }
        let mut footer = b"APETAGEX".to_vec();
        footer.extend_from_slice(&2000u32.to_le_bytes());
        footer.extend_from_slice(&((body.len() + 32) as u32).to_le_bytes());
        footer.extend_from_slice(&(items.len() as u32).to_le_bytes());
        footer.extend_from_slice(&0u32.to_le_bytes());
        footer.extend_from_slice(&[0; 8]);
        body.extend_from_slice(&footer);
        body
    }

    #[test]
    fn replay_gain_comes_from_the_ape_tag_even_before_id3v1() {
        let path = std::env::temp_dir().join(format!("nyanoto-apetag-{}", std::process::id()));
        let mut data = vec![0u8; 100];
        data.extend(ape_tag(&[("Artist", "x"), ("REPLAYGAIN_TRACK_GAIN", "-7.25 dB"), ("replaygain_album_peak", "0.98")]));
        let mut id3v1 = b"TAG".to_vec();
        id3v1.resize(128, 0);
        data.extend(id3v1);
        std::fs::write(&path, &data).unwrap();
        let gain = replay_gain(&mut File::open(&path).unwrap(), None);
        assert_eq!(gain.track_gain_db, Some(-7.25));
        assert_eq!(gain.album_peak, Some(0.98));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn id3v2_size_counts_header_and_footer() {
        assert_eq!(id3v2_size(b"ID3\x04\x00\x00\x00\x00\x02\x01"), Some(10 + 257));
        assert_eq!(id3v2_size(b"ID3\x04\x00\x10\x00\x00\x00\x05"), Some(25));
        assert_eq!(id3v2_size(b"ID3\x04\x00\x00\x00\x80\x00\x00"), None);
    }
}
