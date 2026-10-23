//! «Полёт сквозь слова»: камера летит, слово прилетает из дали, когда его поют, и пролетает мимо.
//! Скорость своя у строки, коридор — по «времени песни», без толчков на долях.

use std::f32::consts::TAU;

use tiny_skia::{PathBuilder, Transform};

use super::{Ctx, Lazy, Part, Piece, Scene, draw_word, ink_center, pieces};
use crate::stage::motion::{self, Look, Rng, ease_out, progress, tempo};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::Fonts;

struct Flying {
    piece: Piece,
    /// Место на экране на «расстоянии чтения» (от середины).
    x: f32,
    y: f32,
    degrees: f32,
    size: f32,
}

pub struct Flight {
    part: Part,
    lines: Lazy<Vec<Flying>>,
    size: (f32, f32),
}

/// Расстояние, на котором слово читается в свой размер.
const FOCUS: f32 = 1.0;
/// Ближе — слово пролетело.
const NEAR: f32 = 0.42;

impl Flight {
    pub fn new(part: &Part) -> Self {
        Self { part: *part, lines: Lazy::default(), size: (0.0, 0.0) }
    }

    fn layout(part: &Part, ctx: &Ctx, fonts: &mut Fonts, index: usize) -> Vec<Flying> {
        let line = &ctx.lines[index];
        let mut rng = Rng::new(motion::mix(part.seed, index as u64));
        let unit = ctx.unit();
        let mut placed: Vec<Flying> = Vec::new();
        for piece in pieces(fonts, line, part) {
            let size = unit * if piece.key { 0.12 } else { 0.08 } * if line.chorus { 1.1 } else { 1.0 };
            let degrees = if part.strong && rng.chance(0.12) { -90.0 } else { rng.range(-15.0, 15.0) };
            let half = (piece.word.ink_width() * size / 2.0, piece.word.ink_height() * size / 2.0);
            let (half_x, half_y) = if degrees.abs() > 45.0 { (half.1, half.0) } else { half };
            // Место подальше от остальных слов строки — чтобы не наезжали.
            let mut best = (0.0, 0.0, f32::MIN);
            for _ in 0..16 {
                let (x, y) = (ctx.width * rng.range(-0.32, 0.32), ctx.height * rng.range(-0.3, 0.3));
                let room = placed
                    .iter()
                    .map(|other| {
                        let (ox, oy) = (other.piece.word.ink_width() * other.size / 2.0, other.piece.word.ink_height() * other.size / 2.0);
                        ((x - other.x).abs() - half_x - ox).max((y - other.y).abs() - half_y - oy)
                    })
                    .fold(f32::MAX, f32::min);
                if room > best.2 {
                    best = (x, y, room);
                }
            }
            placed.push(Flying { x: best.0, y: best.1, degrees, size, piece });
        }
        placed
    }
}

/// Слово у `FOCUS` в момент пения, дальше со скоростью `speed`.
fn distance(at: f32, t: f32, speed: f32, arrive: f32) -> Option<f32> {
    if t < at - arrive {
        return None;
    }
    if t < at {
        let p = ease_out(progress(t, at - arrive, arrive));
        return Some(FOCUS * (3.0 - 2.0 * p));
    }
    Some(FOCUS - speed * (t - at))
}

impl Scene for Flight {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.lines.clear();
        }
        let part = self.part;
        let (width, height) = (ctx.width, ctx.height);
        let (cx, cy) = (width / 2.0, height / 2.0);
        let t = ctx.t;
        let ink = ctx.palette.ink;
        let hair = canvas.hair();
        // Крен и росчерки — от всей части, а не от слова.
        let calm = 1.0 - ctx.mood.mean(part.start, part.end);
        // Камера медленно кренится — без накопления, слова не переворачиваются.
        let roll = (4.0 - 2.0 * calm) * motion::wander(t, part.seed, 0.02);
        let camera = Transform::from_rotate_at(roll, cx, cy);

        // Коридор: тонкие кольца глубины плывут навстречу и проступают из дали.
        let travel = 0.45 * ctx.drift();
        for ring in 0..5 {
            let depth = (ring as f32 + 1.0 - travel.rem_euclid(1.0)) * 0.8;
            let radius = width.min(height) * 0.45 * FOCUS / depth;
            if depth <= 0.15 || radius > width.hypot(height) {
                continue;
            }
            // Только что возникшее (дальнее) кольцо проступает постепенно.
            let alpha = 0.1 * motion::clamp01(4.0 - depth);
            if let Some(path) = paint::circle(cx, cy, radius) {
                canvas.stroke(&path, faded(ink, alpha * visible), hair, camera, None);
            }
        }
        // Рёбра коридора и росчерки скорости.
        let mut streaks = PathBuilder::new();
        for corner in 0..4 {
            let angle = TAU * (corner as f32 + 0.5) / 4.0 + 0.0;
            let (dx, dy) = (angle.cos() * width, angle.sin() * height);
            streaks.move_to(cx + dx * 0.04, cy + dy * 0.04);
            streaks.line_to(cx + dx, cy + dy);
        }
        let mut rng = Rng::new(part.seed ^ 0xF1);
        let count = 3 + (3.0 * (1.0 - calm)) as usize;
        let drift = ctx.drift();
        for number in 0..count {
            let angle = rng.range(0.0, TAU);
            let phase = (drift * rng.range(0.3, 0.5) + number as f32 * 0.37).fract();
            let near = 0.15 + phase * 0.9;
            let far = near + 0.05 + 0.2 * phase;
            let reach = width.hypot(height) * 0.5;
            streaks.move_to(cx + angle.cos() * reach * near, cy + angle.sin() * reach * near);
            streaks.line_to(cx + angle.cos() * reach * far.min(1.2), cy + angle.sin() * reach * far.min(1.2));
        }
        if let Some(path) = streaks.finish() {
            canvas.stroke(&path, faded(ink, 0.12 * visible), hair, camera, None);
        }

        // Слова — от дальних к ближним.
        let current = ctx.current(&part);
        let indices: Vec<usize> = ctx.recent(&part, 4.0).collect();
        self.lines.keep(|index| indices.contains(&index) || current.is_some_and(|current| index + 1 >= current && index <= current + 1));
        // Слова строки приближаются с одной скоростью (быстрее в напряжённых строках).
        let speed_of = |index: usize| 0.12 + 0.2 * ctx.mood.mean(ctx.lines[index].start, ctx.lines[index].end);
        let mut drawn: Vec<(f32, usize, usize)> = Vec::new();
        for &index in &indices {
            let words = self.lines.get_or(index, || Flight::layout(&part, ctx, fonts, index));
            let pace = tempo(ctx.mood.mean(ctx.lines[index].start, ctx.lines[index].end));
            let speed = speed_of(index);
            for (number, word) in words.iter().enumerate() {
                if let Some(depth) = distance(word.piece.at, t, speed, 0.3 * pace)
                    && depth > NEAR
                {
                    drawn.push((depth, index, number));
                }
            }
        }
        drawn.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (depth, index, number) in drawn {
            let Some(words) = self.lines.get(index) else {
                continue;
            };
            let word = &words[number];
            // Слова прежней строки быстро гаснут, как только началась новая.
            let next = ctx.lines.get(index + 1).filter(|_| index < part.last).map_or(f32::MAX, |line| line.start);
            let older = 1.0 - progress(t, next, 0.5);
            if older <= 0.0 {
                continue;
            }
            let scale = FOCUS / depth;
            let (x, y) = (cx + word.x * scale, cy + word.y * scale);
            let size = word.size * scale;
            let fade_in = motion::clamp01((3.0 - depth) / 1.2);
            let fade_out = motion::clamp01((depth - NEAR) / 0.35);
            let alpha = fade_in * fade_out * older * visible;
            let (ix, iy) = ink_center(&word.piece.word);
            let base = Transform::from_translate(-ix, -iy).post_scale(size, size).post_rotate(word.degrees).post_translate(x, y).post_concat(camera);
            let look = Look { alpha, ..Look::STILL };
            draw_word(canvas, &word.piece.word, base, (x, y), size, &look, ink, None);
        }
    }
}
