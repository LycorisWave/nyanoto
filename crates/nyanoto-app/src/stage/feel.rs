//! Настроение по звуку: «напряжение» 0…1 на каждые 100 мс и атаки.
//! Лучше всего отличают громкость, доля верха и шумность. Плотность атак — только для вздрагиваний.

use nyanoto_core::{FEEL_PER_SECOND, Feel};

/// Напряжение песни: кривая и общее значение.
#[derive(Debug, Clone, Default)]
pub struct Mood {
    /// Напряжение момента, 0…1, по 100 мс, сглаженное.
    curve: Vec<f32>,
    /// Громкость момента 0…1 по 100 мс: быстро вверх, плавно вниз — сцены «дышат».
    level: Vec<f32>,
    /// Атаки, секунды.
    beats: Vec<f32>,
    /// Темп: длительность доли (с) и момент одной из долей.
    tempo: Option<(f32, f32)>,
    /// Доли, подтянутые к атакам: одна сетка на песню за три минуты уезжает на секунду.
    grid: Vec<f32>,
    /// Интеграл напряжения: скорость меняется, а положение непрерывно.
    travel: Vec<f32>,
    /// Напряжение песни в целом.
    pub song: f32,
}

/// Порог «спокойно / средне» и «средне / агрессивно».
pub const CALM: f32 = 0.4;
pub const FIERCE: f32 = 0.62;

fn share(value: f32, from: f32, to: f32) -> f32 {
    ((value - from) / (to - from)).clamp(0.0, 1.0)
}

fn percentile(values: &[f32], at: f32) -> f32 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted.get(((sorted.len().max(1) - 1) as f32 * at) as usize).copied().unwrap_or(0.0)
}

impl Mood {
    pub fn from_feel(feel: &Feel) -> Self {
        let count = feel.loud.len();
        if count == 0 {
            let tempo = tempo(&feel.beats);
            return Self { song: 0.5, beats: feel.beats.clone(), tempo, grid: track(&feel.beats, tempo), ..Self::default() };
        }
        // Звучание: громкость (−30…−8 дБ), яркость и перегруз.
        let timbre: Vec<f32> = (0..count)
            .map(|index| {
                0.4 * share(feel.loud[index], -30.0, -8.0)
                    + 0.3 * share(feel.bright[index], 0.0, 0.1)
                    + 0.3 * share(feel.noise[index], 0.0, 0.1)
            })
            .collect();
        // Внутри песни: где громче и плотнее, чем обычно для неё самой.
        let audible: Vec<f32> = feel.loud.iter().copied().filter(|loud| *loud > -45.0).collect();
        let (quiet, loud) = (percentile(&audible, 0.1), percentile(&audible, 0.9).max(percentile(&audible, 0.1) + 1.0));
        let busy = percentile(&feel.flux, 0.9).max(1e-6);
        let raw: Vec<f32> = (0..count)
            .map(|index| {
                let relative = share(feel.loud[index], quiet, loud);
                let activity = (feel.flux[index] / busy).clamp(0.0, 1.0);
                // Тишина — всегда спокойно.
                let silence = share(feel.loud[index], -50.0, -35.0);
                (0.55 * timbre[index] + 0.25 * activity + 0.2 * relative) * silence
            })
            .collect();
        // Подъём 0,5 с, спад 2 с — удар виден сразу, настроение не скачет.
        let window = |seconds: f32| ((seconds * FEEL_PER_SECOND as f32) as usize).max(1);
        let average = |values: &[f32], half: usize| -> Vec<f32> {
            let mut prefix = vec![0.0f64; values.len() + 1];
            for (index, value) in values.iter().enumerate() {
                prefix[index + 1] = prefix[index] + f64::from(*value);
            }
            (0..values.len())
                .map(|index| {
                    let from = index.saturating_sub(half);
                    let to = (index + half + 1).min(values.len());
                    ((prefix[to] - prefix[from]) / (to - from) as f64) as f32
                })
                .collect()
        };
        let fast = average(&raw, window(0.25));
        let slow = average(&raw, window(1.0));
        let curve: Vec<f32> = fast.iter().zip(&slow).map(|(fast, slow)| fast.max(*slow).clamp(0.0, 1.0)).collect();
        let heard: Vec<f32> = timbre.iter().zip(&feel.loud).filter(|(_, loud)| **loud > -40.0).map(|(value, _)| *value).collect();
        let song = if heard.is_empty() { 0.3 } else { percentile(&heard, 0.6) };
        // Огибающая между p10 и p95, спад ~0,4 с.
        let top = percentile(&audible, 0.95).max(quiet + 1.0);
        let mut level = Vec::with_capacity(count);
        let mut held = 0.0f32;
        for loud in &feel.loud {
            let value = share(*loud, quiet, top);
            held = if value > held { value } else { held + (value - held) * 0.25 };
            level.push(held);
        }
        let step = 1.0 / FEEL_PER_SECOND as f32;
        let mut travel = Vec::with_capacity(count);
        let mut sum = 0.0f32;
        for index in 0..count {
            travel.push(sum);
            sum += (curve[index] + curve[(index + 1).min(count - 1)]) / 2.0 * step;
        }
        let tempo = tempo(&feel.beats);
        Self { curve, level, beats: feel.beats.clone(), tempo, grid: track(&feel.beats, tempo), travel, song }
    }

    /// Напряжение в момент `t` (секунды трека).
    pub fn at(&self, t: f32) -> f32 {
        if self.curve.is_empty() {
            return self.song;
        }
        let position = (t * FEEL_PER_SECOND as f32).max(0.0);
        let index = position as usize;
        let next = (index + 1).min(self.curve.len() - 1);
        let index = index.min(self.curve.len() - 1);
        let share = position.fract();
        self.curve[index] * (1.0 - share) + self.curve[next] * share
    }

    /// Накопленное напряжение к `t` — растёт быстрее в громком, не скачет.
    pub fn travel(&self, t: f32) -> f32 {
        let t = t.max(0.0);
        if self.travel.is_empty() {
            return self.song * t;
        }
        let step = 1.0 / FEEL_PER_SECOND as f32;
        let position = t * FEEL_PER_SECOND as f32;
        let index = position as usize;
        if index >= self.travel.len() - 1 {
            let last = self.travel.len() - 1;
            return self.travel[last] + self.curve[last] * (t - last as f32 * step);
        }
        let share = position.fract();
        // Внутри отсчёта напряжение меняется линейно — интеграл квадратичный.
        let (from, to) = (self.curve[index], self.curve[index + 1]);
        self.travel[index] + step * (from * share + (to - from) * share * share / 2.0)
    }

    /// Громкость момента внутри песни (0…1); без разбора — середина.
    pub fn level(&self, t: f32) -> f32 {
        if self.level.is_empty() {
            return 0.5;
        }
        let position = (t * FEEL_PER_SECOND as f32).max(0.0);
        let index = (position as usize).min(self.level.len() - 1);
        let next = (index + 1).min(self.level.len() - 1);
        self.level[index] + (self.level[next] - self.level[index]) * position.fract()
    }

    /// Фаза внутри доли 0…1. Без темпа — ровные полсекунды.
    pub fn phase(&self, t: f32) -> f32 {
        self.count(t).rem_euclid(1.0)
    }

    /// Номер доли от начала (с долями внутри — дробный): для шагов «в такт».
    pub fn count(&self, t: f32) -> f32 {
        let (period, anchor) = self.tempo.unwrap_or((0.5, 0.0));
        let grid = &self.grid;
        if grid.len() < 2 {
            return (t - anchor) / period;
        }
        let next = grid.partition_point(|beat| *beat <= t);
        if next == 0 {
            return (t - grid[0]) / period;
        }
        if next == grid.len() {
            return (grid.len() - 1) as f32 + (t - grid[grid.len() - 1]) / period;
        }
        let (from, to) = (grid[next - 1], grid[next]);
        (next - 1) as f32 + (t - from) / (to - from).max(1e-3)
    }

    /// Длительность доли, с.
    #[cfg(test)]
    pub fn period(&self) -> f32 {
        self.tempo.map_or(0.5, |(period, _)| period)
    }

    /// Среднее напряжение отрезка.
    pub fn mean(&self, from: f32, to: f32) -> f32 {
        if self.curve.is_empty() {
            return self.song;
        }
        let first = ((from * FEEL_PER_SECOND as f32).max(0.0) as usize).min(self.curve.len() - 1);
        let last = ((to * FEEL_PER_SECOND as f32).max(0.0) as usize).clamp(first, self.curve.len() - 1);
        self.curve[first..=last].iter().sum::<f32>() / (last - first + 1) as f32
    }

    /// Атаки в отрезке `from..to`.
    pub fn beats_between(&self, from: f32, to: f32) -> &[f32] {
        let first = self.beats.partition_point(|beat| *beat < from);
        let last = self.beats.partition_point(|beat| *beat <= to);
        &self.beats[first..last.max(first)]
    }

    /// Темп, долей в минуту, если нашёлся (для порядка в плейлисте дня).
    pub fn bpm(&self) -> Option<f32> {
        self.tempo.map(|(period, _)| 60.0 / period)
    }

    /// Готово ли по звуку (иначе — общее значение по умолчанию).
    pub fn measured(&self) -> bool {
        !self.curve.is_empty()
    }

    /// Без разбора звука: среднее.
    pub fn neutral(song: f32) -> Self {
        Self { song, ..Self::default() }
    }
}

/// Темп — самый частый промежуток атак в 60–180 BPM (лучше 80–150).
fn tempo(beats: &[f32]) -> Option<(f32, f32)> {
    if beats.len() < 16 {
        return None;
    }
    // Гистограмма промежутков до 2 с по 10 мс.
    let mut histogram = vec![0.0f32; 201];
    for (index, first) in beats.iter().enumerate() {
        for second in &beats[index + 1..] {
            let gap = second - first;
            if gap > 2.0 {
                break;
            }
            let bin = (gap * 100.0).round() as usize;
            for (offset, weight) in [(0usize, 1.0f32), (1, 0.5)] {
                if bin + offset < histogram.len() {
                    histogram[bin + offset] += weight;
                }
                if bin >= offset {
                    histogram[bin - offset] += weight * if offset == 0 { 0.0 } else { 1.0 };
                }
            }
        }
    }
    let at = |seconds: f32| histogram.get((seconds * 100.0).round() as usize).copied().unwrap_or(0.0);
    let mut best = (0.0f32, 0.5f32);
    let mut period = 0.33f32;
    while period <= 1.0 {
        // Доля подтверждается и удвоенным, утроенным промежутком.
        let score = at(period) + 0.6 * at(period * 2.0) + 0.4 * at(period * 3.0) + 0.3 * at(period * 4.0);
        let prior = (-((period / 0.5).ln()).powi(2) / (2.0 * 0.35f32.powi(2))).exp();
        if score * prior > best.0 {
            best = (score * prior, period);
        }
        period += 0.005;
    }
    if best.0 <= 0.0 {
        return None;
    }
    let period = best.1;
    let (mut x, mut y) = (0.0f32, 0.0f32);
    for beat in beats {
        let angle = std::f32::consts::TAU * (beat / period).fract();
        x += angle.cos();
        y += angle.sin();
    }
    let anchor = y.atan2(x).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * period;
    Some((period, anchor))
}

/// Шагаем на долю и подтягиваемся к атаке (до 18%) — сетка идёт за музыкой.
fn track(onsets: &[f32], tempo: Option<(f32, f32)>) -> Vec<f32> {
    let (Some((period, anchor)), Some(&first), Some(&last)) = (tempo, onsets.first(), onsets.last()) else {
        return Vec::new();
    };
    let reach = 0.18 * period;
    let mut grid = Vec::new();
    let mut beat = anchor + ((first - anchor) / period).round() * period;
    let mut step = period;
    while beat <= last + period {
        let index = onsets.partition_point(|onset| *onset < beat - reach);
        let nearest = onsets[index..].iter().take_while(|onset| **onset <= beat + reach).min_by(|a, b| (*a - beat).abs().total_cmp(&(*b - beat).abs()));
        let placed = nearest.map_or(beat, |onset| beat + (onset - beat) * 0.7);
        if let Some(previous) = grid.last() {
            let gap: f32 = placed - previous;
            if nearest.is_some() && (gap - step).abs() < 0.25 * step {
                step = (step + (gap - step) * 0.15).clamp(period * 0.92, period * 1.08);
            }
        }
        grid.push(placed);
        beat = placed + step;
    }
    grid
}

/// Кэш разбора: по байту на 100 мс, атаки — мс. ~6 КБ на песню.
pub fn to_bytes(feel: &Feel) -> Vec<u8> {
    let mut bytes = b"NKF1".to_vec();
    bytes.extend_from_slice(&(feel.loud.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(feel.beats.len() as u32).to_le_bytes());
    let busy = feel.flux.iter().copied().fold(1e-6f32, f32::max);
    bytes.extend_from_slice(&busy.to_le_bytes());
    for index in 0..feel.loud.len() {
        bytes.push(((feel.loud[index] + 90.0) / 90.0 * 255.0).round().clamp(0.0, 255.0) as u8);
        bytes.push((feel.bright[index] * 255.0).round().clamp(0.0, 255.0) as u8);
        bytes.push((feel.noise[index] * 255.0).round().clamp(0.0, 255.0) as u8);
        bytes.push((feel.flux[index] / busy * 255.0).round().clamp(0.0, 255.0) as u8);
    }
    for beat in &feel.beats {
        bytes.extend_from_slice(&((beat * 1000.0).round() as u32).to_le_bytes());
    }
    bytes
}

pub fn from_bytes(bytes: &[u8]) -> Option<Feel> {
    let rest = bytes.strip_prefix(b"NKF1")?;
    let frames = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let beats = u32::from_le_bytes(rest.get(4..8)?.try_into().ok()?) as usize;
    let busy = f32::from_le_bytes(rest.get(8..12)?.try_into().ok()?);
    let data = rest.get(12..)?;
    if data.len() != frames * 4 + beats * 4 {
        return None;
    }
    let mut feel = Feel::default();
    for frame in data[..frames * 4].as_chunks::<4>().0 {
        feel.loud.push(f32::from(frame[0]) / 255.0 * 90.0 - 90.0);
        feel.bright.push(f32::from(frame[1]) / 255.0);
        feel.noise.push(f32::from(frame[2]) / 255.0);
        feel.flux.push(f32::from(frame[3]) / 255.0 * busy);
    }
    for beat in data[frames * 4..].as_chunks::<4>().0 {
        feel.beats.push(u32::from_le_bytes(*beat) as f32 / 1000.0);
    }
    Some(feel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feel(loud: f32, bright: f32, noise: f32, seconds: usize) -> Feel {
        let count = seconds * FEEL_PER_SECOND as usize;
        Feel {
            loud: vec![loud; count],
            flux: vec![0.1; count],
            bright: vec![bright; count],
            noise: vec![noise; count],
            beats: (0..seconds * 2).map(|beat| beat as f32 * 0.5).collect(),
        }
    }

    #[test]
    fn punk_is_fierce_and_acoustic_is_calm() {
        // Как «Куда пропала Элис» и «портреты (Acoustic)».
        let punk = Mood::from_feel(&feel(-8.5, 0.08, 0.1, 30));
        let acoustic = Mood::from_feel(&feel(-17.0, 0.015, 0.01, 30));
        assert!(punk.song > FIERCE, "{}", punk.song);
        assert!(acoustic.song < CALM, "{}", acoustic.song);
        assert!(punk.at(15.0) > acoustic.at(15.0));
        assert_eq!(punk.beats_between(10.2, 11.0), &[10.5, 11.0]);
    }

    #[test]
    fn tempo_is_found_from_onsets_with_offbeats() {
        // 120 долей в минуту (0,5 с) с началом в 0,13 с, плюс слабые атаки между долями.
        let mut beats: Vec<f32> = (0..80).map(|beat| 0.13 + beat as f32 * 0.5).collect();
        beats.extend((0..40).map(|beat| 0.38 + beat as f32 * 1.0));
        beats.sort_by(f32::total_cmp);
        let feel = Feel { loud: vec![-10.0; 400], flux: vec![0.1; 400], bright: vec![0.05; 400], noise: vec![0.05; 400], beats };
        let mood = Mood::from_feel(&feel);
        assert!((mood.period() - 0.5).abs() < 0.02, "{}", mood.period());
        // На доле фаза около нуля, посередине — около половины.
        let near = |phase: f32, target: f32| (phase - target).abs().min(1.0 - (phase - target).abs()) < 0.12;
        assert!(near(mood.phase(10.13), 0.0), "{}", mood.phase(10.13));
        assert!(near(mood.phase(10.38), 0.5), "{}", mood.phase(10.38));
        // Огибающая громкости: тихое место ниже громкого.
        let mut quiet = feel.clone();
        quiet.loud = (0..400).map(|frame| if frame < 200 { -30.0 } else { -8.0 }).collect();
        let mood = Mood::from_feel(&quiet);
        assert!(mood.level(5.0) < 0.2 && mood.level(30.0) > 0.8, "{} {}", mood.level(5.0), mood.level(30.0));
    }

    #[test]
    fn travel_speeds_up_in_loud_places_without_jumps() {
        let mut song = feel(-24.0, 0.01, 0.01, 20);
        let loud = feel(-8.0, 0.09, 0.09, 20);
        song.loud.extend(&loud.loud);
        song.bright.extend(&loud.bright);
        song.noise.extend(&loud.noise);
        song.flux.extend(vec![0.3; loud.flux.len()]);
        let mood = Mood::from_feel(&song);
        // Шаг кадра — маленький шаг пути везде, и на смене настроения тоже.
        let frame = 1.0 / 60.0;
        let mut previous = mood.travel(0.0);
        let mut t = frame;
        while t < 45.0 {
            let now = mood.travel(t);
            assert!(now >= previous && now - previous <= frame * 1.01, "{t}: {previous} → {now}");
            previous = now;
            t += frame;
        }
        // Громко путь идёт быстрее, чем тихо.
        assert!(mood.travel(35.0) - mood.travel(30.0) > 2.0 * (mood.travel(10.0) - mood.travel(5.0)));
        assert!((Mood::neutral(0.5).travel(4.0) - 2.0).abs() < 1e-5);
    }

    #[test]
    fn beat_grid_follows_a_drifting_tempo() {
        // Темп плывёт 0,500 → 0,470 с — одна сетка разошлась бы на полдоли.
        let mut onsets = Vec::new();
        let mut at = 0.2f32;
        let mut truth = Vec::new();
        while at < 120.0 {
            truth.push(at);
            onsets.push(at);
            let period = 0.5 - 0.03 * at / 120.0;
            onsets.push(at + period * 0.5 + 0.03);
            at += period;
        }
        onsets.sort_by(f32::total_cmp);
        let count = (120.0 * FEEL_PER_SECOND as f32) as usize;
        let feel = Feel { loud: vec![-10.0; count], flux: vec![0.1; count], bright: vec![0.05; count], noise: vec![0.05; count], beats: onsets };
        let mood = Mood::from_feel(&feel);
        let off = |phase: f32| phase.min(1.0 - phase);
        let worst = truth.iter().skip(4).map(|beat| off(mood.phase(*beat + 0.001))).fold(0.0f32, f32::max);
        assert!(worst < 0.12, "доля уходит от удара на {worst}");
    }

    #[test]
    fn quiet_verse_and_loud_chorus_differ_and_cache_round_trips() {
        let mut song = feel(-24.0, 0.01, 0.01, 20);
        let loud = feel(-8.0, 0.09, 0.09, 20);
        song.loud.extend(&loud.loud);
        song.bright.extend(&loud.bright);
        song.noise.extend(&loud.noise);
        song.flux.extend(vec![0.3; loud.flux.len()]);
        let mood = Mood::from_feel(&song);
        assert!(mood.at(8.0) < CALM && mood.at(32.0) > FIERCE, "{} {}", mood.at(8.0), mood.at(32.0));
        assert!(mood.mean(25.0, 35.0) > mood.mean(0.0, 10.0));
        let back = from_bytes(&to_bytes(&song)).unwrap();
        assert_eq!(back.loud.len(), song.loud.len());
        assert!((back.loud[5] - song.loud[5]).abs() < 0.4);
        assert_eq!(back.beats.len(), song.beats.len());
        assert!(from_bytes(b"NKF1\x01\x00\x00\x00").is_none());
    }
}

#[cfg(test)]
mod report {
    /// `NYANOTO_FEEL_FILES=список.txt cargo test --release -p nyanoto-app mood_report -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn mood_report() {
        let Some(list) = std::env::var_os("NYANOTO_FEEL_FILES") else {
            return;
        };
        for path in std::fs::read_to_string(list).unwrap().lines() {
            let feel = nyanoto_core::feel(std::path::Path::new(path), &std::sync::atomic::AtomicBool::new(false)).unwrap().unwrap();
            let mood = super::Mood::from_feel(&feel);
            let name = path.rsplit('/').next().unwrap();
            let curve: String = (0..(feel.seconds() as usize / 10))
                .map(|step| format!(" {:.2}", mood.mean(step as f32 * 10.0, step as f32 * 10.0 + 10.0)))
                .collect();
            // Попадания на долю (±12%): по живой сетке и по одной на песню.
            let near = |phase: f32| phase.min(1.0 - phase) < 0.12;
            let (period, anchor) = mood.tempo.unwrap_or((0.5, 0.0));
            let on_grid = feel.beats.iter().filter(|beat| near(mood.phase(**beat))).count() as f32 / feel.beats.len().max(1) as f32;
            let on_global = feel.beats.iter().filter(|beat| near(((**beat - anchor) / period).rem_euclid(1.0))).count() as f32 / feel.beats.len().max(1) as f32;
            println!(
                "{name:.44}: песня {:.2}, темп {:.0} в минуту, атак на доле {:.0}% (одной сеткой {:.0}%) |{curve}",
                mood.song,
                60.0 / mood.period(),
                on_grid * 100.0,
                on_global * 100.0
            );
        }
    }
}
