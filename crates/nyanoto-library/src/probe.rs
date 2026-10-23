//! Сведения о том, что не разбирает lofty: Matroska, CAF и т. п. (Symphonia), TTA и DSD (ядро). Только заголовки.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::{CODEC_TYPE_NULL, CodecType};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Tag as SymTag};
use symphonia::core::probe::Hint;

use crate::tags::{TagField, Tags};

/// Что удалось узнать о файле: свойства звука и теги в том виде, как они записаны.
pub(crate) struct Probed {
    pub tags: Tags,
    pub fields: Vec<TagField>,
}

pub(crate) fn probe(path: &Path) -> Option<Probed> {
    symphonia_probe(path).or_else(|| native(path))
}

/// Форматы, которые декодирует само ядро плеера.
fn native(path: &Path) -> Option<Probed> {
    let probe = nyanoto_core::probe(path)?;
    let size = std::fs::metadata(path).ok()?.len();
    let duration_ms = i64::try_from(probe.duration.as_millis()).ok();
    let mut tags = Tags {
        format: Some(probe.format.to_string()),
        duration_ms,
        sample_rate: Some(probe.sample_rate),
        bit_depth: probe.bits.and_then(|bits| u8::try_from(bits).ok()),
        channels: u8::try_from(probe.channels).ok(),
        bitrate: duration_ms.filter(|ms| *ms > 0).and_then(|ms| u32::try_from(size * 8 / ms as u64).ok()),
        ..Tags::default()
    };
    for tag in &probe.tags {
        fill(&mut tags, tag);
    }
    let fields = probe.tags.iter().map(|tag| TagField { key: tag.key.clone(), value: tag.value.to_string() }).collect();
    Some(Probed { tags, fields })
}

fn symphonia_probe(path: &Path) -> Option<Probed> {
    let file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let mut probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    let mut found: Vec<SymTag> = Vec::new();
    if let Some(revision) = probed.metadata.get().as_ref().and_then(|metadata| metadata.current()) {
        found.extend(revision.tags().iter().cloned());
    }
    if let Some(revision) = probed.format.metadata().current() {
        found.extend(revision.tags().iter().cloned());
    }
    let track = probed.format.tracks().iter().find(|track| track.codec_params.codec != CODEC_TYPE_NULL)?;
    let params = &track.codec_params;

    let duration_ms = params.n_frames.zip(params.time_base).map(|(frames, base)| {
        let time = base.calc_time(frames);
        time.seconds as i64 * 1000 + (time.frac * 1000.0) as i64
    });
    let mut tags = Tags {
        format: Some(codec_name(params.codec).to_string()),
        duration_ms,
        sample_rate: params.sample_rate,
        bit_depth: params.bits_per_sample.and_then(|bits| u8::try_from(bits).ok()),
        // Matroska не всегда сообщает каналы; у Opus они есть в заголовке OpusHead.
        channels: params
            .channels
            .and_then(|channels| u8::try_from(channels.count()).ok())
            .or_else(|| params.extra_data.as_deref().filter(|head| head.starts_with(b"OpusHead")).and_then(|head| head.get(9).copied())),
        // Средний битрейт по размеру файла, кбит/с.
        bitrate: duration_ms
            .filter(|ms| *ms > 0)
            .and_then(|ms| u32::try_from(size * 8 / ms as u64).ok()),
        ..Tags::default()
    };
    for tag in &found {
        fill(&mut tags, tag);
    }
    let fields = found
        .iter()
        .map(|tag| TagField { key: tag.key.clone(), value: tag.value.to_string() })
        .collect();
    Some(Probed { tags, fields })
}

/// Имя формата так же, как его называет lofty: по нему строится подпись качества.
fn codec_name(codec: CodecType) -> &'static str {
    use symphonia::core::codecs::*;
    match codec {
        CODEC_TYPE_OPUS => "opus",
        CODEC_TYPE_VORBIS => "vorbis",
        CODEC_TYPE_FLAC => "flac",
        CODEC_TYPE_MP3 | CODEC_TYPE_MP2 | CODEC_TYPE_MP1 => "mpeg",
        CODEC_TYPE_AAC => "aac",
        CODEC_TYPE_ALAC => "alac",
        codec => match symphonia::default::get_codecs().get_codec(codec) {
            Some(descriptor) if descriptor.short_name.starts_with("pcm") => "pcm",
            _ => "?",
        },
    }
}

fn fill(tags: &mut Tags, tag: &SymTag) {
    let value = tag.value.to_string();
    let Some(text) = Some(value.trim().trim_matches('\0').trim().to_string()).filter(|text| !text.is_empty()) else {
        return;
    };
    let number = || text.split('/').next().and_then(|first| first.trim().parse().ok());
    let set = |slot: &mut Option<String>| {
        if slot.is_none() {
            *slot = Some(text.clone());
        }
    };
    match tag.std_key {
        Some(StandardTagKey::TrackTitle) => set(&mut tags.title),
        Some(StandardTagKey::Album) => set(&mut tags.album),
        Some(StandardTagKey::Artist) => tags.artists.push(text.clone()),
        Some(StandardTagKey::AlbumArtist) => tags.album_artists.push(text.clone()),
        Some(StandardTagKey::Genre) => tags.genres.push(text.clone()),
        Some(StandardTagKey::Composer) => tags.composers.push(text.clone()),
        Some(StandardTagKey::Conductor) => set(&mut tags.conductor),
        Some(StandardTagKey::MovementName) => set(&mut tags.movement),
        Some(StandardTagKey::MovementNumber) => tags.movement_no = tags.movement_no.or_else(number),
        Some(StandardTagKey::TrackNumber) => tags.track_no = tags.track_no.or_else(number),
        Some(StandardTagKey::TrackTotal) => tags.track_total = tags.track_total.or_else(number),
        Some(StandardTagKey::DiscNumber) => tags.disc_no = tags.disc_no.or_else(number),
        Some(StandardTagKey::DiscTotal) => tags.disc_total = tags.disc_total.or_else(number),
        Some(StandardTagKey::Date | StandardTagKey::ReleaseDate | StandardTagKey::OriginalDate) => {
            tags.year = tags.year.or_else(|| text.get(..4).and_then(|year| year.parse().ok()));
        }
        Some(StandardTagKey::SortTrackTitle) => set(&mut tags.sort_title),
        Some(StandardTagKey::SortArtist) => set(&mut tags.sort_artist),
        Some(StandardTagKey::SortAlbum) => set(&mut tags.sort_album),
        Some(StandardTagKey::SortAlbumArtist) => set(&mut tags.sort_album_artist),
        _ => {}
    }
}
