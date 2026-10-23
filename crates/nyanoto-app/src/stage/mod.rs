//! Сцены текста в духе Cotodama Lyric Speaker.
//!
//! Кадр рисует tiny-skia: программный Slint не поворачивает текст. Настроение — из звука (feel.rs).
//! Режимы: одна сцена на песню или смена по ходу (напряжение, границы припева).

pub mod feel;
mod fluid;
mod motion;
mod paint;
mod styles;
mod text;

use std::collections::HashMap;
use std::time::Instant;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{PixmapMut, Transform};

use crate::lyrics::Lyrics;
use feel::Mood;
use motion::{Rng, ease_in_out, progress};
pub use paint::Palette;
pub use styles::Kind;
use styles::{Ctx, Letters, Line, Part, Scene};
use text::Fonts;

/// Как выбираются сцены.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Одна сцена на всю песню — по её настроению.
    Song,
    /// Сцена меняется по ходу песни — по напряжению момента.
    Moment,
    /// Всегда этот стиль.
    Only(Kind),
}

/// Дольше строка на экране не держится: дальше пауза, а не пение.
const LINE_SPAN: f32 = 10.0;
/// Пауза длиннее — новая часть (в режиме «по ходу песни»).
const PART_GAP: f32 = 5.0;
/// Насколько должно измениться напряжение, чтобы сменилась сцена.
const MOOD_STEP: f32 = 0.16;
/// С какого напряжения — жирный узкий шрифт заглавными.
const STRONG_FROM: f32 = 0.5;
/// Переход между частями: прошлая тает, новая проявляется.
const CROSSFADE: f32 = 0.7;

/// Окно сцены: размер в логических точках, плотность точек экрана, цвета.
pub struct View {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub palette: Palette,
    /// На сколько секунд трека кадр опережает звук.
    pub lead: f32,
    /// Не чаще стольких кадров в секунду.
    pub max_fps: f32,
}

#[derive(Default)]
pub struct Stage {
    /// Шрифты читаются при первом кадре: без сцен они не нужны.
    fonts: Option<Fonts>,
    lines: Vec<Line>,
    parts: Vec<Part>,
    choice: Option<Choice>,
    /// Какие виды сцен выпадали — живёт между треками.
    luck: Luck,
    /// Шрифт по жанру песни (`None` — по напряжению частей).
    letters: Option<Letters>,
    salt: u64,
    mood: Mood,
    scenes: HashMap<usize, Box<dyn Scene>>,
    size: (u32, u32),
    /// Два буфера по очереди: окно показывает один, пока рисуется другой.
    buffers: [Option<SharedPixelBuffer<Rgba8Pixel>>; 2],
    front: usize,
    spare: Option<tiny_skia::Mask>,
}

impl Stage {
    /// Новый текст. `salt` — каждый показ другой, `mood` — настроение, `genre` — для шрифта.
    pub fn set_lyrics(&mut self, lyrics: &Lyrics, choice: Choice, salt: u64, mood: Option<Mood>, genre: &str) {
        self.choice = Some(choice);
        self.salt = salt;
        self.letters = Letters::for_genre(genre);
        self.mood = mood.clone().unwrap_or_else(|| Mood::neutral(0.5));
        self.lines.clear();
        let timed: Vec<(usize, f32)> = lyrics
            .lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.at.map(|at| (index, at.as_secs_f32())))
            .collect();
        for (place, &(index, start)) in timed.iter().enumerate() {
            let line = &lyrics.lines[index];
            if line.text.trim().is_empty() {
                continue;
            }
            let next = timed.get(place + 1).map_or(start + LINE_SPAN, |(_, at)| *at);
            let end = next.min(start + LINE_SPAN).max(start + 0.3);
            self.lines.push(Line {
                text: line.text.clone(),
                words: lyrics.word_starts(index).into_iter().map(|at| at.as_secs_f32()).collect(),
                estimated: line.words.iter().all(Option::is_none),
                start,
                end,
                chorus: lyrics.chorus.get(index).copied().unwrap_or(false),
            });
        }
        if mood.is_some() {
            self.snap_words();
        }
        self.plan();
    }

    /// Настроение по звуку пришло (разбор идёт в фоне): сцены выбираются заново.
    pub fn set_mood(&mut self, mood: Mood) {
        self.mood = mood;
        self.snap_words();
        self.plan();
    }

    /// Притягивает слова к ближайшей атаке (до 0,12 с). Первое слово и метки из LRC не трогаем.
    fn snap_words(&mut self) {
        const REACH: f32 = 0.12;
        for line in self.lines.iter_mut().filter(|line| line.estimated) {
            let mut previous = line.start;
            for (number, at) in line.words.iter_mut().enumerate() {
                if number > 0 {
                    let nearest = self
                        .mood
                        .beats_between(*at - REACH, *at + REACH)
                        .iter()
                        .copied()
                        .filter(|beat| *beat > previous + 0.05 && *beat < line.end)
                        .min_by(|a, b| (a - *at).abs().total_cmp(&(b - *at).abs()));
                    if let Some(beat) = nearest {
                        *at = beat;
                    }
                }
                *at = at.max(previous);
                previous = *at;
            }
        }
    }

    pub fn has_lyrics(&self) -> bool {
        self.choice.is_some()
    }

    /// Забыть текст (новый трек).
    pub fn clear(&mut self) {
        self.choice = None;
        self.lines.clear();
        self.parts.clear();
        self.scenes.clear();
        self.mood = Mood::default();
    }

    /// Какие стили сейчас на экране (для журнала замеров).
    pub fn shown(&self, t: f32) -> String {
        (0..self.parts.len()).filter(|index| self.visibility(*index, t) > 0.0).map(|index| self.parts[index].kind.code()).collect::<Vec<_>>().join("+")
    }

    /// Освобождает буферы кадров и раскладки, когда сцену не видно.
    pub fn release(&mut self) {
        self.buffers = [None, None];
        self.spare = None;
        self.scenes.clear();
    }

    /// Делит песню на части и выбирает им стили.
    fn plan(&mut self) {
        self.parts.clear();
        self.scenes.clear();
        let Some(choice) = self.choice else {
            return;
        };
        if self.lines.is_empty() {
            return;
        }
        let mut rng = Rng::new(self.salt);
        let genre = self.letters;
        let letters = |strong: bool| genre.unwrap_or_else(|| Letters::for_mood(strong));
        let whole = |kind: Kind, strong: bool, seed: u64, lines: &[Line]| Part {
            kind,
            first: 0,
            last: lines.len() - 1,
            start: lines[0].start,
            end: lines[lines.len() - 1].end,
            seed,
            strong,
            letters: letters(strong),
            variant: 0,
        };
        let song = self.mood.song;
        match choice {
            Choice::Song => {
                let kind = Kind::pick(song, None, &mut rng);
                self.parts.push(whole(kind, song >= STRONG_FROM, rng.next(), &self.lines));
            }
            Choice::Only(kind) => self.parts.push(whole(kind, song >= STRONG_FROM, rng.next(), &self.lines)),
            Choice::Moment => {
                let energies: Vec<f32> = self.lines.iter().map(|line| self.mood.mean(line.start, line.end)).collect();
                let mut first = 0;
                let mut previous: Option<Kind> = None;
                for index in 1..=self.lines.len() {
                    let part_energy = energies[first..index].iter().sum::<f32>() / (index - first) as f32;
                    let most = if part_energy < feel::CALM { 6 } else if part_energy < feel::FIERCE { 4 } else { 3 };
                    let split = index == self.lines.len()
                        || index - first >= most
                        || (energies[index] - part_energy).abs() > MOOD_STEP
                        || self.lines[index].chorus != self.lines[index - 1].chorus
                        || self.lines[index].start - self.lines[index - 1].end > PART_GAP;
                    if split {
                        let kind = Kind::pick(part_energy, previous, &mut rng);
                        previous = Some(kind);
                        self.parts.push(Part {
                            kind,
                            first,
                            last: index - 1,
                            start: self.lines[first].start,
                            end: self.lines[index - 1].end,
                            seed: rng.next(),
                            strong: part_energy >= STRONG_FROM,
                            letters: letters(part_energy >= STRONG_FROM),
                            variant: 0,
                        });
                        first = index;
                    }
                }
            }
        }
        self.pick_variants();
    }

    /// Виды для частей со стилями, у которых их несколько.
    fn pick_variants(&mut self) {
        let mut drawn = 0;
        for part in &mut self.parts {
            let count = part.kind.variants();
            if count > 1 {
                part.variant = self.luck.variant(self.salt, drawn, count);
                drawn += 1;
            }
        }
    }

    /// Видимость части в момент `t` (вступление проступает заранее).
    fn visibility(&self, index: usize, t: f32) -> f32 {
        let part = &self.parts[index];
        let single = self.parts.len() == 1;
        let lead = if single { 3.0 } else if index == 0 { 1.0 } else { CROSSFADE };
        let appear = ease_in_out(progress(t, part.start - lead, lead.min(1.2)));
        let tail = if single { 1.5 } else { 0.2 };
        let vanish = ease_in_out(progress(t, part.end + tail, CROSSFADE));
        appear * (1.0 - vanish)
    }

    /// Кадр в момент `time` и когда нужен следующий. `None` — нулевой размер.
    pub fn render(&mut self, time: f32, view: &View) -> Option<(Image, f32)> {
        let (width, height) = ((view.width * view.scale).round() as u32, (view.height * view.scale).round() as u32);
        if width == 0 || height == 0 || view.width < 1.0 || view.height < 1.0 {
            return None;
        }
        if self.size != (width, height) {
            self.size = (width, height);
            self.scenes.clear();
        }
        // Видимые части: обычно одна, на переходе — две.
        let visible: Vec<(usize, f32)> =
            (0..self.parts.len()).map(|index| (index, self.visibility(index, time))).filter(|(_, visible)| *visible > 0.0).collect();
        self.scenes.retain(|index, _| visible.iter().any(|(shown, _)| shown == index));
        let fonts = self.fonts.get_or_insert_with(Fonts::new);

        self.front = 1 - self.front;
        let buffer = &mut self.buffers[self.front];
        if buffer.as_ref().is_none_or(|buffer| buffer.width() != width || buffer.height() != height) {
            *buffer = Some(SharedPixelBuffer::new(width, height));
        }
        let buffer = buffer.as_mut()?;
        let bytes = buffer.make_mut_bytes();
        bytes.fill(0);
        let pixmap = PixmapMut::from_bytes(bytes, width, height)?;
        let mut canvas = paint::Canvas {
            pixmap,
            root: Transform::from_scale(width as f32 / view.width, height as f32 / view.height),
            spare: self.spare.take(),
            blank: true,
        };
        let ctx = Ctx {
            t: time,
            width: view.width,
            height: view.height,
            palette: view.palette,
            lines: &self.lines,
            mood: &self.mood,
            lead: view.lead,
        };
        let mut pace = IDLE;
        for &(index, shown) in &visible {
            let scene = self.scenes.entry(index).or_insert_with(|| styles::create(&self.parts[index]));
            scene.draw(&mut canvas, fonts, &ctx, shown);
            pace = pace.min(scene.pace(&ctx));
        }
        self.spare = canvas.spare.take();
        // Ничего не видно — проснуться к появлению следующей части.
        if visible.is_empty()
            && let Some(next) = (0..self.parts.len()).map(|index| self.parts[index].start - 3.0).filter(|start| *start > time).reduce(f32::min)
        {
            pace = pace.min((next - time).max(0.05));
        }
        let image = Image::from_rgba8_premultiplied(buffer.clone());
        Some((image, pace.max(1.0 / view.max_fps.max(1.0))))
    }
}

/// Выбор «с защитой от невезения»: вес = (1 + промахи подряд)².
/// Решается раз на трек (`salt`).
#[derive(Default)]
struct Luck {
    misses: Vec<u32>,
    salt: Option<u64>,
    drawn: Vec<u8>,
}

impl Luck {
    /// Вид для `index`-й части с вариантами при загрузке `salt`.
    fn variant(&mut self, salt: u64, index: usize, count: u8) -> u8 {
        if self.salt != Some(salt) {
            self.salt = Some(salt);
            self.drawn.clear();
        }
        while self.drawn.len() <= index {
            let variant = self.draw(salt, self.drawn.len(), count);
            self.drawn.push(variant);
        }
        self.drawn[index] % count
    }

    fn draw(&mut self, salt: u64, number: usize, count: u8) -> u8 {
        self.misses.resize(usize::from(count).max(self.misses.len()), 0);
        let weights: Vec<f32> = self.misses[..usize::from(count)].iter().map(|miss| (1.0 + *miss as f32).powi(2)).collect();
        let mut roll = Rng::new(motion::mix(salt ^ 0x1C4, number as u64)).unit() * weights.iter().sum::<f32>();
        let mut chosen = count - 1;
        for (variant, weight) in weights.iter().enumerate() {
            if roll < *weight {
                chosen = variant as u8;
                break;
            }
            roll -= weight;
        }
        for (variant, miss) in self.misses.iter_mut().enumerate().take(usize::from(count)) {
            *miss = if variant == usize::from(chosen) { 0 } else { *miss + 1 };
        }
        chosen
    }
}

/// Без сцены на экране: проверить через секунду.
const IDLE: f32 = 1.0;

/// Часы кадра: позиция движка идёт ступеньками, а движение нужно ровное.
#[derive(Default)]
pub struct Clock {
    origin: Option<(Instant, f64)>,
    offset: f64,
}

impl Clock {
    pub fn time(&mut self, engine: f64, playing: bool, speed: f64) -> f64 {
        let now = Instant::now();
        let speed = speed.max(0.01);
        match self.origin {
            Some((start, at_speed)) if playing && at_speed == speed => {
                let real = now.duration_since(start).as_secs_f64() * speed;
                let offset = engine - real;
                // Перемотка — сразу; ступенька выше — по ней.
                if (offset - self.offset).abs() > 0.3 || offset > self.offset {
                    self.offset = offset;
                } else {
                    // Часы системы и звуковой карты чуть расходятся — медленно вниз.
                    self.offset -= (self.offset - offset).min(0.0005);
                }
                real + self.offset
            }
            _ => {
                self.origin = playing.then_some((now, speed));
                self.offset = engine;
                engine
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics;
    use nyanoto_core::Feel;

    const SONG: &str = "[00:03.00]Мы будем петь и смеяться как дети\n\
        [00:06.00]Среди упорной борьбы и труда\n\
        [00:09.00]Ведь мы такими родились на свете\n\
        [00:12.00]Что не сдаёмся нигде и никогда\n\
        [00:15.00]Hold on to the night we are alive\n\
        [00:18.00]Hold on to the night we are alive\n\
        [00:21.00]\n\
        [00:27.00]И снова утро над рекой\n";

    fn palette() -> Palette {
        Palette {
            ink: tiny_skia::Color::from_rgba8(235, 235, 235, 255),
            ground: tiny_skia::Color::from_rgba8(20, 20, 20, 255),
            accent: tiny_skia::Color::from_rgba8(80, 140, 255, 255),
        }
    }

    /// Песня: тихий куплет (до 14 с), громкий припев, тихий конец.
    fn mood() -> Mood {
        let count = 300;
        let loud = |frame: usize| (140..230).contains(&frame);
        let feel = Feel {
            loud: (0..count).map(|frame| if loud(frame) { -8.0 } else { -24.0 }).collect(),
            flux: (0..count).map(|frame| if loud(frame) { 0.3 } else { 0.05 }).collect(),
            bright: (0..count).map(|frame| if loud(frame) { 0.09 } else { 0.01 }).collect(),
            noise: (0..count).map(|frame| if loud(frame) { 0.09 } else { 0.01 }).collect(),
            beats: (0..60).map(|beat| beat as f32 * 0.5).collect(),
        };
        Mood::from_feel(&feel)
    }

    fn view() -> View {
        View { width: 320.0, height: 200.0, scale: 1.0, palette: palette(), lead: 0.15, max_fps: 60.0 }
    }

    #[test]
    fn modes_plan_parts_by_mood() {
        let lyrics = lyrics::parse(SONG);
        let mut stage = Stage::default();
        stage.set_lyrics(&lyrics, Choice::Moment, 7, Some(mood()), "");
        assert_eq!(stage.lines.len(), 7);
        // Громкий припев — отдельной частью, жирным шрифтом; соседние — разными стилями.
        let loud = stage.parts.iter().find(|part| part.first == 4).expect("часть с припева");
        assert!(loud.strong);
        // Тихий куплет — антиквой.
        assert!(!stage.parts[0].strong && stage.parts[0].first == 0);
        for pair in stage.parts.windows(2) {
            assert_ne!(pair[0].kind, pair[1].kind);
        }
        stage.set_lyrics(&lyrics, Choice::Song, 7, Some(mood()), "");
        assert_eq!(stage.parts.len(), 1);
        assert_eq!((stage.parts[0].first, stage.parts[0].last), (0, 6));
        stage.set_lyrics(&lyrics, Choice::Only(Kind::Terminal), 7, None, "");
        assert_eq!(stage.parts[0].kind, Kind::Terminal);
    }

    #[test]
    fn genre_picks_the_font_and_mood_decides_without_it() {
        // Теги из музыки пользователя и несколько спорных.
        for (genre, letters) in [
            ("Rock", Some(Letters::Narrow)),
            ("Punk / New Wave", Some(Letters::Narrow)),
            ("Pop", Some(Letters::Serif)),
            ("Alternative & Indie", None),
            ("Hip-Hop/Rap", Some(Letters::Wide)),
            ("Synthpop", Some(Letters::Wide)),
            ("Pop Punk", Some(Letters::Narrow)),
            ("Русский рок", Some(Letters::Narrow)),
            ("", None),
        ] {
            assert_eq!(Letters::for_genre(genre), letters, "{genre}");
        }
        let lyrics = lyrics::parse(SONG);
        let mut stage = Stage::default();
        stage.set_lyrics(&lyrics, Choice::Moment, 7, Some(mood()), "Pop");
        assert!(stage.parts.iter().all(|part| part.letters == Letters::Serif));
        // Без жанра: громкое — узким гротеском, тихое — антиквой.
        stage.set_lyrics(&lyrics, Choice::Moment, 7, Some(mood()), "Alternative & Indie");
        assert!(stage.parts.iter().all(|part| part.letters == Letters::for_mood(part.strong)));
        assert!(stage.parts.iter().any(|part| part.letters == Letters::Narrow) && stage.parts.iter().any(|part| part.letters == Letters::Serif));
    }

    #[test]
    fn every_kind_draws_and_moves() {
        let lyrics = lyrics::parse(SONG);
        for kind in Kind::EVERY {
            let mut stage = Stage::default();
            stage.set_lyrics(&lyrics, Choice::Only(kind), 3, Some(mood()), "");
            let frame = |stage: &mut Stage, t: f32| {
                let (image, pace) = stage.render(t, &view()).expect("кадр");
                assert!(pace > 0.0 && pace <= 1.0);
                image.to_rgba8_premultiplied().expect("пиксели")
            };
            let a = frame(&mut stage, 7.5);
            let painted = a.as_slice().iter().filter(|pixel| pixel.a > 0).count();
            assert!(painted > 300, "{kind:?}: {painted}");
            // Сцена не стоит на месте: через долю секунды кадр другой.
            let b = frame(&mut stage, 7.9);
            assert_ne!(a.as_slice(), b.as_slice(), "{kind:?} застыл");
            // Задолго до первой строки — пусто.
            let mut early = Stage::default();
            early.set_lyrics(&lyrics, Choice::Only(kind), 3, Some(mood()), "");
            let (image, _) = early.render(-5.0, &view()).unwrap();
            assert!(image.to_rgba8_premultiplied().unwrap().as_slice().iter().all(|pixel| pixel.a == 0), "{kind:?}");
        }
    }

    #[test]
    fn estimated_words_snap_to_onsets_but_stamped_ones_stay() {
        let lyrics = lyrics::parse("[00:10.00]раз два три четыре\n[00:14.00]<00:14.00>пять <00:14.90>шесть\n[00:18.00]\n");
        let mut stage = Stage::default();
        stage.set_lyrics(&lyrics, Choice::Only(Kind::Block), 1, None, "");
        let guess = stage.lines[0].words.clone();
        // Атаки чуть позже оценки второго и третьего слова, и одна — далеко.
        let feel = Feel {
            loud: vec![-12.0; 200],
            flux: vec![0.1; 200],
            bright: vec![0.03; 200],
            noise: vec![0.03; 200],
            beats: vec![guess[1] + 0.08, guess[2] + 0.05, guess[3] + 0.5, 14.95],
        };
        stage.set_mood(Mood::from_feel(&feel));
        let words = &stage.lines[0].words;
        assert_eq!(words[0], 10.0, "первое слово — на метке строки");
        assert!((words[1] - (guess[1] + 0.08)).abs() < 1e-4);
        assert!((words[2] - (guess[2] + 0.05)).abs() < 1e-4);
        assert_eq!(words[3], guess[3], "далёкая атака не тянет");
        // Пословные метки — как в файле.
        assert!((stage.lines[1].words[1] - 14.9).abs() < 1e-4);
    }

    #[test]
    fn luck_makes_every_variant_come_soon() {
        let mut luck = Luck::default();
        let mut longest = [0u32; 3];
        let mut since = [0u32; 3];
        for salt in 0..300u64 {
            let variant = usize::from(luck.variant(salt * 7919, 0, 3));
            // Тот же трек (та же загрузка) — тот же вид.
            assert_eq!(usize::from(luck.variant(salt * 7919, 0, 3)), variant);
            for (other, gap) in since.iter_mut().enumerate() {
                *gap = if other == variant { 0 } else { *gap + 1 };
                longest[other] = longest[other].max(*gap);
            }
        }
        // Ни один вид не пропадает надолго: самое долгое невезение — 6 треков.
        assert!(longest.iter().all(|gap| *gap <= 6), "{longest:?}");
    }

    #[test]
    fn clock_is_smooth_and_follows_seeks() {
        let mut clock = Clock::default();
        assert_eq!(clock.time(10.0, true, 1.0), 10.0);
        let t = clock.time(10.0, true, 1.0);
        assert!(t >= 10.0);
        let t = clock.time(42.0, true, 1.0);
        assert!((t - 42.0).abs() < 0.05, "{t}");
        assert_eq!(clock.time(43.5, false, 1.0), 43.5);
    }

    /// Цена сцены при масштабе 2:
    /// `STAGE_KINDS=ink cargo test --release -p nyanoto-app stage_costs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn stage_costs() {
        let lyrics = lyrics::parse(SONG);
        let only: Option<Vec<String>> = std::env::var("STAGE_KINDS").ok().map(|kinds| kinds.split(',').map(str::to_string).collect());
        for kind in Kind::EVERY {
            if only.as_ref().is_some_and(|only| !only.iter().any(|code| code == kind.code())) {
                continue;
            }
            let mut stage = Stage::default();
            let salt = std::env::var("STAGE_SALT").ok().and_then(|salt| salt.parse().ok()).unwrap_or(11);
            stage.set_lyrics(&lyrics, Choice::Only(kind), salt, Some(mood()), "");
            let view = View { width: 588.0, height: 444.0, scale: 2.0, palette: palette(), lead: 0.15, max_fps: 60.0 };
            let timing = Instant::now();
            let mut frames = 0;
            let mut t = 5.0;
            while t < 11.0 {
                stage.render(t, &view).unwrap();
                frames += 1;
                t += 1.0 / 30.0;
            }
            println!("{:>10}: {:.2} мс на кадр", kind.code(), timing.elapsed().as_secs_f64() * 1000.0 / f64::from(frames));
        }
    }

    /// Кадры в PNG: `NYANOTO_STAGE_FRAMES=папка cargo test --release -p nyanoto-app stage_frames -- --ignored --nocapture`.
    /// Ещё `STAGE_KINDS`, `STAGE_STRIP=17.5`, `STAGE_GENRE`, `STAGE_TIMES=5.5,6.2`.
    #[test]
    #[ignore]
    fn stage_frames() {
        let Some(dir) = std::env::var_os("NYANOTO_STAGE_FRAMES") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lyrics = lyrics::parse(SONG);
        let only: Option<Vec<String>> = std::env::var("STAGE_KINDS").ok().map(|kinds| kinds.split(',').map(str::to_string).collect());
        let strip: Option<f32> = std::env::var("STAGE_STRIP").ok().and_then(|from| from.parse().ok());
        // `STAGE_GENRE=Pop` — шрифт этого жанра.
        let genre = std::env::var("STAGE_GENRE").unwrap_or_default();
        // `STAGE_SIZE=600x600` — свой размер сцены.
        let (width, height) = std::env::var("STAGE_SIZE")
            .ok()
            .and_then(|size| size.split_once('x').and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?))))
            .unwrap_or((720.0f32, 460.0f32));
        // `STAGE_TIMES=5.5,6.2,7` — кадры в эти секунды.
        let times: Option<Vec<f32>> = std::env::var("STAGE_TIMES").ok().map(|list| list.split(',').filter_map(|at| at.trim().parse().ok()).collect());
        let moments: Vec<f32> = if let Some(times) = times {
            times
        } else if let Some(from) = strip {
            (0..12).map(|frame| from + frame as f32 / 15.0).collect()
        } else {
            vec![3.15, 4.2, 5.6, 7.4, 9.6, 15.3, 16.6, 18.4]
        };
        for kind in Kind::EVERY {
            if only.as_ref().is_some_and(|only| !only.iter().any(|code| code == kind.code())) {
                continue;
            }
            let mut stage = Stage::default();
            // `STAGE_SALT=5` — другое зерно случая (другой вид у сцен с вариантами).
            let salt = std::env::var("STAGE_SALT").ok().and_then(|salt| salt.parse().ok()).unwrap_or(11);
            stage.set_lyrics(&lyrics, Choice::Only(kind), salt, Some(mood()), &genre);
            // `STAGE_LIGHT=1` — светлая тема (тёмные буквы на светлом).
            let palette = if std::env::var_os("STAGE_LIGHT").is_some() {
                Palette {
                    ink: tiny_skia::Color::from_rgba8(30, 30, 30, 255),
                    ground: tiny_skia::Color::from_rgba8(245, 245, 245, 255),
                    accent: tiny_skia::Color::from_rgba8(40, 100, 220, 255),
                }
            } else {
                palette()
            };
            let view = View { width, height, scale: 1.0, palette, lead: 0.15, max_fps: 60.0 };
            for (number, moment) in moments.iter().enumerate() {
                let (image, _) = stage.render(*moment, &view).unwrap();
                let buffer = image.to_rgba8_premultiplied().unwrap();
                let mut picture = image::RgbaImage::new(buffer.width(), buffer.height());
                for (pixel, source) in picture.pixels_mut().zip(buffer.as_slice()) {
                    let back = f32::from(palette.ground.to_color_u8().red()) * (1.0 - f32::from(source.a) / 255.0);
                    *pixel = image::Rgba([
                        (f32::from(source.r) + back) as u8,
                        (f32::from(source.g) + back) as u8,
                        (f32::from(source.b) + back) as u8,
                        255,
                    ]);
                }
                picture.save(dir.join(format!("{}-{number:02}.png", kind.code()))).unwrap();
            }
            let timing = Instant::now();
            for frame in 0..120 {
                stage.render(6.0 + frame as f32 / 60.0, &view).unwrap();
            }
            let quiet = timing.elapsed().as_secs_f64() * 1000.0 / 120.0;
            let timing = Instant::now();
            for frame in 0..120 {
                stage.render(15.0 + frame as f32 / 60.0, &view).unwrap();
            }
            let loud = timing.elapsed().as_secs_f64() * 1000.0 / 120.0;
            println!("{:>10}: кадр {quiet:.2} мс тихо, {loud:.2} мс громко", kind.code());
        }
    }
}
