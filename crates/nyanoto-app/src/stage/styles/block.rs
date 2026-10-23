//! «Блок»: слова разного размера в плотном прямоугольнике, строки во всю ширину, блок наклонён.
//! Плывёт вдоль строки, строки въезжают и уходят по-своему.

use tiny_skia::Transform;

use super::{Ctx, Lazy, Part, Piece, Scene, chorus_band, draw_word, ink_center, pieces};
use crate::stage::motion::{self, Entry, Exit, Look, Rng, apply, ease_out, progress, tempo};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::Fonts;

struct Placed {
    piece: Piece,
    /// Кегль 1 → координаты блока.
    local: Transform,
    pivot: (f32, f32),
    size: f32,
}

struct Layout {
    words: Vec<Placed>,
    along: f32,
    height: f32,
    fit: f32,
    degrees: f32,
    /// Куда плывёт блок (единичный вектор на экране).
    drift: (f32, f32),
    entry: Entry,
    exit: Exit,
    /// Как въезжает весь блок: 0 — слова сами, 1 — сбоку, 2 — из глубины.
    arrival: u8,
    center: (f32, f32),
}

pub struct Block {
    part: Part,
    lines: Lazy<Layout>,
    size: (f32, f32),
}

impl Block {
    pub fn new(part: &Part) -> Self {
        Self { part: *part, lines: Lazy::default(), size: (0.0, 0.0) }
    }

    fn layout(part: &Part, ctx: &Ctx, fonts: &mut Fonts, index: usize) -> Layout {
        let line = &ctx.lines[index];
        let mut rng = Rng::new(motion::mix(part.seed, index as u64));
        let pieces = pieces(fonts, line, part);
        let (width, height) = (ctx.width, ctx.height);
        let energy = ctx.mood.mean(line.start, line.end);
        let calm = !part.strong;
        let vertical = !calm && pieces.len() > 1 && rng.chance(0.2 + 0.15 * energy);
        let degrees = if vertical {
            -90.0
        } else if calm {
            rng.sign() * rng.range(1.5, 4.0)
        } else {
            rng.sign() * rng.range(4.0, 11.0)
        };
        let (along, across) = if vertical { (height * 0.84, width * 0.7) } else { (width * 0.84, height * 0.66) };
        let class = |piece: &Piece| if piece.key { 1.0 } else { 0.46 };
        let max_stretch = if calm { 1.3 } else { 1.8 };
        let leading = if calm { 0.2 } else { 0.08 };
        type Row = (Vec<usize>, f32, f32, f32);
        let pack = |size: f32| -> Vec<Row> {
            let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
            let mut used = 0.0;
            for (index, piece) in pieces.iter().enumerate() {
                let word = piece.word.advance * class(piece) * size;
                let gap = 0.24 * class(piece) * size;
                let row = rows.last_mut().expect("есть строка");
                if !row.is_empty() && used + gap + word > along {
                    rows.push(vec![index]);
                    used = word;
                } else {
                    used += if row.is_empty() { word } else { gap + word };
                    row.push(index);
                }
            }
            rows.into_iter()
                .filter(|row| !row.is_empty())
                .map(|row| {
                    let natural: f32 = row
                        .iter()
                        .enumerate()
                        .map(|(place, &index)| {
                            let piece = &pieces[index];
                            (piece.word.advance + if place > 0 { 0.24 } else { 0.0 }) * class(piece) * size
                        })
                        .sum();
                    let stretch = (along / natural.max(1.0)).min(max_stretch);
                    let above = row.iter().map(|&index| -pieces[index].word.ink.top() * class(&pieces[index])).fold(0.0, f32::max);
                    let below = row.iter().map(|&index| pieces[index].word.ink.bottom().max(0.0) * class(&pieces[index])).fold(0.0, f32::max);
                    (row, stretch, above * size * stretch, below * size * stretch)
                })
                .collect()
        };
        let total = |rows: &[Row], size: f32| -> f32 {
            rows.iter().map(|(_, stretch, above, below)| above + below + leading * size * stretch).sum::<f32>()
                - rows.last().map_or(0.0, |(_, stretch, _, _)| leading * size * stretch)
        };
        let (mut low, mut high) = (2.0f32, width.min(height));
        for _ in 0..22 {
            let middle = (low + high) / 2.0;
            if total(&pack(middle), middle) <= across { low = middle } else { high = middle }
        }
        let size = low;
        let rows = pack(size);
        let block_height = total(&rows, size).max(1.0);
        let angle = degrees.to_radians();
        let (cos, sin) = (angle.cos().abs(), angle.sin().abs());
        let fit = (width * 0.92 / (along * cos + block_height * sin)).min(height * 0.9 / (along * sin + block_height * cos)).min(1.0);
        let mut slots: Vec<Option<(Transform, f32)>> = vec![None; pieces.len()];
        let mut y = 0.0;
        for (row, stretch, above, below) in &rows {
            let baseline = y + above;
            let natural: f32 = row
                .iter()
                .enumerate()
                .map(|(place, &index)| (pieces[index].word.advance + if place > 0 { 0.24 } else { 0.0 }) * class(&pieces[index]) * size * stretch)
                .sum();
            let mut x = if calm { (along - natural) / 2.0 } else { 0.0 };
            for (place, &index) in row.iter().enumerate() {
                let scale = class(&pieces[index]) * size * stretch;
                if place > 0 {
                    x += 0.24 * scale;
                }
                slots[index] = Some((Transform::from_scale(scale, scale).post_translate(x, baseline), scale));
                x += pieces[index].word.advance * scale;
            }
            y = baseline + below + leading * size * stretch;
        }
        let words = pieces
            .into_iter()
            .zip(slots)
            .filter_map(|(piece, slot)| {
                let (local, size) = slot?;
                let (x, y) = ink_center(&piece.word);
                Some(Placed { pivot: apply(local, x, y), piece, local, size })
            })
            .collect();
        // Плывёт вдоль своей строки, в одну из сторон.
        let along_screen = (angle.cos(), angle.sin());
        let side = rng.sign();
        Layout {
            words,
            along,
            height: block_height,
            fit,
            degrees,
            drift: (along_screen.0 * side, along_screen.1 * side),
            entry: Entry::pick(&mut rng, energy),
            exit: Exit::pick(&mut rng, energy),
            arrival: rng.below(3) as u8,
            center: (width * (0.5 + rng.range(-0.03, 0.03)), height * (0.5 + rng.range(-0.03, 0.03))),
        }
    }
}

impl Scene for Block {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.lines.clear();
        }
        let part = self.part;
        let unit = ctx.unit();
        let (width, height) = (ctx.width, ctx.height);
        let ink = ctx.palette.ink;
        let level = ctx.level();
        // Свечение светлеет с громкостью, круг с разрывом крутится ровно. Под ритм — не всё.
        let drift = ctx.drift();
        let orbit = 0.04 * drift;
        let (dx, dy) = (width * (0.5 + 0.06 * orbit.cos()), height * (0.5 + 0.05 * (orbit * 1.3).sin()));
        canvas.glow((dx, dy), unit * 0.36, ink, (0.05 + 0.06 * level) * visible);
        let turn = (part.seed % 628) as f32 / 100.0 + 0.06 * drift;
        if let Some(arc) = paint::arc_path(dx, dy, unit * 0.42, turn, turn + 5.2) {
            canvas.stroke(&arc, faded(ink, 0.3 * visible), canvas.hair(), Transform::identity(), None);
        }

        let current = ctx.current(&part);
        let indices: Vec<usize> = ctx.recent(&part, 1.2).collect();
        self.lines.keep(|index| indices.contains(&index) || current.is_some_and(|current| index + 2 >= current && index <= current + 1));
        for &index in &indices {
            let layout = self.lines.get_or(index, || Block::layout(&part, ctx, fonts, index));
            let line = &ctx.lines[index];
            let pace = tempo(ctx.mood.mean(line.start, line.end));
            let since = ctx.t - line.start;
            // Блок плывёт всё время, пока виден (быстрее в напряжённых строках).
            let speed = unit * (0.008 + 0.02 * ctx.mood.mean(line.start, line.end));
            let (mut cx, mut cy) = (layout.center.0 + layout.drift.0 * speed * since, layout.center.1 + layout.drift.1 * speed * since);
            let mut group_scale = layout.fit;
            let mut alpha = visible;
            let arrive = ease_out(progress(ctx.t, line.start, 0.5 * pace));
            match layout.arrival {
                1 => {
                    cx -= layout.drift.0 * width * 0.12 * (1.0 - arrive);
                    cy -= layout.drift.1 * width * 0.12 * (1.0 - arrive);
                    alpha *= arrive.max(0.05);
                }
                2 => {
                    group_scale *= 1.0 + 0.08 * (1.0 - arrive);
                    alpha *= arrive.max(0.05);
                }
                _ => {}
            }
            let sway = 0.6 * motion::wander(ctx.t, part.seed ^ index as u64, 0.05);
            let group = Transform::from_translate(-layout.along / 2.0, -layout.height / 2.0)
                .post_scale(group_scale, group_scale)
                .post_rotate(layout.degrees + sway)
                .post_translate(cx, cy);
            // Уход — всем блоком, чуть раньше конца строки.
            let leave_at = line.end - 0.08 * pace;
            let leave = layout.exit.look(progress(ctx.t, leave_at, layout.exit.length(pace)));
            let leave_group = if leave.letters < 0.0 { Look::STILL } else { leave };
            let group = leave_group.place(group, (cx, cy), unit * 0.3);
            alpha *= leave_group.alpha;
            if alpha <= 0.003 {
                continue;
            }
            let band = chorus_band(ctx, &part, index, pace);
            // Полоса — одна на весь припев: через середину сцены, общий наклон.
            let band_turn = super::band_angle(ctx, &part, index, 14.0, 28.0);
            let band_center = (width / 2.0, height / 2.0);
            let draw_words = |canvas: &mut Canvas, color, mask: Option<&tiny_skia::Mask>| {
                for word in &layout.words {
                    if mask.is_some() {
                        let pivot = apply(group, word.pivot.0, word.pivot.1);
                        let reach = word.size * layout.fit * word.piece.word.ink_width().max(word.piece.word.ink_height()) * 0.8;
                        if !super::near_band(pivot, reach, band_center, band_turn, unit * 0.2) {
                            continue;
                        }
                    }
                    // Строка видна целиком: спетые слова разгораются.
                    let mut look = motion::combine(
                        super::line_look(ctx.t, line.start, word.piece.at, layout.entry.length(pace)),
                        if leave.letters < 0.0 { leave } else { Look::STILL },
                    );
                    look.alpha *= alpha;
                    let base = word.local.post_concat(group);
                    let pivot = apply(group, word.pivot.0, word.pivot.1);
                    draw_word(canvas, &word.piece.word, base, pivot, word.size * layout.fit, &look, color, mask);
                }
            };
            draw_words(canvas, ink, None);
            // Припев: полоса поперёк блока, буквы на ней — цветом фона.
            if let Some((from, to)) = band {
                let diagonal = width.hypot(height);
                if let Some(path) = paint::band(diagonal * 1.6, unit * 0.2, from, to) {
                    let transform = Transform::from_rotate(band_turn).post_translate(band_center.0, band_center.1);
                    canvas.fill(&path, faded(ink, visible), transform, None);
                    if let Some(mask) = canvas.cover(&[(&path, transform)]) {
                        draw_words(canvas, ctx.palette.ground, Some(&mask));
                        canvas.give_back(mask);
                    }
                }
            }
        }
    }
}
