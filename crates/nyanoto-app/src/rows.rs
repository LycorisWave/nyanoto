//! Строки списков и подписи на выбранном языке.

use std::path::Path;

use nyanoto_library::{Album, Artist, Grouping, Quality, Track};
use slint::SharedString;

use crate::lang::{Lex, Noun};
use crate::{ArtistRow, OptionRow, TrackRow};

pub fn options(lex: &Lex) -> Vec<OptionRow> {
    lex.future
        .iter()
        .map(|(title, detail)| OptionRow { title: (*title).into(), detail: (*detail).into() })
        .collect()
}

pub fn artist_name<'a>(artist: &'a Artist, lex: &'static Lex) -> &'a str {
    match (artist.name.is_empty(), artist.grouping) {
        (false, _) => &artist.name,
        (true, Grouping::Folders) => lex.unknown_artist_folder,
        (true, Grouping::Tags | Grouping::PerRoot) => lex.unknown_artist_tag,
        (true, Grouping::Composers) => lex.unknown_composer,
    }
}

pub fn artist_row(artist: &Artist, lex: &'static Lex) -> ArtistRow {
    let name = artist_name(artist, lex);
    ArtistRow {
        name: name.into(),
        letter: first_letter(name),
        avatar: Default::default(),
        detail: format!(
            "{} · {}",
            lex.count(artist.albums, Noun::Album),
            lex.count(artist.tracks, Noun::Track)
        )
        .into(),
    }
}

/// Строка списка плейлистов в том же виде, что строка исполнителя.
pub fn playlist_row(name: &str, tracks: u32, lex: &Lex) -> ArtistRow {
    ArtistRow {
        name: name.into(),
        letter: first_letter(name),
        avatar: Default::default(),
        detail: lex.count(tracks, Noun::Track).into(),
    }
}

pub fn first_letter(name: &str) -> SharedString {
    name.chars()
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_default()
        .into()
}

pub fn album_title(album: &Album, lex: &Lex) -> String {
    album.title.clone().unwrap_or_else(|| {
        match album.grouping {
            Grouping::Folders => lex.unknown_album_folder,
            Grouping::Tags | Grouping::PerRoot => lex.unknown_album_tag,
            Grouping::Composers => lex.unknown_work,
        }
        .to_string()
    })
}

pub fn album_detail(album: &Album, lex: &Lex) -> String {
    let mut parts = Vec::new();
    // Альбом из одного трека — сингл.
    if album.tracks == 1 {
        parts.push(lex.single.to_string());
    }
    // По папкам год обычно уже есть в имени папки.
    if album.grouping != Grouping::Folders
        && let Some(year) = album.year
    {
        parts.push(year.to_string());
    }
    parts.push(lex.count(album.tracks, Noun::Track));
    parts.push(duration_words(album.duration_ms.max(0) as u64, lex));
    parts.join(" · ")
}

/// Виды значка качества (цвета — `Badge` в app.slint): Lossy, CD, Hi-Res, DSD, «подделка?», ручной.
pub const BADGE_GOLD: i32 = 0;
pub const BADGE_RED: i32 = 1;
pub const BADGE_GREY: i32 = 2;
pub const BADGE_LOSSY: i32 = 3;
pub const BADGE_CD: i32 = 4;
pub const BADGE_DSD: i32 = 5;

/// DSD, Hi-Res (>16 бит или >48 кГц), CD или Lossy. Нечитаемый — без значка.
pub fn quality_tier(quality: &Quality) -> (&'static str, i32) {
    if quality.dsd {
        ("DSD", BADGE_DSD)
    } else if quality.hires {
        ("Hi-Res", BADGE_GOLD)
    } else if quality.lossless {
        ("CD", BADGE_CD)
    } else if quality.format != "?" {
        ("Lossy", BADGE_LOSSY)
    } else {
        ("", BADGE_LOSSY)
    }
}

pub fn quality_label(quality: &Quality, lex: &Lex) -> String {
    let mut parts = vec![quality.format.to_string()];
    if let Some(bits) = quality.bit_depth {
        parts.push(format!("{bits} {}", lex.bit));
    }
    if let Some(rate) = quality.sample_rate {
        let khz = format!("{:.1}", f64::from(rate) / 1000.0);
        let khz = khz.trim_end_matches(".0").replace('.', &lex.decimal.to_string());
        parts.push(format!("{khz} {}", lex.khz));
    }
    match quality.channels {
        Some(1) => parts.push(lex.mono.to_string()),
        Some(2) => parts.push(lex.stereo.to_string()),
        Some(count) => parts.push(format!("{count} {}", lex.channels_short)),
        None => {}
    }
    if let Some(bitrate) = quality.bitrate {
        parts.push(format!("{bitrate} {}", lex.kbps));
    }
    parts.join(" · ")
}

/// Как подписать трек в каком-то из списков.
pub struct RowContext<'a> {
    pub lex: &'static Lex,
    pub grouping: Grouping,
    /// Папка открытого альбома, если список — это альбом.
    pub album_folder: Option<&'a Path>,
    /// Есть ли в открытом альбоме треки со второго диска и дальше.
    pub multi_disc: bool,
    pub playing: Option<&'a Path>,
    pub liked: &'a dyn Fn(i64) -> bool,
    /// Значок: текст, вид (0 золотой, 1 красный, 2 серый) и можно ли нажать.
    pub badge: &'a dyn Fn(&Track) -> (String, i32, bool),
}

pub fn track_row(track: &Track, context: &RowContext<'_>, extra: Option<String>) -> TrackRow {
    let lex = context.lex;
    let (number, title, mut detail) = match context.grouping {
        // По папкам: имя файла как есть, номер уже в нём.
        Grouping::Folders => {
            let detail = match context.album_folder {
                Some(folder) => track
                    .path
                    .parent()
                    .and_then(|parent| parent.strip_prefix(folder).ok())
                    .map(|inside| inside.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default(),
                None => folders_of(track, lex),
            };
            (String::new(), file_name(&track.path), detail)
        }
        // Списки вперемешку (поиск, очередь, история): как по тегам.
        Grouping::Tags | Grouping::PerRoot => {
            let number = match (context.album_folder, track.track_no) {
                (Some(_), Some(number)) if context.multi_disc => {
                    format!("{}.{number:02}", track.disc_no.unwrap_or(1))
                }
                (Some(_), Some(number)) => number.to_string(),
                _ => String::new(),
            };
            let title = track.title.clone().unwrap_or_else(|| file_name(&track.path));
            let detail = match context.album_folder {
                // В альбоме исполнитель показывается, только если он не исполнитель альбома.
                Some(_) => track
                    .artist
                    .clone()
                    .filter(|artist| track.owner.as_deref().is_none_or(|owner| !artist.eq_ignore_ascii_case(owner)))
                    .unwrap_or_default(),
                None => tags_of(track, lex),
            };
            (number, title, detail)
        }
        // Классика: части по номерам, исполнители — дирижёр и оркестр.
        Grouping::Composers => {
            let number = match (context.album_folder, track.movement_no, track.track_no) {
                (Some(_), Some(movement), _) => roman(movement),
                (Some(_), None, Some(number)) => number.to_string(),
                _ => String::new(),
            };
            let detail = match context.album_folder {
                Some(_) => performers(track),
                None => classical_of(track, lex),
            };
            (number, movement_title(track), detail)
        }
    };
    if let Some(extra) = extra {
        detail = if detail.is_empty() { extra } else { format!("{detail} · {extra}") };
    }
    let (badge, badge_kind, badge_clickable) = (context.badge)(track);
    TrackRow {
        number: number.into(),
        title: title.into(),
        detail: detail.into(),
        duration: track.duration_ms.map(|ms| clock(ms as f32 / 1000.0)).unwrap_or_default().into(),
        playing: context.playing == Some(track.path.as_path()),
        liked: (context.liked)(track.id),
        badge: badge.into(),
        badge_kind,
        badge_clickable,
        lyrics: track.lyrics,
    }
}

/// Папки исполнителя и альбома одной строкой.
pub fn folders_of(track: &Track, lex: &Lex) -> String {
    let artist = track.folder_artist.as_deref().unwrap_or(lex.unknown_artist_folder);
    match track.folder_album.as_deref() {
        Some(album) => format!("{artist} · {album}"),
        None => artist.to_string(),
    }
}

/// Исполнитель и альбом из тегов одной строкой.
pub fn tags_of(track: &Track, lex: &Lex) -> String {
    let artist = track.artist.as_deref().unwrap_or(lex.unknown_artist_tag);
    let album = track.album.as_deref().unwrap_or(lex.unknown_album_tag);
    format!("{artist} · {album}")
}

/// Композитор и произведение одной строкой.
pub fn classical_of(track: &Track, lex: &Lex) -> String {
    let composer = track.composer.as_deref().unwrap_or(lex.unknown_composer);
    let work = track.work.as_deref().or(track.album.as_deref()).unwrap_or(lex.unknown_work);
    format!("{composer} · {work}")
}

/// Название части, а если его нет — название трека или имя файла.
pub fn movement_title(track: &Track) -> String {
    track
        .movement
        .clone()
        .or_else(|| track.title.clone())
        .unwrap_or_else(|| file_name(&track.path))
}

/// Кто исполняет: дирижёр и исполнитель трека.
pub fn performers(track: &Track) -> String {
    [track.conductor.as_deref(), track.artist.as_deref()]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Римская запись номера части: I, II, III, IV…
pub fn roman(mut number: u32) -> String {
    if number == 0 || number >= 4000 {
        return number.to_string();
    }
    const DIGITS: [(u32, &str); 13] = [
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
        (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    let mut text = String::new();
    for (value, digit) in DIGITS {
        while number >= value {
            text.push_str(digit);
            number -= value;
        }
    }
    text
}

pub fn file_name(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Число с разрядами через неразрывный пробел: 12 345.
pub fn count(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::with_capacity(digits.len() * 2);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push('\u{a0}');
        }
        grouped.push(digit);
    }
    grouped
}

pub fn clock(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

pub fn duration_words(ms: u64, lex: &Lex) -> String {
    let minutes = ms / 60_000;
    if minutes >= 60 {
        format!("{} {} {} {}", minutes / 60, lex.hours, minutes % 60, lex.minutes)
    } else {
        format!("{minutes} {}", lex.minutes)
    }
}

/// Дата из секунд Unix в виде «15.10.2026», по местному времени.
pub fn date(unix: i64) -> String {
    calendar(crate::clock::local_day(unix))
}

/// Номер дня от 1970-01-01 → «15.10.2026».
fn calendar(day: i64) -> String {
    // Перевод числа дней в календарную дату по алгоритму Говарда Хиннанта.
    let days = day + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{day:02}.{month:02}.{year}")
}

/// Ошибка файла по-человечески, подробность — от декодера как есть.
pub fn track_error(error: &nyanoto_core::TrackError, lex: &Lex) -> String {
    use nyanoto_core::TrackError;
    match error {
        TrackError::Io(error) => format!("{}: {error}", lex.err_read),
        TrackError::Unsupported(what) => format!("{}: {what}", lex.err_unsupported),
        TrackError::Malformed(what) => format!("{}: {what}", lex.err_malformed),
        TrackError::NoAudioTrack => lex.err_no_audio.to_string(),
        TrackError::Panicked(what) => format!("{}: {what}", lex.err_crash),
        TrackError::Write(error) => format!("{}: {error}", lex.err_write),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{EN, RU};

    #[test]
    fn roman_numbers() {
        let written: Vec<String> = [1, 2, 4, 9, 14, 40, 1994].into_iter().map(roman).collect();
        assert_eq!(written, ["I", "II", "IV", "IX", "XIV", "XL", "MCMXCIV"]);
        assert_eq!(roman(0), "0");
    }

    #[test]
    fn counts_are_grouped() {
        assert_eq!(count(7), "7");
        assert_eq!(count(2_703), "2\u{a0}703");
        assert_eq!(count(500_000), "500\u{a0}000");
        assert_eq!(count(1_234_567), "1\u{a0}234\u{a0}567");
    }

    #[test]
    fn dates_and_clock() {
        assert_eq!(calendar(0), "01.01.1970");
        assert_eq!(calendar(1_791_331_200 / 86_400), "07.10.2026");
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(duration_words(3_900_000, &RU), "1 ч 5 мин");
        assert_eq!(duration_words(3_900_000, &EN), "1 h 5 min");
    }

    #[test]
    fn quality_is_worded_per_language() {
        let quality = Quality {
            format: "FLAC",
            lossless: true,
            hires: true,
            dsd: false,
            bit_depth: Some(24),
            sample_rate: Some(44_100),
            channels: Some(2),
            bitrate: Some(1681),
        };
        assert_eq!(quality_label(&quality, &RU), "FLAC · 24 бит · 44,1 кГц · стерео · 1681 кбит/с");
        assert_eq!(quality_label(&quality, &EN), "FLAC · 24 bit · 44.1 kHz · stereo · 1681 kbps");
    }

    /// Альбом, прослушанный по времени: «0,3 раза», «1 раз», «2 раза».
    #[test]
    fn album_times_are_fractional() {
        assert_eq!(RU.times(0.31), "0,3 раза");
        assert_eq!(RU.times(1.0), "1 раз");
        assert_eq!(RU.times(1.96), "2 раза");
        assert_eq!(RU.times(5.0), "5 раз");
        assert_eq!(EN.times(1.5), "1.5 times");
        assert_eq!(EN.times(1.02), "1 time");
    }

    /// Значок есть у каждого читаемого трека: Lossy, CD, Hi-Res или DSD.
    #[test]
    fn every_quality_has_its_badge() {
        let track = |format: &str, bits: Option<u8>, rate: u32| Track {
            id: 1,
            path: "a".into(),
            title: None,
            artist: None,
            album: None,
            album_artist: None,
            year: None,
            track_no: None,
            disc_no: None,
            duration_ms: None,
            format: Some(format.into()),
            problem: None,
            owner: None,
            folder_artist: None,
            folder_album: None,
            bit_depth: bits,
            sample_rate: Some(rate),
            channels: Some(2),
            bitrate: None,
            genre: None,
            composer: None,
            conductor: None,
            work: None,
            movement: None,
            movement_no: None,
            lyrics: false,
        };
        let tier = |format: &str, bits: Option<u8>, rate: u32| quality_tier(&nyanoto_library::quality(&track(format, bits, rate)));
        assert_eq!(tier("mpeg", None, 44_100), ("Lossy", BADGE_LOSSY));
        assert_eq!(tier("opus", None, 48_000), ("Lossy", BADGE_LOSSY));
        assert_eq!(tier("flac", Some(16), 44_100), ("CD", BADGE_CD));
        assert_eq!(tier("flac", Some(24), 44_100), ("Hi-Res", BADGE_GOLD));
        assert_eq!(tier("flac", Some(16), 96_000), ("Hi-Res", BADGE_GOLD));
        assert_eq!(tier("dsd", Some(1), 2_822_400), ("DSD", BADGE_DSD));
        assert_eq!(tier("wavpack-lossy", Some(16), 44_100), ("Lossy", BADGE_LOSSY));
        assert_eq!(tier("unknown-codec", None, 44_100).0, "");
    }
}
