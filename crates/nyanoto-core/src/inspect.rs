//! Подлинность качества: «без потерь» из MP3 и растянутый Hi-Res.
//!
//! Признаки: отвесный срез верха, стена фильтра CD у Hi-Res, пустые младшие биты у 24 бит.
//! Это эвристика — поэтому «вероятно».

use std::path::Path;
use std::time::Duration;

use realfft::RealFftPlanner;

use crate::source::{OpenOptions, TrackDecoder, TrackError, guard};

/// Размер окна преобразования Фурье.
const FFT: usize = 8192;
/// Сколько отрезков файла слушать и сколько кадров в каждом.
const SEGMENTS: usize = 10;
const SEGMENT_FRAMES: usize = FFT * 6;
/// Ширина полосы, по которой усредняется спектр, Гц.
const BAND_HZ: f32 = 250.0;
/// Кадры тише этого уровня (по среднеквадратичному) не учитываются.
const SILENCE_RMS: f32 = 1e-4;
/// Ниже этой частоты срез — уже не MP3, а скорее шумная плёнка.
const LOSSY_FROM_HZ: f32 = 10_000.0;
/// Стена здесь у Hi-Res — след фильтра CD.
const CD_WALL_FROM_HZ: f32 = 19_000.0;
const CD_CEILING_HZ: f32 = 22_600.0;

/// Чем строже, тем больше находит и тем чаще ошибается.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Sensitivity {
    /// Только явные случаи: меньше всего ложных тревог.
    #[default]
    Low,
    Medium,
    /// Всё подозрительное, включая старые аналоговые записи в Hi-Res.
    High,
}

impl Sensitivity {
    pub const ALL: [Sensitivity; 3] = [Sensitivity::Low, Sensitivity::Medium, Sensitivity::High];

    fn rules(self) -> Rules {
        match self {
            Sensitivity::Low => Rules { wall_db: 20.0, silence_db: 50.0, lossy_below_hz: 19_600.0, cd_wall_needed: true },
            Sensitivity::Medium => Rules { wall_db: 15.0, silence_db: 35.0, lossy_below_hz: 20_200.0, cd_wall_needed: true },
            Sensitivity::High => Rules { wall_db: 12.0, silence_db: 25.0, lossy_below_hz: 20_800.0, cd_wall_needed: false },
        }
    }
}

/// Пороги одной чувствительности.
struct Rules {
    /// Насколько должен упасть уровень у среза, чтобы срез считался стеной, дБ.
    wall_db: f32,
    /// Насколько тише музыки должно быть выше среза кодировщика, дБ.
    silence_db: f32,
    /// Срез ниже этой частоты со стеной — след кодировщика с потерями.
    lossy_below_hz: f32,
    /// Без стены под подозрение попадают старые записи с плавным спадом.
    cd_wall_needed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Признаков подделки нет.
    Genuine,
    /// Спектр обрезан стеной: файл, вероятно, перекодирован из MP3/AAC.
    LossyOrigin,
    /// Выше 22 кГц пусто: Hi-Res, вероятно, пересчитан из CD.
    Upsampled,
    /// Младшие биты пустые: 24 бит, вероятно, дополнены из 16.
    PaddedBits,
    /// Судить не по чему: файл слишком короткий или тихий.
    Unknown,
}

/// Измерения; вердикт — по чувствительности, слушать заново не надо.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inspection {
    /// Было ли что слушать: файл не слишком короткий и не тихий.
    pub measured: bool,
    pub sample_rate: u32,
    /// Где кончается полезный сигнал, Гц.
    pub cutoff_hz: Option<u32>,
    /// Насколько падает уровень сразу за срезом, дБ.
    pub wall_db: f32,
    /// Насколько верх спектра тише музыки, дБ.
    pub silence_db: f32,
    /// У файла 24 бит младшие 8 бит пустые.
    pub padded_bits: bool,
}

impl Inspection {
    pub fn verdict(&self, sensitivity: Sensitivity) -> Verdict {
        let Some(cutoff) = self.cutoff_hz.filter(|_| self.measured) else {
            return Verdict::Unknown;
        };
        let cutoff = cutoff as f32;
        let rules = sensitivity.rules();
        let wall = self.wall_db >= rules.wall_db;
        let hires = self.sample_rate > 48_000;
        if wall
            && self.silence_db >= rules.silence_db
            && (LOSSY_FROM_HZ..rules.lossy_below_hz).contains(&cutoff)
        {
            Verdict::LossyOrigin
        } else if hires
            && cutoff <= CD_CEILING_HZ
            && (!rules.cd_wall_needed || (wall && cutoff >= CD_WALL_FROM_HZ))
        {
            Verdict::Upsampled
        } else if self.padded_bits {
            Verdict::PaddedBits
        } else {
            Verdict::Genuine
        }
    }

    /// Из какой частоты, вероятно, пересчитан Hi-Res.
    pub fn real_rate(&self) -> Option<u32> {
        let cutoff = self.cutoff_hz?;
        (self.sample_rate > 48_000 && cutoff as f32 <= CD_CEILING_HZ)
            .then_some(if cutoff <= 22_100 { 44_100 } else { 48_000 })
    }

    /// Сколько бит занято на самом деле, если меньше заявленного.
    pub fn real_bits(&self) -> Option<u32> {
        self.padded_bits.then_some(16)
    }
}

/// Проверяет файл: слушает несколько отрезков по всей длине.
pub fn inspect(path: &Path) -> Result<Inspection, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions::default())?;
        let info = decoder.info().clone();
        let mut analyzer: Option<Analyzer> = None;

        let positions: Vec<Option<Duration>> = match info.duration {
            Some(duration) if duration > Duration::from_secs(20) => (0..SEGMENTS)
                .map(|index| {
                    let share = 0.05 + 0.9 * index as f64 / (SEGMENTS - 1) as f64;
                    Some(duration.mul_f64(share))
                })
                .collect(),
            // Короткий файл или длина неизвестна: читаем подряд с начала.
            _ => vec![None; SEGMENTS],
        };
        for position in positions {
            if let Some(position) = position
                && decoder.seek(position).is_err()
            {
                continue;
            }
            let mut taken = 0;
            while taken < SEGMENT_FRAMES {
                let Some(chunk) = decoder.next_chunk()? else {
                    break;
                };
                let channels = chunk.channels.count().max(1);
                let analyzer = analyzer.get_or_insert_with(|| {
                    Analyzer::new(chunk.rate, info.bits_per_sample.unwrap_or(16))
                });
                analyzer.push(chunk.samples, channels);
                taken += chunk.samples.len() / channels;
            }
        }
        Ok(analyzer.map_or(UNKNOWN, Analyzer::finish))
    })
}

const UNKNOWN: Inspection = Inspection {
    measured: false,
    sample_rate: 0,
    cutoff_hz: None,
    wall_db: 0.0,
    silence_db: 0.0,
    padded_bits: false,
};

/// Копит усреднённый спектр и следы разрядности по мере подачи звука.
pub struct Analyzer {
    rate: u32,
    bits: u32,
    window: Vec<f32>,
    frame: Vec<f32>,
    power: Vec<f64>,
    frames: usize,
    planner: RealFftPlanner<f32>,
    /// Объединение младших 8 бит всех сэмплов (для файлов 24 бит).
    low_bits: u32,
    loud_samples: u64,
}

impl Analyzer {
    pub fn new(rate: u32, bits: u32) -> Self {
        let window = (0..FFT)
            .map(|index| {
                let phase = std::f32::consts::TAU * index as f32 / FFT as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        Self {
            rate,
            bits,
            window,
            frame: Vec::with_capacity(FFT),
            power: vec![0.0; FFT / 2 + 1],
            frames: 0,
            planner: RealFftPlanner::new(),
            low_bits: 0,
            loud_samples: 0,
        }
    }

    /// Подаёт кусок звука: сэмплы идут кадрами по `channels`.
    pub fn push(&mut self, samples: &[f32], channels: usize) {
        if self.bits >= 24 {
            for sample in samples {
                let value = (sample * 8_388_608.0).round() as i32;
                if value != 0 {
                    self.low_bits |= (value & 0xFF) as u32;
                    self.loud_samples += 1;
                }
            }
        }
        for frame in samples.chunks_exact(channels) {
            self.frame.push(frame.iter().sum::<f32>() / channels as f32);
            if self.frame.len() == FFT {
                self.transform();
                // Окна перекрываются наполовину.
                self.frame.drain(..FFT / 2);
            }
        }
    }

    fn transform(&mut self) {
        let rms = (self.frame.iter().map(|sample| sample * sample).sum::<f32>() / FFT as f32).sqrt();
        if rms < SILENCE_RMS {
            return;
        }
        let fft = self.planner.plan_fft_forward(FFT);
        let mut input: Vec<f32> = self
            .frame
            .iter()
            .zip(&self.window)
            .map(|(sample, weight)| sample * weight)
            .collect();
        let mut output = fft.make_output_vec();
        if fft.process(&mut input, &mut output).is_err() {
            return;
        }
        for (total, bin) in self.power.iter_mut().zip(&output) {
            *total += f64::from(bin.norm_sqr());
        }
        self.frames += 1;
    }

    pub fn finish(self) -> Inspection {
        if self.frames < 8 || self.rate == 0 {
            return UNKNOWN;
        }
        let bin_hz = self.rate as f32 / FFT as f32;
        let per_band = ((BAND_HZ / bin_hz).round() as usize).max(1);
        let bands: Vec<f32> = self
            .power
            .chunks(per_band)
            .map(|chunk| {
                let mean = chunk.iter().sum::<f64>() / chunk.len() as f64 / self.frames as f64;
                (10.0 * (mean + 1e-30).log10()) as f32
            })
            .collect();
        let band_hz = per_band as f32 * bin_hz;
        let band_of = |hz: f32| ((hz / band_hz) as usize).min(bands.len() - 1);

        // Уровень музыки: верхняя часть полос от 200 Гц до 10 кГц.
        let mut music: Vec<f32> = bands[band_of(200.0)..=band_of(10_000.0)].to_vec();
        music.sort_by(f32::total_cmp);
        let reference = music[music.len() * 9 / 10];
        // Шумовой пол: самые тихие полосы в верхней четверти спектра.
        let mut top: Vec<f32> = bands[bands.len() * 3 / 4..bands.len().saturating_sub(1)].to_vec();
        if top.is_empty() {
            return UNKNOWN;
        }
        top.sort_by(f32::total_cmp);
        let floor = top[top.len() / 4];
        let threshold = (floor + 10.0).max(reference - 85.0);

        // Срез: самая высокая полоса, где ещё есть сигнал.
        let Some(edge) = (0..bands.len()).rev().find(|&band| bands[band] > threshold) else {
            return UNKNOWN;
        };
        let cutoff = (edge as f32 + 1.0) * band_hz;
        let mean = |range: std::ops::Range<usize>| {
            let range = range.start.min(bands.len())..range.end.min(bands.len());
            if range.is_empty() {
                return floor;
            }
            bands[range.clone()].iter().sum::<f32>() / range.len() as f32
        };
        let below = mean(edge.saturating_sub(3)..edge + 1);
        let above = mean(edge + 1..edge + 5);

        Inspection {
            measured: true,
            sample_rate: self.rate,
            cutoff_hz: Some(cutoff.round() as u32),
            wall_db: below - above,
            silence_db: reference - floor,
            padded_bits: self.bits >= 24 && self.loud_samples > 50_000 && self.low_bits == 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Синусоиды до `top` Гц плюс тихий шум — «музыка» с заданным верхом.
    fn signal(rate: u32, seconds: f32, top: f32, seed: u64) -> Vec<f32> {
        let mut state = seed;
        let mut random = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f32 / (1u64 << 53) as f32
        };
        let tones: Vec<(f32, f32, f32)> = (0..120)
            .map(|_| {
                let frequency = 40.0 + random() * (top - 40.0);
                // Верхние частоты тише: как в настоящей музыке.
                let level = 0.02 / (1.0 + frequency / 3000.0);
                (frequency, level, random() * std::f32::consts::TAU)
            })
            .collect();
        let frames = (rate as f32 * seconds) as usize;
        (0..frames)
            .map(|index| {
                let time = index as f32 / rate as f32;
                let tone: f32 = tones
                    .iter()
                    .map(|(frequency, level, phase)| level * (std::f32::consts::TAU * frequency * time + phase).sin())
                    .sum();
                tone + (random() - 0.5) * 2e-6
            })
            .collect()
    }

    fn analyze(samples: &[f32], rate: u32, bits: u32) -> Inspection {
        let mut analyzer = Analyzer::new(rate, bits);
        analyzer.push(samples, 1);
        analyzer.finish()
    }

    #[test]
    fn full_band_cd_is_genuine() {
        let result = analyze(&signal(44_100, 4.0, 21_500.0, 1), 44_100, 16);
        assert_eq!(result.verdict(Sensitivity::Low), Verdict::Genuine, "{result:?}");
    }

    #[test]
    fn mp3_like_cutoff_is_caught() {
        let result = analyze(&signal(44_100, 4.0, 16_000.0, 2), 44_100, 16);
        assert_eq!(result.verdict(Sensitivity::Low), Verdict::LossyOrigin, "{result:?}");
        let cutoff = result.cutoff_hz.unwrap();
        assert!((15_500..=16_500).contains(&cutoff), "{cutoff}");
    }

    #[test]
    fn upsampled_hires_is_caught_and_real_hires_is_not() {
        let fake = analyze(&signal(96_000, 3.0, 21_000.0, 3), 96_000, 24);
        assert_eq!(fake.verdict(Sensitivity::Low), Verdict::Upsampled, "{fake:?}");
        assert_eq!(fake.real_rate(), Some(44_100));

        let real = analyze(&signal(96_000, 3.0, 40_000.0, 4), 96_000, 24);
        assert_eq!(real.verdict(Sensitivity::Low), Verdict::Genuine, "{real:?}");
    }

    #[test]
    fn gently_fading_analog_transfer_is_not_called_upsampled() {
        // Старая плёнка в Hi-Res: верх плавно уходит в шум, стены нет.
        let rate = 96_000;
        let frames = rate as usize * 3;
        let mut state = 7u64;
        let mut random = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f32 / (1u64 << 53) as f32
        };
        let tones: Vec<(f32, f32)> = (0..200)
            .map(|_| {
                let frequency = 40.0 + random() * 30_000.0;
                let fade_db = ((frequency - 6_000.0).max(0.0) / 1_000.0) * 5.0;
                (frequency, 0.02 * 10f32.powf(-fade_db / 20.0))
            })
            .collect();
        let samples: Vec<f32> = (0..frames)
            .map(|index| {
                let time = index as f32 / rate as f32;
                tones
                    .iter()
                    .map(|(frequency, level)| level * (std::f32::consts::TAU * frequency * time).sin())
                    .sum::<f32>()
                    + (random() - 0.5) * 2e-6
            })
            .collect();
        let result = analyze(&samples, rate, 24);
        assert_eq!(result.verdict(Sensitivity::Low), Verdict::Genuine, "{result:?}");
    }

    #[test]
    fn noisy_tape_with_flat_hiss_is_not_called_lossy() {
        // Шумная запись: музыка только внизу, а шум ровный до самого верха.
        let rate = 44_100;
        let mut state = 11u64;
        let mut random = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f32 / (1u64 << 53) as f32
        };
        let samples: Vec<f32> = (0..rate as usize * 4)
            .map(|index| {
                let time = index as f32 / rate as f32;
                0.3 * (std::f32::consts::TAU * 110.0 * time).sin() + (random() - 0.5) * 0.02
            })
            .collect();
        let result = analyze(&samples, rate, 16);
        assert_ne!(result.verdict(Sensitivity::Low), Verdict::LossyOrigin, "{result:?}");
    }

    #[test]
    fn padded_24_bits_are_caught() {
        // Сэмплы на сетке 16 бит, но файл заявлен как 24 бит.
        let padded: Vec<f32> = signal(48_000, 3.0, 23_000.0, 5)
            .into_iter()
            .map(|sample| (sample * 32_768.0).round() / 32_768.0)
            .collect();
        let result = analyze(&padded, 48_000, 24);
        assert_eq!(result.verdict(Sensitivity::Low), Verdict::PaddedBits, "{result:?}");
        assert_eq!(result.real_bits(), Some(16));

        // Настоящие 24 бит не трогаем.
        let real: Vec<f32> = signal(48_000, 3.0, 23_000.0, 6)
            .into_iter()
            .map(|sample| (sample * 8_388_608.0).round() / 8_388_608.0)
            .collect();
        assert_eq!(analyze(&real, 48_000, 24).verdict(Sensitivity::Low), Verdict::Genuine);
    }

    #[test]
    fn higher_sensitivity_catches_more() {
        // Срез ~20 кГц, как MP3 320: низкая пропускает, средняя и высокая ловят.
        let mp3_320 = Inspection {
            measured: true,
            sample_rate: 44_100,
            cutoff_hz: Some(19_900),
            wall_db: 30.0,
            silence_db: 60.0,
            padded_bits: false,
        };
        assert_eq!(mp3_320.verdict(Sensitivity::Low), Verdict::Genuine);
        assert_eq!(mp3_320.verdict(Sensitivity::Medium), Verdict::LossyOrigin);
        assert_eq!(mp3_320.verdict(Sensitivity::High), Verdict::LossyOrigin);

        // Старая запись с плавным спадом — растянутой её считает только высокая.
        let analog = Inspection {
            measured: true,
            sample_rate: 96_000,
            cutoff_hz: Some(17_000),
            wall_db: 6.0,
            silence_db: 40.0,
            padded_bits: false,
        };
        assert_eq!(analog.verdict(Sensitivity::Low), Verdict::Genuine);
        assert_eq!(analog.verdict(Sensitivity::Medium), Verdict::Genuine);
        assert_eq!(analog.verdict(Sensitivity::High), Verdict::Upsampled);
    }

    #[test]
    fn silence_tells_nothing() {
        assert_eq!(analyze(&vec![0.0; 200_000], 44_100, 16).verdict(Sensitivity::Low), Verdict::Unknown);
    }
}
