//! Скорость без смены высоты: Signalsmith Stretch (MIT, `vendor/signalsmith`).
//! Выход выровнен как в `exact()`: из N кадров ровно round(N / скорость), позиция считается без поправок.

use std::ffi::{c_int, c_void};

use crate::source::TrackError;

unsafe extern "C" {
    fn nyanoto_stretch_new(channels: c_int, sample_rate: f32) -> *mut c_void;
    fn nyanoto_stretch_free(handle: *mut c_void);
    fn nyanoto_stretch_input_latency(handle: *mut c_void) -> c_int;
    fn nyanoto_stretch_output_latency(handle: *mut c_void) -> c_int;
    fn nyanoto_stretch_seek(handle: *mut c_void, input: *mut f32, frames: c_int, rate: f64) -> c_int;
    fn nyanoto_stretch_process(
        handle: *mut c_void,
        input: *mut f32,
        input_frames: c_int,
        output: *mut f32,
        output_frames: c_int,
    ) -> c_int;
    fn nyanoto_stretch_flush(handle: *mut c_void, output: *mut f32, output_frames: c_int) -> c_int;
}

pub(crate) struct Stretch {
    handle: *mut c_void,
    channels: usize,
    speed: f64,
    input_latency: usize,
    output_latency: usize,
    /// Начало входа, пока его не набралось на разгон.
    priming: Option<Vec<f32>>,
    /// Сколько кадров входа получено всего.
    received: u64,
    /// Сколько кадров отдано библиотеке после разгона, вместе с тишиной в конце.
    fed: u64,
    /// Сколько кадров выхода у неё запрошено.
    produced: u64,
    /// Сколько начальных кадров выхода ещё выбросить: это задержка библиотеки.
    skip: usize,
    scratch: Vec<f32>,
}

// Объект C++ не привязан к потоку и используется только через `&mut self`.
unsafe impl Send for Stretch {}

impl Stretch {
    /// `speed` — во сколько раз быстрее исходного: 0.5 — вдвое медленнее.
    pub fn new(channels: usize, rate: u32, speed: f64) -> Result<Self, TrackError> {
        if channels == 0 || !(speed.is_finite() && speed > 0.0) {
            return Err(TrackError::Unsupported(format!("растяжение: {channels} каналов, скорость {speed}")));
        }
        // SAFETY: обёртка не бросает исключений, ноль проверяем, освобождаем в `drop`.
        let handle = unsafe { nyanoto_stretch_new(channels as c_int, rate as f32) };
        if handle.is_null() {
            return Err(TrackError::Unsupported("растяжение: не удалось создать".into()));
        }
        let (input_latency, output_latency) = unsafe {
            (
                nyanoto_stretch_input_latency(handle).max(0) as usize,
                nyanoto_stretch_output_latency(handle).max(0) as usize,
            )
        };
        Ok(Self {
            handle,
            channels,
            speed,
            input_latency,
            output_latency,
            priming: Some(Vec::new()),
            received: 0,
            fed: 0,
            produced: 0,
            skip: output_latency,
            scratch: Vec::new(),
        })
    }

    /// Растягивает чередующиеся кадры `input` и дописывает результат в `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), TrackError> {
        self.received += (input.len() / self.channels) as u64;
        let Some(mut priming) = self.priming.take() else {
            return self.feed(input, out);
        };
        priming.extend_from_slice(input);
        if priming.len() / self.channels < self.input_latency {
            self.priming = Some(priming);
            return Ok(());
        }
        let split = self.input_latency * self.channels;
        self.seek(&mut priming[..split])?;
        self.feed(&priming[split..], out)
    }

    /// Дописывает хвост. После этого выход по длине ровно round(N / скорость).
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        if let Some(mut priming) = self.priming.take() {
            // Трек короче разгона: весь он уходит на разгон.
            self.seek(&mut priming)?;
        }
        // Вход сдвинут на разгон: добиваем его тишиной до полной длины.
        let pad = self.received.saturating_sub(self.fed) as usize;
        if pad > 0 {
            self.feed(&vec![0.0; pad * self.channels], out)?;
        }
        let mut tail = vec![0.0f32; self.output_latency * self.channels];
        // SAFETY: буфер вмещает `output_latency` кадров по `channels` отсчётов.
        check(unsafe { nyanoto_stretch_flush(self.handle, tail.as_mut_ptr(), self.output_latency as c_int) })?;
        self.emit(&tail, out);
        Ok(())
    }

    fn seek(&mut self, input: &mut [f32]) -> Result<(), TrackError> {
        let frames = input.len() / self.channels;
        if frames == 0 {
            return Ok(());
        }
        // SAFETY: во входе ровно `frames` кадров по `channels` отсчётов.
        check(unsafe { nyanoto_stretch_seek(self.handle, input.as_mut_ptr(), frames as c_int, self.speed) })
    }

    fn feed(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), TrackError> {
        let frames = input.len() / self.channels;
        if frames == 0 {
            return Ok(());
        }
        self.fed += frames as u64;
        let target = (self.fed as f64 / self.speed).round() as u64;
        let wanted = (target - self.produced) as usize;
        self.produced = target;
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.resize(wanted * self.channels, 0.0);
        // SAFETY: читает `frames` кадров, пишет `wanted` в буфер того же размера.
        let done = check(unsafe {
            nyanoto_stretch_process(
                self.handle,
                input.as_ptr().cast_mut(),
                frames as c_int,
                scratch.as_mut_ptr(),
                wanted as c_int,
            )
        });
        if done.is_ok() {
            self.emit(&scratch, out);
        }
        self.scratch = scratch;
        done
    }

    fn emit(&mut self, frames: &[f32], out: &mut Vec<f32>) {
        let skip = self.skip.min(frames.len() / self.channels);
        self.skip -= skip;
        out.extend_from_slice(&frames[skip * self.channels..]);
    }
}

impl Drop for Stretch {
    fn drop(&mut self) {
        // SAFETY: указатель получен из `nyanoto_stretch_new` и освобождается один раз.
        unsafe { nyanoto_stretch_free(self.handle) }
    }
}

fn check(code: c_int) -> Result<(), TrackError> {
    if code == 0 {
        return Err(TrackError::Unsupported("растяжение: сбой внутри библиотеки".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(frames: usize, hz: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| {
                let value = (frame as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.5;
                [value, value]
            })
            .collect()
    }

    /// Частота по числу переходов через ноль в середине сигнала.
    fn frequency(stereo: &[f32]) -> f32 {
        let left: Vec<f32> = stereo.iter().step_by(2).copied().collect();
        let middle = &left[left.len() / 4..left.len() * 3 / 4];
        let crossings = middle.windows(2).filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0).count();
        crossings as f32 * RATE as f32 / middle.len() as f32
    }

    #[test]
    fn length_follows_speed_and_pitch_stays() {
        for speed in [0.05, 0.25, 0.5, 1.5, 3.0] {
            let frames = RATE as usize * 2;
            let input = tone(frames, 440.0);
            let mut stretch = Stretch::new(2, RATE, speed).unwrap();
            let mut out = Vec::new();
            // Кусками неровной длины, как от декодера.
            for piece in input.chunks(2 * 1777) {
                stretch.process(piece, &mut out).unwrap();
            }
            stretch.finish(&mut out).unwrap();
            let expected = (frames as f64 / speed).round() as usize;
            assert_eq!(out.len() / 2, expected, "скорость {speed}");
            let hz = frequency(&out);
            assert!((hz - 440.0).abs() < 8.0, "скорость {speed}: {hz} Гц вместо 440");
        }
    }

    #[test]
    fn output_stays_aligned_with_input() {
        // Щелчок на 1-й секунде при 0.5 — на 2-й секунде выхода.
        let frames = RATE as usize * 3;
        let mut input = vec![0.0f32; frames * 2];
        for frame in RATE as usize..RATE as usize + 480 {
            let value = (frame as f32 * 1000.0 * std::f32::consts::TAU / RATE as f32).sin();
            input[frame * 2] = value;
            input[frame * 2 + 1] = value;
        }
        let mut stretch = Stretch::new(2, RATE, 0.5).unwrap();
        let mut out = Vec::new();
        for piece in input.chunks(2 * 4096) {
            stretch.process(piece, &mut out).unwrap();
        }
        stretch.finish(&mut out).unwrap();
        let loudest = out
            .iter()
            .step_by(2)
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(frame, _)| frame)
            .unwrap();
        let expected = 2 * RATE as usize + 480;
        let error_ms = (loudest as f64 - expected as f64).abs() / f64::from(RATE) * 1000.0;
        assert!(error_ms < 30.0, "щелчок съехал на {error_ms:.1} мс");
    }

    #[test]
    fn short_input_still_comes_out() {
        let mut stretch = Stretch::new(2, RATE, 0.25).unwrap();
        let mut out = Vec::new();
        stretch.process(&tone(100, 440.0), &mut out).unwrap();
        stretch.finish(&mut out).unwrap();
        assert_eq!(out.len() / 2, 400);
    }
}
