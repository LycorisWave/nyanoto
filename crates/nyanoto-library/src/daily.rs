//! Плейлист дня по вкусу: лайки, частое, любимые исполнители.
//!
//! Вкус — по тому, как обходились с треком: не в счёт прослушивания из самого плейлиста,
//! под таймер сна и в коротких заходах; ранний пропуск — минус; давнее весит меньше.
//! ~70 % знакомого и 30 % нового, похожего. Порядок — волной, соседи близки по темпу.

use std::collections::{HashMap, HashSet};

use rusqlite::OptionalExtension;

use crate::user::now;
use crate::{Library, LibraryError, TRACK_COLUMNS, Track};

/// Отметки прослушивания и пропуска (столбец `flags`).
pub const PLAY_DAILY: u8 = 1;
pub const PLAY_SLEEPY: u8 = 2;
pub const PLAY_CHOSEN: u8 = 4;

/// Длина плейлиста на выбор.
pub const DAILY_LENGTHS: [usize; 4] = [15, 25, 40, 60];

const DAY: i64 = 86_400;
/// Новый день — с 5 утра, чтобы плейлист не менялся ночью посреди прослушивания.
const DAY_STARTS: i64 = 5 * 3600;
/// Старше полугода не в счёт; вес падает вдвое за месяц (пропуски — за две недели).
const HORIZON: i64 = 180 * DAY;
const HALF_LIFE_DAYS: f32 = 30.0;
const SKIP_HALF_LIFE_DAYS: f32 = 14.0;
const LIKE_HALF_LIFE_DAYS: f32 = 120.0;
/// Заход — без перерыва дольше получаса; короткий о вкусе не говорит.
const SESSION_GAP: i64 = 30 * 60;
const SESSION_TRACKS: usize = 5;
const SESSION_MS: i64 = 25 * 60 * 1000;
/// С какого числа прослушиваний трек «любимый».
const FAVOURITE_PLAYS: u32 = 5;
const FAVOURITE_SCORE: f32 = 1.5;
/// Пропуск в первые секунды — явное «не то».
const QUICK_SKIP_MS: i64 = 4_000;
/// В плейлист не идут сценки и вступления короче минуты и длинные записи.
const SHORTEST_MS: i64 = 60_000;
const LONGEST_MS: i64 = 15 * 60_000;
/// Насколько прошлые плейлисты понижают шансы своих треков.
const PAST_FACTORS: [f32; 4] = [0.25, 0.4, 0.6, 0.8];
const REBUILT_FACTOR: f32 = 0.15;
/// Из последнего плейлиста — не больше трети.
const REPEAT_SHARE: f32 = 0.3;
/// Слушали в последние полтора дня — шансы вдвое меньше.
const RECENT: i64 = 36 * 3600;

/// День плейлиста для момента `unix` при смещении местного времени `offset`.
pub fn daily_day(unix: i64, offset: i64) -> i64 {
    (unix + offset - DAY_STARTS).div_euclid(DAY)
}

/// С какого размера библиотеки и какой статистики плейлист дня имеет смысл.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DailyThreshold {
    /// 300 треков, неделя, 100 прослушиваний.
    Early,
    /// 500 треков, две недели, 150 прослушиваний.
    #[default]
    Usual,
    /// 1000 треков, месяц, 300 прослушиваний.
    Strict,
}

impl DailyThreshold {
    pub const ALL: [Self; 3] = [Self::Early, Self::Usual, Self::Strict];

    pub fn tracks(self) -> u64 {
        match self {
            Self::Early => 300,
            Self::Usual => 500,
            Self::Strict => 1000,
        }
    }

    pub fn days(self) -> i64 {
        match self {
            Self::Early => 7,
            Self::Usual => 14,
            Self::Strict => 30,
        }
    }

    pub fn plays(self) -> u64 {
        match self {
            Self::Early => 100,
            Self::Usual => 150,
            Self::Strict => 300,
        }
    }
}

/// Сколько есть для плейлиста: треков, прослушиваний, дней.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DailyReadiness {
    pub tracks: u64,
    pub plays: u64,
    pub days: i64,
}

impl DailyReadiness {
    pub fn ready(&self, threshold: DailyThreshold) -> bool {
        self.tracks >= threshold.tracks() && self.plays >= threshold.plays() && self.days >= threshold.days()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DailyOptions {
    pub length: usize,
    /// Доля нового, 0…0,6.
    pub fresh: f32,
    pub seed: u64,
    /// Принудительно: без порогов, недостающее — случайными треками.
    pub force: bool,
}

/// Что известно о звуке трека для порядка в плейлисте.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackFeel {
    /// Долей в минуту.
    pub tempo: Option<f32>,
    /// Напряжение звука, 0…1.
    pub energy: Option<f32>,
    /// Слов в минуту (если текстов в библиотеке много).
    pub words: Option<f32>,
}

// ───────────────────────────── вкус ─────────────────────────────

/// Трек, который может попасть в плейлист.
#[derive(Clone, Debug, Default)]
pub(crate) struct Candidate {
    pub id: i64,
    pub artist: String,
    pub album: String,
    pub genre: String,
}

/// Прослушивание или пропуск. `length` — длина трека, мс.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Event {
    pub track: i64,
    pub at: i64,
    pub ms: i64,
    pub flags: u8,
    pub length: i64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    /// По времени.
    pub plays: Vec<Event>,
    pub skips: Vec<Event>,
    /// Лайк и когда поставлен.
    pub likes: HashMap<i64, i64>,
    /// Прошлые плейлисты и во сколько раз они понижают шансы своих треков.
    pub past: Vec<(HashSet<i64>, f32)>,
    /// Последний плейлист: из него — не больше трети.
    pub last: HashSet<i64>,
}

fn fade(age: i64, half_life_days: f32) -> f32 {
    0.5f32.powf(age.max(0) as f32 / DAY as f32 / half_life_days)
}

/// Прослушивания, которые говорят о вкусе (`plays` — по времени).
pub(crate) fn tasteful(plays: &[Event]) -> Vec<Event> {
    let mut chosen = Vec::new();
    let mut start = 0;
    while start < plays.len() {
        let mut end = start + 1;
        while end < plays.len() && plays[end].at - plays[end].ms / 1000 - plays[end - 1].at <= SESSION_GAP {
            end += 1;
        }
        let session = &plays[start..end];
        let long = session.len() >= SESSION_TRACKS || session.iter().map(|play| play.ms).sum::<i64>() >= SESSION_MS;
        if long {
            chosen.extend(session.iter().filter(|play| play.flags & (PLAY_DAILY | PLAY_SLEEPY) == 0));
        }
        start = end;
    }
    chosen
}

#[derive(Clone, Copy, Debug, Default)]
struct Taste {
    score: f32,
    plays: u32,
    last: i64,
    skips: f32,
}

fn like_value(added: i64, now: i64) -> f32 {
    2.5 * (0.4 + 0.6 * fade(now - added, LIKE_HALF_LIFE_DAYS))
}

/// Простой генератор (splitmix64): выбор повторяется при том же зерне.
struct Random(u64);

impl Random {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }
}

/// Доля от самого большого, под корнем — любимый не забивает остальных.
fn normalize<K: std::hash::Hash + Eq>(values: HashMap<K, f32>) -> HashMap<K, f32> {
    let top = values.values().copied().fold(0.0f32, f32::max);
    if top <= 0.0 {
        return HashMap::new();
    }
    values.into_iter().map(|(key, value)| (key, (value.max(0.0) / top).sqrt())).collect()
}

fn first_genre(genre: &str) -> &str {
    genre.split([';', '/', ',']).next().unwrap_or("").trim()
}

/// Отбирает треки плейлиста (номера в `candidates`), без порядка.
pub(crate) fn choose(candidates: &[Candidate], history: &History, options: &DailyOptions, now: i64) -> Vec<usize> {
    let mut taste: HashMap<i64, Taste> = HashMap::new();
    for play in tasteful(&history.plays) {
        if now - play.at > HORIZON {
            continue;
        }
        let completion = if play.length > 0 { (play.ms as f32 / play.length as f32).clamp(0.3, 1.0) } else { 0.8 };
        let chosen = if play.flags & PLAY_CHOSEN != 0 { 1.4 } else { 1.0 };
        let entry = taste.entry(play.track).or_default();
        entry.score += fade(now - play.at, HALF_LIFE_DAYS) * completion * chosen;
        entry.plays += 1;
        entry.last = entry.last.max(play.at);
    }
    for skip in &history.skips {
        if skip.flags & PLAY_SLEEPY != 0 || now - skip.at > HORIZON {
            continue;
        }
        let weight = if skip.ms <= QUICK_SKIP_MS { 1.0 } else { 0.5 };
        taste.entry(skip.track).or_default().skips += weight * fade(now - skip.at, SKIP_HALF_LIFE_DAYS);
    }

    // Любовь к исполнителям, альбомам, жанрам.
    let (mut artists, mut albums, mut genres) = (HashMap::new(), HashMap::new(), HashMap::new());
    for candidate in candidates {
        let taste = taste.get(&candidate.id).copied().unwrap_or_default();
        let like = history.likes.get(&candidate.id).map_or(0.0, |added| like_value(*added, now));
        let weight = if taste.plays >= FAVOURITE_PLAYS { 1.0 } else { 0.4 };
        let signal = taste.score * weight + like - taste.skips;
        if signal == 0.0 {
            continue;
        }
        if !candidate.artist.is_empty() {
            *artists.entry(candidate.artist.as_str()).or_insert(0.0) += signal;
            if !candidate.album.is_empty() {
                *albums.entry((candidate.artist.as_str(), candidate.album.as_str())).or_insert(0.0) += signal;
            }
        }
        let genre = first_genre(&candidate.genre);
        if !genre.is_empty() {
            *genres.entry(genre).or_insert(0.0) += signal;
        }
    }
    let (artists, albums, genres) = (normalize(artists), normalize(albums), normalize(genres));

    // Знакомое — любимое и лайкнутое; новое — мало слышанное, но рядом с любимым.
    let mut known = Vec::new();
    let mut fresh = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let taste = taste.get(&candidate.id).copied().unwrap_or_default();
        let liked = history.likes.get(&candidate.id);
        let artist = artists.get(candidate.artist.as_str()).copied().unwrap_or(0.0);
        let mut weight;
        let familiar = liked.is_some() || (taste.plays >= FAVOURITE_PLAYS && taste.score >= FAVOURITE_SCORE);
        if familiar {
            let like = liked.map_or(0.0, |added| like_value(*added, now));
            weight = taste.score + like - taste.skips;
            if weight <= 0.0 {
                continue;
            }
            weight = weight.powf(0.6) * (0.6 + 0.4 * artist);
            if taste.plays > 0 && now - taste.last < RECENT {
                weight *= 0.5;
            }
        } else {
            let album = albums.get(&(candidate.artist.as_str(), candidate.album.as_str())).copied().unwrap_or(0.0);
            let genre = genres.get(first_genre(&candidate.genre)).copied().unwrap_or(0.0);
            let novelty = if taste.plays == 0 { 1.0 } else { 0.8 };
            weight = (0.45 * artist + 0.2 * album + 0.35 * genre) * novelty - 0.5 * taste.skips;
            if weight < 0.03 {
                continue;
            }
        }
        for (past, factor) in &history.past {
            if past.contains(&candidate.id) {
                weight *= factor;
            }
        }
        if familiar { known.push((index, weight)) } else { fresh.push((index, weight)) }
    }

    // Выбор с весами без повторов (Эфраимидис — Спиракис), с пределами на исполнителя.
    let mut random = Random(options.seed);
    let mut sorted = |pool: Vec<(usize, f32)>| -> Vec<usize> {
        let mut keyed: Vec<(f32, usize)> =
            pool.into_iter().map(|(index, weight)| (random.next().ln() / weight.max(1e-6), index)).collect();
        keyed.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        keyed.into_iter().map(|(_, index)| index).collect()
    };
    let known = sorted(known);
    let fresh = sorted(fresh);
    // Для принудительного сбора — вся библиотека в случайном порядке, про запас.
    let spare = if options.force { sorted((0..candidates.len()).map(|index| (index, 1.0)).collect()) } else { Vec::new() };

    let length = options.length.max(1);
    let fresh_wanted = (length as f32 * options.fresh.clamp(0.0, 0.6)).round() as usize;
    let mut picker = Picker {
        candidates,
        last: &history.last,
        artist_cap: (length / 8).max(2),
        album_cap: (length / 10).max(2),
        repeat_cap: (length as f32 * REPEAT_SHARE).floor() as usize,
        chosen: Vec::new(),
        taken: HashSet::new(),
        artists: HashMap::new(),
        albums: HashMap::new(),
        repeats: 0,
    };
    picker.take(&known, length - fresh_wanted, true);
    picker.take(&fresh, length, true);
    // Нового не хватило — знакомым; дальше — с пределами вдвое мягче.
    picker.take(&known, length, true);
    picker.take(&known, length, false);
    picker.take(&fresh, length, false);
    picker.take(&spare, length, true);
    picker.take(&spare, length, false);
    picker.chosen
}

struct Picker<'a> {
    candidates: &'a [Candidate],
    last: &'a HashSet<i64>,
    artist_cap: usize,
    album_cap: usize,
    repeat_cap: usize,
    chosen: Vec<usize>,
    taken: HashSet<usize>,
    artists: HashMap<&'a str, usize>,
    albums: HashMap<(&'a str, &'a str), usize>,
    repeats: usize,
}

impl<'a> Picker<'a> {
    /// Добирает из `pool` (по порядку), пока выбранных меньше `upto`.
    fn take(&mut self, pool: &[usize], upto: usize, strict: bool) {
        let soft = if strict { 1 } else { 2 };
        for &index in pool {
            if self.chosen.len() >= upto {
                return;
            }
            if self.taken.contains(&index) {
                continue;
            }
            let candidate = &self.candidates[index];
            let artist = candidate.artist.as_str();
            let album = (artist, candidate.album.as_str());
            let repeat = self.last.contains(&candidate.id);
            if !artist.is_empty() && self.artists.get(artist).copied().unwrap_or(0) >= self.artist_cap * soft {
                continue;
            }
            if !candidate.album.is_empty() && self.albums.get(&album).copied().unwrap_or(0) >= self.album_cap * soft {
                continue;
            }
            if strict && repeat && self.repeats >= self.repeat_cap {
                continue;
            }
            self.taken.insert(index);
            self.chosen.push(index);
            *self.artists.entry(artist).or_insert(0) += 1;
            *self.albums.entry(album).or_insert(0) += 1;
            self.repeats += usize::from(repeat);
        }
    }
}

// ───────────────────────────── порядок ─────────────────────────────

/// Агрессивность жанра 0…1; несколько жанров — среднее, незнакомый — `None`.
pub fn genre_aggression(genre: &str) -> Option<f32> {
    const KNOWN: &[(&str, f32)] = &[
        ("ambient", 0.08), ("эмбиент", 0.08), ("drone", 0.1), ("new age", 0.1), ("meditat", 0.05), ("медитац", 0.05),
        ("sleep", 0.05), ("lullab", 0.05), ("колыбел", 0.05), ("relax", 0.1), ("chill", 0.2), ("lo-fi", 0.2),
        ("lofi", 0.2), ("downtempo", 0.25), ("trip-hop", 0.3), ("trip hop", 0.3), ("classical", 0.2),
        ("классик", 0.2), ("барокко", 0.2), ("baroque", 0.2), ("opera", 0.3), ("опера", 0.3), ("piano", 0.15),
        ("фортепиано", 0.15), ("neoclassical", 0.15), ("soundtrack", 0.3), ("саундтрек", 0.3), ("score", 0.3),
        ("acoustic", 0.25), ("акустик", 0.25), ("folk", 0.3), ("фолк", 0.3), ("singer-songwriter", 0.25),
        ("авторская", 0.25), ("бард", 0.25), ("jazz", 0.3), ("джаз", 0.3), ("blues", 0.35), ("блюз", 0.35),
        ("soul", 0.35), ("соул", 0.35), ("r&b", 0.4), ("rnb", 0.4), ("bossa", 0.2), ("lounge", 0.2),
        ("chanson", 0.35), ("шансон", 0.35), ("romance", 0.3), ("романс", 0.3), ("country", 0.35),
        ("кантри", 0.35), ("reggae", 0.35), ("регги", 0.35), ("dream pop", 0.3), ("shoegaze", 0.45),
        ("post-rock", 0.45), ("пост-рок", 0.45), ("indie", 0.45), ("инди", 0.45), ("эстрада", 0.45),
        ("pop", 0.5), ("поп", 0.5), ("synthpop", 0.5), ("synthwave", 0.5), ("k-pop", 0.55), ("j-pop", 0.5),
        ("disco", 0.55), ("диско", 0.55), ("dance", 0.6), ("new wave", 0.55), ("нью-вейв", 0.55), ("funk", 0.5), ("фанк", 0.5), ("electronic", 0.5), ("электрон", 0.5),
        ("electro", 0.6), ("house", 0.6), ("хаус", 0.6), ("trance", 0.65), ("транс", 0.65), ("techno", 0.7),
        ("техно", 0.7), ("edm", 0.7), ("dubstep", 0.8), ("drum and bass", 0.75), ("drum & bass", 0.75),
        ("drum'n'bass", 0.75), ("dnb", 0.75), ("jungle", 0.75), ("hip hop", 0.6), ("hip-hop", 0.6),
        ("хип-хоп", 0.6), ("rap", 0.6), ("рэп", 0.6), ("trap", 0.65), ("phonk", 0.75), ("фонк", 0.75),
        ("ska", 0.6), ("ска", 0.6), ("rock", 0.65), ("рок", 0.65), ("alternative", 0.6), ("альтернатив", 0.6),
        ("post-punk", 0.6), ("пост-панк", 0.6), ("gothic", 0.6), ("готик", 0.6), ("emo", 0.65), ("эмо", 0.65),
        ("grunge", 0.7), ("гранж", 0.7), ("hard rock", 0.75), ("хард-рок", 0.75), ("punk", 0.8), ("панк", 0.8),
        ("industrial", 0.8), ("индастриал", 0.8), ("metal", 0.85), ("метал", 0.85), ("nu metal", 0.85),
        ("metalcore", 0.9), ("металкор", 0.9), ("hardcore", 0.9), ("хардкор", 0.9), ("hardstyle", 0.9),
        ("gabber", 0.95), ("noise", 0.9), ("thrash", 0.92), ("трэш", 0.92), ("death", 0.95), ("дэт", 0.95),
        ("black metal", 0.95), ("блэк", 0.95), ("deathcore", 0.97), ("grindcore", 0.98), ("грайнд", 0.98),
    ];
    let values: Vec<f32> = genre
        .split([';', '/', ','])
        .filter_map(|part| {
            let part = part.trim().to_lowercase();
            KNOWN
                .iter()
                .filter(|(word, _)| part.contains(word))
                .max_by_key(|(word, _)| word.chars().count())
                .map(|(_, value)| *value)
        })
        .collect();
    (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
}

/// Напряжение трека 0…1: звук, жанр и слова в минуту, сколько из них известно.
fn intensity(track: &Track, feel: &TrackFeel) -> f32 {
    let mut parts = Vec::new();
    if let Some(energy) = feel.energy {
        parts.push((energy.clamp(0.0, 1.0), 0.5));
    }
    if let Some(aggression) = track.genre.as_deref().and_then(genre_aggression) {
        parts.push((aggression, 0.35));
    }
    if let Some(words) = feel.words {
        parts.push((((words - 40.0) / 140.0).clamp(0.0, 1.0), 0.15));
    }
    let weight: f32 = parts.iter().map(|(_, weight)| weight).sum();
    if weight <= 0.0 {
        return 0.5;
    }
    parts.iter().map(|(value, weight)| value * weight).sum::<f32>() / weight
}

/// Насколько далеки темпы; вдвое быстрее или медленнее — почти то же.
pub fn tempo_gap(a: f32, b: f32) -> f32 {
    let ratio = (a / b).ln().abs();
    let octave = (ratio - std::f32::consts::LN_2).abs() + 0.15;
    ratio.min(octave) / 1.2f32.ln()
}

/// Волна плейлиста: от спокойного (0) к пику на двух третях (1) и мягкий спад.
fn wave(x: f32) -> f32 {
    const PEAK: f32 = 0.65;
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    if x < PEAK { 0.15 + 0.85 * smooth(x / PEAK) } else { 1.0 - 0.4 * smooth((x - PEAK) / (1.0 - PEAK)) }
}

struct Item {
    tempo: Option<f32>,
    intensity: f32,
    artist: String,
}

fn pair_cost(a: &Item, b: &Item) -> f32 {
    let mut cost = ((a.intensity - b.intensity) / 0.15).powi(2);
    if let (Some(x), Some(y)) = (a.tempo, b.tempo) {
        cost += tempo_gap(x, y).min(3.0).powi(2);
    }
    if !a.artist.is_empty() && a.artist == b.artist {
        cost += 3.0;
    }
    cost
}

/// Цена порядка: соседи и волна. Волна весит заметно, иначе резкий трек уезжает в начало.
fn order_cost(order: &[usize], items: &[Item], targets: &[f32]) -> f32 {
    let mut total = 0.0;
    let opening = (order.len() as f32 * 0.2).ceil() as usize;
    for (position, &index) in order.iter().enumerate() {
        let over = (items[index].intensity - targets[position]) / 0.2;
        total += 1.5 * over.powi(2);
        if position < opening && over > 0.0 {
            total += 3.0 * over.powi(2);
        }
        if position > 0 {
            total += pair_cost(&items[order[position - 1]], &items[index]);
        }
    }
    total
}

/// Порядок плейлиста: номера `tracks`.
pub fn daily_order(tracks: &[Track], feels: &[TrackFeel]) -> Vec<usize> {
    let count = tracks.len();
    if count < 3 {
        return (0..count).collect();
    }
    let items: Vec<Item> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let feel = feels.get(index).copied().unwrap_or_default();
            Item {
                tempo: feel.tempo.filter(|tempo| *tempo > 0.0),
                intensity: intensity(track, &feel),
                artist: track.owner.as_deref().or(track.artist.as_deref()).unwrap_or("").trim().to_lowercase(),
            }
        })
        .collect();
    let low = items.iter().map(|item| item.intensity).fold(f32::MAX, f32::min);
    let high = items.iter().map(|item| item.intensity).fold(f32::MIN, f32::max);
    let targets: Vec<f32> = (0..count).map(|position| low + (high - low) * wave(position as f32 / (count - 1) as f32)).collect();

    // Начало: самые спокойные треки — на самые спокойные места волны.
    let mut places: Vec<usize> = (0..count).collect();
    places.sort_by(|a, b| targets[*a].total_cmp(&targets[*b]).then(a.cmp(b)));
    let mut calm: Vec<usize> = (0..count).collect();
    calm.sort_by(|a, b| items[*a].intensity.total_cmp(&items[*b].intensity).then(a.cmp(b)));
    let mut order = vec![0; count];
    for (place, track) in places.iter().zip(&calm) {
        order[*place] = *track;
    }

    // Дальше — обмены и развороты отрезков, пока порядок улучшается.
    let mut best = order_cost(&order, &items, &targets);
    for _ in 0..40 {
        let mut improved = false;
        for first in 0..count {
            for second in first + 1..count {
                order.swap(first, second);
                let cost = order_cost(&order, &items, &targets);
                if cost + 1e-4 < best {
                    best = cost;
                    improved = true;
                } else {
                    order.swap(first, second);
                }
                order[first..=second].reverse();
                let cost = order_cost(&order, &items, &targets);
                if cost + 1e-4 < best {
                    best = cost;
                    improved = true;
                } else {
                    order[first..=second].reverse();
                }
            }
        }
        if !improved {
            break;
        }
    }
    order
}

// ───────────────────────────── база ─────────────────────────────

impl Library {
    /// Прослушивание с отметками (только при включённой статистике).
    pub fn record_play_as(&self, track: i64, listened_ms: u64, flags: u8) -> Result<(), LibraryError> {
        self.connection.execute(
            "INSERT INTO plays (track_id, at, listened_ms, flags) SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
            (track, now(), listened_ms as i64, i64::from(flags)),
        )?;
        Ok(())
    }

    /// Пропуск: переключили до того, как прослушивание засчиталось.
    pub fn record_skip(&self, track: i64, listened_ms: u64, flags: u8) -> Result<(), LibraryError> {
        let at = now();
        self.connection.execute(
            "INSERT INTO skips (track_id, at, listened_ms, flags) SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
            (track, at, listened_ms as i64, i64::from(flags)),
        )?;
        self.connection.execute("DELETE FROM skips WHERE at < ?1", [at - HORIZON])?;
        Ok(())
    }

    fn daily_history(&self, since: i64) -> Result<History, LibraryError> {
        let mut history = History::default();
        let mut statement = self.connection.prepare(
            "SELECT p.track_id, p.at, p.listened_ms, p.flags, COALESCE(t.duration_ms, 0) FROM plays p
             CROSS JOIN tracks t ON t.id = p.track_id WHERE p.at >= ?1 ORDER BY p.at, p.id",
        )?;
        let event = |row: &rusqlite::Row<'_>| {
            Ok(Event { track: row.get(0)?, at: row.get(1)?, ms: row.get(2)?, flags: row.get::<_, i64>(3)? as u8, length: row.get(4)? })
        };
        history.plays = statement.query_map([since], event)?.collect::<Result<_, _>>()?;
        let mut statement = self.connection.prepare(
            "SELECT s.track_id, s.at, s.listened_ms, s.flags, COALESCE(t.duration_ms, 0) FROM skips s
             CROSS JOIN tracks t ON t.id = s.track_id WHERE s.at >= ?1",
        )?;
        history.skips = statement.query_map([since], event)?.collect::<Result<_, _>>()?;
        let mut statement = self.connection.prepare("SELECT track_id, added FROM favorites")?;
        history.likes = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(history)
    }

    /// Хватает ли библиотеки и статистики для плейлиста дня.
    pub fn daily_readiness(&self) -> Result<DailyReadiness, LibraryError> {
        let now = now();
        let tracks: i64 = self.connection.query_row("SELECT COUNT(*) FROM tracks WHERE missing = 0", [], |row| row.get(0))?;
        let first: Option<i64> = self.connection.query_row("SELECT MIN(at) FROM plays", [], |row| row.get(0))?;
        let history = self.daily_history(now - HORIZON)?;
        Ok(DailyReadiness {
            tracks: tracks.max(0) as u64,
            plays: tasteful(&history.plays).len() as u64,
            days: first.map_or(0, |first| (now - first).max(0) / DAY),
        })
    }

    /// Треки плейлиста дня без порядка. `rebuild` — собрать заново, прежний — в «не повторять».
    pub fn daily_pick(&self, day: i64, options: &DailyOptions, rebuild: bool) -> Result<Vec<Track>, LibraryError> {
        let now = now();
        let mut history = self.daily_history(now - HORIZON)?;
        let mut statement =
            self.connection.prepare("SELECT day, tracks FROM daily WHERE day <= ?1 ORDER BY day DESC LIMIT 5")?;
        let lists: Vec<(i64, String)> =
            statement.query_map([day], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
        let mut rebuilt = None;
        let mut earlier = Vec::new();
        for (list_day, ids) in lists {
            let ids: HashSet<i64> = ids.split(',').filter_map(|id| id.parse().ok()).collect();
            if list_day == day {
                if rebuild {
                    rebuilt = Some(ids);
                }
            } else if earlier.len() < PAST_FACTORS.len() {
                let factor = PAST_FACTORS[earlier.len()];
                earlier.push((ids, factor));
            }
        }
        // Не больше трети — ни из вчерашнего, ни из пересобираемого сегодняшнего.
        history.last = earlier.first().map(|(ids, _)| ids.clone()).unwrap_or_default();
        history.last.extend(rebuilt.iter().flatten().copied());
        history.past = rebuilt.map(|ids| (ids, REBUILT_FACTOR)).into_iter().chain(earlier).collect();

        let mut statement = self.connection.prepare(
            "SELECT id, COALESCE(owner, artist, folder_artist, ''), COALESCE(album, folder_album, ''), COALESCE(genre, '')
             FROM tracks WHERE missing = 0 AND problem IS NULL AND duration_ms BETWEEN ?1 AND ?2 ORDER BY id",
        )?;
        let candidates: Vec<Candidate> = statement
            .query_map([SHORTEST_MS, LONGEST_MS], |row| {
                Ok(Candidate {
                    id: row.get(0)?,
                    artist: row.get::<_, String>(1)?.trim().to_lowercase(),
                    album: row.get::<_, String>(2)?.trim().to_lowercase(),
                    genre: row.get::<_, String>(3)?.to_lowercase(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let chosen = choose(&candidates, &history, options, now);
        let ids: Vec<i64> = chosen.iter().map(|index| candidates[*index].id).collect();
        self.tracks_by_ids(&ids)
    }

    /// Треки по номерам, в том же порядке; пропавшие файлы пропускаются.
    fn tracks_by_ids(&self, ids: &[i64]) -> Result<Vec<Track>, LibraryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let found = self.query(
            &format!(
                "SELECT {TRACK_COLUMNS} FROM tracks t JOIN roots r ON r.id = t.root_id
                 WHERE t.missing = 0 AND t.id IN ({list})"
            ),
            [],
        )?;
        let mut by_id: HashMap<i64, Track> = found.into_iter().map(|track| (track.id, track)).collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Плейлист дня `day`, если он уже собран: треки по порядку и когда собран.
    pub fn daily_list(&self, day: i64) -> Result<Option<(Vec<Track>, i64)>, LibraryError> {
        let row: Option<(String, i64)> = self
            .connection
            .query_row("SELECT tracks, created FROM daily WHERE day = ?1", [day], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let Some((ids, created)) = row else {
            return Ok(None);
        };
        let ids: Vec<i64> = ids.split(',').filter_map(|id| id.parse().ok()).collect();
        Ok(Some((self.tracks_by_ids(&ids)?, created)))
    }

    /// Запоминает плейлист дня. Хранятся последние шесть.
    pub fn save_daily(&self, day: i64, tracks: &[i64]) -> Result<(), LibraryError> {
        let list = tracks.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.connection.execute(
            "INSERT INTO daily (day, created, tracks) VALUES (?1, ?2, ?3)
             ON CONFLICT (day) DO UPDATE SET created = excluded.created, tracks = excluded.tracks",
            (day, now(), list),
        )?;
        self.connection
            .execute("DELETE FROM daily WHERE day NOT IN (SELECT day FROM daily ORDER BY day DESC LIMIT 6)", [])?;
        Ok(())
    }

    /// Сколько треков с текстом песни и сколько всего.
    pub fn lyrics_share(&self) -> Result<(u64, u64), LibraryError> {
        let (with, total): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*) FILTER (WHERE lyrics != 0), COUNT(*) FROM tracks WHERE missing = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((with.max(0) as u64, total.max(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn track(genre: &str, artist: &str) -> Track {
        Track {
            id: 0,
            path: Default::default(),
            title: None,
            artist: Some(artist.into()),
            album: None,
            album_artist: None,
            year: None,
            track_no: None,
            disc_no: None,
            duration_ms: None,
            format: None,
            problem: None,
            owner: None,
            folder_artist: None,
            folder_album: None,
            bit_depth: None,
            sample_rate: None,
            channels: None,
            bitrate: None,
            genre: Some(genre.into()),
            composer: None,
            conductor: None,
            work: None,
            movement: None,
            movement_no: None,
            lyrics: false,
        }
    }

    /// Библиотека: 40 исполнителей по 3 альбома по 5 треков.
    fn library() -> Vec<Candidate> {
        let mut candidates = Vec::new();
        for artist in 0..40 {
            for album in 0..3 {
                for number in 0..5 {
                    candidates.push(Candidate {
                        id: (artist * 100 + album * 10 + number) as i64,
                        artist: format!("artist {artist}"),
                        album: format!("album {album}"),
                        genre: if artist < 20 { "rock".into() } else { "ambient".into() },
                    });
                }
            }
        }
        candidates
    }

    /// Заход: `tracks` подряд по 3 минуты, закончился в `end`.
    fn session(tracks: &[i64], end: i64, flags: u8) -> Vec<Event> {
        let count = tracks.len() as i64;
        tracks
            .iter()
            .enumerate()
            .map(|(index, track)| Event {
                track: *track,
                at: end - (count - 1 - index as i64) * 180,
                ms: 180_000,
                flags,
                length: 180_000,
            })
            .collect()
    }

    fn options(seed: u64) -> DailyOptions {
        DailyOptions { length: 25, fresh: 0.3, seed, force: false }
    }

    #[test]
    fn day_starts_at_five_in_the_morning() {
        // 04:59 и 05:00 по местному времени (UTC+3) — разные дни плейлиста.
        let midnight_utc = 1_800_057_600 - 1_800_057_600 % DAY;
        let local = |hour: i64, minute: i64| midnight_utc + hour * 3600 + minute * 60 - 3 * 3600;
        assert_eq!(daily_day(local(4, 59), 3 * 3600) + 1, daily_day(local(5, 0), 3 * 3600));
        assert_eq!(daily_day(local(5, 0), 3 * 3600), daily_day(local(23, 59), 3 * 3600));
    }

    /// Плейлист дня, таймер сна и короткие заходы о вкусе не говорят.
    #[test]
    fn only_real_choices_count() {
        let mut plays = Vec::new();
        plays.extend(session(&[1, 2, 3], NOW - 10 * DAY, 0)); // короткий заход
        plays.extend(session(&[4, 5, 6, 7, 8, 9], NOW - 5 * DAY, 0));
        plays.extend(session(&[10, 11, 12, 13, 14], NOW - 3 * DAY, PLAY_DAILY));
        let mut sleepy = session(&[15, 16, 17, 18, 19], NOW - DAY, 0);
        sleepy[3].flags = PLAY_SLEEPY;
        sleepy[4].flags = PLAY_SLEEPY;
        plays.extend(sleepy);
        let counted: Vec<i64> = tasteful(&plays).iter().map(|play| play.track).collect();
        assert_eq!(counted, [4, 5, 6, 7, 8, 9, 15, 16, 17]);
        // Три длинных трека подряд — уже не короткий заход.
        let long: Vec<Event> = (0..3)
            .map(|index| Event { track: 30 + index, at: NOW - (2 - index) * 600, ms: 600_000, flags: 0, length: 600_000 })
            .collect();
        assert_eq!(tasteful(&long).len(), 3);
    }

    fn history_for(artist_loved: i64, days_ago: i64) -> History {
        let mut history = History::default();
        // Любимые треки исполнителя: альбом 0, по 6 раз; заходы длинные.
        for round in 0..6 {
            let tracks: Vec<i64> = (0..5).map(|number| artist_loved * 100 + number).collect();
            history.plays.extend(session(&tracks, NOW - days_ago * DAY - round * DAY, 0));
        }
        history.plays.sort_by_key(|play| play.at);
        history
    }

    #[test]
    fn familiar_and_fresh_come_from_what_is_loved() {
        let candidates = library();
        let mut history = history_for(3, 2);
        for id in [2501, 2502, 2503] {
            history.likes.insert(id, NOW - 30 * DAY);
        }
        let chosen = choose(&candidates, &history, &options(7), NOW);
        assert_eq!(chosen.len(), 25, "библиотеки хватает на весь плейлист");
        let ids: HashSet<i64> = chosen.iter().map(|index| candidates[*index].id).collect();
        // Любимое — в плейлисте, новое — не наугад.
        assert!(ids.contains(&2501) || ids.contains(&2502) || ids.contains(&2503));
        let loved = ids.iter().filter(|id| **id / 100 == 3).count();
        assert!((2..=3).contains(&loved), "не больше трёх треков одного исполнителя: {loved}");
        for id in &ids {
            let candidate = candidates.iter().find(|candidate| candidate.id == *id).unwrap();
            let related = candidate.artist == "artist 3" || candidate.artist == "artist 25" || candidate.genre == "rock" || candidate.genre == "ambient";
            assert!(related);
        }
        // При тех же данных и зерне — тот же выбор.
        assert_eq!(chosen, choose(&candidates, &history, &options(7), NOW));
    }

    /// Принудительно без статистики — всё равно плейлист, не больше трёх от исполнителя.
    #[test]
    fn forced_build_fills_from_the_library() {
        let candidates = library();
        let history = History::default();
        assert!(choose(&candidates, &history, &options(3), NOW).is_empty(), "без вкуса и без принуждения — пусто");
        let forced = choose(&candidates, &history, &DailyOptions { force: true, ..options(3) }, NOW);
        assert_eq!(forced.len(), 25);
        let mut artists: HashMap<&str, usize> = HashMap::new();
        for index in &forced {
            *artists.entry(candidates[*index].artist.as_str()).or_insert(0) += 1;
        }
        assert!(artists.values().all(|count| *count <= 3), "{artists:?}");
        // Мало вкуса — сначала он, остальное случайное.
        let loved = choose(&candidates, &history_for(3, 2), &DailyOptions { force: true, ..options(3) }, NOW);
        assert_eq!(loved.len(), 25);
        assert!(loved.iter().filter(|index| candidates[**index].artist == "artist 3").count() >= 2);
    }

    #[test]
    fn quick_skips_push_tracks_out() {
        let candidates = library();
        let mut history = history_for(3, 2);
        for track in 300..305 {
            for times in 0..6 {
                history.skips.push(Event { track, at: NOW - DAY - times * 3600, ms: 2_000, flags: 0, length: 180_000 });
            }
        }
        for seed in 0..10 {
            let chosen = choose(&candidates, &history, &options(seed), NOW);
            assert!(chosen.iter().all(|index| !(300..305).contains(&candidates[*index].id)));
        }
    }

    /// Вкус меняется: давно слушанное почти не весит.
    #[test]
    fn old_listening_fades() {
        let candidates = library();
        let mut history = history_for(5, 170);
        let recent = history_for(30, 1);
        history.plays.extend(recent.plays);
        history.plays.sort_by_key(|play| play.at);
        let mut old = 0;
        let mut new = 0;
        for seed in 0..20 {
            for index in choose(&candidates, &history, &options(seed), NOW) {
                match candidates[index].id / 100 {
                    5 => old += 1,
                    30 => new += 1,
                    _ => {}
                }
            }
        }
        assert!(new > old * 2, "свежий вкус важнее: {new} против {old}");
    }

    /// Вчерашний плейлист повторяется не больше чем на треть.
    #[test]
    fn yesterday_is_not_repeated() {
        let candidates = library();
        let mut history = History::default();
        for artist in 0..12 {
            for round in 0..6 {
                let tracks: Vec<i64> = (0..5).map(|number| artist * 100 + number).collect();
                history.plays.extend(session(&tracks, NOW - (2 + round) * DAY - artist * 7200, 0));
            }
        }
        history.plays.sort_by_key(|play| play.at);
        let first: Vec<i64> = choose(&candidates, &history, &options(1), NOW).iter().map(|index| candidates[*index].id).collect();
        let yesterday: HashSet<i64> = first.iter().copied().collect();
        history.past = vec![(yesterday.clone(), PAST_FACTORS[0])];
        history.last = yesterday.clone();
        let second: Vec<i64> = choose(&candidates, &history, &options(2), NOW).iter().map(|index| candidates[*index].id).collect();
        let repeated = second.iter().filter(|id| yesterday.contains(id)).count();
        assert!(repeated <= 7, "повторов {repeated} из {}", second.len());
        assert_eq!(second.len(), 25);
    }

    #[test]
    fn genres_know_their_temper() {
        assert!(genre_aggression("Ambient").unwrap() < 0.2);
        assert!(genre_aggression("Post-Rock").unwrap() < genre_aggression("Rock").unwrap());
        assert!(genre_aggression("Death Metal").unwrap() > 0.9);
        assert!(genre_aggression("Барокко").unwrap() < 0.3, "барокко — не рок");
        assert!(genre_aggression("Хип-хоп").is_some());
        let mixed = genre_aggression("Ambient; Metal").unwrap();
        assert!(mixed > 0.4 && mixed < 0.6);
        assert_eq!(genre_aggression("Speech"), None);
    }

    #[test]
    fn half_and_double_tempo_are_close() {
        assert!(tempo_gap(140.0, 70.0) < tempo_gap(140.0, 100.0));
        assert!(tempo_gap(120.0, 126.0) < 0.3);
        // 140 → 60 → 200 — плохо: оба скачка больше, чем 140 → 150.
        assert!(tempo_gap(60.0, 200.0) > 3.0);
        assert!(tempo_gap(140.0, 60.0) > tempo_gap(140.0, 150.0));
    }

    /// Порядок: спокойное начало, пик ближе к концу, без скачков темпа.
    #[test]
    fn order_rides_a_wave_without_jumps() {
        let genres = ["ambient", "folk", "pop", "rock", "metal"];
        let mut tracks = Vec::new();
        let mut feels = Vec::new();
        for index in 0..20 {
            let genre = genres[index % 5];
            tracks.push(track(genre, &format!("artist {index}")));
            let level = genre_aggression(genre).unwrap();
            feels.push(TrackFeel { tempo: Some(70.0 + level * 110.0 + (index % 3) as f32 * 4.0), energy: Some(level), words: None });
        }
        let order = daily_order(&tracks, &feels);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>(), "каждый трек ровно один раз");
        let level = |position: usize| feels[order[position]].energy.unwrap();
        let start: f32 = (0..4).map(level).sum::<f32>() / 4.0;
        let peak: f32 = (11..15).map(level).sum::<f32>() / 4.0;
        assert!(start < 0.35 && peak > 0.6, "начало {start}, пик {peak}");
        for pair in order.windows(2) {
            let (a, b) = (feels[pair[0]].tempo.unwrap(), feels[pair[1]].tempo.unwrap());
            assert!(tempo_gap(a, b) < 2.2, "скачок темпа {a} → {b}");
        }
    }

    /// Резкий трек не должен вставать первым.
    #[test]
    fn loud_outlier_does_not_open_the_playlist() {
        let rows: [(f32, f32, &str); 25] = [
            (160.0, 1.00, "Alternative & Indie"), (151.9, 0.72, "Punk / New Wave"), (142.9, 0.38, "Techno"),
            (125.0, 0.47, "Pop"), (126.3, 0.21, "Hip-Hop/Rap"), (120.0, 0.42, "Pop"), (120.0, 0.52, "Pop"),
            (131.9, 0.42, "Dance"), (125.0, 0.49, "Alternative & Indie"), (125.0, 0.56, "Punk / New Wave"),
            (109.1, 0.38, "Rock"), (113.2, 0.51, "Pop"), (107.1, 0.54, "Alternative & Indie"),
            (99.2, 0.83, "Alternative & Indie"), (120.0, 0.96, "Electronic"), (130.4, 0.81, "Rock"),
            (122.4, 0.74, "Punk / New Wave"), (120.0, 0.76, "Rock"), (123.7, 0.74, "Rock"),
            (129.0, 0.52, "Alternative & Indie"), (131.9, 0.63, "Rock"), (142.9, 0.40, "Techno"),
            (146.3, 0.49, "Punk / New Wave"), (162.2, 0.46, "Hip-Hop/Rap"), (155.8, 0.74, "Rock"),
        ];
        let tracks: Vec<Track> = rows.iter().enumerate().map(|(index, row)| track(row.2, &format!("artist {}", index % 7))).collect();
        let feels: Vec<TrackFeel> = rows.iter().map(|row| TrackFeel { tempo: Some(row.0), energy: Some(row.1), words: None }).collect();
        let order = daily_order(&tracks, &feels);
        let energy = |position: usize| feels[order[position]].energy.unwrap();
        for position in 0..4 {
            assert!(energy(position) < 0.6, "начало: {:?}", (0..6).map(energy).collect::<Vec<_>>());
        }
        let loudest = order.iter().position(|index| *index == 0).unwrap();
        assert!((8..22).contains(&loudest), "самый резкий трек на месте {loudest}");
    }

    /// Один исполнитель два раза подряд — только если иначе никак.
    #[test]
    fn same_artist_is_not_back_to_back() {
        let mut tracks = Vec::new();
        let mut feels = Vec::new();
        for index in 0..12 {
            tracks.push(track("pop", &format!("artist {}", index % 4)));
            feels.push(TrackFeel { tempo: Some(118.0 + index as f32), energy: Some(0.5), words: None });
        }
        let order = daily_order(&tracks, &feels);
        for pair in order.windows(2) {
            assert_ne!(tracks[pair[0]].artist, tracks[pair[1]].artist);
        }
    }

    /// На настоящей базе: готовность, сбор, сохранение и «собрать заново».
    #[test]
    fn builds_saves_and_rebuilds() {
        use nyanoto_testkit::{Scratch, encode_flac_tagged, signal};
        let scratch = Scratch::new("daily-db");
        for artist in 0..6 {
            for number in 0..6 {
                let (artist_name, title) = (format!("Исполнитель {artist}"), format!("Песня {number}"));
                let file = encode_flac_tagged(
                    &signal(512, 44_100),
                    44_100,
                    &[("ARTIST", artist_name.as_str()), ("TITLE", title.as_str()), ("GENRE", "Rock")],
                    0,
                );
                scratch.write(&format!("music/{artist_name}/Альбом/{number}.flac"), &file.bytes);
            }
        }
        let mut library = Library::open(&scratch.path("data/library.db")).unwrap();
        library.add_root(&scratch.path("music")).unwrap();
        library.scan(&crate::ScanProgress::default()).unwrap();
        library.connection.execute("UPDATE tracks SET duration_ms = 180000", []).unwrap();
        let ids: Vec<i64> = library
            .connection
            // По пути, а не по номеру: номера раздаются в порядке готовности потоков скана.
            .prepare("SELECT id FROM tracks ORDER BY rel_path")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(ids.len(), 36);
        // Три недели по заходу в день: первые 12 треков (два исполнителя), по кругу.
        let now = now();
        for day in 0..21 {
            for (index, id) in ids.iter().take(12).enumerate() {
                let at = now - (21 - day) * DAY + index as i64 * 180;
                let flags = if day == 20 { PLAY_DAILY } else { 0 };
                library
                    .connection
                    .execute("INSERT INTO plays (track_id, at, listened_ms, flags) VALUES (?1, ?2, 180000, ?3)", (id, at, flags))
                    .unwrap();
            }
        }
        let readiness = library.daily_readiness().unwrap();
        assert_eq!(readiness, DailyReadiness { tracks: 36, plays: 240, days: 21 });
        assert!(!readiness.ready(DailyThreshold::Early), "36 треков — мало");

        let today = daily_day(now, 0);
        let options = DailyOptions { length: 15, fresh: 0.3, seed: 1, force: false };
        let first = library.daily_pick(today, &options, false).unwrap();
        assert_eq!(first.len(), 15);
        let first_ids: Vec<i64> = first.iter().map(|track| track.id).collect();
        library.save_daily(today, &first_ids).unwrap();
        let (saved, _) = library.daily_list(today).unwrap().unwrap();
        assert_eq!(saved.iter().map(|track| track.id).collect::<Vec<_>>(), first_ids);
        // Заново — другой набор: из прежнего не больше трети.
        let again = library.daily_pick(today, &DailyOptions { seed: 2, ..options }, true).unwrap();
        let repeated = again.iter().filter(|track| first_ids.contains(&track.id)).count();
        assert!(repeated <= 5, "повторов {repeated}");
        // Хранятся последние шесть.
        for day in 1..10 {
            library.save_daily(today - day, &first_ids).unwrap();
        }
        let kept: i64 = library.connection.query_row("SELECT COUNT(*) FROM daily", [], |row| row.get(0)).unwrap();
        assert_eq!(kept, 6);
        library.record_skip(ids[0], 2_000, 0).unwrap();
        library.record_play_as(ids[1], 90_000, PLAY_CHOSEN).unwrap();
        let flags: i64 = library.connection.query_row("SELECT flags FROM plays ORDER BY id DESC LIMIT 1", [], |row| row.get(0)).unwrap();
        assert_eq!(flags, i64::from(PLAY_CHOSEN));
    }
}
