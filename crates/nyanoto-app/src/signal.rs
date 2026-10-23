//! Тракт сигнала: что происходит со звуком по дороге от файла до устройства.

use nyanoto_library::quality;

use crate::App;

/// Открытое устройство вывода.
#[derive(Clone, Debug, Default)]
pub struct OutputInfo {
    pub device: String,
    pub rate: u32,
    /// Значащих бит в эксклюзивном режиме; `None` — общий режим.
    pub exclusive_bits: Option<u16>,
}

impl App {
    /// Строка тракта для панели: источник → обработка → устройство.
    pub(crate) fn show_signal_path(&self) {
        let text = self.signal_path().map(|path| format!("{}: {path}", self.lex().path_label)).unwrap_or_default();
        self.window.set_signal_path(text.into());
    }

    /// Тракт играющего трека, если модуль включён.
    pub(crate) fn signal_path(&self) -> Option<String> {
        if !self.exclusive_on.get() {
            return None;
        }
        let lex = self.lex();
        let track = self.current.borrow().clone()?;
        let output = self.output_info.borrow().clone();
        let source = quality(&track);
        let mut steps: Vec<String> = Vec::new();
        // DSD переводится в PCM ещё при декодировании: это первый шаг тракта и
        // никогда не «бит в бит».
        let dsd = track.format.as_deref() == Some("dsd");
        let decoded_rate = if dsd { track.sample_rate.and_then(nyanoto_core::dsd_pcm_rate) } else { source.sample_rate };
        if let Some(rate) = decoded_rate.filter(|_| dsd) {
            steps.push(format!("{} {}", lex.path_dsd, khz(rate, lex)));
        }

        let speed = self.engine.speed();
        if (speed - 1.0).abs() > 1e-6 {
            let how = if self.keep_pitch.get() { lex.path_keep_pitch } else { lex.path_record };
            steps.push(format!("{} {} ({how})", lex.path_speed, self.speed_text(speed)));
        }
        if let (Some(rate), Some(output)) = (decoded_rate, output.as_ref())
            && rate != output.rate
            && output.rate > 0
        {
            steps.push(format!("{} → {}", lex.path_resample, khz(output.rate, lex)));
        }
        if self.replaygain.get() {
            steps.push("ReplayGain".into());
        }
        if self.window.get_eq_active() {
            steps.push(lex.path_eq.to_string());
        }
        let volume = self.window.get_volume();
        if volume < 0.999 {
            steps.push(format!("{} {}%", lex.path_volume, (volume * 100.0).round()));
        }

        let device = match &output {
            Some(output) => match output.exclusive_bits {
                Some(bits) => format!("{} ({}, {bits} {})", output.device, lex.path_exclusive, lex.bit),
                None => format!("{} ({})", output.device, lex.path_shared),
            },
            None => "…".into(),
        };
        let bit_perfect = steps.is_empty()
            && output.as_ref().is_some_and(|output| {
                output.exclusive_bits.is_some_and(|bits| u32::from(bits) >= u32::from(source.bit_depth.unwrap_or(16)))
            })
            && source.lossless;
        let mut path = vec![format!(
            "{} {}",
            source.format,
            source.sample_rate.map(|rate| khz(rate, lex)).unwrap_or_default()
        )];
        path.extend(steps);
        path.push(device);
        let mut text = path.join(" → ");
        if bit_perfect {
            text.push_str(&format!(" · {}", lex.path_bitperfect));
        }
        Some(text)
    }
}

fn khz(rate: u32, lex: &crate::lang::Lex) -> String {
    let text = format!("{:.1}", f64::from(rate) / 1000.0);
    format!("{} {}", text.trim_end_matches(".0").replace('.', &lex.decimal.to_string()), lex.khz)
}
