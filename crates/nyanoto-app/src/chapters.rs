//! Главы длинных записей: из тегов `CHAPTER001=00:00:00.000` +
//! `CHAPTER001NAME=…` (так их пишут для аудиокниг в FLAC и Ogg) или из
//! `.cue` рядом с файлом.

use std::path::Path;

use nyanoto_library::{decode_text, read_fields};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub at_ms: i64,
    pub name: String,
}

/// Главы файла по порядку. Пустой список, если глав меньше двух.
pub fn chapters(path: &Path) -> Vec<Chapter> {
    let mut found = from_tags(path);
    if found.len() < 2 {
        found = from_cue(path);
    }
    if found.len() < 2 {
        return Vec::new();
    }
    found.sort_by_key(|chapter| chapter.at_ms);
    found
}

fn from_tags(path: &Path) -> Vec<Chapter> {
    let Ok(tags) = read_fields(path) else {
        return Vec::new();
    };
    let mut chapters: Vec<(String, Chapter)> = Vec::new();
    for field in &tags.fields {
        let key = field.key.to_ascii_uppercase();
        let Some(rest) = key.strip_prefix("CHAPTER") else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        let tail = &rest[digits.len()..];
        let entry = match chapters.iter().position(|(number, _)| *number == digits) {
            Some(index) => &mut chapters[index].1,
            None => {
                chapters.push((digits.clone(), Chapter { at_ms: -1, name: String::new() }));
                &mut chapters.last_mut().expect("только что добавлена").1
            }
        };
        match tail {
            "" => entry.at_ms = parse_clock(&field.value).unwrap_or(-1),
            "NAME" => entry.name = field.value.trim().to_string(),
            _ => {}
        }
    }
    chapters
        .into_iter()
        .map(|(number, mut chapter)| {
            if chapter.name.is_empty() {
                chapter.name = number.trim_start_matches('0').to_string();
            }
            chapter
        })
        .filter(|chapter| chapter.at_ms >= 0)
        .collect()
}

/// `ЧЧ:ММ:СС.ммм`, `ММ:СС` или `СС.ммм` в миллисекунды.
fn parse_clock(text: &str) -> Option<i64> {
    let (whole, fraction) = text.trim().split_once('.').unwrap_or((text.trim(), ""));
    let mut seconds = 0i64;
    for part in whole.split(':') {
        seconds = seconds * 60 + part.trim().parse::<i64>().ok()?;
    }
    let digits: String = fraction.chars().take(3).collect();
    let millis = if digits.is_empty() {
        0
    } else {
        digits.parse::<i64>().ok()? * 10i64.pow(3 - digits.len() as u32)
    };
    Some(seconds * 1000 + millis)
}

fn from_cue(path: &Path) -> Vec<Chapter> {
    let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let candidates = [path.with_extension("cue"), path.with_extension("CUE")];
    for cue in candidates {
        if let Ok(bytes) = std::fs::read(&cue) {
            let found = parse_cue(&decode_text(&bytes).0, &name);
            if !found.is_empty() {
                return found;
            }
        }
    }
    Vec::new()
}

/// Дорожки CUE, относящиеся к файлу `file_name`. Если в CUE один файл,
/// его имя не сверяется: при переименовании CUE часто остаётся старым.
pub fn parse_cue(text: &str, file_name: &str) -> Vec<Chapter> {
    let files = text
        .lines()
        .filter(|line| line.trim_start().to_ascii_uppercase().starts_with("FILE "))
        .count();
    let mut current_file_matches = files <= 1;
    let mut chapters = Vec::new();
    let mut title: Option<String> = None;
    let mut in_track = false;
    for line in text.lines() {
        let line = line.trim();
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("FILE ") {
            current_file_matches =
                files <= 1 || quoted(line).is_some_and(|quoted| quoted.eq_ignore_ascii_case(file_name));
            in_track = false;
        } else if upper.starts_with("TRACK ") {
            in_track = true;
            title = None;
        } else if upper.starts_with("TITLE ") && in_track {
            title = quoted(line).map(str::to_string);
        } else if upper.starts_with("INDEX 01 ")
            && in_track
            && current_file_matches
            && let Some(at_ms) = cue_time(line.get(9..).unwrap_or_default().trim())
        {
            let name = title.clone().unwrap_or_else(|| (chapters.len() + 1).to_string());
            chapters.push(Chapter { at_ms, name });
        }
    }
    chapters
}

fn quoted(line: &str) -> Option<&str> {
    let start = line.find('"')? + 1;
    let end = line[start..].rfind('"')? + start;
    Some(&line[start..end])
}

/// `ММ:СС:КК`, где КК — кадры CD, 75 в секунде.
fn cue_time(text: &str) -> Option<i64> {
    let mut parts = text.split(':').map(|part| part.trim().parse::<i64>().ok());
    let (minutes, seconds, frames) = (parts.next()??, parts.next()??, parts.next()??);
    Some((minutes * 60 + seconds) * 1000 + frames * 1000 / 75)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(parse_clock("00:00:00.000"), Some(0));
        assert_eq!(parse_clock("01:02:03.5"), Some(3_723_500));
        assert_eq!(parse_clock("02:03"), Some(123_000));
        assert_eq!(parse_clock("глава"), None);
        assert_eq!(cue_time("03:20:15"), Some(200_200));
    }

    #[test]
    fn cue_sheet_for_one_file_and_for_several() {
        let single = "REM GENRE Audiobook\nTITLE \"Книга\"\nFILE \"old name.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Пролог\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Глава 1\"\n    INDEX 00 05:59:00\n    INDEX 01 06:00:00\n";
        let found = parse_cue(single, "book.flac");
        assert_eq!(
            found,
            [
                Chapter { at_ms: 0, name: "Пролог".into() },
                Chapter { at_ms: 360_000, name: "Глава 1".into() }
            ]
        );

        let several = "FILE \"a.flac\" WAVE\n TRACK 01 AUDIO\n  INDEX 01 00:00:00\nFILE \"b.flac\" WAVE\n TRACK 02 AUDIO\n  TITLE \"Вторая\"\n  INDEX 01 00:00:00\n TRACK 03 AUDIO\n  TITLE \"Третья\"\n  INDEX 01 01:00:00\n";
        let names: Vec<String> = parse_cue(several, "B.FLAC").into_iter().map(|chapter| chapter.name).collect();
        assert_eq!(names, ["Вторая", "Третья"]);
    }
}
