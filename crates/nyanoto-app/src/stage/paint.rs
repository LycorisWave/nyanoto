//! Рисование кадра tiny-skia. Координаты — логические точки, углы — радианы по часовой (y вниз).

use std::f32::consts::{FRAC_PI_2, TAU};

use tiny_skia::{
    Color, FillRule, FilterQuality, LineCap, Mask, Paint, Path, PathBuilder, Pattern, Pixmap, PixmapMut,
    SpreadMode, Stroke, StrokeDash, Transform,
};

use super::text::Word;

/// Цвета сцены: буквы и линии, фон (им пишутся буквы на заливке) и акцент.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub ink: Color,
    pub ground: Color,
    pub accent: Color,
}

pub fn faded(color: Color, alpha: f32) -> Color {
    let mut color = color;
    color.set_alpha((color.alpha() * alpha).clamp(0.0, 1.0));
    color
}

pub struct Canvas<'a> {
    pub pixmap: PixmapMut<'a>,
    /// Логические точки → точки экрана.
    pub root: Transform,
    /// Запасная маска (буквы на заливке): живёт между кадрами.
    pub spare: Option<Mask>,
    /// На холсте ещё ничего нет (неподвижный слой можно просто скопировать).
    pub blank: bool,
}

impl Canvas<'_> {
    /// Линия в точку экрана — у tiny-skia в разы быстрее толстых.
    pub fn hair(&self) -> f32 {
        1.0 / self.root.sx.max(0.01)
    }

    fn paint(color: Color) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        paint
    }

    pub fn fill(&mut self, path: &Path, color: Color, transform: Transform, mask: Option<&Mask>) {
        if color.alpha() <= 0.003 {
            return;
        }
        self.blank = false;
        let paint = Self::paint(color);
        self.pixmap.fill_path(path, &paint, FillRule::Winding, transform.post_concat(self.root), mask);
    }

    pub fn stroke(&mut self, path: &Path, color: Color, width: f32, transform: Transform, dash: Option<(f32, f32)>) {
        if color.alpha() <= 0.003 {
            return;
        }
        self.blank = false;
        let device = transform.post_concat(self.root);
        // Тонкие — своим способом: у tiny-skia ~0,5 мкс на точку длины.
        if width * self.root.sx <= 1.01 {
            let dashed;
            let path = match dash.and_then(|(on, off)| StrokeDash::new(vec![on, off], 0.0)) {
                Some(dash) => match path.dash(&dash, 1.0) {
                    Some(path) => {
                        dashed = path;
                        &dashed
                    }
                    None => return,
                },
                None => path,
            };
            hairlines(&mut self.pixmap, path, device, color);
            return;
        }
        let paint = Self::paint(color);
        let stroke = Stroke {
            width,
            line_cap: LineCap::Butt,
            dash: dash.and_then(|(on, off)| StrokeDash::new(vec![on, off], 0.0)),
            ..Stroke::default()
        };
        self.pixmap.stroke_path(path, &paint, &stroke, device, None);
    }

    /// Свечение прямо в буфер, (1 − d²/R²)²: градиент tiny-skia стоил ~11 мс, так ~1 мс.
    pub fn glow(&mut self, center: (f32, f32), radius: f32, color: Color, alpha: f32) {
        if alpha <= 0.003 || radius <= 0.5 {
            return;
        }
        self.blank = false;
        let device = self.root;
        let (cx, cy) = (device.sx * center.0 + device.tx, device.sy * center.1 + device.ty);
        let r = radius * device.sx;
        let (width, height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let premultiplied = faded(color, alpha).premultiply();
        let ink = [premultiplied.red(), premultiplied.green(), premultiplied.blue(), premultiplied.alpha()].map(|channel| channel * 255.0);
        let opacity = premultiplied.alpha();
        let inverse = 1.0 / (r * r);
        let data = self.pixmap.data_mut();
        for py in ((cy - r).floor() as i32).max(0)..((cy + r).ceil() as i32).min(height) {
            let dy = py as f32 + 0.5 - cy;
            let reach = (r * r - dy * dy).max(0.0).sqrt();
            let row = (py * width) as usize;
            for px in ((cx - reach).floor() as i32).max(0)..((cx + reach).ceil() as i32).min(width) {
                let dx = px as f32 + 0.5 - cx;
                let near = 1.0 - (dx * dx + dy * dy) * inverse;
                if near <= 0.0 {
                    continue;
                }
                let cover = near * near;
                let index = (row + px as usize) * 4;
                let pixel = &mut data[index..index + 4];
                let keep = 1.0 - opacity * cover;
                for channel in 0..4 {
                    pixel[channel] = (ink[channel] * cover + f32::from(pixel[channel]) * keep + 0.5) as u8;
                }
            }
        }
    }

    /// Облачка дыма прямо в буфер — с градиентом на каждое было бы в разы дороже.
    pub fn smoke(&mut self, puffs: &[(f32, f32, f32, f32)], color: Color) {
        if puffs.is_empty() {
            return;
        }
        self.blank = false;
        let device = self.root;
        let scale = device.sx;
        let (width, height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let premultiplied = color.premultiply();
        let ink = [premultiplied.red(), premultiplied.green(), premultiplied.blue(), premultiplied.alpha()];
        let data = self.pixmap.data_mut();
        for &(x, y, r, alpha) in puffs {
            let alpha = alpha.clamp(0.0, 1.0);
            if alpha < 0.004 {
                continue;
            }
            let (cx, cy) = (device.sx * x + device.tx, device.sy * y + device.ty);
            let r = (r * scale).max(0.6);
            let (left, right) = ((cx - r).floor() as i32, (cx + r).ceil() as i32);
            let (top, bottom) = ((cy - r).floor() as i32, (cy + r).ceil() as i32);
            if right < 0 || bottom < 0 || left >= width || top >= height {
                continue;
            }
            let inverse = 1.0 / (r * r);
            for py in top.max(0)..=bottom.min(height - 1) {
                let dy = py as f32 + 0.5 - cy;
                let row = (py * width) as usize;
                for px in left.max(0)..=right.min(width - 1) {
                    let dx = px as f32 + 0.5 - cx;
                    let near = 1.0 - (dx * dx + dy * dy) * inverse;
                    if near <= 0.0 {
                        continue;
                    }
                    let cover = near * near * alpha;
                    let index = (row + px as usize) * 4;
                    let pixel = &mut data[index..index + 4];
                    let keep = 1.0 - ink[3] * cover;
                    for channel in 0..4 {
                        pixel[channel] = (ink[channel] * cover * 255.0 + f32::from(pixel[channel]) * keep + 0.5) as u8;
                    }
                }
            }
        }
    }

    /// Мелкие точки прямо в буфер — tiny-skia на крохи тратит много.
    pub fn dots(&mut self, dots: &[(f32, f32, f32)], color: Color, transform: Transform) {
        if color.alpha() <= 0.003 || dots.is_empty() {
            return;
        }
        self.blank = false;
        let device = transform.post_concat(self.root);
        let scale = (device.sx * device.sx + device.ky * device.ky).sqrt();
        let (width, height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let premultiplied = color.premultiply();
        let ink = [premultiplied.red(), premultiplied.green(), premultiplied.blue(), premultiplied.alpha()];
        let data = self.pixmap.data_mut();
        for &(x, y, r) in dots {
            let (cx, cy) = (device.sx * x + device.kx * y + device.tx, device.ky * x + device.sy * y + device.ty);
            let r = (r * scale).max(0.35);
            let (left, right) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
            let (top, bottom) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
            if right < 0 || bottom < 0 || left >= width || top >= height {
                continue;
            }
            // Совсем мелкая точка — бледнее, а не меньше пикселя.
            let faint = (r * 2.0).min(1.0);
            for py in top.max(0)..=bottom.min(height - 1) {
                for px in left.max(0)..=right.min(width - 1) {
                    let distance = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
                    let cover = (r.max(0.5) + 0.5 - distance).clamp(0.0, 1.0) * faint;
                    if cover <= 0.0 {
                        continue;
                    }
                    let index = ((py * width + px) * 4) as usize;
                    let pixel = &mut data[index..index + 4];
                    let keep = 1.0 - ink[3] * cover;
                    for channel in 0..4 {
                        pixel[channel] = (ink[channel] * cover * 255.0 + f32::from(pixel[channel]) * keep).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }

    /// Стирает под фигурой на долю `strength` — сквозь видно фон окна.
    pub fn erase(&mut self, path: &Path, strength: f32) {
        if strength <= 0.003 {
            return;
        }
        let mut paint = Self::paint(Color::from_rgba(0.0, 0.0, 0.0, strength.clamp(0.0, 1.0)).unwrap_or(Color::BLACK));
        paint.blend_mode = tiny_skia::BlendMode::DestinationOut;
        self.pixmap.fill_path(path, &paint, FillRule::Winding, self.root, None);
    }

    /// Тысячи светодиодов за кадр: край без корня, по строке — только внутри круга.
    pub fn lights(&mut self, lights: &[(f32, f32, f32, f32)], color: Color) {
        if color.alpha() <= 0.003 || lights.is_empty() {
            return;
        }
        self.blank = false;
        let device = self.root;
        let (width, height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let premultiplied = color.premultiply();
        let ink = [premultiplied.red(), premultiplied.green(), premultiplied.blue(), premultiplied.alpha()].map(|channel| channel * 255.0);
        let opacity = premultiplied.alpha();
        let data = self.pixmap.data_mut();
        for &(x, y, r, alpha) in lights {
            let alpha = alpha.clamp(0.0, 1.0);
            if alpha <= 0.004 {
                continue;
            }
            let (cx, cy) = (device.sx * x + device.tx, device.sy * y + device.ty);
            let r = (r * device.sx).max(0.45);
            let outer = r + 0.5;
            let (top, bottom) = (((cy - outer).floor() as i32).max(0), ((cy + outer).ceil() as i32).min(height - 1));
            if cx + outer < 0.0 || cx - outer >= width as f32 || top > bottom {
                continue;
            }
            let faint = (r * 2.0).min(1.0) * alpha;
            let (square, half) = (r * r, 0.5 / r);
            for py in top..=bottom {
                let dy = py as f32 + 0.5 - cy;
                let left_over = outer * outer - dy * dy;
                if left_over <= 0.0 {
                    continue;
                }
                let reach = left_over.sqrt();
                let (left, right) = (((cx - reach).floor() as i32).max(0), ((cx + reach).ceil() as i32).min(width - 1));
                let row = (py * width) as usize;
                for px in left..=right {
                    let dx = px as f32 + 0.5 - cx;
                    let cover = ((square - dx * dx - dy * dy) * half + 0.5).clamp(0.0, 1.0) * faint;
                    if cover <= 0.002 {
                        continue;
                    }
                    let index = (row + px as usize) * 4;
                    let pixel = &mut data[index..index + 4];
                    let keep = 1.0 - opacity * cover;
                    for channel in 0..4 {
                        pixel[channel] = (ink[channel] * cover + f32::from(pixel[channel]) * keep + 0.5) as u8;
                    }
                }
            }
        }
    }

    /// Слово кеглем 1, поставленное преобразованием `transform`.
    pub fn word(&mut self, word: &Word, color: Color, transform: Transform, mask: Option<&Mask>) {
        if let Some(path) = &word.path {
            self.fill(path, color, transform, mask);
        }
    }

    /// Картинка в точке экрана, видна доля `share` ширины.
    pub fn blit(&mut self, picture: &Pixmap, x: i32, y: i32, share: f32, alpha: f32) {
        if alpha <= 0.003 || share <= 0.0 {
            return;
        }
        let width = picture.width() as f32 * share.min(1.0);
        let Some(rect) = tiny_skia::Rect::from_xywh(x as f32, y as f32, width.max(1.0), picture.height() as f32) else {
            return;
        };
        let pattern = Pattern::new(
            picture.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            alpha.clamp(0.0, 1.0),
            Transform::from_translate(x as f32, y as f32),
        );
        let paint = Paint { shader: pattern, anti_alias: false, ..Paint::default() };
        self.blank = false;
        self.pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }

    /// Непрозрачность сетки `cover` сглаженно, цветом `color`, только клетки `bounds`.
    /// Пустые клетки пропускаем — цена от площади туши, а не экрана.
    pub fn cover_picture(&mut self, cover: &[f32], width: usize, cell: f32, bounds: (usize, usize, usize, usize), color: Color) {
        if color.alpha() <= 0.003 || width == 0 {
            return;
        }
        let height = cover.len() / width;
        let (left, top, right, bottom) = bounds;
        let scale = cell * self.root.sx;
        let (device_width, device_height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        self.blank = false;
        let premultiplied = color.premultiply();
        let ink = [premultiplied.red(), premultiplied.green(), premultiplied.blue(), premultiplied.alpha()].map(|channel| channel * 255.0);
        let opacity = premultiplied.alpha();
        let stride = device_width as usize;
        let data = self.pixmap.data_mut();
        // Точки экрана, чья середина в клетке `index`.
        let span = |index: usize, limit: i32| {
            let from = (((index as f32 + 0.5) * scale - 0.5).ceil() as i32).clamp(0, limit);
            let to = (((index as f32 + 1.5) * scale - 0.5).ceil() as i32).clamp(0, limit);
            (from, to)
        };
        for j in top.saturating_sub(1)..bottom.min(height.saturating_sub(2)) + 1 {
            let (row, next) = (j * width, (j + 1).min(height - 1) * width);
            let (y_from, y_to) = span(j, device_height);
            for i in left.saturating_sub(1)..right.min(width.saturating_sub(2)) + 1 {
                let i1 = (i + 1).min(width - 1);
                let corners = [cover[row + i], cover[row + i1], cover[next + i], cover[next + i1]];
                if corners.iter().all(|value| *value <= 0.002) {
                    continue;
                }
                let (x_from, x_to) = span(i, device_width);
                for y in y_from..y_to {
                    let fy = (y as f32 + 0.5) / scale - 0.5 - j as f32;
                    let upper = (corners[0], corners[1] - corners[0]);
                    let lower = (corners[2], corners[3] - corners[2]);
                    let line = y as usize * stride;
                    for x in x_from..x_to {
                        let fx = (x as f32 + 0.5) / scale - 0.5 - i as f32;
                        let top_value = upper.0 + upper.1 * fx;
                        let alpha = top_value + (lower.0 + lower.1 * fx - top_value) * fy;
                        if alpha <= 0.002 {
                            continue;
                        }
                        let index = (line + x as usize) * 4;
                        let pixel = &mut data[index..index + 4];
                        let keep = 1.0 - opacity * alpha;
                        for channel in 0..4 {
                            pixel[channel] = (ink[channel] * alpha + f32::from(pixel[channel]) * keep + 0.5) as u8;
                        }
                    }
                }
            }
        }
    }

    /// Маска фигур `paths` из запасной; вернуть — [`Canvas::give_back`].
    pub fn cover(&mut self, paths: &[(&Path, Transform)]) -> Option<Mask> {
        let (width, height) = (self.pixmap.width(), self.pixmap.height());
        let mut mask = match self.spare.take() {
            Some(mask) if mask.width() == width && mask.height() == height => mask,
            _ => Mask::new(width, height)?,
        };
        mask.clear();
        for (path, transform) in paths {
            mask.fill_path(path, FillRule::Winding, true, transform.post_concat(self.root));
        }
        Some(mask)
    }

    pub fn give_back(&mut self, mask: Mask) {
        self.spare = Some(mask);
    }
}

/// Контур линиями Ву в одну точку, прямо в буфер.
fn hairlines(pixmap: &mut PixmapMut, path: &Path, device: Transform, color: Color) {
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let premultiplied = color.premultiply();
    let ink = [premultiplied.red(), premultiplied.green(), premultiplied.blue(), premultiplied.alpha()];
    let data = pixmap.data_mut();
    let map = |point: tiny_skia::Point| {
        let (x, y) = (point.x, point.y);
        (device.sx * x + device.kx * y + device.tx, device.ky * x + device.sy * y + device.ty)
    };
    let mut plot = |x: i32, y: i32, cover: f32| {
        if x < 0 || y < 0 || x >= width || y >= height || cover <= 0.0 {
            return;
        }
        let index = ((y * width + x) * 4) as usize;
        let pixel = &mut data[index..index + 4];
        let alpha = ink[3] * cover;
        let keep = 1.0 - alpha;
        for channel in 0..4 {
            let value = ink[channel] * cover * 255.0 + f32::from(pixel[channel]) * keep;
            pixel[channel] = value.round().clamp(0.0, 255.0) as u8;
        }
    };
    let mut line = |from: (f32, f32), to: (f32, f32)| wu_line(from, to, &mut plot);
    let mut start = (0.0, 0.0);
    let mut last = (0.0, 0.0);
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(point) => {
                last = map(point);
                start = last;
            }
            tiny_skia::PathSegment::LineTo(point) => {
                let next = map(point);
                line(last, next);
                last = next;
            }
            tiny_skia::PathSegment::QuadTo(control, point) => {
                let (c, end) = (map(control), map(point));
                let steps = (((c.0 - last.0).hypot(c.1 - last.1) + (end.0 - c.0).hypot(end.1 - c.1)) / 6.0).clamp(2.0, 64.0) as usize;
                let begin = last;
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    let u = 1.0 - t;
                    let next = (u * u * begin.0 + 2.0 * u * t * c.0 + t * t * end.0, u * u * begin.1 + 2.0 * u * t * c.1 + t * t * end.1);
                    line(last, next);
                    last = next;
                }
            }
            tiny_skia::PathSegment::CubicTo(c1, c2, point) => {
                let (a, b, end) = (map(c1), map(c2), map(point));
                let length = (a.0 - last.0).hypot(a.1 - last.1) + (b.0 - a.0).hypot(b.1 - a.1) + (end.0 - b.0).hypot(end.1 - b.1);
                let steps = (length / 6.0).clamp(2.0, 96.0) as usize;
                let begin = last;
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    let u = 1.0 - t;
                    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    let next = (
                        w0 * begin.0 + w1 * a.0 + w2 * b.0 + w3 * end.0,
                        w0 * begin.1 + w1 * a.1 + w2 * b.1 + w3 * end.1,
                    );
                    line(last, next);
                    last = next;
                }
            }
            tiny_skia::PathSegment::Close => {
                line(last, start);
                last = start;
            }
        }
    }
}

/// Сглаженная линия Ву: по две точки на шаг, яркость — по близости к линии.
fn wu_line(from: (f32, f32), to: (f32, f32), plot: &mut impl FnMut(i32, i32, f32)) {
    let (mut x0, mut y0, mut x1, mut y1) = (from.0, from.1, to.0, to.1);
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return;
    }
    // Слишком длинные (за краем на километры) не рисуем.
    if (x1 - x0).abs() + (y1 - y0).abs() > 20000.0 {
        return;
    }
    let steep = (y1 - y0).abs() > (x1 - x0).abs();
    if steep {
        std::mem::swap(&mut x0, &mut y0);
        std::mem::swap(&mut x1, &mut y1);
    }
    if x0 > x1 {
        std::mem::swap(&mut x0, &mut x1);
        std::mem::swap(&mut y0, &mut y1);
    }
    let (dx, dy) = (x1 - x0, y1 - y0);
    let gradient = if dx < 1e-4 { 1.0 } else { dy / dx };
    let mut put = |x: i32, y: i32, cover: f32| if steep { plot(y, x, cover) } else { plot(x, y, cover) };
    // Короче точки — одна точка.
    if dx < 1.0 {
        let y = (y0 + y1) / 2.0;
        put(x0.round() as i32, y.floor() as i32, (1.0 - y.fract()) * dx.max(0.3));
        put(x0.round() as i32, y.floor() as i32 + 1, y.fract() * dx.max(0.3));
        return;
    }
    let first = x0.round();
    let mut y = y0 + gradient * (first - x0);
    let last = x1.round();
    let mut x = first as i32;
    let end = last as i32;
    // Концы — с поправкой на длину перекрытия.
    let start_gap = 1.0 - (x0 + 0.5 - first).fract().abs();
    let end_gap = (x1 + 0.5 - last).fract().abs();
    while x <= end {
        let edge = if x == first as i32 {
            start_gap
        } else if x == end {
            end_gap
        } else {
            1.0
        };
        let floor = y.floor();
        let share = y - floor;
        put(x, floor as i32, (1.0 - share) * edge);
        put(x, floor as i32 + 1, share * edge);
        y += gradient;
        x += 1;
    }
}

/// Дуга от `from` до `to`: по часовой, если `to` > `from`. `start` — новый контур.
pub fn arc(builder: &mut PathBuilder, cx: f32, cy: f32, r: f32, from: f32, to: f32, start: bool) {
    let sweep = (to - from).clamp(-TAU, TAU);
    let parts = (sweep.abs() / FRAC_PI_2).ceil().max(1.0) as usize;
    let step = sweep / parts as f32;
    let point = |angle: f32| (cx + r * angle.cos(), cy + r * angle.sin());
    let (x, y) = point(from);
    if start {
        builder.move_to(x, y);
    } else {
        builder.line_to(x, y);
    }
    // Четверть окружности и меньше — одной кубической кривой.
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    for part in 0..parts {
        let a0 = from + step * part as f32;
        let a1 = a0 + step;
        let (x0, y0) = point(a0);
        let (x1, y1) = point(a1);
        builder.cubic_to(
            x0 - k * r * a0.sin(),
            y0 + k * r * a0.cos(),
            x1 + k * r * a1.sin(),
            y1 - k * r * a1.cos(),
            x1,
            y1,
        );
    }
}

pub fn arc_path(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Option<Path> {
    if to - from <= 0.0001 || r <= 0.0 {
        return None;
    }
    let mut builder = PathBuilder::new();
    arc(&mut builder, cx, cy, r, from, to, true);
    builder.finish()
}

pub fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, r.max(0.01))
}

pub fn segment(x0: f32, y0: f32, x1: f32, y1: f32) -> Option<Path> {
    let mut builder = PathBuilder::new();
    builder.move_to(x0, y0);
    builder.line_to(x1, y1);
    builder.finish()
}

pub fn polygon(points: &[(f32, f32)]) -> Option<Path> {
    let mut builder = PathBuilder::new();
    let (first, rest) = points.split_first()?;
    builder.move_to(first.0, first.1);
    for (x, y) in rest {
        builder.line_to(*x, *y);
    }
    builder.close();
    builder.finish()
}

/// Полоса `length` × `thickness` с центром в нуле, видна доля `from..to`.
pub fn band(length: f32, thickness: f32, from: f32, to: f32) -> Option<Path> {
    let (x0, x1) = (-length / 2.0 + length * from.clamp(0.0, 1.0), -length / 2.0 + length * to.clamp(0.0, 1.0));
    if x1 - x0 < 0.5 {
        return None;
    }
    polygon(&[(x0, -thickness / 2.0), (x1, -thickness / 2.0), (x1, thickness / 2.0), (x0, thickness / 2.0)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test --release -p nyanoto-app paint_costs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn paint_costs() {
        let (width, height) = (720u32, 460u32);
        let mut bytes = vec![0u8; (width * height * 4) as usize];
        let ink = Color::from_rgba8(235, 235, 235, 255);
        let mut fonts = crate::stage::text::Fonts::new();
        let giant = fonts.word("СВОБОДА", crate::stage::text::Face::Heavy);
        let small: Vec<_> = ["МЫ", "БУДЕМ", "ПЕТЬ", "И", "СМЕЯТЬСЯ", "КАК", "ДЕТИ"].iter().map(|word| fonts.word(word, crate::stage::text::Face::Heavy)).collect();
        let mut measure = |name: &str, draw: &mut dyn FnMut(&mut Canvas)| {
            let started = std::time::Instant::now();
            for _ in 0..200 {
                let pixmap = PixmapMut::from_bytes(&mut bytes, width, height).unwrap();
                let mut canvas = Canvas { pixmap, root: Transform::identity(), spare: None, blank: true };
                draw(&mut canvas);
            }
            println!("{name}: {:.3} мс", started.elapsed().as_secs_f64() * 1000.0 / 200.0);
        };
        measure("очистка", &mut |canvas| canvas.pixmap.data_mut().fill(0));
        measure("треугольник на полэкрана, прозрачный", &mut |canvas| {
            let path = polygon(&[(0.0, 0.0), (720.0, 0.0), (0.0, 460.0)]).unwrap();
            canvas.fill(&path, faded(ink, 0.05), Transform::identity(), None);
        });
        measure("круг r=140 прозрачный", &mut |canvas| {
            canvas.fill(&circle(360.0, 230.0, 140.0).unwrap(), faded(ink, 0.06), Transform::identity(), None);
        });
        measure("тонкая окружность r=190", &mut |canvas| {
            canvas.stroke(&circle(360.0, 230.0, 190.0).unwrap(), faded(ink, 0.5), 1.3, Transform::identity(), None);
        });
        measure("огромное слово", &mut |canvas| {
            let size = 460.0 * 0.58 / giant.ink_height();
            canvas.word(&giant, ink, Transform::from_scale(size, size).post_translate(-40.0, 430.0), None);
        });
        measure("семь слов кеглем 60", &mut |canvas| {
            for (index, word) in small.iter().enumerate() {
                canvas.word(word, ink, Transform::from_scale(60.0, 60.0).post_rotate(8.0).post_translate(40.0 + 80.0 * index as f32, 100.0 + 40.0 * index as f32), None);
            }
        });
        let square = polygon(&[(-150.0, -150.0), (150.0, -150.0), (150.0, 150.0), (-150.0, 150.0)]).unwrap();
        let turned = Transform::from_rotate(17.0).post_translate(360.0, 230.0);
        measure("квадрат: линия в точку", &mut |canvas| {
            for _ in 0..5 {
                canvas.stroke(&square, faded(ink, 0.5), 1.0, turned, None);
            }
        });
        measure("квадрат: линия 1,2", &mut |canvas| {
            for _ in 0..5 {
                canvas.stroke(&square, faded(ink, 0.5), 1.2, turned, None);
            }
        });
        measure("квадрат: линия 1,0 без сглаживания", &mut |canvas| {
            let mut paint = Paint::default();
            paint.set_color(faded(ink, 0.5));
            paint.anti_alias = false;
            for _ in 0..5 {
                canvas.pixmap.stroke_path(&square, &paint, &Stroke { width: 1.0, ..Stroke::default() }, turned, None);
            }
        });
        measure("квадрат: залитые полоски", &mut |canvas| {
            let mut builder = PathBuilder::new();
            let corners = [(-150.0f32, -150.0f32), (150.0, -150.0), (150.0, 150.0), (-150.0, 150.0)];
            for index in 0..4 {
                let (a, b) = (corners[index], corners[(index + 1) % 4]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let length = dx.hypot(dy);
                let (nx, ny) = (-dy / length * 0.5, dx / length * 0.5);
                builder.move_to(a.0 + nx, a.1 + ny);
                builder.line_to(b.0 + nx, b.1 + ny);
                builder.line_to(b.0 - nx, b.1 - ny);
                builder.line_to(a.0 - nx, a.1 - ny);
                builder.close();
            }
            let path = builder.finish().unwrap();
            for _ in 0..5 {
                canvas.fill(&path, faded(ink, 0.5), turned, None);
            }
        });
        measure("пунктирная окружность", &mut |canvas| {
            canvas.stroke(&circle(360.0, 230.0, 110.0).unwrap(), faded(ink, 0.4), 1.3, Transform::identity(), Some((3.0, 6.0)));
        });
    }
}
