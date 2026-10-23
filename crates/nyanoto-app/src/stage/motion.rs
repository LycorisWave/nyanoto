//! Общая механика движения сцен.
//! Всё — функции времени: кадр не зависит от прошлых, перемотка ничего не ломает.

use std::f32::consts::TAU;

use tiny_skia::Transform;

/// Генератор случайных чисел (SplitMix64): раскладка зависит только от зерна.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x2545_F491_4F6C_DD1D)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Число от 0 до 1.
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, from: f32, to: f32) -> f32 {
        from + (to - from) * self.unit()
    }

    pub fn sign(&mut self) -> f32 {
        if self.next() & 1 == 0 { 1.0 } else { -1.0 }
    }

    pub fn below(&mut self, count: usize) -> usize {
        (self.next() % count.max(1) as u64) as usize
    }

    pub fn chance(&mut self, share: f32) -> bool {
        self.unit() < share
    }
}

/// Зерно из нескольких чисел.
pub fn mix(seed: u64, value: u64) -> u64 {
    let mut rng = Rng::new(seed ^ value.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    rng.next()
}

pub fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

pub fn lerp(from: f32, to: f32, share: f32) -> f32 {
    from + (to - from) * share
}

/// Доля пройденного движения, которое начинается в `from` и длится `length`.
pub fn progress(t: f32, from: f32, length: f32) -> f32 {
    if length <= 0.0 {
        if t >= from { 1.0 } else { 0.0 }
    } else {
        clamp01((t - from) / length)
    }
}

pub fn ease_out(p: f32) -> f32 {
    let q = 1.0 - clamp01(p);
    1.0 - q * q * q
}

pub fn ease_in(p: f32) -> f32 {
    let p = clamp01(p);
    p * p * p
}

pub fn ease_in_out(p: f32) -> f32 {
    let p = clamp01(p);
    if p < 0.5 { 4.0 * p * p * p } else { 1.0 - (-2.0 * p + 2.0).powi(3) / 2.0 }
}

pub fn apply(transform: Transform, x: f32, y: f32) -> (f32, f32) {
    (transform.sx * x + transform.kx * y + transform.tx, transform.ky * x + transform.sy * y + transform.ty)
}

/// Масштаб и поворот (градусы) вокруг точки.
pub fn around(point: (f32, f32), scale: f32, degrees: f32) -> Transform {
    Transform::from_translate(-point.0, -point.1).post_scale(scale, scale).post_rotate(degrees).post_translate(point.0, point.1)
}

/// Медленное блуждание: сумма синусов с частотами и фазами от зерна, −1…1.
pub fn wander(t: f32, seed: u64, speed: f32) -> f32 {
    let mut rng = Rng::new(seed);
    let mut sum = 0.0;
    let mut weight = 0.0;
    for octave in 0..3 {
        let frequency = rng.range(0.6, 1.4) * speed * (1.0 + octave as f32 * 1.7);
        let phase = rng.range(0.0, TAU);
        let amplitude = 1.0 / (1.0 + octave as f32);
        sum += amplitude * (t * frequency * TAU + phase).sin();
        weight += amplitude;
    }
    sum / weight
}

/// Вспышка после события: 1 сразу, дальше быстро к нулю.
pub fn kick(since: f32, decay: f32) -> f32 {
    if since.is_finite() && since >= 0.0 { (-since * decay).exp() } else { 0.0 }
}

/// «Камера»: что в центре экрана, увеличение и поворот (радианы).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub turn: f32,
}

impl Camera {
    /// Мир → экран размером `width` × `height`.
    pub fn view(&self, width: f32, height: f32) -> Transform {
        Transform::from_translate(-self.x, -self.y)
            .post_rotate(self.turn.to_degrees())
            .post_scale(self.zoom, self.zoom)
            .post_translate(width / 2.0, height / 2.0)
    }
}

/// Как слово появляется — сдержанно, без ввинчивания и отскоков. Удар — только в самом громком.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Entry {
    /// Проявляется.
    Fade,
    /// Всплывает снизу.
    Rise,
    /// Выходит из точки с лёгким перелётом.
    Pop,
    /// Влетает крупнее и ложится на место.
    Slam,
    /// Въезжает сбоку (угол в радианах — откуда).
    Slide(f32),
    /// Буквы по одной.
    Type,
    /// Приближается из глубины.
    Zoom,
}

/// Прозрачность, сдвиг (в кеглях), масштаб, поворот и доля видимых букв.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub alpha: f32,
    pub shift: (f32, f32),
    pub scale_x: f32,
    pub scale_y: f32,
    pub turn: f32,
    pub letters: f32,
}

impl Look {
    pub const STILL: Look = Look { alpha: 1.0, shift: (0.0, 0.0), scale_x: 1.0, scale_y: 1.0, turn: 0.0, letters: 1.0 };

    /// Слово кеглем 1 с базой `base` и серединой `pivot` (на экране), кегль `size`.
    pub fn place(&self, base: Transform, pivot: (f32, f32), size: f32) -> Transform {
        base.post_translate(self.shift.0 * size, self.shift.1 * size)
            .post_concat(
                Transform::from_translate(-pivot.0, -pivot.1)
                    .post_scale(self.scale_x, self.scale_y)
                    .post_rotate(self.turn)
                    .post_translate(pivot.0, pivot.1),
            )
    }
}

/// Мягкий перелёт (меньше, чем у `back_out`).
fn soft_back(p: f32) -> f32 {
    let p = clamp01(p) - 1.0;
    let (c1, c3) = (0.9, 1.9);
    1.0 + c3 * p * p * p + c1 * p * p
}

impl Entry {
    /// Спокойные и резкие появления: из чего выбирать при таком напряжении.
    pub fn pick(rng: &mut Rng, energy: f32) -> Entry {
        let calm = [Entry::Fade, Entry::Rise, Entry::Type, Entry::Zoom];
        let lively = [Entry::Rise, Entry::Zoom, Entry::Slide(0.0), Entry::Pop];
        let pool: &[Entry] = if energy > 0.8 && rng.chance(0.4) {
            &[Entry::Slam, Entry::Pop]
        } else if rng.unit() < energy {
            &lively
        } else {
            &calm
        };
        match pool[rng.below(pool.len())] {
            // Сбоку — только слева или справа: сверху-снизу выглядит суетливо.
            Entry::Slide(_) => Entry::Slide(if rng.chance(0.5) { 0.0 } else { std::f32::consts::PI }),
            other => other,
        }
    }

    /// Длительность при темпе `tempo` (больше — медленнее).
    pub fn length(self, tempo: f32) -> f32 {
        tempo
            * match self {
                Entry::Fade | Entry::Rise | Entry::Zoom => 0.55,
                Entry::Type => 0.45,
                Entry::Pop => 0.35,
                Entry::Slam => 0.3,
                Entry::Slide(_) => 0.4,
            }
    }

    pub fn look(self, p: f32) -> Look {
        let mut look = Look::STILL;
        if p >= 1.0 {
            return look;
        }
        let p = clamp01(p);
        match self {
            Entry::Fade => look.alpha = ease_out(p),
            Entry::Rise => {
                look.alpha = ease_out(p);
                look.shift.1 = 0.3 * (1.0 - ease_out(p));
            }
            Entry::Pop => {
                look.alpha = clamp01(p * 2.5);
                let scale = 0.6 + 0.4 * soft_back(p);
                (look.scale_x, look.scale_y) = (scale, scale);
            }
            Entry::Slam => {
                look.alpha = clamp01(p * 2.5);
                let scale = 1.0 + 0.3 * (1.0 - soft_back(p));
                (look.scale_x, look.scale_y) = (scale, scale);
            }
            Entry::Slide(angle) => {
                look.alpha = ease_out(p);
                let away = 1.2 * (1.0 - ease_out(p));
                look.shift = (angle.cos() * away, angle.sin() * away);
            }
            Entry::Type => {
                look.letters = p;
            }
            Entry::Zoom => {
                look.alpha = ease_out(p);
                let scale = 0.8 + 0.2 * ease_out(p);
                (look.scale_x, look.scale_y) = (scale, scale);
            }
        }
        look
    }
}

/// Как слово уходит.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exit {
    Fade,
    /// Чуть растёт и тает — будто проходит мимо.
    Fly,
    /// Опускается и тает.
    Sink,
    /// Уезжает в сторону (угол — куда).
    Slide(f32),
    /// Буквы расходятся (только на самых громких местах).
    Burst,
}

impl Exit {
    pub fn pick(rng: &mut Rng, energy: f32) -> Exit {
        if energy > 0.8 && rng.chance(0.15) {
            return Exit::Burst;
        }
        let calm = [Exit::Fade, Exit::Sink, Exit::Fade];
        let lively = [Exit::Fade, Exit::Fly, Exit::Slide(0.0)];
        let pool: &[Exit] = if rng.unit() < energy { &lively } else { &calm };
        match pool[rng.below(pool.len())] {
            Exit::Slide(_) => Exit::Slide(if rng.chance(0.5) { 0.0 } else { std::f32::consts::PI }),
            other => other,
        }
    }

    pub fn length(self, tempo: f32) -> f32 {
        tempo
            * match self {
                Exit::Fade | Exit::Sink => 0.45,
                Exit::Fly | Exit::Burst => 0.4,
                Exit::Slide(_) => 0.35,
            }
    }

    pub fn look(self, p: f32) -> Look {
        let mut look = Look::STILL;
        if p <= 0.0 {
            return look;
        }
        let p = clamp01(p);
        match self {
            Exit::Fade => look.alpha = 1.0 - ease_out(p),
            Exit::Fly => {
                look.alpha = 1.0 - ease_out(p);
                let scale = 1.0 + 0.25 * ease_out(p);
                (look.scale_x, look.scale_y) = (scale, scale);
            }
            Exit::Sink => {
                look.alpha = 1.0 - ease_out(p);
                look.shift.1 = 0.25 * ease_out(p);
            }
            Exit::Slide(angle) => {
                look.alpha = 1.0 - ease_out(p);
                let away = 1.0 * ease_out(p);
                look.shift = (angle.cos() * away, angle.sin() * away);
            }
            Exit::Burst => {
                look.alpha = 1.0 - ease_out(p);
                look.letters = -ease_out(p) * 0.6;
            }
        }
        look
    }
}

/// Сводит появление и уход: прозрачности перемножаются, сдвиги складываются.
pub fn combine(entry: Look, exit: Look) -> Look {
    Look {
        alpha: entry.alpha * exit.alpha,
        shift: (entry.shift.0 + exit.shift.0, entry.shift.1 + exit.shift.1),
        scale_x: entry.scale_x * exit.scale_x,
        scale_y: entry.scale_y * exit.scale_y,
        turn: entry.turn + exit.turn,
        letters: if exit.letters < 0.0 { exit.letters } else { entry.letters },
    }
}

/// Темп движений при напряжении `energy`: спокойно — медленнее.
pub fn tempo(energy: f32) -> f32 {
    lerp(1.6, 0.75, clamp01(energy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_end_at_rest_and_wander_stays_in_bounds() {
        for entry in [Entry::Fade, Entry::Rise, Entry::Pop, Entry::Slam, Entry::Slide(1.0), Entry::Type, Entry::Zoom] {
            let end = entry.look(1.0);
            assert_eq!((end.alpha, end.shift, end.scale_x, end.turn, end.letters), (1.0, (0.0, 0.0), 1.0, 0.0, 1.0), "{entry:?}");
            assert!(entry.look(0.0).alpha <= 0.01 || entry == Entry::Type, "{entry:?}");
            // Без резких скачков: масштаб в пределах 0,6…1,3.
            for step in 0..=20 {
                let look = entry.look(step as f32 / 20.0);
                assert!((0.55..=1.35).contains(&look.scale_x), "{entry:?} {}", look.scale_x);
            }
        }
        for exit in [Exit::Fade, Exit::Fly, Exit::Sink, Exit::Slide(0.5), Exit::Burst] {
            assert_eq!(exit.look(0.0).alpha, 1.0);
            assert!(exit.look(1.0).alpha < 0.01, "{exit:?}");
        }
        for step in 0..500 {
            let value = wander(step as f32 * 0.37, 9, 0.2);
            assert!((-1.0..=1.0).contains(&value));
        }
        let camera = Camera { x: 10.0, y: 20.0, zoom: 2.0, turn: 0.0 };
        assert_eq!(apply(camera.view(100.0, 50.0), 10.0, 20.0), (50.0, 25.0));
        assert_eq!(apply(camera.view(100.0, 50.0), 11.0, 20.0), (52.0, 25.0));
    }
}
