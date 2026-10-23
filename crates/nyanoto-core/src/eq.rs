//! Параметрический EQ: биквады RBJ и профили AutoEQ.
//! Работает в звуковом потоке — слышно сразу. Без аллокаций и блокировок: новые берём через `try_lock`.

use std::f64::consts::PI;

/// Больше полос эквалайзер не держит.
pub const MAX_BANDS: usize = 20;
/// Каналов, для которых хранится состояние фильтров.
pub const MAX_CHANNELS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqBand {
    pub kind: FilterKind,
    /// Частота, Гц.
    pub freq: f32,
    /// Усиление, дБ; у ФНЧ и ФВЧ не используется.
    pub gain_db: f32,
    pub q: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Equalizer {
    pub enabled: bool,
    /// Общее ослабление перед фильтрами, дБ: чтобы подъём полос не клиппировал.
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

/// Коэффициенты одного фильтра, уже поделённые на a0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn new(band: &EqBand, rate: u32) -> Self {
        let rate = f64::from(rate.max(1));
        let freq = f64::from(band.freq).clamp(10.0, rate * 0.49);
        let q = f64::from(band.q).clamp(0.05, 50.0);
        let w0 = 2.0 * PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10f64.powf(f64::from(band.gain_db) / 40.0);
        let root = a.sqrt();
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            FilterKind::Peaking => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
            FilterKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cos + 2.0 * root * alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - 2.0 * root * alpha),
                (a + 1.0) + (a - 1.0) * cos + 2.0 * root * alpha,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - 2.0 * root * alpha,
            ),
            FilterKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cos + 2.0 * root * alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - 2.0 * root * alpha),
                (a + 1.0) - (a - 1.0) * cos + 2.0 * root * alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - 2.0 * root * alpha,
            ),
            FilterKind::LowPass => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            FilterKind::HighPass => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// Усиление на частоте `freq`, дБ.
    fn response_db(&self, freq: f64, rate: f64) -> f64 {
        let w = 2.0 * PI * freq / rate;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let power = (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im).max(1e-30);
        10.0 * power.max(1e-30).log10()
    }
}

/// Готовые коэффициенты для звукового потока. Копируется целиком, без кучи.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EqCoefficients {
    pub active: bool,
    pub preamp: f32,
    pub count: usize,
    pub filters: [Biquad; MAX_BANDS],
}

impl Default for EqCoefficients {
    fn default() -> Self {
        Self { active: false, preamp: 1.0, count: 0, filters: [Biquad::default(); MAX_BANDS] }
    }
}

impl EqCoefficients {
    pub fn new(eq: &Equalizer, rate: u32) -> Self {
        let mut result = Self::default();
        if !eq.enabled {
            return result;
        }
        for band in eq.bands.iter().take(MAX_BANDS) {
            result.filters[result.count] = Biquad::new(band, rate);
            result.count += 1;
        }
        result.preamp = 10f32.powf(eq.preamp_db.clamp(-30.0, 12.0) / 20.0);
        result.active = true;
        result
    }
}

/// Фильтры на стороне звукового потока: коэффициенты и память фильтров.
pub(crate) struct EqRuntime {
    pub version: u64,
    pub coefficients: EqCoefficients,
    /// Два прошлых значения на канал и фильтр (транспонированная прямая II).
    state: [[[f64; 2]; MAX_BANDS]; MAX_CHANNELS],
}

impl EqRuntime {
    pub fn new() -> Self {
        Self { version: 0, coefficients: EqCoefficients::default(), state: [[[0.0; 2]; MAX_BANDS]; MAX_CHANNELS] }
    }

    /// Память фильтров сохраняем, чтобы не щёлкало; новые полосы — с нуля.
    pub fn update(&mut self, coefficients: EqCoefficients, version: u64) {
        let before = if self.coefficients.active { self.coefficients.count } else { 0 };
        for channel in &mut self.state {
            for state in channel.iter_mut().skip(before) {
                *state = [0.0; 2];
            }
        }
        self.coefficients = coefficients;
        self.version = version;
    }

    /// Звук начинается с другого места: память фильтров обнуляется.
    pub fn reset(&mut self) {
        self.state = [[[0.0; 2]; MAX_BANDS]; MAX_CHANNELS];
    }

    /// Пропускает сэмпл канала `channel` через все фильтры.
    #[inline]
    pub fn process(&mut self, channel: usize, sample: f32) -> f32 {
        let coefficients = &self.coefficients;
        if !coefficients.active || channel >= MAX_CHANNELS {
            return sample;
        }
        let mut x = f64::from(sample * coefficients.preamp);
        let states = &mut self.state[channel];
        for (filter, state) in coefficients.filters[..coefficients.count].iter().zip(states.iter_mut()) {
            let y = filter.b0 * x + state[0];
            state[0] = filter.b1 * x - filter.a1 * y + state[1];
            state[1] = filter.b2 * x - filter.a2 * y;
            x = y;
        }
        x as f32
    }
}

impl Equalizer {
    /// Суммарное усиление всех полос и предусилителя на частоте `freq`, дБ.
    pub fn response_db(&self, freq: f32, rate: u32) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        let rate = f64::from(rate.max(1));
        let total: f64 = self
            .bands
            .iter()
            .take(MAX_BANDS)
            .map(|band| Biquad::new(band, rate as u32).response_db(f64::from(freq), rate))
            .sum();
        (total + f64::from(self.preamp_db)) as f32
    }

    /// AutoEQ / Equalizer APO: `Preamp: -6.2 dB`, `Filter 1: ON PK Fc 105 Hz Gain -2.3 dB Q 0.70`.
    pub fn parse_autoeq(text: &str) -> Result<Self, String> {
        let mut eq = Equalizer { enabled: true, ..Equalizer::default() };
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Preamp:") {
                eq.preamp_db = number_before(rest, "dB").ok_or_else(|| format!("не понял строку «{line}»"))?;
                continue;
            }
            let Some((_, rest)) = line.split_once(':').filter(|_| line.starts_with("Filter")) else {
                continue;
            };
            let words: Vec<&str> = rest.split_whitespace().collect();
            if words.first() != Some(&"ON") {
                continue;
            }
            let kind = match words.get(1).copied() {
                Some("PK" | "PEQ") => FilterKind::Peaking,
                Some("LSC" | "LS" | "LSQ") => FilterKind::LowShelf,
                Some("HSC" | "HS" | "HSQ") => FilterKind::HighShelf,
                Some("LP" | "LPQ") => FilterKind::LowPass,
                Some("HP" | "HPQ") => FilterKind::HighPass,
                _ => continue,
            };
            let value = |name: &str| {
                words
                    .iter()
                    .position(|word| *word == name)
                    .and_then(|at| words.get(at + 1))
                    .and_then(|word| word.parse::<f32>().ok())
            };
            let freq = value("Fc").ok_or_else(|| format!("нет частоты в «{line}»"))?;
            eq.bands.push(EqBand {
                kind,
                freq,
                gain_db: value("Gain").unwrap_or(0.0),
                q: value("Q").unwrap_or(std::f32::consts::FRAC_1_SQRT_2),
            });
        }
        if eq.bands.is_empty() {
            return Err("в файле нет фильтров (ожидался ParametricEQ.txt из AutoEQ)".into());
        }
        if eq.bands.len() > MAX_BANDS {
            return Err(format!("фильтров {} — больше {MAX_BANDS}", eq.bands.len()));
        }
        Ok(eq)
    }

    /// Десять полос, как у привычного графического эквалайзера, все на нуле.
    pub fn ten_band() -> Self {
        let bands = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0]
            .into_iter()
            .map(|freq| EqBand { kind: FilterKind::Peaking, freq, gain_db: 0.0, q: 1.41 })
            .collect();
        Equalizer { enabled: true, preamp_db: 0.0, bands }
    }
}

fn number_before(text: &str, unit: &str) -> Option<f32> {
    text.trim().trim_end_matches(unit).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn level_after(eq: &Equalizer, hz: f32) -> f32 {
        let mut runtime = EqRuntime::new();
        runtime.update(EqCoefficients::new(eq, RATE), 1);
        let mut peak = 0.0f32;
        for frame in 0..RATE as usize {
            let input = (frame as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.25;
            let output = runtime.process(0, input);
            if frame > RATE as usize / 2 {
                peak = peak.max(output.abs());
            }
        }
        20.0 * (peak / 0.25).log10()
    }

    #[test]
    fn peaking_band_lifts_its_frequency_and_leaves_others() {
        let eq = Equalizer {
            enabled: true,
            preamp_db: 0.0,
            bands: vec![EqBand { kind: FilterKind::Peaking, freq: 1000.0, gain_db: 6.0, q: 1.0 }],
        };
        assert!((level_after(&eq, 1000.0) - 6.0).abs() < 0.2);
        assert!(level_after(&eq, 100.0).abs() < 0.3);
        assert!((eq.response_db(1000.0, RATE) - 6.0).abs() < 0.01);
        assert!(eq.response_db(20.0, RATE).abs() < 0.1);
    }

    #[test]
    fn shelves_and_passes_go_the_right_way() {
        let band = |kind, freq, gain_db| Equalizer {
            enabled: true,
            preamp_db: 0.0,
            bands: vec![EqBand { kind, freq, gain_db, q: 0.707 }],
        };
        let low = band(FilterKind::LowShelf, 200.0, 6.0);
        assert!((low.response_db(30.0, RATE) - 6.0).abs() < 0.3);
        assert!(low.response_db(5000.0, RATE).abs() < 0.3);
        let high = band(FilterKind::HighShelf, 4000.0, -6.0);
        assert!((high.response_db(15000.0, RATE) + 6.0).abs() < 0.5);
        assert!(band(FilterKind::LowPass, 1000.0, 0.0).response_db(8000.0, RATE) < -30.0);
        assert!(band(FilterKind::HighPass, 1000.0, 0.0).response_db(100.0, RATE) < -30.0);
    }

    #[test]
    fn disabled_equalizer_does_nothing() {
        let mut eq = Equalizer::ten_band();
        eq.bands[5].gain_db = 12.0;
        eq.enabled = false;
        let mut runtime = EqRuntime::new();
        runtime.update(EqCoefficients::new(&eq, RATE), 1);
        assert_eq!(runtime.process(0, 0.123), 0.123);
    }

    #[test]
    fn autoeq_profile_is_read() {
        let text = "Preamp: -6.4 dB\nFilter 1: ON LSC Fc 105 Hz Gain 6.7 dB Q 0.70\nFilter 2: ON PK Fc 152 Hz Gain -3.4 dB Q 0.53\nFilter 3: OFF PK Fc 300 Hz Gain 1 dB Q 1\nFilter 4: ON HSC Fc 10000 Hz Gain -2.3 dB Q 0.70\n";
        let eq = Equalizer::parse_autoeq(text).unwrap();
        assert!(eq.enabled);
        assert_eq!(eq.preamp_db, -6.4);
        assert_eq!(eq.bands.len(), 3, "выключенный фильтр пропущен");
        assert_eq!(eq.bands[0], EqBand { kind: FilterKind::LowShelf, freq: 105.0, gain_db: 6.7, q: 0.7 });
        assert_eq!(eq.bands[2].kind, FilterKind::HighShelf);
        assert!(Equalizer::parse_autoeq("просто текст").is_err());
    }
}
