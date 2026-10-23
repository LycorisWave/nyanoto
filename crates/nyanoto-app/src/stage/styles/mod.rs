//! Стили сцен. У каждого свой «словарь», чужих приёмов не берут.
//! Всё от времени `t`, напряжения и атак (feel.rs) — сцена не стоит на месте.

mod block;
mod flight;
mod fx;
mod ink;
mod net;
mod orbits;
mod pinwheel;
mod scatter;
mod web;

use std::collections::HashMap;

use tiny_skia::{Color, Mask, Transform};

use super::feel::{CALM, FIERCE, Mood};
use super::motion::{self, Look, Rng};
use super::paint::{Canvas, Palette, faded};
use super::text::{Face, Fonts, Word};

/// Стиль сцены.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Плотный наклонённый блок слов разного размера.
    Block,
    /// Орбита: кольцо — светодиодный экран из точек, по нему бежит строка.
    Orbits,
    /// Слова строки разложены по экрану под разными углами.
    Scatter,
    /// Паутина: камера летит за словом.
    Web,
    /// Паутина каждой строки на экране.
    WebScreen,
    /// Созвездие: слова — звёзды на медленно вращающемся небе.
    Stars,
    /// Сеть: слова — узлы, между ними летят пакеты TCP/UDP.
    Packets,
    /// Сеть: бегущий лог как в tcpdump.
    Terminal,
    /// Сеть: узлы с пакетами и лог внизу.
    Network,
    /// Полёт сквозь слова.
    Flight,
    /// Вертушка: строки по сторонам квадрата.
    Pinwheel,
    /// Чернила: строка пишется пером, уходя — стекает подтёками.
    Ink,
}

impl Kind {
    /// Стили в настройках по порядку. «Пакеты» и «Чернила» скрыты, но живы — тесты через [`Kind::EVERY`].
    pub const ALL: [Kind; 10] = [
        Kind::Block,
        Kind::Orbits,
        Kind::Scatter,
        Kind::Web,
        Kind::WebScreen,
        Kind::Stars,
        Kind::Terminal,
        Kind::Network,
        Kind::Flight,
        Kind::Pinwheel,
    ];

    /// Все стили, со скрытыми.
    pub const EVERY: [Kind; 12] = [
        Kind::Block,
        Kind::Orbits,
        Kind::Scatter,
        Kind::Web,
        Kind::WebScreen,
        Kind::Stars,
        Kind::Packets,
        Kind::Terminal,
        Kind::Network,
        Kind::Flight,
        Kind::Pinwheel,
        Kind::Ink,
    ];

    /// Код для настроек.
    pub fn code(self) -> &'static str {
        match self {
            Kind::Block => "block",
            Kind::Orbits => "orbits",
            Kind::Scatter => "scatter",
            Kind::Web => "web",
            Kind::WebScreen => "webscreen",
            Kind::Stars => "stars",
            Kind::Packets => "packets",
            Kind::Terminal => "terminal",
            Kind::Network => "network",
            Kind::Flight => "flight",
            Kind::Pinwheel => "pinwheel",
            Kind::Ink => "ink",
        }
    }

    /// По коду из настроек. Скрытые «Пакеты» → «Узлы и лог».
    pub fn from_code(code: &str) -> Option<Kind> {
        if code == "packets" {
            return Some(Kind::Network);
        }
        Kind::EVERY.into_iter().find(|kind| kind.code() == code)
    }

    /// Из чего выбирать при таком напряжении (спокойно, средне, резко).
    pub fn family(energy: f32) -> &'static [Kind] {
        if energy < CALM {
            &[Kind::Stars, Kind::WebScreen, Kind::Orbits, Kind::Block, Kind::Web]
        } else if energy < FIERCE {
            &[Kind::Web, Kind::Pinwheel, Kind::Terminal, Kind::Block, Kind::Orbits, Kind::Network, Kind::WebScreen]
        } else {
            &[Kind::Scatter, Kind::Flight, Kind::Network, Kind::Web, Kind::Block, Kind::Pinwheel]
        }
    }

    /// Сколько у стиля видов (`Part::variant`).
    pub fn variants(self) -> u8 {
        if self == Kind::Orbits { 3 } else { 1 }
    }

    /// Случайный стиль для напряжения, не `previous`.
    pub fn pick(energy: f32, previous: Option<Kind>, rng: &mut Rng) -> Kind {
        let family: Vec<Kind> = Kind::family(energy).iter().copied().filter(|kind| Some(*kind) != previous).collect();
        family[rng.below(family.len())]
    }
}

/// Шрифт песни — по жанру из тегов, а без жанра — по напряжению части.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Letters {
    /// Узкий гротеск Oswald: рок, панк, метал.
    Narrow,
    /// Широкий гротеск Unbounded: хип-хоп, электроника.
    Wide,
    /// Антиква Playfair Display: поп, баллады, джаз, классика.
    Serif,
}

/// Сначала широкие жанры (synthpop — электроника), потом узкие (pop punk — панк).
const WIDE_GENRES: &[&str] = &[
    "electro", "edm", "techno", "house", "trance", "dubstep", "drum", "dnb", "synth", "dance", "hip-hop", "hip hop",
    "hiphop", "rap", "trap", "phonk", "grime", "drill", "club", "электро", "хип-хоп", "рэп", "реп", "техно", "хаус", "танц",
];
const NARROW_GENRES: &[&str] = &[
    "rock", "metal", "punk", "grunge", "hardcore", "emo", "new wave", "рок", "метал", "панк", "гранж",
];
const SERIF_GENRES: &[&str] = &[
    "pop", "ballad", "jazz", "blues", "soul", "r&b", "rnb", "classic", "folk", "acoustic", "singer", "chanson", "lounge",
    "ambient", "lo-fi", "lofi", "country", "romance", "поп", "баллад", "джаз", "блюз", "соул", "классик", "фолк",
    "акуст", "шансон", "романс", "бард", "эстрад",
];

impl Letters {
    /// Шрифт по тегу жанра; `None` — выбирать по напряжению.
    pub fn for_genre(genre: &str) -> Option<Letters> {
        let genre = genre.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| genre.contains(word));
        if has(WIDE_GENRES) {
            Some(Letters::Wide)
        } else if has(NARROW_GENRES) {
            Some(Letters::Narrow)
        } else if has(SERIF_GENRES) {
            Some(Letters::Serif)
        } else {
            None
        }
    }

    /// Шрифт части без жанра — по напряжению.
    pub fn for_mood(strong: bool) -> Letters {
        if strong { Letters::Narrow } else { Letters::Serif }
    }
}

/// Строка песни на шкале времени (секунды трека).
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    /// Начало каждого слова (`split_whitespace`).
    pub words: Vec<f32>,
    /// Времена слов оценены по длине слов (пословных меток в LRC не было).
    pub estimated: bool,
    pub start: f32,
    pub end: f32,
    pub chorus: bool,
}

/// Часть песни, которую рисует одна сцена.
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub kind: Kind,
    /// Строки части: `first..=last`.
    pub first: usize,
    pub last: usize,
    pub start: f32,
    pub end: f32,
    pub seed: u64,
    /// Громкая часть: жирнее, заглавными, с заливкой на припеве.
    pub strong: bool,
    /// Шрифт.
    pub letters: Letters,
    /// Вариант сцены (у орбиты: кольцо, два, цепь) — через `Luck`.
    pub variant: u8,
}

/// Всё, что сцене нужно знать о кадре.
pub struct Ctx<'a> {
    pub t: f32,
    pub width: f32,
    pub height: f32,
    pub palette: Palette,
    pub lines: &'a [Line],
    pub mood: &'a Mood,
    /// На сколько секунд кадр опережает звук.
    pub lead: f32,
}

impl Ctx<'_> {
    pub fn unit(&self) -> f32 {
        self.width.min(self.height)
    }

    /// Напряжение момента.
    pub fn energy(&self) -> f32 {
        self.mood.at(self.t)
    }

    /// «Время песни»: в напряжённых местах быстрее (до ×1,5), в тихих медленнее (×0,5).
    /// Скорость следует за музыкой, а положение не скачет.
    pub fn drift(&self) -> f32 {
        0.5 * self.t + self.mood.travel(self.t)
    }

    /// Громкость момента внутри песни (0…1) — этим сцены «дышат».
    pub fn level(&self) -> f32 {
        self.mood.level(self.t)
    }

    /// Момент песни, который сейчас слышен.
    pub fn heard(&self) -> f32 {
        self.t - self.lead
    }

    /// Сколько долей прошло к слышимому моменту (дробное): для шагов в такт.
    pub fn beats(&self) -> f32 {
        self.mood.count(self.heard())
    }

    /// Шаг в такт: доля — сдвиг на 1, внутри доли — мягко.
    pub fn steps(&self) -> f32 {
        let count = self.beats();
        count.floor() + motion::ease_out(count.fract())
    }

    /// Текущая строка части (последняя начавшаяся).
    pub fn current(&self, part: &Part) -> Option<usize> {
        (part.first..=part.last).rev().find(|index| self.lines.get(*index).is_some_and(|line| line.start <= self.t))
    }

    /// Строки части, начавшиеся до `t` и ещё не ушедшие дальше `keep` секунд.
    pub fn recent(&self, part: &Part, keep: f32) -> impl Iterator<Item = usize> + '_ {
        let t = self.t;
        (part.first..=part.last.min(self.lines.len().saturating_sub(1)))
            .filter(move |index| self.lines[*index].start <= t + 0.5 && self.lines[*index].end + keep >= t)
    }
}

/// Сцена — рисует свою часть песни.
pub trait Scene {
    /// Кадр в момент `ctx.t`; `visible` — насколько видна часть (переходы).
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32);

    /// Через сколько секунд трека нужен следующий кадр.
    fn pace(&self, ctx: &Ctx) -> f32 {
        // Сцены всегда в движении: спокойно — 30 кадров в секунду, резко — 60.
        if ctx.energy() > 0.5 { 1.0 / 60.0 } else { 1.0 / 30.0 }
    }
}

pub fn create(part: &Part) -> Box<dyn Scene> {
    match part.kind {
        Kind::Block => Box::new(block::Block::new(part)),
        Kind::Orbits => Box::new(orbits::Orbits::new(part)),
        Kind::Scatter => Box::new(scatter::Scatter::new(part)),
        Kind::Web => Box::new(web::Web::new(part, web::Variant::Camera)),
        Kind::WebScreen => Box::new(web::Web::new(part, web::Variant::Screen)),
        Kind::Stars => Box::new(web::Web::new(part, web::Variant::Stars)),
        Kind::Packets => Box::new(net::Net::new(part, net::Variant::Packets)),
        Kind::Terminal => Box::new(net::Net::new(part, net::Variant::Terminal)),
        Kind::Network => Box::new(net::Net::new(part, net::Variant::Both)),
        Kind::Flight => Box::new(flight::Flight::new(part)),
        Kind::Pinwheel => Box::new(pinwheel::Pinwheel::new(part)),
        Kind::Ink => Box::new(ink::Ink::new(part)),
    }
}

/// Служебные слова — мелко: крупно стоят только значимые.
const SMALL_WORDS: &[&str] = &[
    "и", "а", "но", "в", "во", "на", "не", "ни", "я", "ты", "он", "она", "оно", "мы", "вы", "они", "мне", "меня", "тебя",
    "тебе", "мой", "моя", "моё", "мое", "твой", "твоя", "с", "со", "к", "ко", "по", "за", "из", "у", "о", "об", "от",
    "до", "же", "ли", "бы", "то", "что", "как", "так", "да", "это", "все", "всё", "где", "там", "тут", "вот", "уже",
    "ещё", "еще", "для", "при", "без", "над", "под", "про", "или", "если", "the", "a", "an", "and", "or", "but", "to",
    "of", "in", "on", "at", "by", "for", "with", "from", "i", "you", "he", "she", "it", "we", "they", "me", "my",
    "your", "our", "is", "am", "are", "was", "be", "so", "as", "if", "that", "this", "its", "it's", "i'm", "can", "do",
    "no", "not", "just", "all", "up",
];

pub fn is_key(raw: &str) -> bool {
    let bare: String =
        raw.chars().filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '’').collect::<String>().to_lowercase();
    bare.chars().count() > 2 && !SMALL_WORDS.contains(&bare.as_str())
}

/// Слово строки, разложенное кеглем 1.
pub struct Piece {
    pub word: Word,
    pub at: f32,
    pub key: bool,
}

/// Начертания части: главные слова, служебные, заглавными ли.
pub fn faces(part: &Part) -> (Face, Face, bool) {
    match (part.letters, part.strong) {
        (Letters::Narrow, true) => (Face::Heavy, Face::Light, true),
        (Letters::Narrow, false) => (Face::Narrow, Face::NarrowThin, true),
        (Letters::Wide, true) => (Face::WideHeavy, Face::WideThin, true),
        (Letters::Wide, false) => (Face::Wide, Face::WideThin, false),
        (Letters::Serif, true) => (Face::SerifBold, Face::SerifItalic, false),
        (Letters::Serif, false) => (Face::Serif, Face::SerifItalic, false),
    }
}

pub fn pieces(fonts: &mut Fonts, line: &Line, part: &Part) -> Vec<Piece> {
    let (key_face, small_face, upper) = faces(part);
    line.text
        .split_whitespace()
        .enumerate()
        .map(|(index, raw)| {
            let shown = if upper { raw.to_uppercase() } else { raw.to_string() };
            let key = is_key(raw);
            Piece {
                word: fonts.word(&shown, if key { key_face } else { small_face }),
                at: line.words.get(index).copied().unwrap_or(line.start),
                key,
            }
        })
        .collect()
}

/// Середина закрашенной части слова кеглем 1.
pub fn ink_center(word: &Word) -> (f32, f32) {
    ((word.ink.left() + word.ink.right()) / 2.0, (word.ink.top() + word.ink.bottom()) / 2.0)
}

/// Раскладки строк, построенные по требованию; дальние выбрасываются.
pub struct Lazy<T> {
    map: HashMap<usize, T>,
}

impl<T> Default for Lazy<T> {
    fn default() -> Self {
        Self { map: HashMap::new() }
    }
}

impl<T> Lazy<T> {
    pub fn get_or(&mut self, index: usize, build: impl FnOnce() -> T) -> &mut T {
        self.map.entry(index).or_insert_with(build)
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.map.get(&index)
    }

    pub fn keep(&mut self, keep: impl Fn(usize) -> bool) {
        self.map.retain(|index, _| keep(*index));
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// Слово целиком, по буквам (`letters` 0…1) или разлетом (`letters` < 0).
#[allow(clippy::too_many_arguments)]
pub fn draw_word(
    canvas: &mut Canvas,
    word: &Word,
    base: Transform,
    pivot: (f32, f32),
    size: f32,
    look: &Look,
    color: Color,
    mask: Option<&Mask>,
) {
    if look.alpha <= 0.003 {
        return;
    }
    let placed = look.place(base, pivot, size);
    if look.letters >= 1.0 {
        canvas.word(word, faded(color, look.alpha), placed, mask);
        return;
    }
    let count = word.glyphs.len().max(1) as f32;
    let (middle, _) = ink_center(word);
    for (index, glyph) in word.glyphs.iter().enumerate() {
        let Some(path) = &glyph.path else {
            continue;
        };
        let (alpha, local) = if look.letters >= 0.0 {
            // По буквам: каждая следующая проявляется за предыдущей.
            (motion::clamp01(look.letters * count - index as f32), Transform::from_translate(glyph.x, 0.0))
        } else {
            // Разлёт: буквы уходят от середины слова и кружатся.
            let spread = -look.letters;
            let mut rng = Rng::new(index as u64 * 7919 + word.glyphs.len() as u64);
            let away = glyph.x + glyph.advance / 2.0 - middle;
            let dx = away * 1.2 * spread + rng.range(-0.6, 0.6) * spread;
            let dy = rng.range(-1.4, 0.8) * spread;
            let turn = rng.range(-90.0, 90.0) * spread;
            let local = Transform::from_translate(-glyph.advance / 2.0, 0.35)
                .post_rotate(turn)
                .post_translate(glyph.x + glyph.advance / 2.0 + dx, -0.35 + dy);
            (1.0, local)
        };
        if alpha > 0.003 {
            canvas.fill(path, faded(color, alpha * look.alpha), local.post_concat(placed), mask);
        }
    }
}

/// Задевает ли слово полосу заливки — только такие перерисовываем цветом фона.
pub fn near_band(pivot: (f32, f32), reach: f32, center: (f32, f32), degrees: f32, thickness: f32) -> bool {
    let angle = degrees.to_radians();
    ((pivot.0 - center.0) * -angle.sin() + (pivot.1 - center.1) * angle.cos()).abs() <= thickness / 2.0 + reach
}

/// Насколько видны ещё не спетые слова в темах «строка целиком».
pub const WAITING: f32 = 0.2;

/// Строка видна сразу: слово бледное, в момент пения разгорается.
pub fn line_look(t: f32, line_start: f32, at: f32, length: f32) -> Look {
    let shown = motion::ease_out(motion::progress(t, line_start - 0.15, 0.5));
    let p = motion::progress(t, at, length);
    let glow = motion::ease_out(p);
    let scale = 1.0 + 0.04 * (1.0 - glow) * if p > 0.0 { 1.0 } else { 0.0 };
    Look { alpha: shown * (WAITING + (1.0 - WAITING) * glow), scale_x: scale, scale_y: scale, ..Look::STILL }
}

/// Наклон полосы один на весь припев — не крестом.
pub fn band_angle(ctx: &Ctx, part: &Part, index: usize, from: f32, to: f32) -> f32 {
    let mut first = index;
    while first > part.first && ctx.lines.get(first - 1).is_some_and(|line| line.chorus) {
        first -= 1;
    }
    let mut rng = Rng::new(motion::mix(part.seed ^ 0xBA4D, first as u64));
    rng.sign() * rng.range(from, to)
}

/// Заливка припева у строки `index`: `(откуда, докуда)` по длине полосы.
pub fn chorus_band(ctx: &Ctx, part: &Part, index: usize, tempo: f32) -> Option<(f32, f32)> {
    let line = ctx.lines.get(index)?;
    // Полоса — только в громких частях, в тихих припев просто крупнее.
    if !line.chorus || !part.strong {
        return None;
    }
    let before = index > part.first && ctx.lines[index - 1].chorus;
    let after = index < part.last && ctx.lines.get(index + 1).is_some_and(|next| next.chorus);
    let grow = if before { 1.0 } else { motion::ease_out(motion::progress(ctx.t, line.start, 0.45 * tempo)) };
    let leave = if after { 0.0 } else { motion::ease_in(motion::progress(ctx.t, line.end, 0.5 * tempo)) };
    (grow > 0.0 && leave < 1.0 && ctx.t >= line.start).then_some((leave, grow))
}
