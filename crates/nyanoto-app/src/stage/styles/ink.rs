//! «Чернила» — тушь в воде (скрыт, но жив).
//! Строка пишется пером, потом в неё падает капля, и буквы растворяются в воде (`fluid.rs`).
//! Шаги воды одной длины: та же история — та же картина, перемотка считает с удара.

use std::collections::HashMap;

use tiny_skia::{FillRule, Mask, Transform};

use super::{Ctx, Lazy, Part, Scene};
use crate::stage::fluid::{Fluid, Water};
use crate::stage::motion::{self, Rng, apply, ease_in, ease_in_out, ease_out, progress};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::{Face, Fonts, Word};

/// Наклон пера: граница написанного наклонена, как буквы прописи.
const SLANT: f32 = 0.3;
/// Сколько живёт тушь в воде после удара, с.
const LIFE: f32 = 4.5;
/// За сколько до следующей строки капля касается прежней.
const IMPACT: f32 = 0.15;
/// Сколько падает капля.
const FALL: f32 = 0.6;
/// Шаг воды, с.
const STEP: f32 = 1.0 / 30.0;
/// Клеток туши по длинной стороне сцены; скорость воды — втрое грубее.
const CELLS: f32 = 180.0;
const COARSE: usize = 3;
/// Вода: тушь тяжелее воды, вихри держатся, скорость и тушь гаснут.
const WATER: Water = Water { sink: 20.0, swirl: 12.0, keep: 0.75, fade: 0.86 };

/// Буква строки в сетке туши: откуда, маска, когда растворяется.
struct Stamp {
    x: i32,
    y: i32,
    width: usize,
    alpha: Vec<u8>,
    /// От удара капли, с.
    at: f32,
    /// Середина буквы в клетках — туда лёгкий толчок воды.
    center: (f32, f32),
}

struct Written {
    word: Word,
    at: f32,
    /// Слово кеглем 1 → экран.
    base: Transform,
    /// Сколько пишется слово.
    write: f32,
    /// Когда растворяется каждая буква (от удара капли).
    melt: Vec<f32>,
}

struct Layout {
    words: Vec<Written>,
    /// Буквы в сетке туши, по времени растворения.
    stamps: Vec<Stamp>,
    /// Куда падает капля (точка экрана).
    drop: (f32, f32),
}

/// Вода одной строки: сколько уже посчитано и сколько букв в ней растворено.
struct Sim {
    fluid: Fluid,
    time: f32,
    stamped: usize,
    /// Непрозрачность туши по клеткам (рисуется растянутой).
    cover: Vec<f32>,
}

/// Что рисовать из букв: слово целиком, пишущееся (доля) или одну тающую букву.
enum Letter {
    Word,
    Writing(f32),
    Glyph(usize, f32),
}

pub struct Ink {
    part: Part,
    lines: Lazy<Layout>,
    sims: HashMap<usize, Sim>,
    /// Тушь всех вод вместе и где была в прошлом кадре.
    merged: Vec<f32>,
    merged_area: Option<(usize, usize, usize, usize)>,
    size: (f32, f32),
}

/// Сетка туши для сцены: размер клетки (в точках) и число клеток.
fn grid(width: f32, height: f32) -> (f32, usize, usize) {
    let cell = width.max(height) / CELLS;
    let round = |length: f32| ((length / cell).ceil() as usize).div_ceil(COARSE) * COARSE;
    (cell, round(width), round(height))
}

impl Ink {
    pub fn new(part: &Part) -> Self {
        Self { part: *part, lines: Lazy::default(), sims: HashMap::new(), merged: Vec::new(), merged_area: None, size: (0.0, 0.0) }
    }

    /// Строка прописью, по очереди сверху и снизу, с лёгким наклоном.
    fn layout(part: &Part, ctx: &Ctx, fonts: &mut Fonts, index: usize) -> Layout {
        let line = &ctx.lines[index];
        let mut rng = Rng::new(motion::mix(part.seed, index as u64));
        let (width, height) = (ctx.width, ctx.height);
        let unit = ctx.unit();
        let (cell, _, _) = grid(width, height);
        let raw: Vec<&str> = line.text.split_whitespace().collect();
        let words: Vec<Word> = raw.iter().map(|text| fonts.word(text, Face::Script)).collect();
        let space = 0.28;
        let mut size = unit * if line.chorus { 0.2 } else { 0.18 };
        let mut rows: Vec<Vec<usize>> = Vec::new();
        for _ in 0..6 {
            rows = vec![Vec::new()];
            let mut used = 0.0;
            for (number, word) in words.iter().enumerate() {
                let advance = word.advance * size;
                if !rows.last().expect("есть строка").is_empty() && used + advance > width * 0.86 {
                    rows.push(Vec::new());
                    used = 0.0;
                }
                rows.last_mut().expect("есть строка").push(number);
                used += advance + space * size;
            }
            if rows.len() as f32 * size * 0.95 < height * 0.3 && rows.iter().all(|row| row.len() > 1 || words[row[0]].advance * size < width * 0.9) {
                break;
            }
            size *= 0.85;
        }
        let row_height = size * 0.95;
        let block = rows.len() as f32 * row_height;
        let upper = (index - part.first).is_multiple_of(2);
        let middle = height * if upper { 0.28 } else { 0.62 } + height * rng.range(-0.02, 0.02);
        let lean = Transform::from_rotate_at(rng.range(-3.0, 1.5), width / 2.0, middle);
        // Капля падает в строку где-то посередине.
        let drop = (width * rng.range(0.32, 0.68), middle);
        let mut written = Vec::new();
        let mut stamps = Vec::new();
        for (row_number, row) in rows.iter().enumerate() {
            let row_width: f32 = row.iter().map(|number| words[*number].advance * size).sum::<f32>() + space * size * (row.len() - 1) as f32;
            let mut x = (width - row_width) / 2.0 + width * rng.range(-0.03, 0.03);
            let baseline = middle - block / 2.0 + (row_number as f32 + 0.72) * row_height;
            for &number in row {
                let word = fonts.word(raw[number], Face::Script);
                let base = Transform::from_scale(size, size).post_translate(x, baseline).post_concat(lean);
                let mut melt = Vec::with_capacity(word.glyphs.len());
                for glyph in &word.glyphs {
                    let (gx, gy) = apply(base, glyph.x + glyph.advance / 2.0, -0.3);
                    // Растворение идёт от места удара в стороны.
                    let at = (gx - drop.0).hypot((gy - drop.1) * 2.0) / (width * 0.7) + rng.range(0.05, 0.2);
                    melt.push(at);
                    // Маска буквы в сетке туши.
                    let Some(placed) = glyph.path.as_ref().and_then(|path| {
                        path.clone().transform(Transform::from_translate(glyph.x, 0.0).post_concat(base).post_scale(1.0 / cell, 1.0 / cell))
                    }) else {
                        continue;
                    };
                    let bounds = placed.bounds();
                    let (left, top) = (bounds.left().floor() as i32 - 1, bounds.top().floor() as i32 - 1);
                    let (mask_width, mask_height) = ((bounds.width().ceil() as u32 + 3).max(1), (bounds.height().ceil() as u32 + 3).max(1));
                    let Some(mut mask) = Mask::new(mask_width, mask_height) else {
                        continue;
                    };
                    mask.fill_path(&placed, FillRule::Winding, true, Transform::from_translate(-left as f32, -top as f32));
                    stamps.push(Stamp { x: left, y: top, width: mask_width as usize, alpha: mask.data().to_vec(), at, center: (gx / cell, gy / cell) });
                }
                let at = line.words.get(number).copied().unwrap_or(line.start);
                let next = line.words.get(number + 1).copied().unwrap_or(line.end);
                // Перо успевает к следующему слову, но не мечется.
                let write = ((next - at) * 0.9).clamp(0.25, 0.35 + 0.07 * word.glyphs.len() as f32);
                x += (word.advance + space) * size;
                written.push(Written { word, at, base, write, melt });
            }
        }
        stamps.sort_by(|a, b| a.at.total_cmp(&b.at));
        Layout { words: written, stamps, drop }
    }
}

/// Досчитывает воду до `since`. Часы могут отступить на доли мс — это не перемотка.
fn advance(sim: &mut Sim, layout: &Layout, since: f32, cell: f32, seed: u64) {
    if sim.time > since + 0.25 || since - sim.time > 1.5 {
        let (width, height) = (sim.fluid.dye_width, sim.fluid.dye_height);
        sim.fluid = Fluid::new(width, height, COARSE);
        sim.time = 0.0;
        sim.stamped = 0;
    }
    let (dx, dy) = (layout.drop.0 / cell, layout.drop.1 / cell);
    while sim.time + STEP <= since {
        if sim.time == 0.0 {
            // Капля входит в воду: густая тушь.
            sim.fluid.blot(dx, dy, 2.5, 3.0);
        }
        // Удар: короткий толчок вниз — из него вода сама делает «гриб».
        if sim.time < 0.35 {
            sim.fluid.push(dx, dy + 2.0, 6.0, (0.0, 26.0));
        }
        // Растворённые буквы уходят в воду, у каждой своё завихрение.
        while let Some(stamp) = layout.stamps.get(sim.stamped)
            && stamp.at <= sim.time
        {
            sim.fluid.ink(stamp.x, stamp.y, stamp.width, &stamp.alpha, 1.0);
            let mut rng = Rng::new(motion::mix(seed, sim.stamped as u64));
            sim.fluid.push(stamp.center.0, stamp.center.1, 3.0, (rng.range(-10.0, 10.0), rng.range(6.0, 24.0)));
            sim.stamped += 1;
        }
        sim.fluid.step(STEP, WATER);
        sim.time += STEP;
    }
}

impl Scene for Ink {
    /// Вода считается 30 раз в секунду — чаще рисовать незачем.
    fn pace(&self, _ctx: &Ctx) -> f32 {
        STEP
    }

    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.lines.clear();
            self.sims.clear();
            self.merged.clear();
            self.merged_area = None;
        }
        let part = self.part;
        let (width, height) = (ctx.width, ctx.height);
        let unit = ctx.unit();
        let t = ctx.t;
        let ink = ctx.palette.ink;
        let (cell, dye_width, dye_height) = grid(width, height);

        // Взвесь всплывает сама по себе. Облако на весь экран (~5 мс) убрано.
        let mut rng = Rng::new(part.seed ^ 0x1A5);
        let mut specks = Vec::new();
        for speck in 0..16 {
            let rise = (rng.range(0.0, 1.0) - t * rng.range(0.004, 0.012)).rem_euclid(1.0);
            let sway = unit * 0.01 * (t * rng.range(0.1, 0.3) + speck as f32).sin();
            specks.push((width * rng.range(0.03, 0.97) + sway, height * rise, unit * rng.range(0.0015, 0.0035)));
        }
        canvas.dots(&specks, faded(ink, 0.16 * visible), Transform::identity());

        let current = ctx.current(&part);
        let indices: Vec<usize> = ctx.recent(&part, LIFE + 1.0).filter(|index| current.is_none_or(|current| *index + 2 >= current)).collect();
        self.lines.keep(|index| indices.contains(&index) || current.is_some_and(|current| index + 2 >= current && index <= current + 1));
        let mut falling = Vec::new();
        let mut letters: Vec<(usize, usize, Letter)> = Vec::new();
        let mut alive = Vec::new();
        // Стереть тушь прошлого кадра.
        self.merged.resize(dye_width * dye_height, 0.0);
        if let Some((left, top, right, bottom)) = self.merged_area.take() {
            for row in top..=bottom {
                self.merged[row * dye_width + left..=row * dye_width + right].fill(0.0);
            }
        }
        for &index in &indices {
            let layout = self.lines.get_or(index, || Ink::layout(&part, ctx, fonts, index));
            let line = &ctx.lines[index];
            // Капля касается строки чуть раньше следующей строки (или после паузы).
            let leave = ctx.lines.get(index + 1).filter(|_| index < part.last).map_or(line.end + 1.5, |next| next.start.min(line.end + 1.5));
            let since = t - (leave - IMPACT);
            if since < 0.0 {
                // Капля падает, разгоняясь, с тонким хвостом.
                let p = ease_in(progress(since, -FALL, FALL));
                if p > 0.0 {
                    let y = -unit * 0.05 + (layout.drop.1 + unit * 0.05) * p;
                    falling.push((layout.drop.0, y, unit * 0.012, 0.95));
                    for tail in 1..6 {
                        falling.push((layout.drop.0, y - unit * 0.012 * tail as f32 * (0.4 + p), unit * 0.008 * (1.0 - tail as f32 / 7.0), 0.5 / tail as f32));
                    }
                }
            } else if since < LIFE
                && let crowded = ctx.lines.get(index + 2).filter(|_| index + 2 <= part.last).map_or(1.0, |later| 1.0 - progress(t, later.start - 0.6, 0.6))
                && crowded > 0.0
            {
                // Не больше двух вод сразу.
                alive.push(index);
                let sim = self.sims.entry(index).or_insert_with(|| Sim {
                    fluid: Fluid::new(dye_width, dye_height, COARSE),
                    time: 0.0,
                    stamped: 0,
                    cover: Vec::new(),
                });
                advance(sim, layout, since, cell, part.seed ^ index as u64);
                // Тушь гаснет к концу жизни строки.
                let fade = (1.0 - progress(since, LIFE - 1.0, 1.0)) * crowded;
                // Сводим в общую: непрозрачности складываются как слои.
                if let Some((left, top, right, bottom)) = sim.fluid.paint(&mut sim.cover, 2.2) {
                    for row in top..=bottom {
                        for index in row * dye_width + left..=row * dye_width + right {
                            let alpha = sim.cover[index] * fade;
                            self.merged[index] = 1.0 - (1.0 - self.merged[index]) * (1.0 - alpha);
                        }
                    }
                    self.merged_area = Some(self.merged_area.map_or((left, top, right, bottom), |(l, t, r, b)| (l.min(left), t.min(top), r.max(right), b.max(bottom))));
                }
            }
            // Буквы: пишутся пером; растворяясь, тают, пока их тушь уходит в воду.
            for (number, word) in layout.words.iter().enumerate() {
                let p = progress(t, word.at, word.write);
                if p <= 0.0 {
                    continue;
                }
                let melting = word.melt.iter().any(|delay| since > delay - 0.05);
                if !melting {
                    letters.push((index, number, if p >= 1.0 { Letter::Word } else { Letter::Writing(p) }));
                    continue;
                }
                for (glyph, delay) in word.melt.iter().enumerate() {
                    let gone = ease_in_out(progress(since, *delay, 0.35));
                    if gone < 1.0 {
                        letters.push((index, number, Letter::Glyph(glyph, 1.0 - gone)));
                    }
                }
            }
        }
        self.sims.retain(|index, _| alive.contains(index));
        if let Some(bounds) = self.merged_area {
            canvas.cover_picture(&self.merged, dye_width, cell, bounds, faded(ink, visible));
        }
        canvas.smoke(&falling, faded(ink, visible));
        for (index, number, letter) in letters {
            let Some(layout) = self.lines.get(index) else {
                continue;
            };
            let word = &layout.words[number];
            match letter {
                Letter::Word => canvas.word(&word.word, faded(ink, visible), word.base, None),
                Letter::Glyph(glyph, alpha) => {
                    let glyph = &word.word.glyphs[glyph];
                    if let Some(path) = &glyph.path {
                        canvas.fill(path, faded(ink, alpha * visible), Transform::from_translate(glyph.x, 0.0).post_concat(word.base), None);
                    }
                }
                Letter::Writing(p) => {
                    // Перо ведёт границу слева направо, наклонно, как пропись.
                    let ink_box = word.word.ink;
                    let (top, bottom) = (ink_box.top() - 0.2, ink_box.bottom() + 0.2);
                    let from = ink_box.left() - SLANT * bottom.max(0.0) - 0.05;
                    let to = ink_box.right() - SLANT * top + 0.05;
                    let pen = from + (to - from) * (0.15 * p + 0.85 * ease_out(p));
                    let edge = [(ink_box.left() - 1.0, top), (pen - SLANT * top, top), (pen - SLANT * bottom, bottom), (ink_box.left() - 1.0, bottom)];
                    if let Some(wipe) = paint::polygon(&edge)
                        && let Some(mask) = canvas.cover(&[(&wipe, word.base)])
                    {
                        canvas.word(&word.word, faded(ink, visible), word.base, Some(&mask));
                        canvas.give_back(mask);
                    }
                }
            }
        }
    }
}
