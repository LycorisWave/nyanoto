//! «Вертушка»: слова крупно по сторонам квадрата, глаз идёт по кругу, следующий круг — внутрь.
//! Квадрат покачивается по «времени песни», спетое слово встаёт на место и разгорается.
//! После двух кругов спираль уходит поворотом, новая проступает.

use tiny_skia::Transform;

use super::{Ctx, Part, Piece, Scene, draw_word, ink_center, pieces};
use crate::stage::motion::{self, Entry, Rng, apply, ease_in_out, ease_out, progress, tempo};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::Fonts;

/// Сторон в одном витке спирали (два круга).
const PER_CYCLE: usize = 8;

/// Кусок строки на одной стороне квадрата.
struct Chunk {
    line: usize,
    slot: usize,
    /// Слово, кегль 1 → координаты квадрата (середина в нуле), середина, кегль.
    words: Vec<(Piece, Transform, (f32, f32), f32)>,
    entry: Entry,
    /// Кромка стороны: откуда (после угла) и куда идёт текст.
    origin: (f32, f32),
    direction: (f32, f32),
    /// Докуда по кромке доходит каждое слово.
    ends: Vec<f32>,
}

pub struct Pinwheel {
    part: Part,
    chunks: Vec<Chunk>,
    laid: usize,
    size: (f32, f32),
}

impl Pinwheel {
    pub fn new(part: &Part) -> Self {
        Self { part: *part, chunks: Vec::new(), laid: 0, size: (0.0, 0.0) }
    }

    fn half(ctx: &Ctx) -> f32 {
        ctx.unit() * 0.43
    }

    /// Строки по сторонам до `upto`. Не влезает в виток — новый виток, но не посреди строки.
    fn grow(&mut self, fonts: &mut Fonts, ctx: &Ctx, upto: usize) {
        let half = Self::half(ctx);
        while self.part.first + self.laid <= upto.min(self.part.last) {
            let line = self.part.first + self.laid;
            let mut rng = Rng::new(motion::mix(self.part.seed, line as u64));
            let energy = ctx.mood.mean(ctx.lines[line].start, ctx.lines[line].end);
            let entry = Entry::pick(&mut rng, energy);
            let first = self.chunks.last().map_or(0, |chunk| chunk.slot + 1);
            let mut chunks = lay(half, pieces(fonts, &ctx.lines[line], &self.part), first, line, entry);
            if !first.is_multiple_of(PER_CYCLE) && chunks.iter().any(|chunk| chunk.slot / PER_CYCLE != first / PER_CYCLE) {
                let next = (first / PER_CYCLE + 1) * PER_CYCLE;
                chunks = lay(half, pieces(fonts, &ctx.lines[line], &self.part), next, line, entry);
            }
            self.chunks.extend(chunks);
            self.laid += 1;
        }
    }
}

/// Куски строки по сторонам начиная с `first`.
fn lay(half: f32, pieces: Vec<Piece>, first: usize, line: usize, entry: Entry) -> Vec<Chunk> {
    let band = half * 0.32;
    let gap = 0.3;
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut pieces = pieces.into_iter().peekable();
    while pieces.peek().is_some() {
        let slot = chunks.last().map_or(first, |chunk| chunk.slot + 1);
        let (side, level) = (slot % 4, (slot % PER_CYCLE) / 4);
        let inset = level as f32 * band * 1.25;
        // Углы свободны: там встречаются стороны, текст идёт между ними.
        let corner = band * 1.05;
        let length = (2.0 * (half - inset) - 2.0 * corner) * 0.96;
        // Сколько слов влезает крупно; одно длинное — мельче.
        let mut taken: Vec<Piece> = Vec::new();
        let mut used = 0.0;
        while let Some(piece) = pieces.peek() {
            let width = (piece.word.advance + if taken.is_empty() { 0.0 } else { gap }) * band;
            if !taken.is_empty() && used + width > length {
                break;
            }
            used += width;
            taken.push(pieces.next().expect("заглянули выше"));
        }
        let size = (length / (used / band).max(0.5)).min(band);
        // Слева вниз, снизу вправо, справа вверх, сверху вправо — вверх ногами не бывает.
        let (start, degrees) = match side {
            0 => ((-half + inset, -half + inset), 90.0),
            1 => ((-half + inset, half - inset), 0.0),
            2 => ((half - inset, half - inset), -90.0),
            _ => ((-half + inset, -half + inset), 0.0),
        };
        let rise = taken.iter().map(|piece| -piece.word.ink.top()).fold(0.0, f32::max);
        let sink = taken.iter().map(|piece| piece.word.ink.bottom().max(0.0)).fold(0.0, f32::max);
        // Сверху буквы «висят» от края внутрь, на остальных сторонах стоят на краю.
        let drop = if side == 3 { rise * size } else { -sink * size };
        let mut x = corner;
        let mut ends = Vec::new();
        let words = taken
            .into_iter()
            .map(|piece| {
                let local = Transform::from_scale(size, size).post_translate(x, drop).post_rotate(degrees).post_translate(start.0, start.1);
                let (ix, iy) = ink_center(&piece.word);
                let pivot = apply(local, ix, iy);
                ends.push(x - corner + piece.word.advance * size);
                x += (piece.word.advance + gap) * size;
                (piece, local, pivot, size)
            })
            .collect();
        let angle = f32::to_radians(degrees);
        let direction = (angle.cos(), angle.sin());
        let origin = (start.0 + direction.0 * corner, start.1 + direction.1 * corner);
        chunks.push(Chunk { line, slot, words, entry, origin, direction, ends });
    }
    chunks
}

impl Scene for Pinwheel {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.chunks.clear();
            self.laid = 0;
        }
        let part = self.part;
        let t = ctx.t;
        let ink = ctx.palette.ink;
        let hair = canvas.hair();
        let (cx, cy) = (ctx.width / 2.0, ctx.height / 2.0);
        let Some(current) = ctx.current(&part) else {
            return;
        };
        self.grow(fonts, ctx, current + 1);
        // Виток — по куску, в котором последнее спетое слово.
        let sounding = self
            .chunks
            .iter()
            .rev()
            .find(|chunk| chunk.words.first().is_some_and(|(piece, ..)| piece.at <= t))
            .map_or(0, |chunk| chunk.slot);
        let cycle = sounding / PER_CYCLE;
        // Размах — по напряжению части, скорость — по «времени песни».
        let lively = ctx.mood.mean(part.start, part.end);
        let phase = (part.seed % 628) as f32 / 100.0;
        let spin = (3.5 + 3.0 * lively) * (0.22 * ctx.drift() + phase).sin();
        let half = Self::half(ctx);
        if let Some(frame) = paint::polygon(&[(-half, -half), (half, -half), (half, half), (-half, half)]) {
            let transform = Transform::from_scale(1.04, 1.04).post_rotate(spin).post_translate(cx, cy);
            canvas.stroke(&frame, faded(ink, 0.12 * visible), hair, transform, None);
        }
        // Начало витка: когда запели первый кусок в нём.
        let cycle_start = |cycle: usize| {
            self.chunks.iter().find(|chunk| chunk.slot / PER_CYCLE == cycle).and_then(|chunk| chunk.words.first()).map(|(piece, ..)| piece.at)
        };
        let mut pens = Vec::new();
        for shown in [cycle.checked_sub(1), Some(cycle)].into_iter().flatten() {
            let (scale, turn, alpha) = if shown < cycle {
                let Some(start) = cycle_start(cycle) else {
                    continue;
                };
                // Прежняя спираль поворачивается к середине и тает.
                let p = ease_in_out(progress(t, start - 0.6, 1.2));
                if p >= 1.0 {
                    continue;
                }
                (1.0 - 0.35 * p, 30.0 * p, 1.0 - p)
            } else if shown > 0 {
                // Новая проступает чуть крупнее и садится на место.
                let p = ease_out(progress(t, cycle_start(shown).unwrap_or(t) - 0.2, 1.0));
                (1.0 + 0.1 * (1.0 - p), -8.0 * (1.0 - p), 1.0)
            } else {
                (1.0, 0.0, 1.0)
            };
            let view = Transform::from_scale(scale, scale).post_rotate(spin + turn).post_translate(cx, cy);
            let opening = self.chunks.iter().find(|chunk| chunk.slot / PER_CYCLE == shown).map(|chunk| chunk.line);
            let mut edges = tiny_skia::PathBuilder::new();
            for chunk in self.chunks.iter().filter(|chunk| chunk.slot / PER_CYCLE == shown) {
                let line = &ctx.lines[chunk.line];
                let pace = tempo(ctx.mood.mean(line.start, line.end));
                let dim = if chunk.line == current { 1.0 } else { 0.4 };
                // Первая строка нового витка проступает, когда прежний уже растаял.
                let delay = if shown > 0 && Some(chunk.line) == opening { 0.35 } else { 0.0 };
                let mut reached = 0.0f32;
                let mut tip: Option<f32> = None;
                for (number, (piece, local, pivot, size)) in chunk.words.iter().enumerate() {
                    // Строка видна сразу бледно, спетое слово подъезжает и разгорается.
                    let arrive = ease_out(progress(t, piece.at, 0.5 * pace));
                    let mut look = super::line_look(t, line.start + delay, piece.at, chunk.entry.length(pace));
                    look.alpha *= dim * alpha * visible;
                    let back = 0.25 * size * (1.0 - arrive);
                    let slide = Transform::from_translate(chunk.direction.0 * back, chunk.direction.1 * back);
                    let base = local.post_concat(slide).post_concat(view);
                    let middle = (pivot.0 + chunk.direction.0 * back, pivot.1 + chunk.direction.1 * back);
                    draw_word(canvas, &piece.word, base, apply(view, middle.0, middle.1), size * scale, &look, ink, None);
                    // Кромка дотягивается до конца слова, пока оно подъезжает.
                    if piece.at <= t {
                        let from = if number == 0 { 0.0 } else { chunk.ends[number - 1] };
                        reached = from + (chunk.ends[number] - from) * ease_out(progress(t, piece.at, 0.45 * pace));
                        tip = (arrive < 1.0).then_some(reached);
                    }
                }
                if reached > 0.0 {
                    let (ox, oy) = chunk.origin;
                    edges.move_to(ox, oy);
                    edges.line_to(ox + chunk.direction.0 * reached, oy + chunk.direction.1 * reached);
                }
                if let Some(tip) = tip {
                    let point = apply(view, chunk.origin.0 + chunk.direction.0 * tip, chunk.origin.1 + chunk.direction.1 * tip);
                    pens.push((point.0, point.1, ctx.unit() * 0.006));
                }
            }
            if let Some(path) = edges.finish() {
                canvas.stroke(&path, faded(ink, 0.5 * alpha * visible), hair, view, None);
            }
        }
        canvas.dots(&pens, faded(ink, 0.9 * visible), Transform::identity());
    }
}
