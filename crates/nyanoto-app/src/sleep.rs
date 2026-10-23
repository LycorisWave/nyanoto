//! Таймер сна: сверху «во сколько», снизу круг «через сколько», крутишь любое.
//! Круг — 12 часов, обороты копятся (как время в Genshin), предел — сутки.
//! Храним цель «во сколько» (Unix, ровно на минуте).

use std::f32::consts::PI;

/// Пределы: не меньше минуты, не больше суток.
pub const MIN_MINUTES: i64 = 1;
pub const MAX_MINUTES: i64 = 24 * 60;
/// Шаг ручки на круге — 5 минут (сверху часы и минуты ставятся точно).
pub const DIAL_STEP: i64 = 5;
/// Минут в одном обороте круга.
const TURN: f32 = 12.0 * 60.0;

/// Радиусы колец в квадрате 200 × 200: первый оборот и второй (внутри).
pub const RING: f32 = 80.0;
pub const INNER_RING: f32 = 64.0;

/// Угол от «12 часов» по часовой (y вниз).
pub fn angle_of(dx: f32, dy: f32) -> f32 {
    dx.atan2(-dy).to_degrees().rem_euclid(360.0)
}

/// Где на циферблате (12 ч) стоит момент: угол в градусах.
pub fn clock_angle(minute_of_day: i64) -> f32 {
    (minute_of_day.rem_euclid(12 * 60) as f32) / TURN * 360.0
}

/// Минуты от «сейчас» до цели, в пределах.
pub fn clamp_minutes(minutes: i64) -> i64 {
    minutes.clamp(MIN_MINUTES, MAX_MINUTES)
}

/// Ручка прыгает под палец: из двух оборотов — ближайший к прежнему, на 5-минутной отметке.
pub fn press(now_minute: i64, previous: i64, angle: f32) -> i64 {
    let face = (angle / 360.0 * TURN).round() as i64;
    let now_face = now_minute.rem_euclid(12 * 60);
    let base = (face - now_face).rem_euclid(12 * 60);
    let best = [base, base + 12 * 60]
        .into_iter()
        .min_by_key(|candidate| (candidate - previous).abs())
        .unwrap_or(base);
    snap(now_minute, best)
}

/// Палец по кругу: ручка идёт непрерывно, упирается в «сейчас» и сутки.
pub fn drag(raw: f32, from_angle: f32, to_angle: f32) -> f32 {
    let mut turn = to_angle - from_angle;
    if turn > 180.0 {
        turn -= 360.0;
    } else if turn < -180.0 {
        turn += 360.0;
    }
    (raw + turn / 360.0 * TURN).clamp(MIN_MINUTES as f32, MAX_MINUTES as f32)
}

/// Округляет «через сколько» так, чтобы цель легла на 5-минутную отметку часов.
pub fn snap(now_minute: i64, minutes: i64) -> i64 {
    let target = now_minute + minutes;
    let snapped = (target as f32 / DIAL_STEP as f32).round() as i64 * DIAL_STEP;
    let minutes = snapped - now_minute;
    // Ближайшая отметка могла оказаться в прошлом — тогда следующая.
    clamp_minutes(if minutes < MIN_MINUTES { minutes + DIAL_STEP } else { minutes })
}

/// Точка на кольце радиуса `radius` под углом `angle` (квадрат 200 × 200).
pub fn point(angle: f32, radius: f32) -> (f32, f32) {
    let radians = angle * PI / 180.0;
    (100.0 + radius * radians.sin(), 100.0 - radius * radians.cos())
}

/// Дуга до цели (200 × 200): первый оборот снаружи, сверх 12 часов — внутри.
pub fn arc(now_minute: i64, minutes: i64) -> String {
    let start = clock_angle(now_minute);
    let mut commands = String::new();
    let mut piece = |radius: f32, from: f32, sweep: f32| {
        if sweep <= 0.01 {
            return;
        }
        // Полный круг дугой не нарисовать: две половины.
        let halves = if sweep >= 359.9 { 2 } else { 1 };
        let part = sweep / halves as f32;
        let (x, y) = point(from, radius);
        commands.push_str(&format!("M {x:.2} {y:.2} "));
        for half in 0..halves {
            let end = from + part * (half + 1) as f32;
            let (x, y) = point(end, radius);
            let large = u8::from(part > 180.0);
            commands.push_str(&format!("A {radius} {radius} 0 {large} 1 {x:.2} {y:.2} "));
        }
    };
    let total = minutes as f32 / TURN * 360.0;
    piece(RING, start, total.min(359.99));
    if total > 360.0 {
        piece(INNER_RING, start, (total - 360.0).min(359.99));
    }
    commands.trim_end().to_string()
}

/// Кольцо вокруг значка на долю `share` — сколько до сна.
pub fn ring(share: f32) -> String {
    let share = share.clamp(0.0, 1.0);
    if share <= 0.005 {
        return String::new();
    }
    let point = |angle: f32| {
        let radians = angle * PI / 180.0;
        (12.0 + 11.0 * radians.sin(), 12.0 - 11.0 * radians.cos())
    };
    let sweep = (share * 360.0).min(359.9);
    let (half, end) = (sweep / 2.0, point(sweep));
    let middle = point(half);
    format!("M 12 1 A 11 11 0 0 1 {:.2} {:.2} A 11 11 0 0 1 {:.2} {:.2}", middle.0, middle.1, end.0, end.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_go_clockwise_from_twelve() {
        assert!((angle_of(0.0, -10.0) - 0.0).abs() < 1e-3);
        assert!((angle_of(10.0, 0.0) - 90.0).abs() < 1e-3);
        assert!((angle_of(0.0, 10.0) - 180.0).abs() < 1e-3);
        assert!((angle_of(-10.0, 0.0) - 270.0).abs() < 1e-3);
        // 22:00 и 10:00 — одно место на циферблате.
        assert_eq!(clock_angle(22 * 60), clock_angle(10 * 60));
        assert!((clock_angle(15 * 60) - 90.0).abs() < 1e-3);
    }

    /// 22:00, ручка на «11» — через час; через «12» обороты копятся.
    #[test]
    fn press_and_drag_follow_the_clock() {
        let now = 22 * 60;
        assert_eq!(press(now, 30, clock_angle(23 * 60)), 60);
        // Тот же угол при прежних 12+ часах — на втором обороте.
        assert_eq!(press(now, 800, clock_angle(23 * 60)), 60 + 12 * 60);
        // Палец прошёл полный круг — плюс 12 часов, не больше суток.
        let mut raw = 60.0;
        let mut angle = clock_angle(23 * 60);
        for _ in 0..24 {
            let next = (angle + 30.0).rem_euclid(360.0);
            raw = drag(raw, angle, next);
            angle = next;
        }
        assert_eq!(raw.round() as i64, MAX_MINUTES);
        // Назад за «сейчас» не уходит.
        let back = drag(10.0, 90.0, 40.0);
        assert_eq!(back, MIN_MINUTES as f32);
    }

    #[test]
    fn snapping_lands_on_five_minute_marks() {
        let now = 22 * 60 + 7;
        let minutes = snap(now, 60);
        assert_eq!((now + minutes) % 5, 0);
        assert_eq!(minutes, 58);
        // Совсем близко к «сейчас» — следующая отметка, не прошлая.
        assert!(snap(now, 1) >= MIN_MINUTES);
        assert_eq!((now + snap(now, 1)) % 5, 0);
        assert_eq!(snap(now, 5000), MAX_MINUTES);
    }

    #[test]
    fn ring_shrinks_with_the_time_left() {
        assert_eq!(ring(0.0), "");
        let half = ring(0.5);
        assert!(half.ends_with("12.00 23.00"), "{half}");
        assert!(ring(1.0).starts_with("M 12 1 A 11 11"));
    }

    #[test]
    fn arc_uses_the_inner_ring_after_twelve_hours() {
        let short = arc(0, 90);
        assert!(short.starts_with("M 100.00 20.00 A 80 80"), "{short}");
        assert!(!short.contains("A 64"));
        let long = arc(0, 13 * 60);
        assert_eq!(long.matches("A 80").count(), 2, "полный первый круг — две половины");
        assert!(long.contains("A 64"));
    }
}
