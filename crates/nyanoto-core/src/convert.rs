//! Звук к формату устройства: каналы, частота и скорость.

use rubato::{FftFixedIn, Resampler};
use symphonia::core::audio::Channels;

use crate::source::TrackError;
use crate::stretch::Stretch;

/// Скорость воспроизведения.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speed {
    /// Во сколько раз быстрее записи: 0.5 — вдвое медленнее.
    pub factor: f64,
    /// Сохранять высоту звука. Иначе — как пластинка: медленнее значит ниже.
    pub keep_pitch: bool,
}

impl Speed {
    pub const NORMAL: Speed = Speed { factor: 1.0, keep_pitch: true };

    pub fn is_normal(self) -> bool {
        (self.factor - 1.0).abs() < 1e-9
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Частота «пластинки», кратная 50 — общий делитель большой, окно ресемплера маленькое.
fn varispeed_rate(in_rate: u32, factor: f64) -> u32 {
    let rate = (f64::from(in_rate) * factor / 50.0).round() * 50.0;
    rate.clamp(50.0, f64::from(u32::MAX)) as u32
}

/// Размер порции ресемплера во входных кадрах.
const RESAMPLER_CHUNK: usize = 1024;

/// Вклад канала в стереопару при сведении: центр и тыл берутся с ослаблением на 3 дБ.
const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;

pub(crate) struct Converter {
    in_rate: u32,
    layout: Channels,
    in_channels: usize,
    /// Матрица сведения: по строке из `in_channels` коэффициентов на каждый канал выхода.
    matrix: Vec<f32>,
    passthrough: bool,
    resampler: Option<Rate>,
    /// Растяжение без изменения высоты, если скорость не 1.
    stretch: Option<Stretch>,
    /// Во сколько раз быстрее записи звучит выход на самом деле.
    effective: f64,
    scratch: Vec<f32>,
}

impl Converter {
    pub fn new(
        in_rate: u32,
        layout: Channels,
        out_rate: u32,
        out_channels: usize,
        speed: Speed,
    ) -> Result<Self, TrackError> {
        let in_channels = layout.count();
        if in_channels == 0 || in_rate == 0 || out_rate == 0 || out_channels == 0 {
            return Err(TrackError::Unsupported(format!(
                "{in_channels} каналов, {in_rate} Гц"
            )));
        }
        // «Пластинка»: ресемплер считает, что у файла другая частота.
        let source_rate = if speed.is_normal() || speed.keep_pitch {
            in_rate
        } else {
            varispeed_rate(in_rate, speed.factor)
        };
        let resampler = if source_rate == out_rate {
            None
        } else {
            Some(Rate::new(source_rate, out_rate, out_channels)?)
        };
        let (stretch, effective) = if speed.is_normal() {
            (None, 1.0)
        } else if speed.keep_pitch {
            (Some(Stretch::new(out_channels, out_rate, speed.factor)?), speed.factor)
        } else {
            (None, f64::from(source_rate) / f64::from(in_rate))
        };
        Ok(Self {
            in_rate,
            layout,
            in_channels,
            matrix: mix_matrix(layout, out_channels),
            passthrough: in_channels == out_channels,
            resampler,
            stretch,
            effective,
            scratch: Vec::new(),
        })
    }

    /// Во сколько раз быстрее записи звучит выход.
    pub fn speed(&self) -> f64 {
        self.effective
    }

    /// Подходит ли преобразователь для звука с такими параметрами.
    pub fn matches(&self, rate: u32, layout: Channels) -> bool {
        self.in_rate == rate && self.layout == layout
    }

    /// Преобразует кадры из `input` и дописывает результат в `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), TrackError> {
        if self.stretch.is_none() {
            return self.convert(input, out);
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        let result = self.convert(input, &mut scratch).and_then(|()| match self.stretch.as_mut() {
            Some(stretch) => stretch.process(&scratch, out),
            None => Ok(()),
        });
        self.scratch = scratch;
        result
    }

    /// Раскладка каналов и частота, без растяжения.
    fn convert(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), TrackError> {
        let Some(rate) = self.resampler.as_mut() else {
            if self.passthrough {
                out.extend_from_slice(input);
            } else {
                for frame in input.chunks_exact(self.in_channels) {
                    for row in self.matrix.chunks_exact(self.in_channels) {
                        out.push(mix(row, frame));
                    }
                }
            }
            return Ok(());
        };

        for frame in input.chunks_exact(self.in_channels) {
            for (row, channel) in self
                .matrix
                .chunks_exact(self.in_channels)
                .zip(rate.input.iter_mut())
            {
                channel.push(mix(row, frame));
            }
        }
        rate.fed += (input.len() / self.in_channels) as u64;
        rate.run(out)
    }

    /// Дописывает хвост ресемплера — выход по длине ровно как вход.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        let Some(stretch) = self.stretch.as_mut() else {
            return match self.resampler.as_mut() {
                Some(rate) => rate.finish(out),
                None => Ok(()),
            };
        };
        let mut tail = Vec::new();
        if let Some(rate) = self.resampler.as_mut() {
            rate.finish(&mut tail)?;
        }
        stretch.process(&tail, out)?;
        stretch.finish(out)
    }
}

fn mix(row: &[f32], frame: &[f32]) -> f32 {
    row.iter().zip(frame).map(|(gain, sample)| gain * sample).sum()
}

struct Rate {
    inner: FftFixedIn<f32>,
    in_rate: u64,
    out_rate: u64,
    channels: usize,
    input: Vec<Vec<f32>>,
    consumed: usize,
    output: Vec<Vec<f32>>,
    /// Начальные кадры выхода, которые надо выбросить: это задержка фильтра.
    skip: usize,
    fed: u64,
    emitted: u64,
}

impl Rate {
    fn new(in_rate: u32, out_rate: u32, channels: usize) -> Result<Self, TrackError> {
        let inner = FftFixedIn::<f32>::new(
            in_rate as usize,
            out_rate as usize,
            RESAMPLER_CHUNK,
            2,
            channels,
        )
        .map_err(|error| TrackError::Unsupported(format!("ресемплер: {error}")))?;
        let output = inner.output_buffer_allocate(true);
        let skip = inner.output_delay();
        Ok(Self {
            inner,
            in_rate: u64::from(in_rate),
            out_rate: u64::from(out_rate),
            channels,
            input: vec![Vec::new(); channels],
            consumed: 0,
            output,
            skip,
            fed: 0,
            emitted: 0,
        })
    }

    fn run(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        loop {
            let needed = self.inner.input_frames_next();
            if self.input[0].len() - self.consumed < needed {
                break;
            }
            let chunk: Vec<&[f32]> = self
                .input
                .iter()
                .map(|channel| &channel[self.consumed..self.consumed + needed])
                .collect();
            let (used, produced) = self
                .inner
                .process_into_buffer(&chunk, &mut self.output, None)
                .map_err(resample_error)?;
            self.consumed += used;
            self.emit(produced, u64::MAX, out);
        }
        if self.consumed > 0 {
            for channel in &mut self.input {
                channel.drain(..self.consumed);
            }
            self.consumed = 0;
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        let expected = (self.fed * self.out_rate).div_ceil(self.in_rate);

        if !self.input[0].is_empty() {
            let rest: Vec<&[f32]> = self.input.iter().map(|channel| &channel[..]).collect();
            let (_, produced) = self
                .inner
                .process_partial_into_buffer(Some(&rest[..]), &mut self.output, None)
                .map_err(resample_error)?;
            self.emit(produced, expected.saturating_sub(self.emitted), out);
            for channel in &mut self.input {
                channel.clear();
            }
        }
        // Фильтр отдаёт хвост только в ответ на новый вход, поэтому дожимаем его тишиной.
        for _ in 0..64 {
            if self.emitted >= expected {
                break;
            }
            let (_, produced) = self
                .inner
                .process_partial_into_buffer(None::<&[&[f32]]>, &mut self.output, None)
                .map_err(resample_error)?;
            if produced == 0 {
                break;
            }
            self.emit(produced, expected - self.emitted, out);
        }
        Ok(())
    }

    fn emit(&mut self, produced: usize, limit: u64, out: &mut Vec<f32>) {
        let skip = self.skip.min(produced);
        self.skip -= skip;
        let frames = ((produced - skip) as u64).min(limit) as usize;
        out.reserve(frames * self.channels);
        for frame in skip..skip + frames {
            for channel in &self.output {
                out.push(channel[frame]);
            }
        }
        self.emitted += frames as u64;
    }
}

fn resample_error(error: rubato::ResampleError) -> TrackError {
    TrackError::Unsupported(format!("ресемплер: {error}"))
}

/// Матрица сведения. Каналов столько же — как есть, иначе в стерео (моно) без клиппинга.
fn mix_matrix(layout: Channels, out_channels: usize) -> Vec<f32> {
    let in_channels = layout.count();
    let mut matrix = vec![0.0f32; out_channels * in_channels];
    if in_channels == out_channels {
        for channel in 0..in_channels {
            matrix[channel * in_channels + channel] = 1.0;
        }
        return matrix;
    }

    let mut left = vec![0.0f32; in_channels];
    let mut right = vec![0.0f32; in_channels];
    match in_channels {
        1 => {
            left[0] = 1.0;
            right[0] = 1.0;
        }
        2 => {
            left[0] = 1.0;
            right[1] = 1.0;
        }
        _ => {
            // Каналы в буфере идут в порядке битов маски раскладки.
            let present = (0..32)
                .map(|bit| Channels::from_bits_truncate(1 << bit))
                .filter(|flag| !flag.is_empty() && layout.contains(*flag));
            for (index, flag) in present.enumerate().take(in_channels) {
                let (to_left, to_right) = if flag == Channels::FRONT_LEFT {
                    (1.0, 0.0)
                } else if flag == Channels::FRONT_RIGHT {
                    (0.0, 1.0)
                } else if flag == Channels::FRONT_CENTRE {
                    (HALF_POWER, HALF_POWER)
                } else if flag == Channels::LFE1 {
                    (0.0, 0.0)
                } else if flag == Channels::REAR_LEFT || flag == Channels::SIDE_LEFT {
                    (HALF_POWER, 0.0)
                } else if flag == Channels::REAR_RIGHT || flag == Channels::SIDE_RIGHT {
                    (0.0, HALF_POWER)
                } else {
                    (0.5, 0.5)
                };
                left[index] = to_left;
                right[index] = to_right;
            }
            let peak = left
                .iter()
                .sum::<f32>()
                .max(right.iter().sum::<f32>())
                .max(1.0);
            for gain in left.iter_mut().chain(right.iter_mut()) {
                *gain /= peak;
            }
        }
    }

    if out_channels == 1 {
        for (cell, (l, r)) in matrix.iter_mut().zip(left.iter().zip(&right)) {
            *cell = (l + r) / 2.0;
        }
    } else {
        matrix[..in_channels].copy_from_slice(&left);
        matrix[in_channels..2 * in_channels].copy_from_slice(&right);
    }
    matrix
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO: Channels = Channels::FRONT_LEFT.union(Channels::FRONT_RIGHT);

    fn sine(frames: usize, rate: u32, channels: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let value = (frame as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                std::iter::repeat_n(value, channels)
            })
            .collect()
    }

    #[test]
    fn same_format_passes_through_untouched() {
        let input = sine(1000, 44_100, 2);
        let mut converter = Converter::new(44_100, STEREO, 44_100, 2, Speed::NORMAL).unwrap();
        let mut out = Vec::new();
        converter.process(&input, &mut out).unwrap();
        converter.flush(&mut out).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn mono_goes_to_both_stereo_channels() {
        let mut converter = Converter::new(48_000, Channels::FRONT_LEFT, 48_000, 2, Speed::NORMAL).unwrap();
        let mut out = Vec::new();
        converter.process(&[0.25, -0.5], &mut out).unwrap();
        assert_eq!(out, [0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn surround_downmix_does_not_clip() {
        let layout = Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT;
        let mut converter = Converter::new(48_000, layout, 48_000, 2, Speed::NORMAL).unwrap();
        let mut out = Vec::new();
        converter.process(&[1.0; 6], &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|sample| *sample > 0.9 && *sample <= 1.0001));
    }

    #[test]
    fn resampled_length_matches_input_exactly() {
        for (in_rate, out_rate) in [(44_100, 48_000), (96_000, 44_100), (22_050, 48_000)] {
            let frames = 30_011;
            let input = sine(frames, in_rate, 2);
            let mut converter = Converter::new(in_rate, STEREO, out_rate, 2, Speed::NORMAL).unwrap();
            let mut out = Vec::new();
            // Кормим неровными кусками, как это делает декодер.
            for piece in input.chunks(2 * 1777) {
                converter.process(piece, &mut out).unwrap();
            }
            converter.flush(&mut out).unwrap();

            let expected = (frames as u64 * u64::from(out_rate)).div_ceil(u64::from(in_rate));
            assert_eq!(out.len() as u64, expected * 2, "{in_rate} -> {out_rate}");
        }
    }

    #[test]
    fn varispeed_changes_length_and_pitch_together() {
        let speed = Speed { factor: 0.5, keep_pitch: false };
        let mut converter = Converter::new(44_100, STEREO, 48_000, 2, speed).unwrap();
        assert!((converter.speed() - 0.5).abs() < 1e-9);
        let mut out = Vec::new();
        converter.process(&sine(44_100, 44_100, 2), &mut out).unwrap();
        converter.flush(&mut out).unwrap();
        // Секунда записи звучит две секунды, а 440 Гц становятся 220.
        assert_eq!(out.len(), 96_000 * 2);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let crossings = left[24_000..72_000].windows(2).filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0).count();
        assert!((219..=221).contains(&crossings), "{crossings} периодов за секунду");
    }

    #[test]
    fn stretched_conversion_keeps_exact_length() {
        let speed = Speed { factor: 0.25, keep_pitch: true };
        let mut converter = Converter::new(44_100, STEREO, 48_000, 2, speed).unwrap();
        let mut out = Vec::new();
        for piece in sine(44_100, 44_100, 2).chunks(2 * 1500) {
            converter.process(piece, &mut out).unwrap();
        }
        converter.flush(&mut out).unwrap();
        assert_eq!(out.len(), 4 * 48_000 * 2);
    }

    #[test]
    fn resampler_keeps_the_signal_aligned() {
        // Синус после ресемплинга совпадает с посчитанным сразу: задержка убрана, фаза на месте.
        let mut converter = Converter::new(44_100, STEREO, 48_000, 2, Speed::NORMAL).unwrap();
        let mut out = Vec::new();
        converter.process(&sine(44_100, 44_100, 2), &mut out).unwrap();
        converter.flush(&mut out).unwrap();

        let reference = sine(48_000, 48_000, 2);
        let worst = out[2000..90_000]
            .iter()
            .zip(&reference[2000..90_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.01, "расхождение {worst}");
    }
}
