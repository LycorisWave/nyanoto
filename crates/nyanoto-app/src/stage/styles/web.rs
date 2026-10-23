//! «Паутина» — главный стиль: слова под наклоном ±30°, между ними нити.
//! Варианты: камера летит за словом, паутина на экране, созвездие.
//! Иногда поперечная нить к старому слову — чтобы паутина, а не цепочка.
//! В такт не качаем, камера едет плавно, небо крутится ровно — иначе дёргает.

use std::f32::consts::TAU;

use tiny_skia::{PathBuilder, Transform};

use super::{Ctx, Lazy, Part, Scene, draw_word, faces, ink_center, is_key};
use crate::stage::motion::{self, Camera, Entry, Exit, Look, Rng, apply, ease_out, progress, tempo};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::{Fonts, Word};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Camera,
    Screen,
    Stars,
}

/// Слово-узел паутины (координаты мира).
struct Node {
    line: usize,
    at: f32,
    x: f32,
    y: f32,
    degrees: f32,
    size: f32,
    /// Полуразмеры закрашенного (уже в масштабе).
    half: (f32, f32),
    word: Word,
    /// Поперечная нить к прежнему узлу.
    extra: Option<usize>,
    /// Первое слово строки (у «экрана» — без нити к прошлой строке).
    first: bool,
}

/// Полуразмеры повёрнутого слова по осям экрана.
fn extents(node: &Node) -> (f32, f32) {
    let angle = node.degrees.to_radians();
    let (cos, sin) = (angle.cos().abs(), angle.sin().abs());
    (cos * node.half.0 + sin * node.half.1, sin * node.half.0 + cos * node.half.1)
}

/// Точка на краю повёрнутого слова в сторону `toward`.
fn edge(node: &Node, at: (f32, f32), toward: (f32, f32), pad: f32) -> (f32, f32) {
    let angle = -node.degrees.to_radians();
    let (dx, dy) = (toward.0 - at.0, toward.1 - at.1);
    // В координатах слова.
    let (lx, ly) = (dx * angle.cos() - dy * angle.sin(), dx * angle.sin() + dy * angle.cos());
    let (hx, hy) = (node.half.0 + pad, node.half.1 + pad);
    let scale = (lx.abs() / hx.max(1e-3)).max(ly.abs() / hy.max(1e-3)).max(1.0);
    let (ex, ey) = (lx / scale, ly / scale);
    let back = -angle;
    (at.0 + ex * back.cos() - ey * back.sin(), at.1 + ex * back.sin() + ey * back.cos())
}

pub struct Web {
    part: Part,
    variant: Variant,
    /// Узлы всей части («камера», «созвездие») по порядку слов.
    nodes: Vec<Node>,
    /// Сколько строк части уже разложено.
    laid: usize,
    /// Узлы строк для «экрана».
    screens: Lazy<Vec<Node>>,
    /// Какие появления и уходы у строк.
    styles: Lazy<(Entry, Exit)>,
    size: (f32, f32),
}

impl Web {
    pub fn new(part: &Part, variant: Variant) -> Self {
        Self { part: *part, variant, nodes: Vec::new(), laid: 0, screens: Lazy::default(), styles: Lazy::default(), size: (0.0, 0.0) }
    }

    /// Кегль слова: главные крупнее, на припеве ещё крупнее.
    fn word_size(&self, unit: f32, key: bool, chorus: bool) -> f32 {
        let base = match self.variant {
            Variant::Stars => if key { 0.065 } else { 0.048 },
            _ => if key { 0.105 } else { 0.07 },
        };
        unit * base * if chorus { 1.2 } else { 1.0 } * if self.part.strong { 1.05 } else { 1.0 }
    }

    fn make_node(&self, fonts: &mut Fonts, ctx: &Ctx, line: usize, number: usize, raw: &str, rng: &mut Rng) -> Node {
        let (key_face, small_face, upper) = faces(&self.part);
        let key = is_key(raw);
        let text = if upper { raw.to_uppercase() } else { raw.to_string() };
        let word = fonts.word(&text, if key { key_face } else { small_face });
        let data = &ctx.lines[line];
        // Не шире половины сцены — паутина должна быть видна, а не одно слово.
        let size = self.word_size(ctx.unit(), key, data.chorus).min(ctx.width * 0.5 / word.advance.max(0.5));
        let spread = if self.part.strong { 30.0 } else { 18.0 };
        Node {
            line,
            at: data.words.get(number).copied().unwrap_or(data.start),
            x: 0.0,
            y: 0.0,
            degrees: rng.range(-spread, spread),
            size,
            half: (word.ink_width() * size / 2.0, word.ink_height() * size / 2.0),
            word,
            extra: None,
            first: number == 0,
        }
    }

    /// Раскладывает слова строк части по порядку до строки `upto`.
    fn grow(&mut self, fonts: &mut Fonts, ctx: &Ctx, upto: usize) {
        let unit = ctx.unit();
        while self.part.first + self.laid <= upto.min(self.part.last) {
            let line = self.part.first + self.laid;
            let mut rng = Rng::new(motion::mix(self.part.seed, line as u64));
            let words: Vec<String> = ctx.lines[line].text.split_whitespace().map(str::to_string).collect();
            for (number, raw) in words.iter().enumerate() {
                let mut node = self.make_node(fonts, ctx, line, number, raw, &mut rng);
                match self.variant {
                    Variant::Stars => {
                        // Звёзды — по всему небу, но новая не слишком далеко от прошлой.
                        let (px, py) = self.nodes.last().map_or((0.0, 0.0), |last| (last.x, last.y));
                        let sky = unit * 1.4;
                        let mut best = (px, py);
                        for _ in 0..12 {
                            let angle = rng.range(0.0, TAU);
                            let distance = rng.range(0.13, 0.26) * unit;
                            let (x, y) = (px + angle.cos() * distance, py + angle.sin() * distance);
                            let from_center = x.hypot(y);
                            let crowded = self.nodes.iter().rev().take(30).any(|other| (other.x - x).hypot(other.y - y) < unit * 0.1);
                            best = (x, y);
                            if from_center < sky && !crowded {
                                break;
                            }
                        }
                        (node.x, node.y) = best;
                    }
                    _ => {
                        // Следующее слово — на шаг дальше, с поворотом пути.
                        if let Some(last) = self.nodes.last() {
                            let previous = self.nodes.len().checked_sub(2).map(|index| &self.nodes[index]);
                            let heading = previous.map_or(rng.range(0.0, TAU), |before| (last.y - before.y).atan2(last.x - before.x));
                            let mut placed = None;
                            for attempt in 0..10 {
                                let swing = rng.range(0.35, 1.2) * rng.sign() * if attempt > 5 { 2.0 } else { 1.0 };
                                let direction = heading + swing;
                                let reach = last.half.0.max(last.half.1) + node.half.0.max(node.half.1) + unit * rng.range(0.08, 0.2);
                                let (x, y) = (last.x + direction.cos() * reach, last.y + direction.sin() * reach);
                                let clear = self.nodes.iter().rev().take(14).all(|other| {
                                    (other.x - x).hypot(other.y - y) > (other.half.0 + node.half.0) * 0.9 + unit * 0.03
                                });
                                placed = Some((x, y));
                                if clear {
                                    break;
                                }
                            }
                            (node.x, node.y) = placed.unwrap_or((last.x, last.y));
                        }
                    }
                }
                // Поперечная нить к одному из недавних узлов поблизости.
                let index = self.nodes.len();
                if index >= 3 && rng.chance(0.2) {
                    let back = index - 2 - rng.below(4.min(index - 2));
                    let other = &self.nodes[back];
                    if (other.x - node.x).hypot(other.y - node.y) < unit * 0.9 {
                        node.extra = Some(back);
                    }
                }
                self.nodes.push(node);
            }
            self.laid += 1;
        }
    }

    /// Узлы одной строки внутри экрана (вариант «экран»).
    fn screen_nodes(&self, fonts: &mut Fonts, ctx: &Ctx, line: usize) -> Vec<Node> {
        let (width, height) = (ctx.width, ctx.height);
        let unit = ctx.unit();
        let mut rng = Rng::new(motion::mix(self.part.seed, line as u64));
        let words: Vec<String> = ctx.lines[line].text.split_whitespace().map(str::to_string).collect();
        let mut nodes: Vec<Node> = Vec::new();
        let mut shrink = 1.0;
        for _ in 0..4 {
            nodes.clear();
            let mut ok = true;
            for (number, raw) in words.iter().enumerate() {
                let mut node = self.make_node(fonts, ctx, line, number, raw, &mut rng);
                node.size *= shrink;
                node.half = (node.half.0 * shrink, node.half.1 * shrink);
                let reach_of = |node: &Node| node.half.0.max(node.half.1);
                // Повёрнутые слова — по их габаритам на экране, с зазором.
                let fits = |x: f32, y: f32, node: &Node, nodes: &[Node]| {
                    let margin = unit * 0.03;
                    let (hx, hy) = extents(node);
                    x - hx > margin
                        && x + hx < width - margin
                        && y - hy > margin
                        && y + hy < height - margin
                        && nodes.iter().all(|other| {
                            let (ox, oy) = extents(other);
                            (other.x - x).abs() > ox + hx + margin * 0.5 || (other.y - y).abs() > oy + hy + margin * 0.5
                        })
                };
                let mut found = None;
                for _ in 0..40 {
                    let (x, y) = match nodes.last() {
                        None => (width * rng.range(0.2, 0.5), height * rng.range(0.2, 0.8)),
                        Some(last) => {
                            let angle = rng.range(0.0, TAU);
                            let reach = reach_of(last) + reach_of(&node) + unit * rng.range(0.04, 0.16);
                            (last.x + angle.cos() * reach, last.y + angle.sin() * reach * 0.8)
                        }
                    };
                    if fits(x, y, &node, &nodes) {
                        found = Some((x, y));
                        break;
                    }
                }
                let Some((x, y)) = found else {
                    ok = false;
                    break;
                };
                (node.x, node.y) = (x, y);
                if nodes.len() >= 2 && rng.chance(0.4) {
                    node.extra = Some(rng.below(nodes.len() - 1));
                }
                nodes.push(node);
            }
            if ok {
                break;
            }
            shrink *= 0.8;
        }
        nodes
    }
}

/// Куда смотрит камера: среднее слов с гауссовым весом по времени, плюс чуть пути
/// «от слова к слову» — чтобы в паузе не прыгать.
fn focus(nodes: &[Node], t: f32, sigma: f32) -> (f32, f32, f32) {
    let next = nodes.partition_point(|node| node.at + 0.15 <= t);
    let path = match (next.checked_sub(1).map(|index| &nodes[index]), nodes.get(next)) {
        (Some(before), Some(after)) => {
            let share = motion::ease_in_out(progress(t, before.at + 0.15, after.at - before.at));
            (motion::lerp(before.x, after.x, share), motion::lerp(before.y, after.y, share), motion::lerp(before.degrees, after.degrees, share))
        }
        (Some(node), None) | (None, Some(node)) => (node.x, node.y, node.degrees),
        (None, None) => (0.0, 0.0, 0.0),
    };
    let anchor = 0.02;
    let (mut x, mut y, mut turn, mut total) = (path.0 * anchor, path.1 * anchor, path.2 * anchor, anchor);
    for node in nodes {
        let offset = t - (node.at + 0.15);
        if offset.abs() > sigma * 4.5 {
            continue;
        }
        let weight = (-(offset / sigma).powi(2)).exp();
        x += node.x * weight;
        y += node.y * weight;
        turn += node.degrees * weight;
        total += weight;
    }
    (x / total, y / total, turn / total)
}

/// Нить от `a` к `b`, прорисованная на долю `grow`.
fn thread(builder: &mut PathBuilder, a: (f32, f32), b: (f32, f32), grow: f32) {
    if grow <= 0.0 {
        return;
    }
    builder.move_to(a.0, a.1);
    builder.line_to(a.0 + (b.0 - a.0) * grow, a.1 + (b.1 - a.1) * grow);
}

impl Scene for Web {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.nodes.clear();
            self.laid = 0;
            self.screens.clear();
        }
        let part = self.part;
        let t = ctx.t;
        let unit = ctx.unit();
        let (width, height) = (ctx.width, ctx.height);
        let ink = ctx.palette.ink;
        let hair = canvas.hair();
        // Темп движений — общий на часть: длительности не скачут от момента к моменту.
        let pace = tempo(ctx.mood.mean(part.start, part.end));
        let current = ctx.current(&part).unwrap_or(part.first);
        let style_of = |styles: &mut Lazy<(Entry, Exit)>, line: usize| {
            *styles.get_or(line, || {
                let mut rng = Rng::new(motion::mix(part.seed ^ 0x57E, line as u64));
                let energy = ctx.mood.mean(ctx.lines[line].start, ctx.lines[line].end);
                (Entry::pick(&mut rng, energy), Exit::pick(&mut rng, energy))
            })
        };

        if self.variant == Variant::Screen {
            let indices: Vec<usize> = ctx.recent(&part, 0.8).collect();
            self.screens.keep(|line| indices.contains(&line) || line + 1 >= current && line <= current + 1);
            self.styles.keep(|line| line + 3 >= current && line <= current + 2);
            let sway = Camera { x: width / 2.0, y: height / 2.0, zoom: 1.0, turn: (1.2 * motion::wander(t, part.seed, 0.03)).to_radians() };
            let view = sway.view(width, height);
            super::fx::dust(canvas, ctx, part.seed, 22, (0.0, 0.0), 0.35 * visible);
            for line in indices {
                let (entry, exit) = style_of(&mut self.styles, line);
                if self.screens.get(line).is_none() {
                    let nodes = self.screen_nodes(fonts, ctx, line);
                    self.screens.get_or(line, || nodes);
                }
                let nodes = self.screens.get(line).expect("разложено выше");
                let data = &ctx.lines[line];
                let leave_at = data.end - 0.1 * pace;
                let unravel = progress(t, leave_at, 0.45 * pace);
                // Слова медленно парят (не в такт); нити тянутся за ними.
                let float = |index: usize, node: &Node| {
                    let seed = part.seed ^ (line * 64 + index) as u64;
                    (node.x + unit * 0.006 * motion::wander(t, seed, 0.04), node.y + unit * 0.006 * motion::wander(t, seed ^ 0x5A, 0.04))
                };
                let mut threads = PathBuilder::new();
                let mut knots = PathBuilder::new();
                for (index, node) in nodes.iter().enumerate() {
                    let here = float(index, node);
                    if index > 0 {
                        let before = &nodes[index - 1];
                        let there = float(index - 1, before);
                        let lead = (0.35 * pace).min((node.at - before.at).max(0.05) * 0.9);
                        let grow = ease_out(progress(t, node.at - lead, lead)) * (1.0 - unravel);
                        let a = edge(before, there, here, unit * 0.012);
                        let b = edge(node, here, there, unit * 0.012);
                        thread(&mut threads, a, b, grow);
                        if grow > 0.0 {
                            knots.push_circle(a.0, a.1, unit * 0.004);
                        }
                        if grow >= 1.0 {
                            knots.push_circle(b.0, b.1, unit * 0.004);
                        }
                    }
                    if let Some(extra) = node.extra {
                        let other = &nodes[extra];
                        let grow = ease_out(progress(t, node.at, 0.5 * pace)) * (1.0 - unravel);
                        thread(&mut threads, float(extra, other), here, grow);
                    }
                }
                if let Some(path) = threads.finish() {
                    canvas.stroke(&path, faded(ink, 0.55 * visible), hair, view, None);
                }
                if let Some(path) = knots.finish() {
                    canvas.fill(&path, faded(ink, 0.7 * visible), view, None);
                }
                for (index, node) in nodes.iter().enumerate() {
                    let here = float(index, node);
                    // Строка видна сразу; спетые слова разгораются, нити тянутся к ним.
                    let mut look = motion::combine(super::line_look(t, data.start, node.at, entry.length(pace)), exit.look(progress(t, leave_at, exit.length(pace))));
                    look.alpha *= visible;
                    let (ix, iy) = ink_center(&node.word);
                    let base = Transform::from_translate(-ix, -iy).post_scale(node.size, node.size).post_rotate(node.degrees).post_translate(here.0, here.1).post_concat(view);
                    draw_word(canvas, &node.word, base, apply(view, here.0, here.1), node.size, &look, ink, None);
                }
            }
            return;
        }

        // Камера смотрит вперёд, поэтому раскладка с запасом — новая строка не дёргает.
        let ahead = (current..=part.last).take_while(|line| ctx.lines[*line].start <= t + 10.0).last().unwrap_or(current);
        self.grow(fonts, ctx, ahead.max(current + 1));
        self.styles.keep(|line| line + 4 >= current && line <= current + 2);
        if self.nodes.is_empty() {
            return;
        }
        let (fx, fy, fturn) = focus(&self.nodes, t, if self.variant == Variant::Stars { 1.3 } else { 1.1 });
        let calm = 1.0 - ctx.mood.mean(part.start, part.end);
        let camera = match self.variant {
            Variant::Stars => {
                // Оборот неба — за 7–9 минут, плюс медленный зум.
                let spin = t * (0.012 + 0.003 * calm) * if part.seed & 1 == 0 { 1.0 } else { -1.0 };
                Camera { x: fx, y: fy, zoom: 1.0 + 0.06 * motion::wander(t, part.seed, 0.01), turn: spin }
            }
            _ => Camera {
                x: fx + unit * 0.008 * motion::wander(t, part.seed ^ 0xCA, 0.03),
                y: fy + unit * 0.008 * motion::wander(t, part.seed ^ 0xCB, 0.03),
                zoom: 0.88 - 0.06 * calm,
                turn: (-0.2 * fturn + 1.0 * motion::wander(t, part.seed ^ 0xCC, 0.015)).to_radians(),
            },
        };
        let view = camera.view(width, height);
        let reach = width.hypot(height) / camera.zoom * 0.75;
        // Пыль сдвигается с камерой (ближняя — сильнее): видно, что летим.
        if self.variant == Variant::Camera {
            super::fx::dust(canvas, ctx, part.seed, 26, (camera.x * camera.zoom, camera.y * camera.zoom), 0.4 * visible);
        }

        if self.variant == Variant::Stars {
            // Фоновые звёзды крутятся медленнее и мерцают сами по себе.
            let mut rng = Rng::new(part.seed ^ 0x57A2);
            let far = Camera { x: camera.x * 0.3, y: camera.y * 0.3, zoom: 1.0, turn: camera.turn * 0.5 }.view(width, height);
            let mut stars = Vec::new();
            for star in 0..28 {
                let (x, y) = (rng.range(-1.2, 1.2) * width, rng.range(-1.2, 1.2) * height);
                let twinkle = 0.75 + 0.25 * (t * rng.range(0.15, 0.35) + star as f32 * 1.7).sin();
                let (sx, sy) = apply(far, x, y);
                if (0.0..width).contains(&sx) && (0.0..height).contains(&sy) {
                    stars.push((sx, sy, unit * rng.range(0.0012, 0.0028) * twinkle));
                }
            }
            canvas.dots(&stars, faded(ink, 0.3 * visible), Transform::identity());
        }

        let visible_node = |node: &Node| (node.x - camera.x).hypot(node.y - camera.y) < reach + node.half.0;
        let mut threads = PathBuilder::new();
        let mut old_threads = PathBuilder::new();
        let mut faded_threads = PathBuilder::new();
        let mut knots = PathBuilder::new();
        for index in 0..self.nodes.len() {
            let node = &self.nodes[index];
            if node.at - 1.0 > t {
                break;
            }
            let fresh = node.line + 1 >= current;
            // Старые нити созвездия гаснут, иначе небо исчерчено.
            if self.variant == Variant::Stars && node.line + 3 < current {
                continue;
            }
            let fading = self.variant == Variant::Stars && node.line + 3 == current;
            if index > 0 && !(self.variant == Variant::Stars && node.first && node.line == part.first) {
                let before = &self.nodes[index - 1];
                if visible_node(node) || visible_node(before) {
                    let lead = (0.35 * pace).min((node.at - before.at).max(0.05) * 0.9);
                    let grow = ease_out(progress(t, node.at - lead, lead));
                    let (a, b) = match self.variant {
                        Variant::Stars => ((before.x, before.y), (node.x, node.y)),
                        _ => (
                            edge(before, (before.x, before.y), (node.x, node.y), unit * 0.012),
                            edge(node, (node.x, node.y), (before.x, before.y), unit * 0.012),
                        ),
                    };
                    thread(if fresh { &mut threads } else if fading { &mut faded_threads } else { &mut old_threads }, a, b, grow);
                    if grow > 0.0 && self.variant != Variant::Stars {
                        knots.push_circle(a.0, a.1, unit * 0.004);
                    }
                }
            }
            if let Some(extra) = node.extra
                && (fresh || self.variant != Variant::Stars)
            {
                let other = &self.nodes[extra];
                if visible_node(node) {
                    let grow = ease_out(progress(t, node.at, 0.6 * pace));
                    thread(&mut old_threads, (other.x, other.y), (node.x, node.y), grow);
                }
            }
        }
        if let Some(path) = faded_threads.finish() {
            let fade = 1.0 - progress(t, ctx.lines[current].start, 1.5);
            canvas.stroke(&path, faded(ink, 0.16 * fade * visible), hair, view, None);
        }
        if let Some(path) = old_threads.finish() {
            let faint = if self.variant == Variant::Stars { 0.16 } else { 0.28 };
            canvas.stroke(&path, faded(ink, faint * visible), hair, view, None);
        }
        if let Some(path) = threads.finish() {
            canvas.stroke(&path, faded(ink, 0.6 * visible), hair, view, None);
        }
        if let Some(path) = knots.finish() {
            canvas.fill(&path, faded(ink, 0.6 * visible), view, None);
        }
        // Звёзды-точки и подписи / слова паутины.
        let mut dots = PathBuilder::new();
        for index in 0..self.nodes.len() {
            let node = &self.nodes[index];
            if node.at > t + 0.05 {
                break;
            }
            if !visible_node(node) {
                continue;
            }
            let (entry, _) = style_of(&mut self.styles, node.line);
            let p = progress(t, node.at, entry.length(pace));
            let age = t - node.at;
            // Старые слова тускнеют, у созвездия подписи гаснут совсем.
            let dim = if node.line >= current {
                1.0
            } else if self.variant == Variant::Stars {
                let next = ctx.lines.get(node.line + 1).map_or(t, |line| line.start);
                0.4 * (1.0 - progress(t, next + 1.0, 2.0))
            } else {
                0.5
            };
            let fade = motion::clamp01(1.0 - (age - 14.0) / 6.0);
            let mut look = if p < 1.0 { entry.look(p) } else { Look::STILL };
            look.alpha *= dim * fade * visible;
            let (ix, iy) = ink_center(&node.word);
            if self.variant == Variant::Stars {
                // Звезда крутится с небом, подпись всегда стоит прямо.
                let star = node.size * 0.11 * (1.0 + 0.5 * (1.0 - ease_out(progress(age, 0.0, 0.8))) * f32::from(age >= 0.0));
                dots.push_circle(node.x, node.y, star.max(unit * 0.004));
                let (sx, sy) = apply(view, node.x, node.y);
                let size = node.size * camera.zoom;
                let (x, y) = (sx + node.half.0 * camera.zoom + unit * 0.02, sy - node.half.1 * camera.zoom - unit * 0.01);
                let base = Transform::from_translate(-ix, -iy).post_scale(size, size).post_rotate(node.degrees * 0.5).post_translate(x, y);
                draw_word(canvas, &node.word, base, (x, y), size, &look, ink, None);
            } else {
                // Звучащее слово — в мягком свечении, оно дышит громкостью.
                if age >= 0.0 && node.line >= current && self.nodes.get(index + 1).is_none_or(|next| next.at > t) {
                    let level = ctx.level();
                    canvas.glow(apply(view, node.x, node.y), node.half.0 * camera.zoom * (1.3 + 0.4 * level), ink, (0.05 + 0.07 * level) * visible);
                }
                let base = Transform::from_translate(-ix, -iy).post_scale(node.size, node.size).post_rotate(node.degrees).post_translate(node.x, node.y).post_concat(view);
                draw_word(canvas, &node.word, base, apply(view, node.x, node.y), node.size * camera.zoom, &look, ink, None);
            }
            // Звезда, которая звучит сейчас, светится кольцом.
            if self.variant == Variant::Stars && age < 1.2 {
                let glow = progress(age, 0.0, 1.2);
                if let Some(ring) = paint::circle(node.x, node.y, unit * (0.01 + 0.05 * ease_out(glow))) {
                    canvas.stroke(&ring, faded(ink, 0.6 * (1.0 - glow) * visible), hair, view, None);
                }
            }
        }
        if let Some(path) = dots.finish() {
            canvas.fill(&path, faded(ink, 0.9 * visible), view, None);
        }
        // Далёкие слова уже не рисуем.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::text::Fonts;

    /// Камера не прыгает ни в паузе, ни на новой строке.
    #[test]
    fn camera_focus_moves_without_jumps() {
        let mut fonts = Fonts::new();
        let node = |at: f32, x: f32, y: f32, fonts: &mut Fonts| Node {
            line: 0,
            at,
            x,
            y,
            degrees: x * 0.05,
            size: 10.0,
            half: (10.0, 5.0),
            word: fonts.word("слово", crate::stage::text::Face::Serif),
            extra: None,
            first: false,
        };
        // Слова строки, пауза в 20 секунд, слова следующей строки далеко.
        let mut nodes: Vec<Node> = (0..6).map(|index| node(index as f32 * 0.5, index as f32 * 60.0, 0.0, &mut fonts)).collect();
        nodes.extend((0..6).map(|index| node(23.0 + index as f32 * 0.5, 1000.0 + index as f32 * 60.0, 400.0, &mut fonts)));
        let mut previous = focus(&nodes, -1.0, 2.0);
        let mut t = -1.0;
        while t < 30.0 {
            t += 1.0 / 120.0;
            let now = focus(&nodes, t, 2.0);
            let step = (now.0 - previous.0).hypot(now.1 - previous.1);
            // Самый быстрый перелёт (1000 точек за паузу) — не больше 3 точек за кадр.
            assert!(step < 3.0, "{t}: {previous:?} → {now:?}");
            previous = now;
        }
    }
}
