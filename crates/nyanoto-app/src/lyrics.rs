//! Тексты песен: `.lrc` рядом или теги; без меток — обычный текст.

use std::path::Path;
use std::time::Duration;

use nyanoto_library::read_fields;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Когда строка начинается; `None` у текста без меток времени.
    pub at: Option<Duration>,
    pub text: String,
    /// Время слов из пословных меток LRC, иначе пусто.
    pub words: Vec<Option<Duration>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub synced: bool,
    /// Строка — часть припева: такой же текст встречается в песне ещё раз.
    pub chorus: Vec<bool>,
}

/// Дольше слово не держится — дальше пауза.
const MAX_LINE_SPAN: Duration = Duration::from_secs(8);

impl Lyrics {
    /// Начала слов: метки LRC как есть, иначе время строки делим по длине слов
    /// (последнее — на 85%).
    pub fn word_starts(&self, index: usize) -> Vec<Duration> {
        let Some(line) = self.lines.get(index) else {
            return Vec::new();
        };
        let Some(start) = line.at else {
            return Vec::new();
        };
        let words: Vec<&str> = line.text.split_whitespace().collect();
        if words.is_empty() {
            return Vec::new();
        }
        let next = self.lines[index + 1..].iter().find_map(|line| line.at);
        let span = next.map_or(MAX_LINE_SPAN, |next| next.saturating_sub(start)).min(MAX_LINE_SPAN);
        let weights: Vec<f64> = words.iter().map(|word| word.chars().count() as f64 + 1.0).collect();
        let total: f64 = weights.iter().sum();
        let mut before = 0.0;
        let mut estimated = Vec::with_capacity(words.len());
        for weight in &weights {
            estimated.push(start + span.mul_f64(0.85 * before / total));
            before += weight;
        }
        // Метки важнее оценки, но не раньше предыдущего слова.
        let mut previous = start;
        estimated
            .into_iter()
            .enumerate()
            .map(|(word, guess)| {
                let at = line.words.get(word).copied().flatten().unwrap_or(guess).max(previous);
                previous = at;
                at
            })
            .collect()
    }

    /// Строка, которая звучит в момент `position`.
    pub fn line_at(&self, position: Duration) -> Option<usize> {
        if !self.synced {
            return None;
        }
        self.lines
            .iter()
            .rposition(|line| line.at.is_some_and(|at| at <= position))
    }
}

/// Ищет текст трека: сначала `.lrc` с тем же именем, затем теги.
pub fn load(path: &Path) -> Option<Lyrics> {
    for extension in ["lrc", "LRC"] {
        if let Ok(bytes) = std::fs::read(path.with_extension(extension)) {
            let lyrics = parse(&nyanoto_library::decode_text(&bytes).0);
            if !lyrics.lines.is_empty() {
                return Some(lyrics);
            }
        }
    }
    tag_text(path).map(|text| parse(&text))
}

/// Текст из тегов трека; синхронный предпочтительнее обычного.
pub fn tag_text(path: &Path) -> Option<String> {
    let tags = read_fields(path).ok()?;
    let mut best: Option<(String, bool)> = None;
    for field in tags.fields.into_iter().filter(|field| field.key.to_ascii_uppercase().contains("LYRIC")) {
        let lyrics = parse(&field.value);
        if lyrics.lines.is_empty() {
            continue;
        }
        if best.as_ref().is_none_or(|(_, synced)| !synced && lyrics.synced) {
            best = Some((field.value, lyrics.synced));
        }
    }
    best.map(|(text, _)| text)
}

/// LRC: `[мм:сс.хх]`, несколько меток, `<мм:сс.хх>`, `[offset:±мс]`.
pub fn parse(text: &str) -> Lyrics {
    let mut offset_ms: i64 = 0;
    let mut synced = Vec::new();
    let mut plain = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else {
                break;
            };
            let tag = &inner[..end];
            if let Some(stamp) = timestamp(tag) {
                stamps.push(stamp);
            } else if let Some(value) = tag.strip_prefix("offset:") {
                offset_ms = value.trim().parse().unwrap_or(0);
            } else if !stamps.is_empty() {
                break;
            }
            rest = inner[end + 1..].trim_start();
        }
        let (words, word_stamps) = word_stamps(rest);
        if stamps.is_empty() {
            // Строки служебных тегов вроде [ar:…] без текста пропускаются.
            if !raw.trim_start().starts_with('[') {
                plain.push(words);
            }
            continue;
        }
        // У повторов время слов едет вместе со строкой.
        let first = stamps.first().copied().unwrap_or(0);
        for stamp in stamps {
            let shifted: Vec<Option<i64>> = word_stamps.iter().map(|at| at.map(|at| at - first + stamp)).collect();
            synced.push((stamp, words.clone(), shifted));
        }
    }

    if synced.is_empty() {
        while plain.last().is_some_and(String::is_empty) {
            plain.pop();
        }
        let lines: Vec<Line> = plain.into_iter().map(|text| Line { at: None, text, words: Vec::new() }).collect();
        let chorus = vec![false; lines.len()];
        return Lyrics { lines, synced: false, chorus };
    }
    synced.sort_by_key(|(at, _, _)| *at);
    // Положительный сдвиг по стандарту LRC показывает строки раньше.
    let shift = |at: i64| Duration::from_millis((at - offset_ms).max(0) as u64);
    let lines: Vec<Line> = synced
        .into_iter()
        .map(|(at, text, words)| Line { at: Some(shift(at)), text, words: words.into_iter().map(|at| at.map(shift)).collect() })
        .collect();
    let chorus = chorus_lines(&lines);
    Lyrics { lines, synced: true, chorus }
}

/// Припев — строки, что повторяются. Без анализа звука.
fn chorus_lines(lines: &[Line]) -> Vec<bool> {
    let key = |text: &str| text.to_lowercase().chars().filter(|char| char.is_alphanumeric()).collect::<String>();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for line in lines {
        let key = key(&line.text);
        if key.chars().count() >= 4 {
            *counts.entry(key).or_default() += 1;
        }
    }
    lines.iter().map(|line| counts.get(&key(&line.text)).is_some_and(|count| *count > 1)).collect()
}

/// `мм:сс`, `мм:сс.хх`, `мм:сс.ххх` или `мм:сс:хх` в миллисекунды.
fn timestamp(tag: &str) -> Option<i64> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes: i64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((seconds, fraction)) => (seconds, fraction),
        None => (rest, ""),
    };
    let seconds: i64 = seconds.trim().parse().ok()?;
    if !(0..60).contains(&seconds) || !fraction.chars().all(|char| char.is_ascii_digit()) {
        return None;
    }
    let fraction_ms = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction[..3].parse::<i64>().ok()?,
    };
    Some(minutes * 60_000 + seconds * 1000 + fraction_ms)
}

/// Строка без `<мм:сс.хх>` и время слов: метка — слову после неё.
fn word_stamps(text: &str) -> (String, Vec<Option<i64>>) {
    // Куски текста и метки по порядку.
    let mut pieces: Vec<(Option<i64>, String)> = vec![(None, String::new())];
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        match rest[start..].find('>') {
            Some(end) if let Some(stamp) = timestamp(&rest[start + 1..start + end]) => {
                if let Some((_, text)) = pieces.last_mut() {
                    text.push_str(&rest[..start]);
                }
                pieces.push((Some(stamp), String::new()));
                rest = &rest[start + end + 1..];
            }
            _ => {
                if let Some((_, text)) = pieces.last_mut() {
                    text.push_str(&rest[..=start]);
                }
                rest = &rest[start + 1..];
            }
        }
    }
    if let Some((_, text)) = pieces.last_mut() {
        text.push_str(rest);
    }
    let mut words = Vec::new();
    let mut times = Vec::new();
    for (stamp, piece) in pieces {
        for (index, word) in piece.split_whitespace().enumerate() {
            words.push(word.to_string());
            times.push(if index == 0 { stamp } else { None });
        }
    }
    (words.join(" "), times)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Option<Duration> {
        Some(Duration::from_millis(value))
    }

    #[test]
    fn word_times_from_stamps_or_estimated_and_chorus() {
        let lyrics = parse(
            "[00:10.00]<00:10.00>Группа <00:10.80>крови <00:11.50>на рукаве\n[00:14.00]Пожелай мне удачи\n\
             [00:20.00][00:40.00]Пожелай мне удачи\n[00:22.00]",
        );
        assert_eq!(lyrics.lines[0].text, "Группа крови на рукаве");
        let starts = lyrics.word_starts(0);
        assert_eq!(&starts[..3], [10_000, 10_800, 11_500].map(Duration::from_millis), "пословные метки как есть");
        // «рукаве» без метки — по оценке, но не раньше «на» и до следующей строки.
        assert!(starts[3] > starts[2] && starts[3] < Duration::from_secs(14), "{starts:?}");
        // Без меток — по длине слов, последнее не позже 85%.
        let starts = lyrics.word_starts(1);
        assert_eq!(starts.len(), 3);
        assert_eq!(starts[0], Duration::from_secs(14));
        assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(starts[2] <= Duration::from_millis(14_000 + 6_000 * 85 / 100));
        // Повторяющийся текст — припев (три раза «Пожелай мне удачи»).
        assert_eq!(lyrics.chorus, [false, true, true, false, true]);
        // Повтор строки с пословными метками сдвигает и их.
        let repeated = parse("[00:05.00][01:05.00]<00:05.00>Ла <00:05.50>ла");
        assert_eq!(repeated.word_starts(1), [Duration::from_millis(65_000), Duration::from_millis(65_500)]);
    }

    #[test]
    fn synced_lines_with_several_stamps_and_offset() {
        let lyrics = parse(
            "[ar:Кино]\n[offset:+500]\n[00:12.30]Группа крови\n[00:05.00][01:00]Припев\n\
             [00:20.5]Последняя <00:21.00>строка <00:22.00>тут",
        );
        assert!(lyrics.synced);
        let lines: Vec<(Option<Duration>, &str)> =
            lyrics.lines.iter().map(|line| (line.at, line.text.as_str())).collect();
        assert_eq!(
            lines,
            [
                (ms(4_500), "Припев"),
                (ms(11_800), "Группа крови"),
                (ms(20_000), "Последняя строка тут"),
                (ms(59_500), "Припев"),
            ]
        );
        assert_eq!(lyrics.line_at(Duration::from_secs(1)), None);
        assert_eq!(lyrics.line_at(Duration::from_secs(12)), Some(1));
        assert_eq!(lyrics.line_at(Duration::from_secs(70)), Some(3));
    }

    #[test]
    fn text_without_stamps_is_plain() {
        let lyrics = parse("Первая строка\n\nВторая [не метка]\n\n");
        assert!(!lyrics.synced);
        let texts: Vec<&str> = lyrics.lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["Первая строка", "", "Вторая [не метка]"]);
        assert_eq!(lyrics.line_at(Duration::from_secs(5)), None);
    }

    #[test]
    fn odd_stamps() {
        assert_eq!(timestamp("01:02"), Some(62_000));
        assert_eq!(timestamp("01:02:50"), Some(62_500));
        assert_eq!(timestamp("01:02.123"), Some(62_123));
        assert_eq!(timestamp("ar:Кино"), None);
        assert_eq!(timestamp("00:75.00"), None);
    }
}
