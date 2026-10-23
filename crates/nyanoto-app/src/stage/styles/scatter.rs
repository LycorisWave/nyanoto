//! «Вихрь»: слова под 0°, ±80°, ±90° и не наезжают. В углу тонкий круг.
//! Было перегружено — теперь слово спокойно проявляется и стоит.

use std::f32::consts::TAU;

use tiny_skia::Transform;

use super::{Ctx, Lazy, Part, Piece, Scene, chorus_band, draw_word, ink_center, pieces};
use crate::stage::motion::{self, Entry, Rng, apply, around, ease_out, progress, tempo};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::Fonts;

struct Placed {
    piece: Piece,
    base: Transform,
    pivot: (f32, f32),
    size: f32,
    /// От центра наружу (единичный вектор).
    dir: (f32, f32),
}

struct Layout {
    words: Vec<Placed>,
    entry: Entry,
    /// Края стоящих слов — следующая строка их обходит.
    taken: Vec<[f32; 4]>,
}

pub struct Scatter {
    part: Part,
    lines: Lazy<Layout>,
    size: (f32, f32),
}

/// Кольцо: в каком углу и какого размера.
fn ring(seed: u64, width: f32, height: f32) -> (f32, f32, f32) {
    let mut rng = Rng::new(seed ^ 0x5157);
    let (x, y) = match rng.below(4) {
        0 => (0.92, 0.9),
        1 => (0.08, 0.9),
        2 => (0.92, 0.1),
        _ => (0.08, 0.12),
    };
    (width * x, height * y, width.min(height) * rng.range(0.36, 0.46))
}

impl Scatter {
    pub fn new(part: &Part) -> Self {
        Self { part: *part, lines: Lazy::default(), size: (0.0, 0.0) }
    }

    fn layout(part: &Part, ctx: &Ctx, fonts: &mut Fonts, index: usize, avoid: &[[f32; 4]]) -> Layout {
        let line = &ctx.lines[index];
        let mut rng = Rng::new(motion::mix(part.seed, index as u64));
        let pieces = pieces(fonts, line, part);
        let (width, height) = (ctx.width, ctx.height);
        let unit = ctx.unit();
        let energy = ctx.mood.mean(line.start, line.end);
        let (columns, rows) = if width >= height { (4, 3) } else { (3, 4) };
        let (cell_width, cell_height) = (width / columns as f32, height / rows as f32);
        let mut free: Vec<usize> = (0..columns * rows).collect();
        for slot in (1..free.len()).rev() {
            free.swap(slot, rng.below(slot + 1));
        }
        let calm = !part.strong;
        let margin = unit * 0.02;
        let mut taken: Vec<[f32; 4]> = Vec::new();
        let mut words = Vec::new();
        for piece in pieces {
            let roll = rng.unit();
            let degrees = if calm {
                if roll < 0.6 {
                    rng.range(-3.0, 3.0)
                } else if roll < 0.8 {
                    -90.0
                } else {
                    rng.sign() * rng.range(6.0, 10.0)
                }
            } else if roll < 0.4 {
                0.0
            } else if roll < 0.55 {
                -90.0
            } else if roll < 0.65 {
                90.0
            } else if roll < 0.85 {
                rng.sign() * rng.range(4.0, 9.0)
            } else {
                rng.sign() * rng.range(80.0, 86.0)
            };
            let upright = degrees.abs() < 45.0 || degrees.abs() > 135.0;
            let (along, across) = if upright { (cell_width * 1.15, cell_height * 0.75) } else { (cell_height * 1.15, cell_width * 0.75) };
            let mut size = (along / piece.word.ink_width()).min(across / piece.word.ink_height()).min(unit * 0.2);
            if !piece.key {
                size *= 0.7;
            }
            let (ink_x, ink_y) = ink_center(&piece.word);
            let ink = piece.word.ink;
            let tries = free.len().max(1);
            let mut chosen = None;
            // Три круга: крупно мимо всех, мельче мимо всех, мельче мимо своих.
            for attempt in 0..tries * 3 {
                let shrink = if attempt >= tries { 0.7 } else { 1.0 };
                let slot = attempt % tries;
                let cell = free.get(slot).copied().unwrap_or_else(|| rng.below(columns * rows));
                let (column, row) = (cell % columns, cell / columns);
                let x = (column as f32 + 0.5 + rng.range(-0.15, 0.15)) * cell_width;
                let y = (row as f32 + 0.5 + rng.range(-0.15, 0.15)) * cell_height;
                let scale = size * shrink;
                let base = Transform::from_translate(-ink_x, -ink_y).post_scale(scale, scale).post_rotate(degrees).post_translate(x, y);
                let corners = [(ink.left(), ink.top()), (ink.right(), ink.top()), (ink.left(), ink.bottom()), (ink.right(), ink.bottom())]
                    .map(|(cx, cy)| apply(base, cx, cy));
                let (min_x, max_x) = corners.iter().fold((f32::MAX, f32::MIN), |(low, high), (cx, _)| (low.min(*cx), high.max(*cx)));
                let (min_y, max_y) = corners.iter().fold((f32::MAX, f32::MIN), |(low, high), (_, cy)| (low.min(*cy), high.max(*cy)));
                let inside = |low: f32, high: f32, limit: f32| {
                    if high - low > limit - 2.0 * margin {
                        (limit - low - high) / 2.0
                    } else if low < margin {
                        margin - low
                    } else if high > limit - margin {
                        limit - margin - high
                    } else {
                        0.0
                    }
                };
                let (dx, dy) = (inside(min_x, max_x, width), inside(min_y, max_y, height));
                let bounds = [min_x + dx, min_y + dy, max_x + dx, max_y + dy];
                let gap = unit * 0.015;
                let strict = attempt < tries * 2;
                let clear = taken.iter().chain(avoid.iter().filter(|_| strict)).all(|other| {
                    bounds[2] + gap < other[0] || other[2] + gap < bounds[0] || bounds[3] + gap < other[1] || other[3] + gap < bounds[1]
                });
                if clear || attempt + 1 == tries * 3 {
                    chosen = Some((slot, base.post_translate(dx, dy), scale, bounds));
                    break;
                }
            }
            let Some((slot, base, scale, bounds)) = chosen else {
                continue;
            };
            if slot < free.len() {
                free.remove(slot);
            }
            taken.push(bounds);
            let pivot = apply(base, ink_x, ink_y);
            let (dx, dy) = (pivot.0 - width / 2.0, pivot.1 - height / 2.0);
            let length = dx.hypot(dy);
            let dir = if length > unit * 0.05 {
                (dx / length, dy / length)
            } else {
                let angle = rng.range(0.0, TAU);
                (angle.cos(), angle.sin())
            };
            words.push(Placed { piece, base, pivot, size: scale, dir });
        }
        // Проявляется — из глубины или просто, без ударов.
        let entry = if rng.unit() < 0.5 + 0.3 * energy { Entry::Zoom } else { Entry::Fade };
        Layout { words, entry, taken }
    }
}

impl Scene for Scatter {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.lines.clear();
        }
        let part = self.part;
        let (width, height) = (ctx.width, ctx.height);
        let unit = ctx.unit();
        let ink = ctx.palette.ink;
        let t = ctx.t;

        // В углу — один тонкий неподвижный круг (как на кадре колонки).
        let (cx, cy, radius) = ring(part.seed, width, height);
        if let Some(circle) = paint::circle(cx, cy, radius) {
            canvas.stroke(&circle, faded(ink, 0.2 * visible), canvas.hair(), Transform::identity(), None);
        }

        let current = ctx.current(&part);
        let indices: Vec<usize> = ctx.recent(&part, 12.0).filter(|index| current.is_none_or(|current| *index + 1 >= current)).collect();
        self.lines.keep(|index| indices.contains(&index) || current.is_some_and(|current| index + 2 >= current && index <= current + 1));
        for &index in &indices {
            let avoid: Vec<[f32; 4]> = index.checked_sub(1).and_then(|before| self.lines.get(before)).map(|before| before.taken.clone()).unwrap_or_default();
            let layout = self.lines.get_or(index, || Scatter::layout(&part, ctx, fonts, index, &avoid));
            let line = &ctx.lines[index];
            let pace = tempo(ctx.mood.mean(line.start, line.end));
            // Прошлая строка тает, пока появляется следующая.
            let next_start = ctx.lines.get(index + 1).filter(|_| index < part.last).map_or(line.end + 3.0, |next| next.start);
            let after = ease_out(progress(t, line.end, 0.8 * pace));
            let gone = progress(t, next_start + 0.4, 1.2);
            let band = chorus_band(ctx, &part, index, pace);
            let band_turn = super::band_angle(ctx, &part, index, 22.0, 30.0);
            let draw_words = |canvas: &mut Canvas, color, mask: Option<&tiny_skia::Mask>| {
                for word in &layout.words {
                    let p = progress(t, word.piece.at, layout.entry.length(pace) * 1.4);
                    if p <= 0.0 {
                        continue;
                    }
                    let mut look = layout.entry.look(p);
                    look.alpha *= (1.0 - 0.7 * after) * (1.0 - gone) * visible;
                    // Едва заметно отходит от середины — ровно, без рывков.
                    let age = (t - word.piece.at).max(0.0);
                    let drift = unit * 0.006 * age;
                    let base = word.base.post_concat(around(word.pivot, 1.0 - 0.06 * after, 0.0)).post_translate(word.dir.0 * drift, word.dir.1 * drift);
                    let pivot = (word.pivot.0 + word.dir.0 * drift, word.pivot.1 + word.dir.1 * drift);
                    let reach = word.size * word.piece.word.ink_width().max(word.piece.word.ink_height()) * 0.7;
                    if mask.is_some() && !super::near_band(pivot, reach, (width / 2.0, height / 2.0), band_turn, unit * 0.2) {
                        continue;
                    }
                    draw_word(canvas, &word.piece.word, base, pivot, word.size, &look, color, mask);
                }
            };
            draw_words(canvas, ink, None);
            if let Some((from, to)) = band
                && let Some(path) = paint::band(width.hypot(height) * 1.6, unit * 0.2, from, to)
            {
                let transform = Transform::from_rotate(band_turn).post_translate(width / 2.0, height / 2.0);
                canvas.fill(&path, faded(ink, visible), transform, None);
                if let Some(mask) = canvas.cover(&[(&path, transform)]) {
                    draw_words(canvas, ctx.palette.ground, Some(&mask));
                    canvas.give_back(mask);
                }
            }
        }
    }
}
