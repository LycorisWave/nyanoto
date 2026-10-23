//! Маленькая вода для «Чернил»: «устойчивые жидкости» Стэма и тушь в ней.
//! Скорость на грубой сетке, тушь — в `factor` раз мельче. Завихрённость держим — отсюда нити.
//! Без случайностей: та же история — та же картина.


pub struct Fluid {
    /// Сетка скорости.
    width: usize,
    height: usize,
    /// Во сколько раз сетка туши мельче.
    factor: usize,
    u: Vec<f32>,
    v: Vec<f32>,
    spare_u: Vec<f32>,
    spare_v: Vec<f32>,
    pressure: Vec<f32>,
    divergence: Vec<f32>,
    curl: Vec<f32>,
    /// Сетка туши.
    pub dye_width: usize,
    pub dye_height: usize,
    dye: Vec<f32>,
    spare_dye: Vec<f32>,
    /// Где есть тушь (клетки включительно), только растёт. Вне — не считаем.
    area: Option<(usize, usize, usize, usize)>,
}

/// Значение поля в дробной точке (билинейно, у краёв — край).
fn sample(field: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (width - 1) as f32);
    let y = y.clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let top = field[y0 * width + x0] + (field[y0 * width + x1] - field[y0 * width + x0]) * fx;
    let bottom = field[y1 * width + x0] + (field[y1 * width + x1] - field[y1 * width + x0]) * fx;
    top + (bottom - top) * fy
}

/// Как ведёт себя вода.
#[derive(Clone, Copy)]
pub struct Water {
    /// Насколько тушь тяжелее воды (ускорение вниз на единицу густоты, клеток/с²).
    pub sink: f32,
    /// Сила удержания вихрей.
    pub swirl: f32,
    /// Сколько скорости остаётся за секунду (вязкость, трение).
    pub keep: f32,
    /// Сколько туши остаётся за секунду (растворяется).
    pub fade: f32,
}

impl Fluid {
    /// Вода `dye_width` × `dye_height` клеток туши; скорость — в `factor` раз грубее.
    pub fn new(dye_width: usize, dye_height: usize, factor: usize) -> Self {
        let factor = factor.max(1);
        let (width, height) = ((dye_width / factor).max(3), (dye_height / factor).max(3));
        let cells = width * height;
        let dye_width = width * factor;
        let dye_height = height * factor;
        Self {
            width,
            height,
            factor,
            u: vec![0.0; cells],
            v: vec![0.0; cells],
            spare_u: vec![0.0; cells],
            spare_v: vec![0.0; cells],
            pressure: vec![0.0; cells],
            divergence: vec![0.0; cells],
            curl: vec![0.0; cells],
            dye_width,
            dye_height,
            dye: vec![0.0; dye_width * dye_height],
            spare_dye: vec![0.0; dye_width * dye_height],
            area: None,
        }
    }

    /// Расширяет область туши до клеток `left..=right`, `top..=bottom`.
    fn cover(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        let clamp = |value: i32, limit: usize| value.clamp(0, limit as i32 - 1) as usize;
        let (left, top) = (clamp(left, self.dye_width), clamp(top, self.dye_height));
        let (right, bottom) = (clamp(right, self.dye_width), clamp(bottom, self.dye_height));
        self.area = Some(self.area.map_or((left, top, right, bottom), |(l, t, r, b)| (l.min(left), t.min(top), r.max(right), b.max(bottom))));
    }

    /// Толчок в точке, всё в клетках туши.
    pub fn push(&mut self, x: f32, y: f32, radius: f32, force: (f32, f32)) {
        let factor = self.factor as f32;
        let (cx, cy, r) = (x / factor, y / factor, (radius / factor).max(0.5));
        let (fx, fy) = (force.0 / factor, force.1 / factor);
        let reach = (r * 2.5).ceil() as i32;
        for j in (cy as i32 - reach).max(1)..=(cy as i32 + reach).min(self.height as i32 - 2) {
            for i in (cx as i32 - reach).max(1)..=(cx as i32 + reach).min(self.width as i32 - 2) {
                let d2 = (i as f32 - cx).powi(2) + (j as f32 - cy).powi(2);
                let weight = (-d2 / (r * r)).exp();
                let index = j as usize * self.width + i as usize;
                self.u[index] += fx * weight;
                self.v[index] += fy * weight;
            }
        }
    }

    /// Тушь по маске `alpha` с углом в `x`, `y`.
    pub fn ink(&mut self, x: i32, y: i32, width: usize, alpha: &[u8], strength: f32) {
        if width == 0 {
            return;
        }
        self.cover(x, y, x + width as i32, y + (alpha.len() / width) as i32);
        for (row, line) in alpha.chunks(width).enumerate() {
            let dy = y + row as i32;
            if dy < 0 || dy >= self.dye_height as i32 {
                continue;
            }
            for (column, value) in line.iter().enumerate() {
                let dx = x + column as i32;
                if *value == 0 || dx < 0 || dx >= self.dye_width as i32 {
                    continue;
                }
                self.dye[dy as usize * self.dye_width + dx as usize] += f32::from(*value) / 255.0 * strength;
            }
        }
    }

    /// Капля туши: густой круг.
    pub fn blot(&mut self, x: f32, y: f32, radius: f32, strength: f32) {
        let reach = radius.ceil() as i32 + 1;
        self.cover(x as i32 - reach, y as i32 - reach, x as i32 + reach, y as i32 + reach);
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px < 0 || py < 0 || px >= self.dye_width as i32 || py >= self.dye_height as i32 {
                    continue;
                }
                let d = ((px as f32 + 0.5 - x).powi(2) + (py as f32 + 0.5 - y).powi(2)).sqrt();
                let cover = (radius + 0.5 - d).clamp(0.0, 1.0);
                self.dye[py as usize * self.dye_width + px as usize] += cover * strength;
            }
        }
    }

    /// Шаг на `dt` секунд.
    pub fn step(&mut self, dt: f32, water: Water) {
        let (width, height, factor) = (self.width, self.height, self.factor);
        let at = |i: usize, j: usize| j * width + i;
        let Some((left, top, right, bottom)) = self.area else {
            return;
        };
        // Тушь тяжелее воды: тянет воду вниз там, где она гуще.
        let area = (factor * factor) as f32;
        for j in (top / factor).max(1)..(bottom / factor + 1).min(height - 1) {
            for i in (left / factor).max(1)..(right / factor + 1).min(width - 1) {
                let mut sum = 0.0;
                for dy in 0..factor {
                    let row = (j * factor + dy) * self.dye_width + i * factor;
                    sum += self.dye[row..row + factor].iter().sum::<f32>();
                }
                self.v[at(i, j)] += water.sink * (sum / area).min(2.0) * dt;
            }
        }
        // Удержание вихрей: подталкивает воду вдоль вихря — завитки не расплываются.
        for j in 1..height - 1 {
            for i in 1..width - 1 {
                self.curl[at(i, j)] = 0.5 * ((self.v[at(i + 1, j)] - self.v[at(i - 1, j)]) - (self.u[at(i, j + 1)] - self.u[at(i, j - 1)]));
            }
        }
        for j in 2..height - 2 {
            for i in 2..width - 2 {
                let gx = 0.5 * (self.curl[at(i + 1, j)].abs() - self.curl[at(i - 1, j)].abs());
                let gy = 0.5 * (self.curl[at(i, j + 1)].abs() - self.curl[at(i, j - 1)].abs());
                let length = (gx * gx + gy * gy).sqrt() + 1e-5;
                let curl = self.curl[at(i, j)];
                self.u[at(i, j)] += water.swirl * (gy / length) * curl * dt;
                self.v[at(i, j)] -= water.swirl * (gx / length) * curl * dt;
            }
        }
        // Перенос скорости самой водой.
        let keep = water.keep.powf(dt);
        let mut fastest = 0.0f32;
        for j in 0..height {
            for i in 0..width {
                let index = at(i, j);
                if i == 0 || j == 0 || i == width - 1 || j == height - 1 {
                    self.spare_u[index] = 0.0;
                    self.spare_v[index] = 0.0;
                    continue;
                }
                let (x, y) = (i as f32 - dt * self.u[index], j as f32 - dt * self.v[index]);
                self.spare_u[index] = sample(&self.u, width, height, x, y) * keep;
                self.spare_v[index] = sample(&self.v, width, height, x, y) * keep;
                fastest = fastest.max(self.spare_u[index].abs()).max(self.spare_v[index].abs());
            }
        }
        std::mem::swap(&mut self.u, &mut self.spare_u);
        std::mem::swap(&mut self.v, &mut self.spare_v);
        // Несжимаемость: убираем расхождение (давление методом Якоби).
        for j in 1..height - 1 {
            for i in 1..width - 1 {
                self.divergence[at(i, j)] = 0.5 * (self.u[at(i + 1, j)] - self.u[at(i - 1, j)] + self.v[at(i, j + 1)] - self.v[at(i, j - 1)]);
            }
        }
        for _ in 0..24 {
            for j in 1..height - 1 {
                for i in 1..width - 1 {
                    let around = self.pressure[at(i - 1, j)] + self.pressure[at(i + 1, j)] + self.pressure[at(i, j - 1)] + self.pressure[at(i, j + 1)];
                    self.spare_u[at(i, j)] = (around - self.divergence[at(i, j)]) / 4.0;
                }
            }
            for j in 1..height - 1 {
                let row = j * width;
                self.pressure[row + 1..row + width - 1].copy_from_slice(&self.spare_u[row + 1..row + width - 1]);
                self.pressure[row] = self.pressure[row + 1];
                self.pressure[row + width - 1] = self.pressure[row + width - 2];
            }
            let (top, bottom) = (width, (height - 2) * width);
            self.pressure.copy_within(top..top + width, 0);
            self.pressure.copy_within(bottom..bottom + width, (height - 1) * width);
        }
        for j in 1..height - 1 {
            for i in 1..width - 1 {
                self.u[at(i, j)] -= 0.5 * (self.pressure[at(i + 1, j)] - self.pressure[at(i - 1, j)]);
                self.v[at(i, j)] -= 0.5 * (self.pressure[at(i, j + 1)] - self.pressure[at(i, j - 1)]);
            }
        }
        // Переносим только там, куда тушь могла дойти за шаг.
        let fade = water.fade.powf(dt);
        let scale = factor as f32;
        let margin = (fastest * dt * scale).ceil() as i32 + 2;
        self.cover(left as i32 - margin, top as i32 - margin, right as i32 + margin, bottom as i32 + margin);
        let (left, top, right, bottom) = self.area.expect("область задана выше");
        for j in top..=bottom {
            let gy = (j as f32 + 0.5) / scale - 0.5;
            for i in left..=right {
                let gx = (i as f32 + 0.5) / scale - 0.5;
                let u = sample(&self.u, width, height, gx, gy);
                let v = sample(&self.v, width, height, gx, gy);
                let (x, y) = (i as f32 - dt * u * scale, j as f32 - dt * v * scale);
                self.spare_dye[j * self.dye_width + i] = sample(&self.dye, self.dye_width, self.dye_height, x, y) * fade;
            }
        }
        std::mem::swap(&mut self.dye, &mut self.spare_dye);
    }

    /// Непрозрачность в `cover`; возвращает, где тушь видна, — рисовать только там.
    pub fn paint(&self, cover: &mut Vec<f32>, gain: f32) -> Option<(usize, usize, usize, usize)> {
        let (left, top, right, bottom) = self.area?;
        let width = self.dye_width;
        cover.resize(self.dye.len(), 0.0);
        let mut seen: Option<(usize, usize, usize, usize)> = None;
        for j in top..=bottom {
            let mut row: Option<(usize, usize)> = None;
            for i in left..=right {
                let index = j * width + i;
                // 1 − e^(−x) без экспоненты: (1 + x/4)^−4 — почти то же.
                let x = gain * self.dye[index].max(0.0) / 4.0 + 1.0;
                let squared = x * x;
                let value = 1.0 - 1.0 / (squared * squared);
                cover[index] = value;
                if value > 0.004 {
                    row = Some(row.map_or((i, i), |(from, _)| (from, i)));
                }
            }
            if let Some((from, to)) = row {
                seen = Some(seen.map_or((from, j, to, j), |(l, t, r, _)| (l.min(from), t, r.max(to), j)));
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Удар вниз даёт гриб, и тушь не берётся из ниоткуда.
    #[test]
    fn a_push_carries_ink_down_into_a_mushroom() {
        let mut fluid = Fluid::new(120, 90, 3);
        fluid.blot(60.0, 20.0, 3.0, 2.0);
        let before: f32 = fluid.dye.iter().sum();
        let water = Water { sink: 6.0, swirl: 8.0, keep: 0.6, fade: 1.0 };
        for step in 0..60 {
            if step < 8 {
                fluid.push(60.0, 22.0, 4.0, (0.0, 220.0));
            }
            fluid.step(1.0 / 30.0, water);
        }
        let total: f32 = fluid.dye.iter().sum();
        assert!((total - before).abs() / before < 0.35, "{before} → {total}");
        // Центр тяжести туши ушёл вниз, а сама она расползлась вширь.
        let (mut cy, mut spread) = (0.0, 0.0);
        for (index, density) in fluid.dye.iter().enumerate() {
            let (x, y) = ((index % fluid.dye_width) as f32, (index / fluid.dye_width) as f32);
            cy += y * density;
            spread += (x - 60.0).abs() * density;
        }
        assert!(cy / total > 35.0, "{}", cy / total);
        assert!(spread / total > 4.0, "{}", spread / total);
    }
}

#[cfg(test)]
mod costs {
    use super::*;

    /// Сколько стоит шаг воды: `cargo test --release -p nyanoto-app fluid_costs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn fluid_costs() {
        let mut fluid = Fluid::new(180, 117, 3);
        let water = Water { sink: 6.0, swirl: 10.0, keep: 0.7, fade: 0.85 };
        for x in 20..160 {
            fluid.blot(x as f32, 40.0, 2.0, 1.0);
        }
        let mut picture = Vec::new();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            fluid.push(90.0, 42.0, 6.0, (0.0, 22.0));
            fluid.step(1.0 / 30.0, water);
        }
        println!("шаг: {:.3} мс, область {:?}", started.elapsed().as_secs_f64() * 10.0, fluid.area);
        let started = std::time::Instant::now();
        for _ in 0..100 {
            fluid.paint(&mut picture, 2.2);
        }
        println!("картинка: {:.3} мс", started.elapsed().as_secs_f64() * 10.0);
    }
}
