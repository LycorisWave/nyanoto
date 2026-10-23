//! Черновик для редактора `.lrc`: набрать, расставить метки на слух, довести руками.
//! Служебные теги и пословные `<мм:сс.хх>` сохраняются как есть.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftLine {
    /// Начало строки, мс; `None`, пока метка не поставлена.
    pub at: Option<i64>,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// Служебные теги: `[ar:…]`, `[ti:…]` и подобные, кроме `[offset:]`.
    pub meta: Vec<String>,
    pub lines: Vec<DraftLine>,
    /// Какую строку отметит следующее нажатие.
    pub next: usize,
    /// Поправка нажатия, мс: человек жмёт чуть позже, чем слышит.
    pub tap_offset: i64,
}

impl Draft {
    /// Черновик из обычного текста: по строке на строку, без меток.
    pub fn from_plain(text: &str) -> Self {
        let mut lines: Vec<DraftLine> = text
            .lines()
            .map(|line| DraftLine { at: None, text: line.trim().to_string() })
            .collect();
        while lines.last().is_some_and(|line| line.text.is_empty()) {
            lines.pop();
        }
        Self { lines, ..Self::default() }
    }

    /// LRC, если есть метки, иначе обычный текст.
    pub fn from_text(text: &str) -> Self {
        if crate::lyrics::parse(text).synced { Self::from_lrc(text) } else { Self::from_plain(text) }
    }

    /// `[offset:]` сразу в метки, строка с несколькими метками — в несколько строк.
    pub fn from_lrc(text: &str) -> Self {
        let mut meta = Vec::new();
        let mut offset = 0i64;
        let mut lines = Vec::new();
        let mut untimed = Vec::new();
        for raw in text.lines() {
            let mut rest = raw.trim();
            let mut stamps = Vec::new();
            let mut tags = false;
            while let Some(inner) = rest.strip_prefix('[') {
                let Some(end) = inner.find(']') else {
                    break;
                };
                let tag = &inner[..end];
                if let Some(stamp) = parse_time(tag) {
                    stamps.push(stamp);
                } else if let Some(value) = tag.strip_prefix("offset:") {
                    offset = value.trim().parse().unwrap_or(0);
                    tags = true;
                } else if stamps.is_empty() && tag.contains(':') {
                    meta.push(format!("[{tag}]"));
                    tags = true;
                } else {
                    break;
                }
                rest = inner[end + 1..].trim_start();
            }
            if stamps.is_empty() {
                // Строка без меток вроде «[Припев]» остаётся текстом целиком.
                if !raw.trim().is_empty() && !(tags && rest.is_empty()) {
                    untimed.push(DraftLine { at: None, text: raw.trim().to_string() });
                }
                continue;
            }
            for stamp in stamps {
                lines.push(DraftLine { at: Some((stamp - offset).max(0)), text: rest.to_string() });
            }
        }
        lines.sort_by_key(|line| line.at);
        lines.extend(untimed);
        let next = lines.iter().position(|line| line.at.is_none()).unwrap_or(lines.len());
        Self { meta, lines, next, tap_offset: 0 }
    }

    /// Текст без меток: для шага «Текст».
    pub fn plain(&self) -> String {
        self.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// Заменяет текст, сохраняя метки строк, которые остались на месте.
    pub fn set_plain(&mut self, text: &str) {
        let fresh = Self::from_plain(text);
        let old = std::mem::take(&mut self.lines);
        self.lines = fresh
            .lines
            .into_iter()
            .enumerate()
            .map(|(index, mut line)| {
                if let Some(previous) = old.get(index)
                    && previous.text == line.text
                {
                    line.at = previous.at;
                }
                line
            })
            .collect();
        self.next = self.lines.iter().position(|line| line.at.is_none()).unwrap_or(self.lines.len());
    }

    /// Метка очередной строке, возвращает её номер.
    pub fn stamp(&mut self, position_ms: i64) -> Option<usize> {
        let index = self.next;
        let line = self.lines.get_mut(index)?;
        line.at = Some((position_ms + self.tap_offset).max(0));
        self.next = index + 1;
        Some(index)
    }

    /// Снимает последнюю поставленную метку и возвращается к этой строке.
    pub fn unstamp(&mut self) -> Option<usize> {
        let index = self.next.checked_sub(1)?;
        self.lines[index].at = None;
        self.next = index;
        Some(index)
    }

    /// Сдвигает метку строки на `delta` мс.
    pub fn nudge(&mut self, index: usize, delta: i64) {
        if let Some(at) = self.lines.get_mut(index).and_then(|line| line.at.as_mut()) {
            *at = (*at + delta).max(0);
        }
    }

    /// Сдвигает все метки сразу.
    pub fn shift_all(&mut self, delta: i64) {
        for at in self.lines.iter_mut().filter_map(|line| line.at.as_mut()) {
            *at = (*at + delta).max(0);
        }
    }

    /// Задаёт метку строки текстом вида `1:23.456`. Пустой текст снимает метку.
    pub fn set_time(&mut self, index: usize, text: &str) -> bool {
        let Some(line) = self.lines.get_mut(index) else {
            return false;
        };
        let text = text.trim();
        if text.is_empty() || text == "—" {
            line.at = None;
            // Стёрли последнюю метку — продолжаем с этой строки, а не со следующей.
            if index < self.next && self.lines[index + 1..self.next.min(self.lines.len())].iter().all(|line| line.at.is_none()) {
                self.next = index;
            }
            return true;
        }
        match parse_time(text) {
            Some(at) => {
                line.at = Some(at);
                true
            }
            None => false,
        }
    }

    /// Строки, чьи метки раньше строк выше, — стоит проверить.
    pub fn out_of_order(&self) -> Vec<usize> {
        let mut latest = i64::MIN;
        let mut result = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            if let Some(at) = line.at {
                if at < latest {
                    result.push(index);
                }
                latest = latest.max(at);
            }
        }
        result
    }

    /// Строка, что звучит в `position_ms`.
    pub fn current(&self, position_ms: i64) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| Some((index, line.at?)))
            .filter(|(_, at)| *at <= position_ms)
            .max_by_key(|(index, at)| (*at, *index))
            .map(|(index, _)| index)
    }

    /// Строки без метки.
    pub fn unstamped(&self) -> usize {
        self.lines.iter().filter(|line| line.at.is_none()).count()
    }

    /// Строки без метки пишем как есть: плееры их пропустят, редактор вернёт.
    pub fn to_lrc(&self) -> String {
        let mut out = String::new();
        for tag in &self.meta {
            let _ = writeln!(out, "{tag}");
        }
        for line in &self.lines {
            match line.at {
                Some(at) => {
                    let _ = writeln!(out, "[{}]{}", format_time(at), line.text);
                }
                None if !line.text.is_empty() => {
                    let _ = writeln!(out, "{}", line.text);
                }
                None => out.push('\n'),
            }
        }
        out
    }
}

/// `мм:сс.хх` — сотые, если их хватает, иначе `мм:сс.ххх`.
pub fn format_time(ms: i64) -> String {
    let ms = ms.max(0);
    let (minutes, seconds, millis) = (ms / 60_000, ms / 1000 % 60, ms % 1000);
    if millis % 10 == 0 {
        format!("{minutes:02}:{seconds:02}.{:02}", millis / 10)
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

/// Позиция для часов редактора: всегда `мм:сс.хх`.
pub fn clock(ms: i64) -> String {
    let ms = ms.max(0);
    format!("{:02}:{:02}.{:02}", ms / 60_000, ms / 1000 % 60, ms % 1000 / 10)
}

/// `мм:сс[.ххх]`, `мм:сс:хх` или секунды `83.5` → мс.
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let (minutes, rest) = match text.split_once(':') {
        Some((minutes, rest)) => (minutes.trim().parse::<i64>().ok()?, rest),
        None => (0, text),
    };
    let (seconds, fraction) = match rest.split_once(['.', ':', ',']) {
        Some((seconds, fraction)) => (seconds, fraction),
        None => (rest, ""),
    };
    let seconds: i64 = seconds.trim().parse().ok()?;
    if minutes < 0 || seconds < 0 || (text.contains(':') && seconds >= 60) {
        return None;
    }
    if !fraction.chars().all(|char| char.is_ascii_digit()) {
        return None;
    }
    let millis = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction[..3].parse::<i64>().ok()?,
    };
    Some(minutes * 60_000 + seconds * 1000 + millis)
}

/// Существующий `.lrc` в любом регистре или новый рядом.
pub fn lrc_path(track: &Path) -> PathBuf {
    let upper = track.with_extension("LRC");
    if upper.exists() && !track.with_extension("lrc").exists() {
        return upper;
    }
    track.with_extension("lrc")
}

/// Через временный файл — оборванная запись не оставит полтекста.
pub fn save(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("lrc-nyanoto-tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_the_last_mark_returns_tapping_to_that_line() {
        let mut draft = Draft::from_plain("раз\nдва\nтри\nчетыре");
        assert_eq!(draft.stamp(1000), Some(0));
        assert!(draft.set_time(0, ""));
        assert_eq!(draft.next, 0, "стёр единственную метку — снова отмечается первая");
        assert_eq!(draft.stamp(1200), Some(0));
        assert_eq!(draft.stamp(2000), Some(1));
        assert_eq!(draft.stamp(3000), Some(2));
        // Стёрта метка в середине: дальше уже размечено — нажатие не прыгает назад.
        assert!(draft.set_time(1, ""));
        assert_eq!(draft.next, 3);
        // Стёрта и последняя — разметка с неё (строку 1 из середины выбирают сами).
        assert!(draft.set_time(2, ""));
        assert_eq!(draft.next, 2);
        assert_eq!(draft.lines[0].at, Some(1200));
        // Начали с выбранной строки 3 и стёрли её метку — снова с 3, а не с начала.
        draft.next = 3;
        assert_eq!(draft.stamp(5000), Some(3));
        assert!(draft.set_time(3, ""));
        assert_eq!(draft.next, 3);
    }

    #[test]
    fn tapping_through_a_song() {
        let mut draft = Draft::from_plain("Первая\nВторая\n\nТретья\n\n");
        assert_eq!(draft.lines.len(), 4);
        draft.tap_offset = -150;
        assert_eq!(draft.stamp(10_150), Some(0));
        assert_eq!(draft.stamp(20_000), Some(1));
        // Ошиблись: отменяем и отмечаем заново.
        assert_eq!(draft.unstamp(), Some(1));
        assert_eq!(draft.stamp(19_650), Some(1));
        draft.stamp(25_000);
        draft.stamp(31_234);
        assert_eq!(draft.stamp(40_000), None, "строк больше нет");

        draft.nudge(3, 10);
        draft.shift_all(-1000);
        assert_eq!(
            draft.to_lrc(),
            "[00:09.00]Первая\n[00:18.50]Вторая\n[00:23.85]\n[00:30.094]Третья\n"
        );
        assert!(draft.out_of_order().is_empty());
        assert_eq!(draft.unstamped(), 0);
    }

    #[test]
    fn existing_lrc_round_trips() {
        let text = "[ar:Кино]\n[offset:+500]\n[00:12.30]Группа <00:13.00>крови\n[00:05.00][01:00]Припев\nбез метки\n";
        let draft = Draft::from_lrc(text);
        assert_eq!(draft.meta, ["[ar:Кино]"]);
        assert_eq!(
            draft.to_lrc(),
            "[ar:Кино]\n[00:04.50]Припев\n[00:11.80]Группа <00:13.00>крови\n[00:59.50]Припев\nбез метки\n"
        );
        assert_eq!(draft.next, 3);
        assert_eq!(draft.unstamped(), 1);
    }

    #[test]
    fn editing_text_keeps_marks_of_unchanged_lines() {
        let mut draft = Draft::from_lrc("[00:01.00]Раз\n[00:02.00]Два\n[00:03.00]Три");
        draft.set_plain("Раз\nДва с правкой\nТри");
        let marks: Vec<Option<i64>> = draft.lines.iter().map(|line| line.at).collect();
        assert_eq!(marks, [Some(1000), None, Some(3000)]);
        assert_eq!(draft.next, 1);
    }

    #[test]
    fn plain_lines_in_brackets_stay_text() {
        let draft = Draft::from_lrc("[ti:Песня]\n[00:01.00]Раз\n[Припев]\n");
        assert_eq!(draft.meta, ["[ti:Песня]"]);
        assert_eq!(draft.lines[1], DraftLine { at: None, text: "[Припев]".into() });

        let draft = Draft::from_text("Куплет\n\nПрипев");
        assert_eq!(draft.lines.len(), 3, "пустая строка между куплетами остаётся");
        let draft = Draft::from_text("[00:02.00]Два\n[00:01.00]Раз");
        assert_eq!(draft.current(1500), Some(0));
        assert_eq!(draft.current(500), None);
        assert_eq!(draft.current(9000), Some(1));
    }

    #[test]
    fn clock_always_shows_hundredths() {
        assert_eq!(clock(83_456), "01:23.45");
    }

    #[test]
    fn times_parse_and_format_precisely() {
        assert_eq!(parse_time("01:23.456"), Some(83_456));
        assert_eq!(parse_time("1:23.4"), Some(83_400));
        assert_eq!(parse_time("01:23:45"), Some(83_450));
        assert_eq!(parse_time("83.5"), Some(83_500));
        assert_eq!(parse_time("1:75"), None);
        assert_eq!(parse_time("ar:Кино"), None);
        assert_eq!(format_time(83_450), "01:23.45");
        assert_eq!(format_time(83_456), "01:23.456");

        let mut draft = Draft::from_plain("a\nb");
        assert!(draft.set_time(1, "0:05.5"));
        assert!(!draft.set_time(1, "пять"));
        assert!(draft.set_time(0, "0:09"));
        assert_eq!(draft.out_of_order(), [1]);
    }
}
