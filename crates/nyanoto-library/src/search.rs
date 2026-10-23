//! Поиск, который прощает раскладку и опечатки.
//! Сначала — ровно набранное («AC/DC», «Blink-182»), мало — расширяем: ghbdtn → привет, првет → привет.

use std::collections::HashSet;

use crate::{Library, LibraryError, TRACK_COLUMNS, Track};

/// Клавиши английской раскладки и буквы русской на тех же местах.
const LAYOUT: &[(char, char)] = &[
    ('`', 'ё'), ('q', 'й'), ('w', 'ц'), ('e', 'у'), ('r', 'к'), ('t', 'е'), ('y', 'н'),
    ('u', 'г'), ('i', 'ш'), ('o', 'щ'), ('p', 'з'), ('[', 'х'), (']', 'ъ'), ('a', 'ф'),
    ('s', 'ы'), ('d', 'в'), ('f', 'а'), ('g', 'п'), ('h', 'р'), ('j', 'о'), ('k', 'л'),
    ('l', 'д'), (';', 'ж'), ('\'', 'э'), ('z', 'я'), ('x', 'ч'), ('c', 'с'), ('v', 'м'),
    ('b', 'и'), ('n', 'т'), ('m', 'ь'), (',', 'б'), ('.', 'ю'),
];

/// Слова полнотекстового индекса и в скольких треках каждое встречается.
pub(crate) type Vocabulary = std::sync::Arc<Vec<(String, i64)>>;

/// Сколько близких слов из словаря берётся на одно слово запроса.
const SIMILAR_PER_WORD: usize = 8;

/// Поправки — только если точных меньше этого, иначе шум и сотни мс.
const RELAXED_BELOW: usize = 50;

/// То же слово в другой раскладке. `None` — переводить нечего.
pub(crate) fn other_layout(word: &str) -> Option<String> {
    let lower = word.to_lowercase();
    let mut changed = false;
    let swapped: String = lower
        .chars()
        .map(|char| {
            let mapped = LAYOUT
                .iter()
                .find_map(|&(latin, cyrillic)| {
                    if char == latin {
                        Some(cyrillic)
                    } else if char == cyrillic {
                        Some(latin)
                    } else {
                        None
                    }
                });
            changed |= mapped.is_some();
            mapped.unwrap_or(char)
        })
        .collect();
    (changed && swapped != lower).then_some(swapped)
}

/// Дамерау — Левенштейн, если не больше `limit`.
#[cfg(test)]
fn distance_within(a: &str, b: &str, limit: usize) -> Option<usize> {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    within(&a, &b, limit, &mut Rows::default())
}

/// Три строки таблицы расстояний — память выделяется раз на словарь.
#[derive(Default)]
struct Rows([Vec<usize>; 3]);

fn within(a: &[char], b: &[char], limit: usize, rows: &mut Rows) -> Option<usize> {
    if a.len().abs_diff(b.len()) > limit {
        return None;
    }
    let width = b.len() + 1;
    let rows = &mut rows.0;
    for row in rows.iter_mut() {
        row.clear();
        row.resize(width, 0);
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        // Строки по кругу: i % 3 — текущая, (i + 2) % 3 — прошлая, (i + 1) % 3 — позапрошлая.
        let (current, previous, before) = (i % 3, (i + 2) % 3, (i + 1) % 3);
        rows[current][0] = i;
        let mut best = i;
        for j in 1..width {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut value = (rows[previous][j] + 1).min(rows[current][j - 1] + 1).min(rows[previous][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                value = value.min(rows[before][j - 2] + 1);
            }
            rows[current][j] = value;
            best = best.min(value);
        }
        if best > limit {
            return None;
        }
    }
    let result = rows[a.len() % 3][b.len()];
    (result <= limit).then_some(result)
}

/// Сколько опечаток прощать. Короткие слова и числа — только точно.
fn tolerance(word: &str) -> usize {
    if word.chars().all(|char| char.is_ascii_digit()) {
        return 0;
    }
    match word.chars().count() {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    }
}

/// Слова запроса так, как их режет полнотекстовый индекс: буквы и цифры.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|char: char| !char.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn quoted(term: &str) -> String {
    format!("\"{}\"", term.replace('"', ""))
}

/// Строгий запрос: каждое слово, как набрано, ищется по началу.
pub(crate) fn strict_pattern(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|word| word.replace('"', ""))
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{word}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

impl Library {
    /// По названию, исполнителю, альбому, жанру и пути. Точные первыми.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Track>, LibraryError> {
        Ok(self.search_ranked(query, limit)?.0)
    }

    /// Плюс сколько первых найдено точно.
    pub fn search_ranked(&self, query: &str, limit: usize) -> Result<(Vec<Track>, usize), LibraryError> {
        let Some(strict) = strict_pattern(query) else {
            let all = self.tracks(limit, 0)?;
            let count = all.len();
            return Ok((all, count));
        };
        let mut found = self.match_tracks(&strict, limit)?;
        let exact = found.len();
        if exact >= limit.min(RELAXED_BELOW) {
            return Ok((found, exact));
        }
        let Some(relaxed) = self.relaxed_pattern(query)? else {
            return Ok((found, exact));
        };
        let seen: HashSet<i64> = found.iter().map(|track| track.id).collect();
        found.extend(
            self.match_tracks(&relaxed, limit)?
                .into_iter()
                .filter(|track| !seen.contains(&track.id))
                .take(limit - found.len()),
        );
        Ok((found, exact))
    }

    /// По релевантности. Если подходит почти всё («the»), это сотни мс —
    /// тогда первые `limit` без сортировки: «лучшие 300» из тысяч одинаковых бессмысленны.
    fn match_tracks(&self, pattern: &str, limit: usize) -> Result<Vec<Track>, LibraryError> {
        let cap = limit.saturating_mul(4).saturating_add(1);
        let many: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM (SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ?1 LIMIT ?2)",
            (pattern, cap as i64),
            |row| row.get(0),
        )?;
        let order = if many as usize >= cap { "" } else { "ORDER BY f.rank" };
        self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks_fts f
                 JOIN tracks t ON t.id = f.rowid JOIN roots r ON r.id = t.root_id
                 WHERE tracks_fts MATCH ?1 AND t.missing = 0
                 {order} LIMIT ?2"
            ),
            (pattern, limit as i64),
        )
    }

    /// Слово, его раскладка и близкие из словаря. `None` — расширять нечего.
    fn relaxed_pattern(&self, query: &str) -> Result<Option<String>, LibraryError> {
        let words = tokens(query);
        if words.is_empty() {
            return Ok(None);
        }
        let vocabulary = self.vocabulary()?;
        let mut groups = Vec::new();
        let mut widened = false;
        for word in &words {
            let mut variants = vec![format!("{}*", quoted(word))];
            let mut spellings = vec![word.clone()];
            if let Some(swapped) = other_layout(word) {
                variants.push(format!("{}*", quoted(&swapped)));
                spellings.push(swapped);
            }
            for spelling in &spellings {
                for term in similar_terms(spelling, &vocabulary) {
                    variants.push(quoted(&term));
                }
            }
            variants.dedup();
            widened |= variants.len() > 1;
            groups.push(format!("({})", variants.join(" OR ")));
        }
        Ok(widened.then(|| groups.join(" AND ")))
    }

    /// Словарь FTS: слова и в скольких треках. В памяти до [`Library::forget_search_cache`].
    fn vocabulary(&self) -> Result<Vocabulary, LibraryError> {
        if let Some(cached) = self.vocabulary.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let mut statement = self.connection.prepare("SELECT term, doc FROM tracks_vocab")?;
        let terms: Vec<(String, i64)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let terms = std::sync::Arc::new(terms);
        *self.vocabulary.borrow_mut() = Some(terms.clone());
        Ok(terms)
    }

    /// Сбрасывает словарь поиска: после сканирования в нём могли появиться слова.
    pub fn forget_search_cache(&self) {
        self.vocabulary.borrow_mut().take();
    }
}

/// Близкие слова, похожие и частые первыми. Сравниваем и с началом — слово может быть не дописано.
fn similar_terms(word: &str, vocabulary: &[(String, i64)]) -> Vec<String> {
    let limit = tolerance(word);
    if limit == 0 {
        return Vec::new();
    }
    let word_chars: Vec<char> = word.chars().collect();
    let length = word_chars.len();
    // Слово с буквами не бывает в двух опечатках от числа (у слова от 4 букв).
    let word_has_digits = word_chars.iter().any(char::is_ascii_digit);
    let (mut term_chars, mut rows) = (Vec::new(), Rows::default());
    let mut scored: Vec<(usize, i64, &str)> = vocabulary
        .iter()
        .filter(|(term, _)| term.as_str() != word)
        .filter(|(term, _)| word_has_digits || !term.bytes().all(|byte| byte.is_ascii_digit()))
        .filter_map(|(term, count)| {
            term_chars.clear();
            term_chars.extend(term.chars());
            if term_chars.len() + limit < length {
                return None;
            }
            let whole = within(&word_chars, &term_chars, limit, &mut rows);
            let prefix = (term_chars.len() > length + 1)
                .then(|| within(&word_chars, &term_chars[..length + 1], limit, &mut rows))
                .flatten();
            let best = match (whole, prefix) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => return None,
            };
            Some((best, -count, term.as_str()))
        })
        .collect();
    scored.sort();
    scored
        .into_iter()
        .take(SIMILAR_PER_WORD)
        .map(|(_, _, term)| term.to_string())
        .collect()
}

/// Та же манера для списков вне индекса. Для длинных — [`TextMatcher`].
pub fn text_matches(query: &str, text: &str) -> bool {
    TextMatcher::new(query).matches(text)
}

/// Запрос, подготовленный раз на много строк.
pub struct TextMatcher {
    /// На слово — написания: текст, буквы и допуск опечаток.
    words: Vec<Vec<(String, Vec<char>, usize)>>,
    rows: std::cell::RefCell<Rows>,
    buffer: std::cell::RefCell<Vec<char>>,
}

impl TextMatcher {
    pub fn new(query: &str) -> Self {
        let words = query
            .split_whitespace()
            .map(|word| {
                let word = word.to_lowercase();
                let mut spellings = vec![word.clone()];
                spellings.extend(other_layout(&word));
                spellings
                    .into_iter()
                    .map(|spelling| {
                        let limit = tolerance(&spelling);
                        let chars = spelling.chars().collect();
                        (spelling, chars, limit)
                    })
                    .collect()
            })
            .collect();
        Self { words, rows: Default::default(), buffer: Default::default() }
    }

    pub fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        // Слова текста режутся только если подстрокой не нашлось.
        let mut text_words: Option<Vec<String>> = None;
        let (mut rows, mut buffer) = (self.rows.borrow_mut(), self.buffer.borrow_mut());
        self.words.iter().all(|spellings| {
            spellings.iter().any(|(spelling, chars, limit)| {
                if text.contains(spelling.as_str()) {
                    return true;
                }
                *limit > 0
                    && text_words.get_or_insert_with(|| tokens(&text)).iter().any(|candidate| {
                        buffer.clear();
                        buffer.extend(candidate.chars());
                        let start = buffer.len().min(chars.len() + 1);
                        within(chars, &buffer, *limit, &mut rows).is_some()
                            || within(chars, &buffer[..start], *limit, &mut rows).is_some()
                    })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_swap_both_ways() {
        assert_eq!(other_layout("ghbdtn").as_deref(), Some("привет"));
        assert_eq!(other_layout("кфвшщруфв").as_deref(), Some("radiohead"));
        assert_eq!(other_layout("Ghbdtn").as_deref(), Some("привет"));
        assert_eq!(other_layout("123"), None);
    }

    #[test]
    fn distance_counts_typos() {
        assert_eq!(distance_within("првет", "привет", 1), Some(1));
        assert_eq!(distance_within("привет3", "привет", 1), Some(1));
        assert_eq!(distance_within("пирвет", "привет", 1), Some(1));
        assert_eq!(distance_within("пока", "привет", 2), None);
    }

    #[test]
    fn strict_pattern_keeps_symbols_inside_words() {
        assert_eq!(strict_pattern("AC/DC").as_deref(), Some("\"AC/DC\"*"));
        assert_eq!(strict_pattern("say \"hi\"").as_deref(), Some("\"say\"* \"hi\"*"));
        assert_eq!(strict_pattern("  * - "), None);
    }

    #[test]
    fn text_matching_forgives_layout_and_typos() {
        assert!(text_matches("ghbdtn", "Привет, мир"));
        assert!(text_matches("првет", "Привет, мир"));
        assert!(text_matches("кфвшщ", "Radiohead"));
        assert!(text_matches("blink-182", "Blink-182"));
        assert!(!text_matches("blink-183", "Blink-182"));
        assert!(!text_matches("пока", "Привет, мир"));
    }
}
