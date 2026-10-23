//! Характер звука для сцен: громкость, атаки, яркость (>3 кГц) и шумность по окнам ~23 мс.
//! Итог — [`FEEL_PER_SECOND`] значений в секунду и атаки. «Спокойно» или «агрессивно» решает приложение.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use realfft::RealFftPlanner;

use crate::source::{OpenOptions, TrackDecoder, TrackError, guard};

/// Сколько значений характера на секунду записи.
pub const FEEL_PER_SECOND: u32 = 10;

/// Граница «высоких» частот, Гц.
const BRIGHT_FROM: f32 = 3000.0;
/// Шумность считается до этой частоты: выше у сжатых файлов пусто.
const NOISE_UNTIL: f32 = 11000.0;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feel {
    /// Громкость, дБ полной шкалы (−90…0).
    pub loud: Vec<f32>,
    /// Сила атак: средний прирост логарифма спектра от окна к окну.
    pub flux: Vec<f32>,
    /// Доля энергии выше 3 кГц, 0…1.
    pub bright: Vec<f32>,
    /// Шумность спектра, 0 (чистый тон) … 1 (белый шум).
    pub noise: Vec<f32>,
    /// Атаки: секунды от начала, по возрастанию.
    pub beats: Vec<f32>,
}

impl Feel {
    /// Длина записи в секундах по числу значений.
    pub fn seconds(&self) -> f32 {
        self.loud.len() as f32 / FEEL_PER_SECOND as f32
    }
}

/// Окно анализа в отсчётах: степень двойки, около 23 мс.
fn window_for(rate: u32) -> usize {
    ((rate as f32 * 0.023) as usize).next_power_of_two().clamp(256, 16384)
}

/// Одно окно: время середины, громкость (мощность), поток, яркость, шумность.
struct Hop {
    at: f32,
    power: f32,
    flux: f32,
    bright: f32,
    noise: f32,
}

/// Читает файл и считает характер. `stop` прерывает работу (тогда `None`).
pub fn feel(path: &Path, stop: &AtomicBool) -> Result<Option<Feel>, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions::default())?;
        let mut planner = RealFftPlanner::<f32>::new();
        let mut size = 0usize;
        let mut rate = 0u32;
        let mut mono: Vec<f32> = Vec::new();
        let mut hops: Vec<Hop> = Vec::new();
        let mut consumed = 0usize; // отсчётов моно уже сдвинуто из буфера
        let mut state: Option<Analysis> = None;
        while let Some(chunk) = decoder.next_chunk()? {
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let channels = chunk.channels.count().max(1);
            if state.is_none() || chunk.rate != rate {
                rate = chunk.rate.max(1);
                size = window_for(rate);
                state = Some(Analysis::new(&mut planner, size, rate));
            }
            let scale = 1.0 / channels as f32;
            mono.extend(chunk.samples.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() * scale));
            let step = size / 2;
            let analysis = state.as_mut().expect("создан выше");
            let mut start = 0;
            while start + size <= mono.len() {
                let at = (consumed + start + size / 2) as f32 / rate as f32;
                hops.push(analysis.hop(&mono[start..start + size], at));
                start += step;
            }
            mono.drain(..start);
            consumed += start;
        }
        Ok(Some(summarize(&hops)))
    })
}

/// Окно Ханна, буферы БПФ и прошлый спектр.
struct Analysis {
    fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<realfft::num_complex::Complex<f32>>,
    previous: Vec<f32>,
    bright_bin: usize,
    noise_bin: usize,
}

impl Analysis {
    fn new(planner: &mut RealFftPlanner<f32>, size: usize, rate: u32) -> Self {
        let fft = planner.plan_fft_forward(size);
        let window = (0..size).map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / size as f32).cos()).collect();
        let bins = size / 2 + 1;
        let hz = rate as f32 / size as f32;
        Self {
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            fft,
            window,
            previous: vec![0.0; bins],
            bright_bin: ((BRIGHT_FROM / hz) as usize).min(bins - 1),
            noise_bin: ((NOISE_UNTIL / hz) as usize).clamp(4, bins - 1),
        }
    }

    fn hop(&mut self, samples: &[f32], at: f32) -> Hop {
        let mut power = 0.0f32;
        for ((slot, sample), weight) in self.input.iter_mut().zip(samples).zip(&self.window) {
            *slot = sample * weight;
            power += sample * sample;
        }
        power /= samples.len() as f32;
        let _ = self.fft.process(&mut self.input, &mut self.output);
        let (mut total, mut high, mut flux) = (0.0f32, 0.0f32, 0.0f32);
        let (mut log_sum, mut linear_sum, mut counted) = (0.0f64, 0.0f64, 0usize);
        let bins = self.output.len();
        for bin in 0..bins {
            let energy = self.output[bin].norm_sqr();
            total += energy;
            if bin >= self.bright_bin {
                high += energy;
            }
            // Логарифм — поток реагирует на появление звука, а не силу.
            // Максимум по соседям (как SuperFlux) — дрожание тона не атака.
            let level = (1.0 + 100.0 * energy.sqrt()).ln();
            let before = self.previous[bin.saturating_sub(1)..(bin + 2).min(bins)].iter().copied().fold(0.0f32, f32::max);
            flux += (level - before).max(0.0);
            if (2..self.noise_bin).contains(&bin) {
                log_sum += f64::from(energy + 1e-12).ln();
                linear_sum += f64::from(energy + 1e-12);
                counted += 1;
            }
        }
        for (previous, value) in self.previous.iter_mut().zip(&self.output) {
            *previous = (1.0 + 100.0 * value.norm_sqr().sqrt()).ln();
        }
        let bins = bins.max(1) as f32;
        let noise = if counted > 0 && linear_sum > 0.0 {
            ((log_sum / counted as f64).exp() / (linear_sum / counted as f64)) as f32
        } else {
            0.0
        };
        Hop {
            at,
            power,
            flux: flux / bins,
            bright: if total > 0.0 { high / total } else { 0.0 },
            noise: noise.clamp(0.0, 1.0),
        }
    }
}

/// Окна → значения по 100 мс и атаки.
fn summarize(hops: &[Hop]) -> Feel {
    let mut feel = Feel::default();
    let Some(last) = hops.last() else {
        return feel;
    };
    let frames = (last.at * FEEL_PER_SECOND as f32).ceil() as usize + 1;
    let mut sums = vec![(0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0u32); frames];
    for hop in hops {
        let frame = ((hop.at * FEEL_PER_SECOND as f32) as usize).min(frames - 1);
        let slot = &mut sums[frame];
        slot.0 += hop.power;
        slot.1 += hop.flux;
        // Яркость и шумность тихих окон не значат ничего: вес — громкость.
        slot.2 += hop.bright * hop.power;
        slot.3 += hop.noise * hop.power;
        slot.4 += hop.power;
        slot.5 += 1;
    }
    for (power, flux, bright, noise, weight, count) in sums {
        let count = count.max(1) as f32;
        let mean = power / count;
        feel.loud.push(if mean > 0.0 { (10.0 * mean.log10()).max(-90.0) } else { -90.0 });
        feel.flux.push(flux / count);
        feel.bright.push(if weight > 0.0 { bright / weight } else { 0.0 });
        feel.noise.push(if weight > 0.0 { noise / weight } else { 0.0 });
    }
    feel.beats = onsets(hops);
    feel
}

/// Пики потока выше скользящего среднего, не чаще раза в 100 мс.
fn onsets(hops: &[Hop]) -> Vec<f32> {
    let flux: Vec<f32> = hops.iter().map(|hop| hop.flux).collect();
    let mut beats = Vec::new();
    if flux.len() < 3 {
        return beats;
    }
    let step = (hops[1].at - hops[0].at).max(1e-4);
    let around = ((0.4 / step) as usize).max(2);
    let peak = ((0.05 / step) as usize).max(1);
    // Скользящее среднее по ±0,4 с через накопленные суммы.
    let mut prefix = vec![0.0f64; flux.len() + 1];
    for (index, value) in flux.iter().enumerate() {
        prefix[index + 1] = prefix[index] + f64::from(*value);
    }
    let loudest = flux.iter().copied().fold(0.0f32, f32::max);
    let floor = loudest * 0.1;
    let mut last = f32::MIN;
    for index in 0..flux.len() {
        let from = index.saturating_sub(around);
        let to = (index + around + 1).min(flux.len());
        let mean = ((prefix[to] - prefix[from]) / (to - from) as f64) as f32;
        let value = flux[index];
        if value < floor || value < mean * 1.35 {
            continue;
        }
        let left = index.saturating_sub(peak);
        let right = (index + peak + 1).min(flux.len());
        if flux[left..right].iter().any(|other| *other > value) {
            continue;
        }
        let at = hops[index].at;
        if at - last >= 0.1 {
            beats.push(at);
            last = at;
        }
    }
    beats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hop_series(rate: u32, samples: &[f32]) -> Vec<Hop> {
        let size = window_for(rate);
        let mut planner = RealFftPlanner::<f32>::new();
        let mut analysis = Analysis::new(&mut planner, size, rate);
        (0..)
            .map(|index| index * size / 2)
            .take_while(|start| start + size <= samples.len())
            .map(|start| analysis.hop(&samples[start..start + size], (start + size / 2) as f32 / rate as f32))
            .collect()
    }

    #[test]
    fn clicks_are_beats_and_noise_is_noisier_than_a_tone() {
        let rate = 44100;
        // Тишина с щелчками каждые полсекунды, 4 секунды.
        let mut clicks = vec![0.0f32; rate as usize * 4];
        for beat in 0..8 {
            let at = beat * rate as usize / 2 + 1000;
            for (offset, sample) in clicks[at..at + 200].iter_mut().enumerate() {
                *sample = if offset % 2 == 0 { 0.8 } else { -0.8 };
            }
        }
        let feel = summarize(&hop_series(rate, &clicks));
        assert_eq!(feel.beats.len(), 8, "{:?}", feel.beats);
        for (index, beat) in feel.beats.iter().enumerate() {
            assert!((beat - (index as f32 * 0.5 + 0.023)).abs() < 0.05, "{beat}");
        }
        assert!((feel.seconds() - 4.0).abs() < 0.2);

        // Тон против шума: у шума шумность и яркость выше, тон −3 дБ.
        let tone: Vec<f32> = (0..rate).map(|n| (std::f32::consts::TAU * 440.0 * n as f32 / rate as f32).sin()).collect();
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..rate)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect();
        let tone = summarize(&hop_series(rate, &tone));
        let noise = summarize(&hop_series(rate, &noise));
        let middle = |values: &[f32]| values[values.len() / 2];
        assert!((middle(&tone.loud) + 3.0).abs() < 0.5, "{}", middle(&tone.loud));
        assert!(middle(&noise.noise) > 0.4 && middle(&tone.noise) < 0.05, "{} {}", middle(&noise.noise), middle(&tone.noise));
        assert!(middle(&noise.bright) > 0.5 && middle(&tone.bright) < 0.01);
        assert!(tone.beats.len() <= 1, "у ровного тона нет атак: {:?}", tone.beats);
    }
}

#[cfg(test)]
mod report {
    /// `NYANOTO_FEEL_FILES=список.txt cargo test --release -p nyanoto-core feel_report -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn feel_report() {
        let Some(list) = std::env::var_os("NYANOTO_FEEL_FILES") else {
            return;
        };
        let percentile = |values: &[f32], share: f32| {
            let mut sorted: Vec<f32> = values.to_vec();
            sorted.sort_by(f32::total_cmp);
            sorted.get(((sorted.len() as f32 - 1.0) * share) as usize).copied().unwrap_or(0.0)
        };
        for path in std::fs::read_to_string(list).unwrap().lines() {
            let started = std::time::Instant::now();
            let feel = super::feel(std::path::Path::new(path), &std::sync::atomic::AtomicBool::new(false)).unwrap().unwrap();
            let name = path.rsplit('/').next().unwrap();
            println!(
                "{name:.48}: {:.0} с за {:.2} с | гр p25 {:.1} p50 {:.1} p90 {:.1} | поток p50 {:.3} p90 {:.3} | ярк {:.3} | шум {:.3} | атак/с {:.2}",
                feel.seconds(),
                started.elapsed().as_secs_f32(),
                percentile(&feel.loud, 0.25),
                percentile(&feel.loud, 0.5),
                percentile(&feel.loud, 0.9),
                percentile(&feel.flux, 0.5),
                percentile(&feel.flux, 0.9),
                percentile(&feel.bright, 0.5),
                percentile(&feel.noise, 0.5),
                feel.beats.len() as f32 / feel.seconds().max(1.0),
            );
            // По 10 секунд: громкость, поток, атаки.
            let mut line = String::from("   ");
            for start in (0..feel.loud.len()).step_by(100) {
                let end = (start + 100).min(feel.loud.len());
                let loud = feel.loud[start..end].iter().sum::<f32>() / (end - start) as f32;
                let flux = feel.flux[start..end].iter().sum::<f32>() / (end - start) as f32;
                let beats = feel.beats.iter().filter(|beat| (start as f32 / 10.0..end as f32 / 10.0).contains(*beat)).count();
                line += &format!(" {loud:.0}/{flux:.2}/{beats}");
            }
            println!("{line}");
        }
    }
}
