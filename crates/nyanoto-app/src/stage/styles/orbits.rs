//! «Орбита»: кольца из светодиодных точек (9 в высоту, 3 вглубь), текст бежит по внешней
//! стенке как на табло. Варианты: одна орбита, две вложенные, цепь из двух звеньев.
//! Кольца рисуются кусками от дальних к ближним — ближнее звено закрывает дальнее.
//! Вокруг — звёздная пыль, пылинки вдоль колец и 1–2 фигуры из точек.

use std::f32::consts::{FRAC_PI_2, TAU};

use tiny_skia::{Color, FillRule, Mask, Transform};

use super::{Ctx, Part, Scene};
use crate::stage::motion::{self, Rng, progress};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::{Face, Fonts};

type Vec3 = [f32; 3];
/// Точка на экране: x, y, радиус, яркость.
type Dot = (f32, f32, f32, f32);

fn plus(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn times(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn unit(a: Vec3) -> Vec3 {
    times(a, 1.0 / dot(a, a).sqrt().max(1e-6))
}

/// 9 рядов, 3 слоя. Шрифт Press Start 2P: 8 точек на кегль — точка в точку с кольцом.
const ROWS: usize = 9;
const LAYERS: usize = 3;
const CAPS: f32 = 7.0;
const BASELINE: f32 = 9.0;
const DOTS_PER_EM: f32 = 8.0;
/// Колонок в куске (кольцо рисуется кусками от дальних к ближним).
const CHUNK: usize = 8;
/// Слои чуть дальше друг от друга — кромка читается как три точки.
const LAYER_STEP: f32 = 1.4;

/// Промежутки между словами и между строками (в колонках точек).
const WORD_GAP: f32 = 5.0;
const LINE_GAP: f32 = 10.0;

/// Фигура рядом с кольцами: облако точек.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Figure {
    Cube,
    Rhombohedron,
    Sphere,
    Pyramid,
}

/// Точек на ребро решётки облака.
const CELLS: usize = 8;

impl Figure {
    const ALL: [Figure; 4] = [Figure::Cube, Figure::Rhombohedron, Figure::Sphere, Figure::Pyramid];

    /// Облако точек в −1…1; вес: внутри 0,35, грани 0,75, рёбра 1.
    /// Точки на гранях и внутри чуть сдвинуты — иначе ряды решётки сливаются в «лучи».
    fn cloud(self) -> Vec<(Vec3, f32)> {
        let weight = |sides: usize| [0.35, 0.75, 1.0][sides.min(2)];
        let mut rng = Rng::new(0xC10D ^ self as u64);
        let mut shake = |value: f32, still: bool, cell: f32| if still { value } else { (value + rng.range(-0.3, 0.3) * cell).clamp(-1.0, 1.0) };
        let mut points: Vec<(Vec3, f32)> = Vec::new();
        match self {
            Figure::Cube | Figure::Rhombohedron => {
                // Ромбоэдр: рёбра под 60°, длинная диагональ вертикально.
                let (a, b) = ((1.0f32 / 3.0).sqrt(), (2.0f32 / 3.0).sqrt());
                let edges: [Vec3; 3] = [0.0f32, 1.0, 2.0].map(|n| {
                    let (sin, cos) = (n * TAU / 3.0).sin_cos();
                    [a * cos, b, a * sin]
                });
                let last = CELLS - 1;
                let cell = 2.0 / last as f32;
                for i in 0..CELLS {
                    for j in 0..CELLS {
                        for k in 0..CELLS {
                            let sides = [i, j, k].iter().filter(|n| **n == 0 || **n == last).count();
                            let point = [i, j, k].map(|n| {
                                // Ромбоэдр не сдвигаем — его ряды к зрителю не смотрят.
                                let on_side = n == 0 || n == last;
                                shake(-1.0 + n as f32 * cell, on_side || sides >= 2 || self == Figure::Rhombohedron, cell)
                            });
                            let point = if self == Figure::Rhombohedron { plus(plus(times(edges[0], point[0]), times(edges[1], point[1])), times(edges[2], point[2])) } else { point };
                            points.push((point, weight(sides)));
                        }
                    }
                }
            }
            Figure::Pyramid => {
                // Слой `level` сверху — квадрат (level + 1) × (level + 1).
                for level in 0..CELLS {
                    let half = level as f32 / 2.0;
                    for i in 0..=level {
                        for k in 0..=level {
                            let (side_i, side_k) = (i == 0 || i == level, k == 0 || k == level);
                            let base = level == CELLS - 1;
                            let sides = usize::from(side_i) + usize::from(side_k) + usize::from(base);
                            // Сдвиг — по слою (на боковой грани — вдоль неё).
                            let x = shake((i as f32 - half) / CELLS as f32, side_i || sides >= 2, 1.0 / CELLS as f32) * CELLS as f32;
                            let z = shake((k as f32 - half) / CELLS as f32, side_k || sides >= 2, 1.0 / CELLS as f32) * CELLS as f32;
                            points.push(([x, -(level as f32) * 0.85, z], weight(sides)));
                        }
                    }
                }
            }
            Figure::Sphere => {
                // Сферы радиусом 1, ⅔, ⅓ и середина; точки по золотому углу.
                let golden = std::f32::consts::PI * (3.0 - 5.0f32.sqrt());
                points.push(([0.0; 3], weight(0)));
                for (radius, count) in [(1.0f32, 150usize), (0.66, 64), (0.33, 16)] {
                    for index in 0..count {
                        let y = 1.0 - 2.0 * (index as f32 + 0.5) / count as f32;
                        let ring = (1.0 - y * y).sqrt();
                        let (sin, cos) = (index as f32 * golden).sin_cos();
                        points.push(([ring * cos * radius, y * radius, ring * sin * radius], weight(usize::from(radius == 1.0))));
                    }
                }
            }
        }
        // Центр и размер — по габаритам: фигуры одного размера, пирамида не уезжает вверх.
        let low = points.iter().fold([f32::MAX; 3], |low, (point, _)| [0, 1, 2].map(|axis| low[axis].min(point[axis])));
        let high = points.iter().fold([f32::MIN; 3], |high, (point, _)| [0, 1, 2].map(|axis| high[axis].max(point[axis])));
        let middle = times(plus(low, high), 0.5);
        let far = points.iter().flat_map(|(point, _)| plus(*point, times(middle, -1.0))).fold(0.0f32, |far, value| far.max(value.abs())).max(1e-6);
        points.iter().map(|(point, weight)| (times(plus(*point, times(middle, -1.0)), 1.0 / far), *weight)).collect()
    }
}

/// Пылинка вдоль кольца, над или под лентой.
struct Mote {
    ring: usize,
    up: f32,
    out: f32,
    /// Угловая скорость (рад/с, знак — направление) и начальный угол.
    speed: f32,
    start: f32,
    /// Фаза лёгкой волны по высоте.
    wave: f32,
}

impl Mote {
    /// Точка пути пылинки на угле `angle` кольца `ring` (шаг точек `pitch`).
    fn point(&self, ring: &Ring, angle: f32, t: f32, pitch: f32) -> Vec3 {
        let (radius, lift) = ring.shape(angle, t);
        let wave = 0.6 * pitch * (0.4 * t + self.wave + 2.0 * angle).sin();
        plus(plus(ring.center, times(ring.radial(angle), radius + self.out * pitch)), times(ring.up, lift + self.up * pitch + wave))
    }
}

/// Вид части.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Single,
    /// Две орбиты с общим центром, разного размера и наклона.
    Double,
    /// Две орбиты, продетые друг в друга.
    Chain,
}

/// Орбита: центр, плоскость, ось, радиус, неровность.
struct Ring {
    center: Vec3,
    across: Vec3,
    toward: Vec3,
    up: Vec3,
    radius: f32,
    /// Фазы неровности (радиус и волна по высоте).
    phases: [f32; 3],
}

impl Ring {
    /// Наклон вокруг оси x на `tilt` (вперёд — вниз) и крен `roll` (градусы).
    fn new(center: Vec3, radius: f32, tilt: f32, roll: f32, phases: [f32; 3]) -> Self {
        let (sa, ca) = tilt.to_radians().sin_cos();
        let (sr, cr) = roll.to_radians().sin_cos();
        Self { center, across: [cr, sr, 0.0], toward: [sa * sr, -sa * cr, ca], up: [-ca * sr, ca * cr, sa], radius, phases }
    }

    fn radial(&self, angle: f32) -> Vec3 {
        plus(times(self.across, angle.cos()), times(self.toward, angle.sin()))
    }

    /// Неровная орбита: радиус и высота чуть гуляют.
    fn shape(&self, angle: f32, t: f32) -> (f32, f32) {
        let [a, b, c] = self.phases;
        // Без волны 2θ — она делала из кольца овал.
        let radius = self.radius * (1.0 + 0.012 * (3.0 * angle + a + 0.02 * t).sin() + 0.008 * (5.0 * angle + b - 0.015 * t).sin());
        let lift = self.radius * 0.012 * (angle + c + 0.012 * t).sin();
        (radius, lift)
    }

    /// Точка орбиты (для проверок).
    #[cfg(test)]
    fn point(&self, angle: f32, up: f32, t: f32) -> Vec3 {
        let (radius, lift) = self.shape(angle, t);
        plus(plus(self.center, times(self.radial(angle), radius)), times(self.up, up + lift))
    }
}

/// Взгляд: середина экрана, угол сверху, крен, расстояние до зрителя.
struct Eye {
    center: (f32, f32),
    rise: (f32, f32),
    roll: (f32, f32),
    distance: f32,
}

impl Eye {
    /// Точка мира → точка экрана и масштаб.
    fn project(&self, point: Vec3) -> ((f32, f32), f32) {
        let [x, y, z] = point;
        let depth = z * self.rise.1 + y * self.rise.0;
        let scale = self.distance / (self.distance - depth).max(self.distance * 0.1);
        let (sx, sy) = (x * scale, (-y * self.rise.1 + z * self.rise.0) * scale);
        let (sin, cos) = self.roll;
        ((self.center.0 + sx * cos - sy * sin, self.center.1 + sx * sin + sy * cos), scale)
    }

    fn toward(&self, point: Vec3) -> Vec3 {
        unit(plus([0.0, self.distance * self.rise.0, self.distance * self.rise.1], times(point, -1.0)))
    }
}

/// Слово на орбите: когда поют, колонка, яркость точек.
struct Moon {
    line: usize,
    at: f32,
    place: f32,
    start: i64,
    width: usize,
    light: Vec<u8>,
}

/// Текст одной орбиты.
#[derive(Default)]
struct Track {
    moons: Vec<Moon>,
}

impl Track {
    /// Опорные точки бега: начала и концы строк.
    fn knots(&self) -> Vec<(f32, f32)> {
        let mut knots: Vec<(f32, f32)> = Vec::new();
        let mut index = 0;
        while index < self.moons.len() {
            let line = self.moons[index].line;
            let last = (index..self.moons.len()).take_while(|other| self.moons[*other].line == line).last().unwrap_or(index);
            for moon in [&self.moons[index], &self.moons[last]] {
                let knot = (moon.at, moon.place);
                if knots.last().is_none_or(|previous| knot.0 > previous.0 + 0.05 && knot.1 > previous.1) {
                    knots.push(knot);
                }
            }
            index = last + 1;
        }
        knots
    }

    /// Какая колонка перед зрителем в момент `t`: монотонная кубическая кривая (Фрич — Карлсон).
    fn turn(&self, t: f32) -> f32 {
        let knots = self.knots();
        let count = knots.len();
        let Some(&(first_at, first_place)) = knots.first() else {
            return 0.0;
        };
        if count < 2 {
            return first_place - 12.0 * (1.0 - (-(first_at - t).max(0.0) / 1.5).exp());
        }
        let slope = |index: usize| (knots[index + 1].1 - knots[index].1) / (knots[index + 1].0 - knots[index].0);
        let speed = |index: usize| {
            if index == 0 {
                return slope(0);
            }
            if index == count - 1 {
                return slope(count - 2);
            }
            let (before, after) = (slope(index - 1), slope(index));
            if before <= 0.0 || after <= 0.0 { 0.0 } else { 2.0 / (1.0 / before + 1.0 / after) }
        };
        if t <= first_at {
            return first_place - speed(0) * 1.5 * (1.0 - (-(first_at - t) / 1.5).exp());
        }
        let next = knots.partition_point(|knot| knot.0 <= t);
        if next >= count {
            let (at, place) = knots[count - 1];
            return place + speed(count - 1) * 1.5 * (1.0 - (-(t - at) / 1.5).exp());
        }
        let index = next - 1;
        let length = knots[next].0 - knots[index].0;
        let share = ((t - knots[index].0) / length).clamp(0.0, 1.0);
        let (s2, s3) = (share * share, share * share * share);
        let (h00, h10, h01, h11) = (2.0 * s3 - 3.0 * s2 + 1.0, s3 - 2.0 * s2 + share, -2.0 * s3 + 3.0 * s2, s3 - s2);
        h00 * knots[index].1 + h10 * length * speed(index) + h01 * knots[next].1 + h11 * length * speed(next)
    }

    /// Строка под колонкой (и между её словами).
    fn line_at(&self, column: i64) -> Option<usize> {
        let index = self.moons.partition_point(|moon| moon.start <= column);
        let before = index.checked_sub(1).map(|index| &self.moons[index])?;
        if column < before.start + before.width as i64 {
            return Some(before.line);
        }
        let after = self.moons.get(index)?;
        (after.line == before.line).then_some(before.line)
    }

    /// Слово под колонкой текста `column`.
    fn moon(&self, column: i64) -> Option<&Moon> {
        let index = self.moons.partition_point(|moon| moon.start <= column).checked_sub(1)?;
        let moon = &self.moons[index];
        ((column - moon.start) < moon.width as i64).then_some(moon)
    }
}

pub struct Orbits {
    part: Part,
    shape: Shape,
    tracks: Vec<Track>,
    laid: usize,
    /// 1–2 фигуры рядом с кольцами и их облака точек.
    figures: Vec<(Figure, Vec<(Vec3, f32)>)>,
    motes: Vec<Mote>,
}

impl Orbits {
    pub fn new(part: &Part) -> Self {
        let shape = match part.variant % 3 {
            0 => Shape::Single,
            1 => Shape::Double,
            _ => Shape::Chain,
        };
        let mut rng = Rng::new(part.seed ^ 0xF16E);
        let mut kinds = Figure::ALL.to_vec();
        let count = if rng.chance(0.5) { 1 } else { 2 };
        let figures = (0..count)
            .map(|_| {
                let figure = kinds.remove(rng.below(kinds.len()));
                (figure, figure.cloud())
            })
            .collect();
        // Пылинки: на одном кольце — три, на двух — по две на каждом.
        let rings = if shape == Shape::Single { 1 } else { 2 };
        let per_ring = if rings == 1 { 3 } else { 2 };
        let edge = ROWS as f32 / 2.0;
        let motes = (0..rings * per_ring)
            .map(|number| Mote {
                ring: number / per_ring,
                up: rng.sign() * (edge + rng.range(3.0, 5.0)),
                out: rng.range(-1.0, 1.5),
                speed: rng.sign() * rng.range(0.1, 0.2),
                start: rng.range(0.0, TAU),
                wave: rng.range(0.0, TAU),
            })
            .collect();
        Self { part: *part, shape, tracks: Vec::new(), laid: 0, figures, motes }
    }

    /// Орбиты, взгляд и шаг точек (в точках сцены) для этого размера сцены.
    fn scene(&self, ctx: &Ctx) -> (Eye, Vec<Ring>, f32) {
        let mut rng = Rng::new(self.part.seed ^ 0x0B17);
        let (width, height) = (ctx.width, ctx.height);
        let size = ctx.unit();
        let phases = |rng: &mut Rng| [rng.range(0.0, TAU), rng.range(0.0, TAU), rng.range(0.0, TAU)];
        let (rise, radius, letter) = match self.shape {
            Shape::Single => (rng.range(22.0, 30.0), (width * 0.42).min(size * 0.95), 0.085),
            Shape::Double => (rng.range(12.0, 18.0), (width * 0.4).min(size * 0.6), 0.07),
            Shape::Chain => (rng.range(10.0, 16.0), (width * 0.27).min(size * 0.55), 0.066),
        };
        let distance = radius * 6.0;
        let front = distance / (distance - radius * rise.to_radians().cos());
        // Шаг точек: заглавная буква на переднем крае — `letter` сцены.
        let pitch = size * letter / front / CAPS;
        let rings = match self.shape {
            Shape::Single => vec![Ring::new([0.0; 3], radius, 0.0, 0.0, phases(&mut rng))],
            Shape::Double => {
                // Общий центр, внутренняя меньше и площе — тексты не ложатся друг на друга.
                let outer = rng.range(30.0, 42.0);
                let inner = outer - rng.range(20.0, (outer - 9.0).min(33.0));
                vec![
                    Ring::new([0.0; 3], radius, outer - rise, rng.range(-3.0, 3.0), phases(&mut rng)),
                    Ring::new([0.0; 3], radius * rng.range(0.55, 0.62), inner - rise, rng.range(-4.0, 4.0), phases(&mut rng)),
                ]
            }
            Shape::Chain => {
                // Звенья: центры ближе радиуса, плоскости под разным наклоном — продеты друг в друга.
                let first = rng.range(14.0, 24.0);
                let apart = rng.range(20.0, 33.0);
                let second = if rng.chance(0.4) && first - apart <= -9.0 { first - apart } else { (first + apart).min(46.0) };
                let (left, right) = if rng.chance(0.5) { (first, second) } else { (second, first) };
                let apart = radius * 0.56;
                vec![
                    Ring::new([-apart, 0.0, 0.0], radius, left - rise, 0.0, phases(&mut rng)),
                    Ring::new([apart, 0.0, 0.0], radius, right - rise, 0.0, phases(&mut rng)),
                ]
            }
        };
        let rise = (rise + 1.2 * motion::wander(ctx.t, self.part.seed ^ 0x51, 0.008)).to_radians();
        let roll = (rng.sign() * rng.range(1.0, 3.5) + 0.8 * motion::wander(ctx.t, self.part.seed ^ 0x52, 0.006)).to_radians();
        let eye = Eye { center: (width / 2.0, height * 0.5), rise: rise.sin_cos(), roll: roll.sin_cos(), distance };
        (eye, rings, pitch)
    }

    /// На каких орбитах строка (биты); припев — на обеих.
    fn rings_of(&self, ctx: &Ctx, index: usize) -> u8 {
        if self.shape == Shape::Single {
            return 1;
        }
        if ctx.lines[index].chorus {
            return 3;
        }
        let mut rng = Rng::new(motion::mix(self.part.seed ^ 0x2B, index as u64));
        if rng.chance(0.12) {
            return 3;
        }
        let turn = (index - self.part.first + (self.part.seed & 1) as usize) % 2;
        let turn = if rng.chance(0.2) { 1 - turn } else { turn };
        1 << turn
    }

    /// Слова до строки `upto` — картинками в точках.
    fn grow(&mut self, fonts: &mut Fonts, ctx: &Ctx, upto: usize, rings: usize) {
        self.tracks.resize_with(rings, Track::default);
        let scale = DOTS_PER_EM;
        while self.part.first + self.laid <= upto.min(self.part.last) {
            let index = self.part.first + self.laid;
            let line = &ctx.lines[index];
            let mask = self.rings_of(ctx, index);
            for (number, raw) in line.text.split_whitespace().enumerate() {
                let word = fonts.word(&raw.to_uppercase(), Face::Pixel);
                // Шрифт по сетке точек — буква попадает ровно в точки кольца.
                let width = ((word.advance * scale).round() as usize).max(1);
                let mut light = vec![0u8; ROWS * width];
                if let (Some(path), Some(mut pixels)) = (&word.path, Mask::new(width as u32, ROWS as u32)) {
                    pixels.fill_path(path, FillRule::Winding, false, Transform::from_scale(scale, scale).post_translate(0.0, BASELINE));
                    for (dot, value) in light.iter_mut().zip(pixels.data()) {
                        *dot = if *value >= 128 { 255 } else { 0 };
                    }
                }
                let at = line.words.get(number).copied().unwrap_or(line.start);
                for ring in 0..rings {
                    if mask & (1 << ring) == 0 {
                        continue;
                    }
                    let track = &mut self.tracks[ring];
                    let place = track.moons.last().map_or(0.0, |last| {
                        let gap = if number == 0 { LINE_GAP } else { WORD_GAP };
                        last.place + last.width as f32 / 2.0 + gap + width as f32 / 2.0
                    });
                    let start = (place - width as f32 / 2.0).round() as i64;
                    track.moons.push(Moon { line: index, at, place, start, width, light: light.clone() });
                }
            }
            self.laid += 1;
        }
    }

    /// Яркость слова: спетое — полная, не спетое — половина.
    fn brightness(&self, ctx: &Ctx, moon: &Moon) -> f32 {
        let t = ctx.t;
        let line = &ctx.lines[moon.line];
        let appear = motion::ease_out(progress(t, line.start - 0.4, 0.6));
        let next_start = ctx.lines.get(moon.line + 1).filter(|_| moon.line < self.part.last).map_or(f32::MAX, |next| next.start);
        let gone = progress(t, next_start + 0.6, 1.2);
        let sung = 0.35 + 0.65 * motion::ease_out(progress(t, moon.at, 0.3));
        appear * (1.0 - gone) * sung
    }
}

/// Кусок кольца: глубина, контуры, которые он заслоняет, и его точки.
#[derive(Default)]
struct Piece3 {
    depth: f32,
    ring: usize,
    cover: Vec<Vec<(f32, f32)>>,
    /// Прямоугольник, в котором лежит кусок (левый, верхний, правый, нижний).
    bounds: [f32; 4],
    /// Точки (x, y, радиус, яркость): обычные (белые) и буквы (чёрные).
    plain: Vec<Dot>,
    lit: Vec<Dot>,
}

/// Мерцание точки: у каждой своя медленная волна (0,75…1), от времени.
fn twinkle(seed: u32, t: f32) -> f32 {
    let hash = seed.wrapping_mul(0x9E37_79B9) ^ (seed >> 15);
    let phase = (hash & 1023) as f32 / 1023.0 * TAU;
    let speed = 0.25 + ((hash >> 10) & 255) as f32 / 255.0 * 0.6;
    0.75 + 0.25 * (t * speed + phase).sin()
}

impl Scene for Orbits {
    /// Табло шагает по точкам — 30 кадров/с хватает.
    fn pace(&self, _ctx: &Ctx) -> f32 {
        1.0 / 30.0
    }

    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        let part = self.part;
        let t = ctx.t;
        let (ink, dark) = Self::colors(ctx);
        // Всё вокруг колец — цветом темы.
        let around = ctx.palette.ink;
        let (eye, rings, pitch) = self.scene(ctx);
        // Раскладка на 10 с вперёд — кривая бега не дёргается.
        let current = ctx.current(&part).unwrap_or(part.first);
        let ahead = (current..=part.last).take_while(|line| ctx.lines[*line].start <= t + 10.0).last().unwrap_or(current);
        self.grow(fonts, ctx, ahead.max(current + 1), rings.len());
        let chorus = ctx.lines.get(current).is_some_and(|line| line.chorus && line.start <= t);
        let plain_alpha = if chorus { 0.46 } else { 0.38 };
        // Слои снаружи внутрь: чёрный букв уходит вглубь, светлея.
        let depth_share = [1.0, 0.75, 0.5];

        let mut pieces3: Vec<Piece3> = Vec::new();
        for (number, ring) in rings.iter().enumerate() {
            let columns = ((TAU * ring.radius / pitch).round() as usize).max(64);
            let step = TAU / columns as f32;
            let front = (FRAC_PI_2 / step).round() as i64;
            let track = self.tracks.get(number);
            // Строка сдвигается на целую колонку точек, как на табло.
            let shift = track.map_or(0, |track| track.turn(t).floor() as i64);
            let height = |row: f32| (ROWS as f32 / 2.0 - 0.5 - row) * pitch;
            for chunk in (0..columns).step_by(CHUNK) {
                let last = (chunk + CHUNK).min(columns);
                let mut piece = Piece3 { ring: number, ..Piece3::default() };
                // Что заслоняет кусок: обе стенки.
                for layer in [0, LAYERS - 1] {
                    let mut outline: Vec<(f32, f32)> = Vec::new();
                    let edge = |at: f32, row: f32| {
                        let angle = at * step;
                        let (radius, lift) = ring.shape(angle, t);
                        let point = plus(ring.center, times(ring.radial(angle), radius - layer as f32 * LAYER_STEP * pitch));
                        eye.project(plus(point, times(ring.up, height(row) + lift))).0
                    };
                    let mut at = chunk as f32 - 0.5;
                    while at <= last as f32 - 0.5 + 1e-3 {
                        outline.push(edge(at, -0.5));
                        at += 1.0;
                    }
                    let mut at = last as f32 - 0.5;
                    while at >= chunk as f32 - 0.5 - 1e-3 {
                        outline.push(edge(at, ROWS as f32 - 0.5));
                        at -= 1.0;
                    }
                    piece.cover.push(outline);
                }
                piece.bounds = piece.cover.iter().flatten().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |[l, t, r, b], (x, y)| [l.min(*x), t.min(*y), r.max(*x), b.max(*y)]);
                let middle = (chunk + last) as f32 / 2.0 * step;
                let (radius, lift) = ring.shape(middle, t);
                let center = plus(plus(ring.center, times(ring.radial(middle), radius - pitch)), times(ring.up, lift));
                piece.depth = center[2] * eye.rise.1 + center[1] * eye.rise.0;
                for column in chunk..last {
                    let angle = column as f32 * step;
                    let radial = ring.radial(angle);
                    // Форма орбиты — один раз на колонку.
                    let (radius, lift) = ring.shape(angle, t);
                    let facing = dot(radial, eye.toward(plus(ring.center, times(radial, radius))));
                    let edge = motion::clamp01((facing.abs() - 0.05) / 0.3);
                    let text = shift - (column as i64 - front);
                    let moon = track.and_then(|track| track.moon(text));
                    let word = moon.map_or(0.0, |moon| self.brightness(ctx, moon));
                    // Слои от дальнего к ближнему.
                    let layers: [usize; LAYERS] = if facing >= 0.0 { [2, 1, 0] } else { [0, 1, 2] };
                    // Кромка, которую видно: там все три слоя.
                    let rim = if dot(ring.up, eye.toward(ring.center)) >= 0.0 { 0 } else { ROWS - 1 };
                    // Под строкой точки ярче — буквы читаются как на табло.
                    let line_here = track.and_then(|track| track.line_at(text));
                    let backing = line_here.map_or(1.0, |line| {
                        let data = &ctx.lines[line];
                        let appear = motion::ease_out(progress(t, data.start - 0.4, 0.6));
                        let next_start = ctx.lines.get(line + 1).filter(|_| line < part.last).map_or(f32::MAX, |next| next.start);
                        let shown = appear * (1.0 - progress(t, next_start + 0.6, 1.2));
                        1.0 + 0.6 * shown
                    });
                    // Сбоку точки плотнее — делаем тусклее.
                    let side = 0.4 + 0.6 * facing.abs();
                    for layer in layers {
                        let base = plus(ring.center, times(radial, radius - layer as f32 * LAYER_STEP * pitch));
                        for row in 0..ROWS {
                            let (screen, scale) = eye.project(plus(base, times(ring.up, height(row as f32) + lift)));
                            let size = pitch * scale;
                            let value = moon.map_or(0, |moon| moon.light[row * moon.width + (text - moon.start) as usize]);
                            let glow = if value >= 30 { (f32::from(value) / 255.0).powf(0.8) * word } else { 0.0 };
                            // Изнанка тусклее, буквы там почти не видны.
                            let behind = if facing >= 0.0 { 1.0 } else { 0.35 };
                            if glow >= 0.02 {
                                let behind = if facing >= 0.0 { 1.0 } else { 0.25 };
                                let alpha = glow * depth_share[layer] * behind * (0.3 + 0.7 * edge);
                                let radius = if layer == 0 { 0.38 } else { 0.32 };
                                piece.lit.push((screen.0, screen.1, size * radius, alpha));
                            } else {
                                // Закрытые слои видны только сбоку; кромка — всегда.
                                let nearest = if facing >= 0.0 { 0 } else { LAYERS - 1 };
                                if layer != nearest && row != rim && facing.abs() > 0.6 {
                                    continue;
                                }
                                let share = [1.0, 0.8, 0.62][layer];
                                let backing = if layer == nearest && row != rim { backing } else { 1.0 };
                                let seed = (column as u32).wrapping_mul(73_856_093) ^ (row as u32).wrapping_mul(19_349_663) ^ (layer as u32 * 83_492_791) ^ (number as u32) << 28;
                                let alpha = plain_alpha * share * behind * backing * side * twinkle(seed, t);
                                piece.plain.push((screen.0, screen.1, size * 0.26, alpha.min(1.0)));
                            }
                        }
                    }
                }
                pieces3.push(piece);
            }
        }

        // От дальних к ближним; стираем только чужое кольцо, иначе на стыках полосы.
        pieces3.sort_by(|a, b| a.depth.total_cmp(&b.depth));
        // Фон: пыль и фигуры.
        self.dust(canvas, ctx, &eye, around, visible);
        let ring_box = pieces3.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |[l, t, r, b], piece| {
            [l.min(piece.bounds[0]), t.min(piece.bounds[1]), r.max(piece.bounds[2]), b.max(piece.bounds[3])]
        });
        if ring_box[0] < ring_box[2] {
            self.draw_figures(canvas, ctx, ring_box, pitch, around, visible);
        }
        // Пылинки — между кусками колец по глубине.
        let mut motes = self.mote_dots(ctx, &eye, &rings, pitch);
        motes.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut motes = motes.into_iter().peekable();
        let mut drawn: Vec<(usize, [f32; 4])> = Vec::new();
        let overlaps = |a: &[f32; 4], b: &[f32; 4]| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
        for piece in &pieces3 {
            while let Some((_, dots)) = motes.next_if(|(depth, _)| *depth < piece.depth) {
                canvas.lights(&dots, faded(around, visible));
            }
            if drawn.iter().any(|(ring, bounds)| *ring != piece.ring && overlaps(bounds, &piece.bounds)) {
                for outline in &piece.cover {
                    if let Some(path) = paint::polygon(outline) {
                        canvas.erase(&path, visible);
                    }
                }
            }
            drawn.push((piece.ring, piece.bounds));
            canvas.lights(&piece.plain, faded(ink, visible));
            canvas.lights(&piece.lit, faded(dark, visible));
        }
        for (_, dots) in motes {
            canvas.lights(&dots, faded(around, visible));
        }
    }
}

/// Цвета наоборот: точки чёрные, буквы белые (`false` — как было).
const SWAPPED: bool = true;

impl Orbits {
    /// Цвет точек колец и букв (в светлой теме наоборот).
    fn colors(ctx: &Ctx) -> (Color, Color) {
        let ink = ctx.palette.ink;
        let light_ink = 0.3 * ink.red() + 0.59 * ink.green() + 0.11 * ink.blue() > 0.5;
        let other = if light_ink { Color::BLACK } else { Color::WHITE };
        if SWAPPED { (other, ink) } else { (ink, other) }
    }

    /// Звёздная пыль: мерцает и чуть плывёт.
    fn dust(&self, canvas: &mut Canvas, ctx: &Ctx, eye: &Eye, ink: Color, visible: f32) {
        let (width, height) = (ctx.width, ctx.height);
        let unit = ctx.unit();
        let mut rng = Rng::new(self.part.seed ^ 0xD057);
        let (sway_x, sway_y) = (unit * 0.03 * eye.roll.0 * 4.0, unit * 0.02 * (eye.rise.0 - 0.3));
        let mut dots = Vec::with_capacity(90);
        for star in 0..90u32 {
            let depth = rng.range(0.3, 1.0);
            let x = (rng.range(0.0, 1.0) * width + sway_x * depth + unit * 0.006 * (ctx.t * rng.range(0.02, 0.05) + star as f32).sin()).rem_euclid(width);
            let y = (rng.range(0.0, 1.0) * height + sway_y * depth).rem_euclid(height);
            let alpha = rng.range(0.06, 0.28) * twinkle(star.wrapping_mul(2_654_435_761), ctx.t * 0.8).powi(3);
            dots.push((x, y, unit * rng.range(0.0018, 0.0042) * depth, alpha));
        }
        canvas.lights(&dots, faded(ink, visible));
    }

    /// Фигуры возле колец: медленно вращаются, ближние точки ярче.
    fn draw_figures(&self, canvas: &mut Canvas, ctx: &Ctx, ring_box: [f32; 4], pitch: f32, ink: Color, visible: f32) {
        let (width, height, t) = (ctx.width, ctx.height, ctx.t);
        let unit = ctx.unit();
        let margin = unit * 0.04;
        let [left, top, right, bottom] = ring_box;
        let mut rng = Rng::new(self.part.seed ^ 0xF1C0);
        let left_first = rng.chance(0.5);
        let mut dots = Vec::new();
        for (number, (figure, cloud)) in self.figures.iter().enumerate() {
            let above = number == 0;
            let on_left = left_first == above;
            let room = if above { top - margin } else { height - margin - bottom };
            let size = (unit * 0.2).min(room * 0.75).max(unit * 0.14);
            let half = size / 2.0;
            let x = if on_left { left + (right - left) * 0.14 } else { right - (right - left) * 0.14 };
            let x = x.clamp(margin + half, (width - margin - half).max(margin + half));
            let bob = unit * 0.008 * (0.3 * t + rng.range(0.0, TAU)).sin();
            // Если места мало — фигура заходит за край колец (она позади них).
            let y = if above { (top - half - unit * 0.03).max(margin + half) } else { (bottom + half + unit * 0.03).min(height - margin - half) } + bob;
            // Вращение вокруг вертикали и наклон к зрителю.
            let spin = rng.sign() * rng.range(0.15, 0.25) * t + rng.range(0.0, TAU);
            let (sin, cos) = spin.sin_cos();
            // Куб — сильнее (виден верх, не плоский квадрат), кристалл — слабее.
            let tilt = match figure {
                Figure::Cube => rng.range(28.0f32, 36.0),
                Figure::Rhombohedron => rng.range(12.0, 20.0),
                Figure::Sphere | Figure::Pyramid => rng.range(16.0, 24.0),
            };
            let (tilt_sin, tilt_cos) = tilt.to_radians().sin_cos();
            for (index, ([px, py, pz], weight)) in cloud.iter().enumerate() {
                let (x1, z1) = (px * cos + pz * sin, -px * sin + pz * cos);
                let (y2, z2) = (py * tilt_cos - z1 * tilt_sin, py * tilt_sin + z1 * tilt_cos);
                let near = 1.0 / (1.0 - 0.2 * z2);
                let seed = (index as u32).wrapping_mul(2_246_822_519) ^ (number as u32) << 29;
                let alpha = 0.34 * weight * (0.35 + 0.65 * (z2 + 1.0) / 2.0) * twinkle(seed, t);
                dots.push((x + x1 * half * near, y - y2 * half * near, pitch * 0.26 * near, alpha));
            }
        }
        canvas.lights(&dots, faded(ink, visible));
    }

    /// Пылинки: голова и след по орбите.
    fn mote_dots(&self, ctx: &Ctx, eye: &Eye, rings: &[Ring], pitch: f32) -> Vec<(f32, Vec<Dot>)> {
        let t = ctx.t;
        self.motes
            .iter()
            .filter_map(|mote| {
                let ring = rings.get(mote.ring)?;
                let head = mote.start + mote.speed * t;
                let point = mote.point(ring, head, t, pitch);
                let depth = point[2] * eye.rise.1 + point[1] * eye.rise.0;
                // Позади кольца — тусклее.
                let facing = dot(ring.radial(head), eye.toward(point));
                let shade = 0.5 + 0.25 * (facing + 1.0);
                let step = 1.3 * pitch / ring.radius;
                let dots = (0..16)
                    .map(|number| {
                        let angle = head - mote.speed.signum() * number as f32 * step;
                        let (screen, scale) = eye.project(mote.point(ring, angle, t, pitch));
                        let fade = 1.0 - number as f32 / 16.0;
                        (screen.0, screen.1, pitch * scale * (0.36 * fade + 0.12), 0.85 * shade * fade * fade)
                    })
                    .collect();
                Some((depth, dots))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::styles::{Kind, Letters, Line};

    fn lines() -> Vec<Line> {
        vec![
            Line { text: "мы будем петь и смеяться".into(), words: vec![1.0, 1.4, 1.7, 2.3, 2.45], estimated: false, start: 1.0, end: 4.0, chorus: false },
            Line { text: "как дети".into(), words: vec![6.0, 6.8], estimated: false, start: 6.0, end: 9.0, chorus: false },
            Line { text: "все вместе".into(), words: vec![9.0, 9.5], estimated: false, start: 9.0, end: 11.0, chorus: true },
        ]
    }

    fn context<'a>(lines: &'a [Line], mood: &'a crate::stage::feel::Mood) -> Ctx<'a> {
        let palette = crate::stage::paint::Palette { ink: Color::WHITE, ground: Color::BLACK, accent: Color::WHITE };
        Ctx { t: 1.0, width: 640.0, height: 400.0, palette, lines, mood, lead: 0.0 }
    }

    fn part(seed: u64, last: usize, variant: u8) -> Part {
        Part { kind: Kind::Orbits, first: 0, last, start: 1.0, end: 11.0, seed, strong: false, letters: Letters::Narrow, variant }
    }

    /// Строка бежит плавно, буквы — в точки.
    #[test]
    fn the_marquee_runs_smoothly_and_letters_become_dots() {
        let mut fonts = Fonts::new();
        let lines = lines();
        let mood = crate::stage::feel::Mood::neutral(0.5);
        let ctx = context(&lines, &mood);
        let mut orbits = Orbits::new(&part(3, 1, 0));
        assert_eq!(orbits.shape, Shape::Single);
        orbits.grow(&mut fonts, &ctx, 1, 1);
        let track = &orbits.tracks[0];
        // У каждого слова есть горящие точки, и не все точки горят.
        for moon in &track.moons {
            let lit = moon.light.iter().filter(|value| **value > 128).count();
            assert!(lit > moon.width && lit < moon.light.len(), "{lit} из {}", moon.light.len());
        }
        for index in [0, 4, 5, 6] {
            let moon = &track.moons[index];
            assert!((track.turn(moon.at) - moon.place).abs() < 1e-3, "{index}");
        }
        let frame = 1.0 / 60.0;
        let speed = |t: f32| (track.turn(t + frame) - track.turn(t)) / frame;
        let line = &track.moons[..5];
        let average = (line[4].place - line[0].place) / (line[4].at - line[0].at);
        let mut t = 1.0;
        while t < 2.1 {
            assert!(speed(t) > average / 3.0, "{t}: {} при средней {average}", speed(t));
            t += frame;
        }
        let top = (0..600).map(|step| speed(step as f32 * frame)).fold(0.0f32, f32::max);
        let mut previous = speed(0.0);
        for step in 1..600 {
            let now = speed(step as f32 * frame);
            assert!(now >= -1e-3 && (now - previous).abs() < top * 0.15, "{}: {previous} → {now}", step as f32 * frame);
            previous = now;
        }
    }

    /// Две орбиты: наклоны различаются, вложенные не пересекаются, звенья продеты.
    #[test]
    fn two_orbits_differ_in_angle_nest_or_link() {
        let mut fonts = Fonts::new();
        let lines = lines();
        let mood = crate::stage::feel::Mood::neutral(0.5);
        let ctx = context(&lines, &mood);
        for seed in 0..40 {
            for variant in [1, 2] {
                let orbits = Orbits::new(&part(seed, 2, variant));
                let (_, rings, pitch) = orbits.scene(&ctx);
                let angle = dot(rings[0].up, rings[1].up).clamp(-1.0, 1.0).acos().to_degrees();
                assert!((19.0..=37.0).contains(&angle), "{:?} {seed}: {angle}°", orbits.shape);
                if orbits.shape == Shape::Double {
                    // Внутреннее — наружный слой, внешнее — внутренний.
                    let half = ROWS as f32 / 2.0 * pitch;
                    let reach = |ring: &Ring, inset: f32| -> Vec<f32> {
                        (0..360)
                            .flat_map(|step| [half, -half].map(|up| (step as f32 / 360.0 * TAU, up)))
                            .map(|(angle, up)| {
                                let (radius, lift) = ring.shape(angle, 3.0);
                                let point = plus(plus(ring.center, times(ring.radial(angle), radius - inset)), times(ring.up, up + lift));
                                dot(point, point).sqrt()
                            })
                            .collect()
                    };
                    let inner = reach(&rings[1], 0.0).into_iter().fold(f32::MIN, f32::max);
                    let outer = reach(&rings[0], (LAYERS - 1) as f32 * LAYER_STEP * pitch).into_iter().fold(f32::MAX, f32::min);
                    assert!(inner < outer, "{seed}: орбиты задевают друг друга ({inner} ≥ {outer})");
                    continue;
                }
                let (a, b) = (&rings[0], &rings[1]);
                let mut inside = 0;
                let mut previous = None;
                for step in 0..=720 {
                    let point = a.point((step as f32 + 0.5) / 720.0 * TAU, 0.0, 3.0);
                    let away = plus(point, times(b.center, -1.0));
                    let side = dot(away, b.up);
                    if let Some(before) = previous
                        && (before > 0.0) != (side > 0.0)
                    {
                        let across = plus(away, times(b.up, -dot(away, b.up)));
                        if dot(across, across).sqrt() < b.radius {
                            inside += 1;
                        }
                    }
                    previous = Some(side);
                }
                assert_eq!(inside, 1, "{seed}: звенья не сцеплены");
            }
        }
        let mut orbits = Orbits::new(&part(5, 2, 2));
        orbits.grow(&mut fonts, &ctx, 2, 2);
        let on = |ring: usize, line: usize| orbits.tracks[ring].moons.iter().any(|moon| moon.line == line);
        assert!(on(0, 2) && on(1, 2), "припев — на обеих");
        for line in 0..2 {
            assert!(on(0, line) || on(1, line), "строка {line} потерялась");
        }
    }

    /// Фигур одна или две, со временем встречаются все.
    #[test]
    fn one_or_two_different_figures_and_all_kinds_come() {
        let mut seen = Vec::new();
        let mut counts = [false; 3];
        for seed in 0..60 {
            let orbits = Orbits::new(&part(seed, 2, 0));
            let kinds: Vec<Figure> = orbits.figures.iter().map(|(figure, _)| *figure).collect();
            assert!((1..=2).contains(&kinds.len()), "зерно {seed}: {} фигур", kinds.len());
            assert!(kinds.len() < 2 || kinds[0] != kinds[1], "зерно {seed}: две одинаковые");
            counts[kinds.len()] = true;
            seen.extend(kinds);
        }
        assert!(counts[1] && counts[2], "не встретилось одно из чисел фигур");
        for figure in Figure::ALL {
            assert!(seen.contains(&figure), "{figure:?} не выпал ни разу");
        }
    }

    /// Облака держат форму.
    #[test]
    fn figure_clouds_keep_their_shape() {
        for figure in Figure::ALL {
            let cloud = figure.cloud();
            assert!(cloud.len() > 150, "{figure:?}: мало точек ({})", cloud.len());
            let reach = cloud.iter().flat_map(|(point, _)| *point).fold(0.0f32, |far, value| far.max(value.abs()));
            assert!((reach - 1.0).abs() < 1e-3, "{figure:?}: габарит {reach}");
            assert!(cloud.iter().any(|(_, weight)| *weight >= 1.0) || figure == Figure::Sphere, "{figure:?}: нет рёбер");
            assert!(cloud.iter().any(|(_, weight)| *weight < 0.5), "{figure:?}: нет точек внутри");
        }
        let pyramid = Figure::Pyramid.cloud();
        let wide = |low: f32, high: f32| pyramid.iter().filter(|(point, _)| point[1] >= low && point[1] <= high).map(|(point, _)| point[0].abs().max(point[2].abs())).fold(0.0f32, f32::max);
        assert!(wide(0.5, 1.0) < 0.5 * wide(-1.0, -0.5), "пирамида не сужается кверху");
        let sphere = Figure::Sphere.cloud();
        let far = sphere.iter().map(|(point, _)| dot(*point, *point).sqrt()).fold(0.0f32, f32::max);
        assert!(far < 1.05, "шар не круглый: {far}");
    }

    /// Пылинки бегут по своему кольцу, не по буквам.
    #[test]
    fn motes_follow_their_ring_above_or_below_the_text() {
        let pitch = 1.0;
        for variant in 0..3 {
            for seed in 0..12 {
                let orbits = Orbits::new(&part(seed, 2, variant));
                let rings = if variant == 0 { 1 } else { 2 };
                for ring_number in 0..rings {
                    assert!(orbits.motes.iter().any(|mote| mote.ring == ring_number), "вариант {variant}, зерно {seed}: у кольца {ring_number} нет пылинок");
                }
                let ring = Ring::new([0.0; 3], 100.0, 25.0, 4.0, [0.3, 1.1, 2.0]);
                for mote in &orbits.motes {
                    for step in 0..40 {
                        let t = step as f32 * 0.7;
                        let angle = mote.start + mote.speed * t;
                        let away = plus(mote.point(&ring, angle, t, pitch), times(ring.center, -1.0));
                        let height = dot(away, ring.up);
                        let flat = plus(away, times(ring.up, -height));
                        let (radius, lift) = ring.shape(angle, t);
                        let out = dot(flat, flat).sqrt() - radius;
                        assert!((-1.01..=1.51).contains(&out), "пылинка ушла от стенки: {out}");
                        assert!((height - lift).abs() >= ROWS as f32 / 2.0 + 2.3, "пылинка на буквах: {}", height - lift);
                    }
                }
            }
        }
    }
}
