//! Волна для редактора текстов — маской, цвет подставляет окно (`colorize`).
//! Пик бледный, RMS плотный — по нему видно, где вступает голос.

use nyanoto_core::{PEAKS_PER_SECOND, Peaks};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Ширина картинок в точках; окно растягивает их по своей ширине.
pub const WIDTH: u32 = 1600;
pub const ZOOM_HEIGHT: u32 = 92;
pub const OVERVIEW_HEIGHT: u32 = 26;

const PEAK_ALPHA: u8 = 110;
const RMS_ALPHA: u8 = 255;

/// Отрезок записи от `from_ms` до `to_ms` (может выходить за края трека).
pub fn render(peaks: &Peaks, from_ms: i64, to_ms: i64, width: u32, height: u32) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let span = (to_ms - from_ms).max(1) as f64;
    let per_ms = f64::from(PEAKS_PER_SECOND) / 1000.0;
    let pixels = buffer.make_mut_slice();
    for column in 0..width {
        let start = from_ms as f64 + span * f64::from(column) / f64::from(width);
        let end = from_ms as f64 + span * f64::from(column + 1) / f64::from(width);
        let first = (start * per_ms).floor();
        let last = (end * per_ms).ceil().max(first + 1.0);
        if last <= 0.0 || first >= peaks.peak.len() as f64 {
            continue;
        }
        let range = first.max(0.0) as usize..(last as usize).min(peaks.peak.len());
        let peak = peaks.peak[range.clone()].iter().copied().fold(0.0f32, f32::max);
        let rms = peaks.rms[range].iter().copied().fold(0.0f32, f32::max);
        for (level, alpha) in [(peak, PEAK_ALPHA), (rms, RMS_ALPHA)] {
            let half = (level * height as f32 / 2.0).round() as u32;
            let top = (height / 2).saturating_sub(half);
            let bottom = (height / 2 + half.max(1)).min(height);
            for row in top..bottom {
                pixels[(row * width + column) as usize] = Rgba8Pixel { r: 255, g: 255, b: 255, a: alpha };
            }
        }
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(buffer: &SharedPixelBuffer<Rgba8Pixel>, column: u32, row: u32) -> u8 {
        buffer.as_slice()[(row * buffer.width() + column) as usize].a
    }

    #[test]
    fn loud_half_is_drawn_and_quiet_half_is_flat() {
        // Секунда тишины, секунда громко: пик 0.8, среднее 0.4.
        let per_second = PEAKS_PER_SECOND as usize;
        let peaks = Peaks {
            peak: [vec![0.0; per_second], vec![0.8; per_second]].concat(),
            rms: [vec![0.0; per_second], vec![0.4; per_second]].concat(),
        };
        let image = render(&peaks, 0, 2000, 100, 40);
        // Тишина — линия в один пиксель посередине, выше и ниже пусто.
        assert_eq!(alpha(&image, 10, 5), 0);
        // Громкая часть: у середины плотно, у края только пик.
        assert_eq!(alpha(&image, 80, 20), RMS_ALPHA);
        assert_eq!(alpha(&image, 80, 6), PEAK_ALPHA);
        assert_eq!(alpha(&image, 80, 1), 0);
    }

    #[test]
    fn outside_the_track_is_empty() {
        let peaks = Peaks { peak: vec![1.0; 200], rms: vec![1.0; 200] };
        let image = render(&peaks, -5000, 5000, 100, 10);
        assert_eq!(alpha(&image, 10, 5), 0, "до начала трека");
        assert_eq!(alpha(&image, 55, 5), RMS_ALPHA, "сам трек");
        assert_eq!(alpha(&image, 80, 5), 0, "после конца");
    }
}
