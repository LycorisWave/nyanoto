//! Фоны сцен: один бледный мотив, медленно. Напряжение меняет только скорость (`Ctx::drift`).
//! Только дешёвое — тонкие линии и точки.

use std::f32::consts::TAU;

use tiny_skia::{PathBuilder, Transform};

use super::Ctx;
use crate::stage::motion::{self, Rng};
use crate::stage::paint::{self, Canvas, faded};

/// Пыль: редкие бледные точки медленно плывут; `parallax` — сдвиг камеры
/// (дальние точки сдвигаются меньше — глубина).
pub fn dust(canvas: &mut Canvas, ctx: &Ctx, seed: u64, count: usize, parallax: (f32, f32), alpha: f32) {
    let (width, height) = (ctx.width, ctx.height);
    let unit = ctx.unit();
    let drift = ctx.drift();
    let mut rng = Rng::new(seed ^ 0xD057);
    let mut dots = Vec::with_capacity(count);
    for index in 0..count {
        let depth = rng.range(0.25, 1.0);
        let speed = unit * rng.range(0.006, 0.015);
        let angle = rng.range(0.0, TAU);
        let x = (rng.range(0.0, 1.0) * width + angle.cos() * speed * drift - parallax.0 * depth).rem_euclid(width);
        let y = (rng.range(0.0, 1.0) * height + angle.sin() * speed * drift - parallax.1 * depth).rem_euclid(height);
        // Мерцание — медленное, не на доли.
        let twinkle = 0.7 + 0.3 * (ctx.t * rng.range(0.2, 0.6) + index as f32).sin();
        dots.push((x, y, unit * 0.002 * depth * twinkle + 0.25));
    }
    canvas.dots(&dots, faded(ctx.palette.ink, alpha), Transform::identity());
}

/// Сетка-карта сети: бледная, едва плывёт; по ней изредка бежит импульс.
pub fn grid(canvas: &mut Canvas, ctx: &Ctx, seed: u64, alpha: f32) {
    let (width, height) = (ctx.width, ctx.height);
    let unit = ctx.unit();
    let energy = ctx.energy();
    let step = unit * 0.16;
    let shift = (unit * 0.03 * motion::wander(ctx.t, seed, 0.02), unit * 0.03 * motion::wander(ctx.t, seed ^ 1, 0.02));
    let mut lines = PathBuilder::new();
    let mut x = shift.0.rem_euclid(step);
    while x < width {
        lines.move_to(x, 0.0);
        lines.line_to(x, height);
        x += step;
    }
    let mut y = shift.1.rem_euclid(step);
    while y < height {
        lines.move_to(0.0, y);
        lines.line_to(width, y);
        y += step;
    }
    if let Some(path) = lines.finish() {
        canvas.stroke(&path, faded(ctx.palette.ink, 0.045 * alpha), canvas.hair(), Transform::identity(), None);
    }
    let count = (2.0 + 4.0 * energy) as usize;
    let mut rng = Rng::new(seed ^ 0x9119);
    let mut pulses = Vec::with_capacity(count);
    let mut trails = PathBuilder::new();
    for index in 0..count {
        let horizontal = rng.chance(0.5);
        let lane = rng.below(10) as f32 * step;
        // Импульс шагает на клетку (или две) за долю — в такт песне.
        let cells = if rng.chance(0.5) { 1.0 } else { 2.0 };
        let span = if horizontal { width } else { height };
        let along = (ctx.steps() * cells * step + index as f32 * span * 0.37).rem_euclid(span + unit * 0.3) - unit * 0.15;
        let tail = unit * 0.1;
        let (point, back) = if horizontal {
            let y = (lane + shift.1.rem_euclid(step)).rem_euclid(height);
            ((along, y), (along - tail, y))
        } else {
            let x = (lane + shift.0.rem_euclid(step)).rem_euclid(width);
            ((x, along), (x, along - tail))
        };
        pulses.push((point.0, point.1, unit * 0.003));
        trails.move_to(back.0, back.1);
        trails.line_to(point.0, point.1);
    }
    if let Some(path) = trails.finish() {
        canvas.stroke(&path, faded(ctx.palette.ink, 0.2 * alpha), canvas.hair(), Transform::identity(), None);
    }
    canvas.dots(&pulses, faded(ctx.palette.accent, 0.6 * alpha), Transform::identity());
}

/// Развёртка экрана терминала: бледная линия медленно ползёт вниз.
pub fn scanline(canvas: &mut Canvas, ctx: &Ctx, top: f32, bottom: f32, alpha: f32) {
    // Проход развёртки — за четыре такта песни.
    let p = (ctx.beats() / 16.0).rem_euclid(1.0);
    let y = top + (bottom - top) * p;
    if let Some(line) = paint::segment(0.0, y, ctx.width, y) {
        canvas.stroke(&line, faded(ctx.palette.ink, 0.12 * alpha), canvas.hair(), Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::feel::Mood;
    use crate::stage::paint::Palette;

    /// Сколько стоит каждый фон: `cargo test --release -p nyanoto-app fx_costs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn fx_costs() {
        let (width, height) = (720u32, 460u32);
        let mut bytes = vec![0u8; (width * height * 4) as usize];
        let feel = nyanoto_core::Feel {
            loud: vec![-9.0; 600],
            flux: vec![0.2; 600],
            bright: vec![0.08; 600],
            noise: vec![0.08; 600],
            beats: (0..120).map(|beat| beat as f32 * 0.5).collect(),
        };
        let mood = Mood::from_feel(&feel);
        let palette = Palette {
            ink: tiny_skia::Color::from_rgba8(235, 235, 235, 255),
            ground: tiny_skia::Color::from_rgba8(20, 20, 20, 255),
            accent: tiny_skia::Color::from_rgba8(80, 140, 255, 255),
        };
        let lines = [];
        let mut measure = |name: &str, draw: &dyn Fn(&mut Canvas, &Ctx)| {
            let started = std::time::Instant::now();
            for frame in 0..100 {
                let ctx = Ctx { t: 20.0 + frame as f32 / 60.0, width: width as f32, height: height as f32, palette, lines: &lines, mood: &mood, lead: 0.0 };
                let pixmap = tiny_skia::PixmapMut::from_bytes(&mut bytes, width, height).unwrap();
                let mut canvas = Canvas { pixmap, root: Transform::identity(), spare: None, blank: true };
                draw(&mut canvas, &ctx);
            }
            println!("{name}: {:.2} мс", started.elapsed().as_secs_f64() * 10.0);
        };
        measure("dust", &|canvas, ctx| dust(canvas, ctx, 1, 30, (0.0, 0.0), 1.0));
        measure("grid", &|canvas, ctx| grid(canvas, ctx, 1, 1.0));
        measure("scanline", &|canvas, ctx| scanline(canvas, ctx, 0.0, 460.0, 1.0));
    }
}
