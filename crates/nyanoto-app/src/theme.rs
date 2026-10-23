//! Оформление: темы, сезоны и масштаб.
//!
//! «Обложка» берёт цвета играющего трека. У светлой, тёмной и «Тьмы» акцент системный.
//! Сезоны подкрашивают акцент и фон; украшения рисует `app.slint`.
//! «100 %» чуть крупнее разметки (`BASE_SCALE`).

use std::cell::Cell;
use std::time::{Duration, Instant};

use slint::{Color, ComponentHandle, Image, ModelRc, TimerMode, VecModel};

use crate::{App, Palette, Theme, ThemeCard, clock, in_ui, load, with_app};

pub const SETTING_THEME: &str = "theme";
pub const SETTING_SEASONS: &str = "seasons";
pub const SETTING_SCALE: &str = "scale";

/// «100 %» в настройках — разметка, увеличенная на столько.
#[cfg_attr(target_os = "android", allow(dead_code))]
const BASE_SCALE: f32 = 1.07;
pub const SCALE_MIN: u32 = 80;
pub const SCALE_MAX: u32 = 150;
const SCALE_STEP: u32 = 10;
/// Смена цветов темы — за столько, кадрами по 16 мс.
const TWEEN: Duration = Duration::from_millis(450);

/// Цвет: sRGB 0…1 и непрозрачность.
pub type Rgba = [f32; 4];

const fn hex(code: u32) -> Rgba {
    hexa(code, 1.0)
}

const fn hexa(code: u32, alpha: f32) -> Rgba {
    [((code >> 16) & 0xff) as f32 / 255.0, ((code >> 8) & 0xff) as f32 / 255.0, (code & 0xff) as f32 / 255.0, alpha]
}

fn color(rgba: Rgba) -> Color {
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::from_argb_u8(byte(rgba[3]), byte(rgba[0]), byte(rgba[1]), byte(rgba[2]))
}

fn mix(a: Rgba, b: Rgba, share: f32) -> Rgba {
    std::array::from_fn(|index| a[index] + (b[index] - a[index]) * share)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Light,
    Dark,
    Black,
    Vinyl,
    Synthwave,
    Terminal,
    Cover,
}

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::Light, Kind::Dark, Kind::Black, Kind::Vinyl, Kind::Synthwave, Kind::Terminal, Kind::Cover];

    pub fn code(self) -> &'static str {
        match self {
            Kind::Light => "light",
            Kind::Dark => "dark",
            Kind::Black => "black",
            Kind::Vinyl => "vinyl",
            Kind::Synthwave => "synthwave",
            Kind::Terminal => "terminal",
            Kind::Cover => "cover",
        }
    }

    pub fn from_code(code: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Цвета темы.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub background: Rgba,
    pub surface: Rgba,
    pub foreground: Rgba,
    pub border: Rgba,
    pub control: Rgba,
    pub deep: Rgba,
    pub dark: bool,
    /// Свой акцент и цвет текста на нём; `None` — системный.
    pub accent: Option<(Rgba, Rgba)>,
    pub mono: bool,
}

pub fn base(kind: Kind) -> Colors {
    let dark = Colors {
        background: hex(0x1c1c1c),
        surface: hex(0x2c2c2c),
        foreground: hex(0xffffff),
        border: hexa(0xffffff, 0.08),
        control: hexa(0xffffff, 0.06),
        deep: hex(0x121212),
        dark: true,
        accent: None,
        mono: false,
    };
    match kind {
        Kind::Dark | Kind::Cover => dark,
        Kind::Light => Colors {
            background: hex(0xf7f7f8),
            surface: hex(0xebebee),
            foreground: hex(0x18181b),
            border: hexa(0x000000, 0.10),
            control: hexa(0x000000, 0.04),
            deep: hex(0x121212),
            dark: false,
            ..dark
        },
        Kind::Black => Colors {
            background: hex(0x000000),
            surface: hex(0x0e0e0e),
            foreground: hex(0xededed),
            border: hexa(0xffffff, 0.07),
            control: hexa(0xffffff, 0.05),
            deep: hex(0x000000),
            ..dark
        },
        Kind::Vinyl => Colors {
            background: hex(0x1e1712),
            surface: hex(0x2a2019),
            foreground: hex(0xf2e4cf),
            border: hexa(0xf2e4cf, 0.12),
            control: hexa(0xf2e4cf, 0.07),
            deep: hex(0x140f0b),
            accent: Some((hex(0xe3a23b), hex(0x1e1712))),
            ..dark
        },
        Kind::Synthwave => Colors {
            background: hex(0x160e2b),
            surface: hex(0x22163f),
            foreground: hex(0xf7eaff),
            border: hexa(0xff5bd6, 0.17),
            control: hexa(0xff5bd6, 0.08),
            deep: hex(0x0e0820),
            accent: Some((hex(0xff4fd8), hex(0x1a0b2e))),
            ..dark
        },
        Kind::Terminal => Colors {
            background: hex(0x030803),
            surface: hex(0x0a140a),
            foreground: hex(0x3cff6e),
            border: hexa(0x3cff6e, 0.20),
            control: hexa(0x3cff6e, 0.07),
            deep: hex(0x000000),
            accent: Some((hex(0x3cff6e), hex(0x021202))),
            mono: true,
            ..dark
        },
    }
}

// ───────────────────────────── сезоны ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Season {
    Halloween = 1,
    NewYear = 2,
    CatDay = 3,
    Valentine = 4,
    Cosmonautics = 5,
    Sakura = 6,
}

impl Season {
    pub const ALL: [Season; 6] =
        [Season::Halloween, Season::NewYear, Season::CatDay, Season::Valentine, Season::Cosmonautics, Season::Sakura];

    /// Первый и последний день (месяц, день), включительно.
    pub fn days(self) -> ((u32, u32), (u32, u32)) {
        match self {
            Season::Halloween => ((10, 25), (11, 1)),
            Season::NewYear => ((12, 20), (1, 10)),
            Season::CatDay => ((3, 1), (3, 1)),
            Season::Valentine => ((2, 12), (2, 14)),
            Season::Cosmonautics => ((4, 12), (4, 12)),
            Season::Sakura => ((4, 20), (5, 31)),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Season::Halloween => "halloween",
            Season::NewYear => "newyear",
            Season::CatDay => "cats",
            Season::Valentine => "valentine",
            Season::Cosmonautics => "space",
            Season::Sakura => "sakura",
        }
    }

    /// Акцент и текст на нём.
    fn accent(self) -> (Rgba, Rgba) {
        match self {
            Season::Halloween => (hex(0xff7a1a), hex(0x1a0e05)),
            Season::NewYear => (hex(0xe8394a), hex(0xffffff)),
            Season::CatDay => (hex(0xf08a24), hex(0x1f1205)),
            Season::Valentine => (hex(0xff4f7b), hex(0x2a0711)),
            Season::Cosmonautics => (hex(0xff8a3d), hex(0x1a0b02)),
            Season::Sakura => (hex(0xff8fb8), hex(0x2a0c18)),
        }
    }

    /// Чем подкрасить фон (тёмная основа, светлая) и насколько.
    fn tint(self, dark: bool) -> (Rgba, f32) {
        match (self, dark) {
            (Season::Halloween, true) => (hex(0x2c0f42), 0.35),
            (Season::Halloween, false) => (hex(0xffe3c7), 0.35),
            (Season::NewYear, true) => (hex(0x0b2a1d), 0.35),
            (Season::NewYear, false) => (hex(0xe4f3ea), 0.4),
            (Season::CatDay, true) => (hex(0x2b1a0b), 0.3),
            (Season::CatDay, false) => (hex(0xfff0de), 0.4),
            (Season::Valentine, true) => (hex(0x2c0c18), 0.35),
            (Season::Valentine, false) => (hex(0xffe6ee), 0.45),
            (Season::Cosmonautics, true) => (hex(0x0a1440), 0.45),
            (Season::Cosmonautics, false) => (hex(0xe4eaff), 0.45),
            (Season::Sakura, true) => (hex(0x2a1220), 0.3),
            (Season::Sakura, false) => (hex(0xfff0f5), 0.5),
        }
    }
}

/// Какой праздник в этот день.
pub fn season_on(month: u32, day: u32) -> Option<Season> {
    let today = month * 100 + day;
    Season::ALL.into_iter().find(|season| {
        let ((from_month, from_day), (to_month, to_day)) = season.days();
        let (from, to) = (from_month * 100 + from_day, to_month * 100 + to_day);
        if from <= to { (from..=to).contains(&today) } else { today >= from || today <= to }
    })
}

/// Сезонный акцент и подкраска. Чёрный «Тьмы» не трогаем — OLED.
pub fn with_season(colors: Colors, season: Season, keep_black: bool) -> Colors {
    let (tint, share) = season.tint(colors.dark);
    let paint = |value: Rgba, share: f32| if keep_black { value } else { mix(value, tint, share) };
    Colors {
        background: paint(colors.background, share),
        surface: paint(colors.surface, share * 0.8),
        deep: paint(colors.deep, share * 0.6),
        accent: Some(season.accent()),
        ..colors
    }
}

/// Месяц и день из номера дня от 1970-01-01 (алгоритм Говарда Хиннанта).
pub fn month_day(days: i64) -> (u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    (month, day)
}

// ───────────────────────────── «Обложка» ─────────────────────────────

fn to_linear(value: f32) -> f32 {
    if value <= 0.040_45 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(value: f32) -> f32 {
    if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB → OKLab.
fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(to_linear);
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab → sRGB (может выйти за пределы — см. `oklch`).
fn srgb(lab: [f32; 3]) -> [f32; 3] {
    let [l, a, b] = lab;
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ]
    .map(from_linear)
}

/// OKLCH; насыщенность снижаем, пока цвет не влезет в sRGB.
fn oklch(lightness: f32, chroma: f32, hue: f32) -> Rgba {
    let mut chroma = chroma;
    loop {
        let rgb = srgb([lightness, chroma * hue.cos(), chroma * hue.sin()]);
        if rgb.iter().all(|value| (-0.001..=1.001).contains(value)) || chroma < 0.002 {
            return [rgb[0].clamp(0.0, 1.0), rgb[1].clamp(0.0, 1.0), rgb[2].clamp(0.0, 1.0), 1.0];
        }
        chroma *= 0.92;
    }
}

/// Цвета «Обложки». Основа всегда тёмная, серая обложка — системный акцент.
pub fn cover_colors(pixels: &[u8]) -> Colors {
    const BUCKETS: usize = 24;
    let count = pixels.len() / 4;
    let stride = (count / 4096).max(1);
    let mut weights = [0.0f32; BUCKETS];
    let mut vectors = [[0.0f32; 2]; BUCKETS];
    let mut peaks = [0.0f32; BUCKETS];
    let (mut sum_a, mut sum_b, mut samples) = (0.0f32, 0.0f32, 0.0f32);
    for index in (0..count).step_by(stride) {
        let pixel = &pixels[index * 4..index * 4 + 4];
        if pixel[3] < 128 {
            continue;
        }
        let [lightness, a, b] = oklab([pixel[0] as f32 / 255.0, pixel[1] as f32 / 255.0, pixel[2] as f32 / 255.0]);
        let chroma = a.hypot(b);
        sum_a += a;
        sum_b += b;
        samples += 1.0;
        if chroma < 0.03 {
            continue;
        }
        // Акцент — из ярких мест: почти чёрное и почти белое весят меньше.
        let weight = chroma * (1.0 - (lightness - 0.62).abs() * 1.3).max(0.05);
        let bucket = ((b.atan2(a) / std::f32::consts::TAU).rem_euclid(1.0) * BUCKETS as f32) as usize % BUCKETS;
        weights[bucket] += weight;
        vectors[bucket][0] += a * weight;
        vectors[bucket][1] += b * weight;
        peaks[bucket] = peaks[bucket].max(chroma);
    }
    let mut colors = base(Kind::Dark);
    if samples == 0.0 {
        return colors;
    }
    // Самый весомый тон вместе с соседями (тон на границе корзин не делится).
    let best = (0..BUCKETS)
        .max_by(|x, y| {
            let around = |index: usize| weights[index] + 0.5 * (weights[(index + 1) % BUCKETS] + weights[(index + BUCKETS - 1) % BUCKETS]);
            around(*x).total_cmp(&around(*y))
        })
        .unwrap_or(0);
    let colourful = weights[best] / samples > 0.004;
    let hue = if colourful {
        let [a, b] = vectors[best];
        b.atan2(a)
    } else {
        sum_b.atan2(sum_a)
    };
    let tone = if colourful { 0.045 } else { (sum_a.hypot(sum_b) / samples).min(0.02) };
    colors.background = oklch(0.19, tone, hue);
    colors.surface = oklch(0.245, tone * 1.1, hue);
    colors.deep = oklch(0.13, tone * 0.8, hue);
    colors.foreground = oklch(0.96, 0.012, hue);
    let [r, g, b, _] = colors.foreground;
    colors.border = [r, g, b, 0.10];
    colors.control = [r, g, b, 0.07];
    if colourful {
        let chroma = (peaks[best] * 0.9).clamp(0.09, 0.17);
        colors.accent = Some((oklch(0.76, chroma, hue), oklch(0.2, 0.03, hue)));
    }
    colors
}

/// Масштаб окна: системный, свой (проценты) и прибавка `BASE_SCALE`.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn scale_factor(system: f32, percent: u32) -> f32 {
    system * percent.clamp(SCALE_MIN, SCALE_MAX) as f32 / 100.0 * BASE_SCALE
}

// ───────────────────────────── окно ─────────────────────────────

pub struct State {
    pub kind: Cell<Kind>,
    pub seasons_on: Cell<bool>,
    pub season: Cell<Option<Season>>,
    /// ВРЕМЕННО: предпросмотр праздника из настроек (не сохраняется).
    preview: Cell<Option<Season>>,
    /// Цвета «Обложки» для играющего трека.
    cover: Cell<Option<Colors>>,
    /// Что на экране и куда идём (плавная смена).
    shown: Cell<Option<Colors>>,
    tween_from: Cell<Option<Colors>>,
    tween_to: Cell<Option<Colors>>,
    tween_start: Cell<Option<Instant>>,
    tween: slint::Timer,
    pub scale: Cell<u32>,
    pub system_scale: Cell<f32>,
    /// Окно в фокусе: только тогда украшения двигаются.
    pub focused: Cell<bool>,
    episodes: slint::Timer,
    hourly: slint::Timer,
}

impl Default for State {
    fn default() -> Self {
        Self {
            kind: Cell::new(Kind::Dark),
            seasons_on: Cell::new(true),
            season: Cell::new(None),
            preview: Cell::new(None),
            cover: Cell::new(None),
            shown: Cell::new(None),
            tween_from: Cell::new(None),
            tween_to: Cell::new(None),
            tween_start: Cell::new(None),
            tween: slint::Timer::default(),
            scale: Cell::new(100),
            system_scale: Cell::new(1.0),
            focused: Cell::new(true),
            episodes: slint::Timer::default(),
            hourly: slint::Timer::default(),
        }
    }
}

/// Сезон сегодня (отладочная сборка: `NYANOTO_SEASON=halloween` и т. п.).
fn today_season() -> Option<Season> {
    if cfg!(debug_assertions)
        && let Ok(code) = std::env::var("NYANOTO_SEASON")
    {
        return Season::ALL.into_iter().find(|season| season.code() == code);
    }
    let (month, day) = month_day(clock::local_day(clock::now()));
    season_on(month, day)
}

impl App {
    pub(crate) fn load_look_settings(&self, setting: &dyn Fn(&str) -> Option<String>) {
        let look = &self.look;
        let kind = setting(SETTING_THEME).and_then(|code| Kind::from_code(&code)).unwrap_or_else(|| {
            // Тема ещё не выбрана — светлая или тёмная, как в системе.
            let scheme = self.window.global::<Palette>().get_color_scheme();
            if scheme == slint::language::ColorScheme::Light { Kind::Light } else { Kind::Dark }
        });
        look.kind.set(kind);
        // Сезоны пока в разработке — выключены всегда.
        look.seasons_on.set(!crate::UNFINISHED && setting(SETTING_SEASONS).as_deref() != Some("0"));
        look.scale.set(setting(SETTING_SCALE).and_then(|value| value.parse().ok()).unwrap_or(100).clamp(SCALE_MIN, SCALE_MAX));
        look.season.set(today_season());
        self.window.set_theme_index(Kind::ALL.iter().position(|known| *known == kind).unwrap_or(1) as i32);
        self.window.set_seasons_enabled(look.seasons_on.get());
        self.window.set_scale_percent(look.scale.get() as f32);
        self.apply_look(false);
    }

    /// Цвета для темы, сезона и обложки — сразу или плавно.
    pub(crate) fn apply_look(&self, smooth: bool) {
        let look = &self.look;
        let kind = look.kind.get();
        let mut colors = if kind == Kind::Cover { look.cover.get().unwrap_or_else(|| base(Kind::Cover)) } else { base(kind) };
        // Предпросмотр показывается, даже если сезонные темы выключены.
        let season = look.preview.get().or_else(|| look.season.get().filter(|_| look.seasons_on.get()));
        if let Some(season) = season {
            colors = with_season(colors, season, kind == Kind::Black);
        }
        self.window.set_season(season.map_or(0, |season| season as i32));
        self.show_season_note();
        match look.shown.get() {
            Some(shown) if smooth && shown != colors => {
                look.tween_from.set(Some(shown));
                look.tween_to.set(Some(colors));
                look.tween_start.set(Some(Instant::now()));
                look.tween.start(TimerMode::Repeated, Duration::from_millis(16), || with_app(|app| app.look_tween_step()));
            }
            _ => {
                look.tween.stop();
                self.paint(&colors, 1.0, &colors);
            }
        }
        self.schedule_episode();
    }

    fn look_tween_step(&self) {
        let look = &self.look;
        let (Some(from), Some(to), Some(start)) = (look.tween_from.get(), look.tween_to.get(), look.tween_start.get()) else {
            look.tween.stop();
            return;
        };
        let share = (start.elapsed().as_secs_f32() / TWEEN.as_secs_f32()).min(1.0);
        // Плавный старт и мягкая остановка.
        let eased = share * share * (3.0 - 2.0 * share);
        self.paint(&from, eased, &to);
        if share >= 1.0 {
            look.tween.stop();
        }
    }

    /// Ставит цвета `from`, сдвинутые к `to` на долю `share`.
    fn paint(&self, from: &Colors, share: f32, to: &Colors) {
        let theme = self.window.global::<Theme>();
        let at = |a: Rgba, b: Rgba| color(mix(a, b, share));
        theme.set_background(at(from.background, to.background));
        theme.set_surface(at(from.surface, to.surface));
        theme.set_foreground(at(from.foreground, to.foreground));
        theme.set_border(at(from.border, to.border));
        theme.set_control(at(from.control, to.control));
        theme.set_deep(at(from.deep, to.deep));
        theme.set_dark(to.dark);
        // Телефон: значки строки состояния — тёмные на светлой теме.
        #[cfg(target_os = "android")]
        crate::android::set_dark_bars(to.dark);
        // Встроенные полосы прокрутки и редактор текста — в тон (светлая или тёмная).
        let scheme = if to.dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light };
        self.window.global::<Palette>().set_color_scheme(scheme);
        match (from.accent, to.accent) {
            (Some(a), Some(b)) => {
                theme.set_system_accent(false);
                theme.set_own_accent(at(a.0, b.0));
                theme.set_own_accent_text(at(a.1, b.1));
            }
            (_, Some(b)) => {
                theme.set_system_accent(false);
                theme.set_own_accent(color(b.0));
                theme.set_own_accent_text(color(b.1));
            }
            (_, None) => theme.set_system_accent(true),
        }
        theme.set_font_family(if to.mono { MONO.into() } else { slint::SharedString::new() });
        let shown = Colors {
            background: mix(from.background, to.background, share),
            surface: mix(from.surface, to.surface, share),
            foreground: mix(from.foreground, to.foreground, share),
            border: mix(from.border, to.border, share),
            control: mix(from.control, to.control, share),
            deep: mix(from.deep, to.deep, share),
            ..*to
        };
        self.look.shown.set(Some(shown));
    }

    pub(crate) fn set_theme(&self, index: usize) {
        let kind = Kind::ALL.get(index).copied().unwrap_or(Kind::Dark);
        self.look.kind.set(kind);
        self.save_setting(SETTING_THEME, kind.code());
        if kind == Kind::Cover {
            let image = self.window.get_now_cover();
            self.look_cover_image(&image);
        }
        self.apply_look(true);
    }

    pub(crate) fn set_seasons(&self, on: bool) {
        self.look.seasons_on.set(on);
        self.save_flag(SETTING_SEASONS, on);
        self.apply_look(true);
    }

    /// Обложка трека в панели загрузилась: для «Обложки» — её цвета.
    pub(crate) fn look_cover_image(&self, image: &Image) {
        if self.look.kind.get() != Kind::Cover {
            return;
        }
        let colors = image.to_rgba8().map(|buffer| cover_colors(buffer.as_bytes()));
        self.look.cover.set(colors);
        self.apply_look(true);
    }

    /// Временно: 0 — по календарю, 1…6 — праздник сейчас.
    pub(crate) fn season_preview(&self, index: i32) {
        let preview = usize::try_from(index - 1).ok().and_then(|index| Season::ALL.get(index).copied());
        self.look.preview.set(preview);
        self.apply_look(true);
        if preview.is_some() {
            self.season_play();
        } else {
            self.window.set_season_episode(0);
        }
    }

    /// ВРЕМЕННО: сценка праздника — сейчас же.
    pub(crate) fn season_play(&self) {
        let season = self.look.preview.get().or_else(|| self.look.season.get().filter(|_| self.look.seasons_on.get()));
        if let Some(season) = season {
            self.run_episode(season);
        }
    }

    fn preview_names(&self) {
        let lex = self.lex();
        // В списке — с большой буквы («весна-сакура» в строке «Сейчас: …» — с маленькой).
        let capital = |name: &str| {
            let mut chars = name.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        };
        let names: Vec<slint::SharedString> =
            std::iter::once(lex.season_none).chain(lex.season_names.iter().copied()).map(|name| capital(name).into()).collect();
        self.window.set_season_preview_names(ModelRc::from(std::rc::Rc::new(VecModel::from(names))));
    }

    /// Раз в час: не начался ли (не кончился ли) праздник.
    pub(crate) fn season_check(&self) {
        let season = today_season();
        if season != self.look.season.get() {
            self.look.season.set(season);
            self.apply_look(true);
        }
    }

    fn show_season_note(&self) {
        let lex = self.lex();
        let note = match self.look.season.get() {
            Some(season) if self.look.seasons_on.get() => {
                let index = Season::ALL.iter().position(|known| *known == season).unwrap_or(0);
                lex.season_now.replace("{}", lex.season_names[index])
            }
            _ => String::new(),
        };
        self.window.set_season_note(note.into());
    }

    /// Карточки тем в настройках: имя и цвета.
    pub(crate) fn show_theme_cards(&self) {
        self.preview_names();
        let lex = self.lex();
        let cards: Vec<ThemeCard> = Kind::ALL
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let colors = base(*kind);
                let accent = match kind {
                    Kind::Cover => hex(0xe0805a),
                    _ => colors.accent.map_or(hex(0x60cdff), |(accent, _)| accent),
                };
                ThemeCard {
                    name: lex.theme_names[index].into(),
                    background: color(colors.background),
                    surface: color(colors.surface),
                    foreground: color(colors.foreground),
                    accent: color(accent),
                    system_accent: colors.accent.is_none() && *kind != Kind::Cover,
                    cover: *kind == Kind::Cover,
                }
            })
            .collect();
        let (first, more) = cards.split_at(4.min(cards.len()));
        self.window.set_theme_cards(ModelRc::from(std::rc::Rc::new(VecModel::from(first.to_vec()))));
        self.window.set_theme_cards_more(ModelRc::from(std::rc::Rc::new(VecModel::from(more.to_vec()))));
    }

    // ───────────────────────── украшения ─────────────────────────

    /// Окно получило или потеряло фокус: украшения двигаются только в фокусе.
    pub(crate) fn look_focus(&self, focused: bool) {
        self.look.focused.set(focused);
        if !focused {
            self.window.set_season_episode(0);
        }
        self.schedule_episode();
    }

    /// Следующая сценка сезона через пару минут, первая — вскоре после запуска.
    fn schedule_episode(&self) {
        let look = &self.look;
        let active = (look.preview.get().is_some() || (look.seasons_on.get() && look.season.get().is_some())) && look.focused.get();
        if !active {
            look.episodes.stop();
            return;
        }
        if look.episodes.running() {
            return;
        }
        let first = self.window.get_season_episode_count() == 0;
        let wait = if first { Duration::from_secs(12) } else { Duration::from_secs(150 + u64::from(self.next_random() as u32 % 90)) };
        look.episodes.start(TimerMode::SingleShot, wait, || with_app(|app| app.start_episode()));
    }

    fn start_episode(&self) {
        let look = &self.look;
        let Some(season) = look.preview.get().or_else(|| look.season.get().filter(|_| look.seasons_on.get())).filter(|_| look.focused.get())
        else {
            return;
        };
        // В энергосбережении — без движения (узнаём в фоне: может ждать D-Bus).
        let _ = std::thread::Builder::new().name("nyanoto-season".into()).spawn(move || {
            let saving = load::power_saving() == Some(true);
            in_ui(move |app| {
                let current = app.look.preview.get().or(app.look.season.get());
                if !saving && app.look.focused.get() && current == Some(season) {
                    app.run_episode(season);
                }
                app.schedule_episode();
            });
        });
    }

    /// Сценка праздника: показать и убрать, когда кончится.
    fn run_episode(&self, season: Season) {
        let count = self.window.get_season_episode_count() + 1;
        self.window.set_season_episode_count(count);
        // Тот же праздник подряд — сценку пересоздаём.
        self.window.set_season_episode(0);
        slint::Timer::single_shot(Duration::from_millis(30), move || {
            with_app(|app| {
                if app.window.get_season_episode_count() == count {
                    app.window.set_season_episode(season as i32);
                }
            });
        });
        let length = Duration::from_secs(match season {
            Season::NewYear | Season::Sakura => 22,
            Season::Valentine => 9,
            Season::CatDay => 6,
            _ => 7,
        });
        slint::Timer::single_shot(length, move || {
            with_app(|app| {
                if app.window.get_season_episode_count() == count {
                    app.window.set_season_episode(0);
                }
            });
        });
    }

    // ───────────────────────── масштаб ─────────────────────────

    /// Масштаб, проверка праздника раз в час и первая сценка.
    pub(crate) fn look_start(&self) {
        self.look_scale_when_shown(0);
        self.look.hourly.start(TimerMode::Repeated, Duration::from_secs(3600), || with_app(|app| app.season_check()));
        self.schedule_episode();
    }

    /// Телефон: масштаб — плотность экрана, своего поверх нет.
    #[cfg(target_os = "android")]
    fn look_scale_when_shown(&self, _tries: u32) {
        self.look.system_scale.set(self.window.window().scale_factor());
    }

    /// Системное окно появляется чуть позже, и Slint ставит свой масштаб — наш после.
    #[cfg(not(target_os = "android"))]
    fn look_scale_when_shown(&self, tries: u32) {
        use slint::winit_030::WinitWindowAccessor;
        match self.window.window().with_winit_window(|winit| winit.scale_factor() as f32) {
            Some(system) => {
                self.look.system_scale.set(system);
                self.apply_scale();
            }
            None if tries < 200 => {
                slint::Timer::single_shot(Duration::from_millis(25), move || with_app(|app| app.look_scale_when_shown(tries + 1)));
            }
            None => {}
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn apply_scale(&self) {
        self.window.set_scale_percent(self.look.scale.get() as f32);
    }

    /// Масштаб окна: системный × свой.
    /// Slint держит размер в точках, и буфер выходил меньше окна — падало. Поэтому пиксели сохраняем.
    #[cfg(not(target_os = "android"))]
    pub(crate) fn apply_scale(&self) {
        use slint::winit_030::WinitWindowAccessor;
        let factor = scale_factor(self.look.system_scale.get(), self.look.scale.get());
        let window = self.window.window();
        let physical = window.with_winit_window(|winit| winit.inner_size());
        window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: factor });
        if let Some(size) = physical.filter(|size| size.width > 0 && size.height > 0) {
            let size = slint::LogicalSize::new(size.width as f32 / factor, size.height as f32 / factor);
            window.dispatch_event(slint::platform::WindowEvent::Resized { size });
        }
        self.window.set_scale_percent(self.look.scale.get() as f32);
        // Наименьший размер пересчитывается, только если меняется в своих единицах.
        let nudge = if self.window.get_min_nudge() > 0.0 { 0.0 } else { 0.01 };
        self.window.set_min_nudge(nudge);
        // Картинки исполнителей — точно в новый размер (иначе края кругов лесенкой).
        self.reload_artist_pictures();
    }

    /// Система сменила масштаб (другой монитор, настройки): наш — поверх.
    pub(crate) fn system_scale_changed(&self, system: f32) {
        self.look.system_scale.set(system);
        slint::Timer::single_shot(Duration::ZERO, || with_app(|app| app.apply_scale()));
    }

    pub(crate) fn set_scale(&self, percent: f32, done: bool) {
        let percent = ((percent / 5.0).round() * 5.0) as u32;
        self.look.scale.set(percent.clamp(SCALE_MIN, SCALE_MAX));
        if done {
            // Не изнутри обработки нажатия (ползунок, клавиши) — следующим шагом.
            slint::Timer::single_shot(Duration::ZERO, || with_app(|app| app.apply_scale()));
            self.save_setting(SETTING_SCALE, &self.look.scale.get().to_string());
        }
    }

    /// Ctrl+«+» / Ctrl+«−» / Ctrl+0.
    pub(crate) fn zoom(&self, step: i32) {
        let now = self.look.scale.get();
        let next = match step {
            0 => 100,
            1 => (now / SCALE_STEP + 1) * SCALE_STEP,
            _ => (now.saturating_sub(1) / SCALE_STEP) * SCALE_STEP,
        };
        self.set_scale(next.clamp(SCALE_MIN, SCALE_MAX) as f32, true);
    }
}

/// Моноширинный шрифт «Терминала» — встроен в разметку (JetBrains Mono, OFL).
const MONO: &str = "JetBrains Mono";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seasons_fall_on_their_days() {
        assert_eq!(season_on(10, 24), None);
        assert_eq!(season_on(10, 25), Some(Season::Halloween));
        assert_eq!(season_on(11, 1), Some(Season::Halloween));
        assert_eq!(season_on(12, 31), Some(Season::NewYear));
        assert_eq!(season_on(1, 10), Some(Season::NewYear));
        assert_eq!(season_on(1, 11), None);
        assert_eq!(season_on(3, 1), Some(Season::CatDay));
        assert_eq!(season_on(2, 13), Some(Season::Valentine));
        assert_eq!(season_on(4, 12), Some(Season::Cosmonautics));
        assert_eq!(season_on(4, 25), Some(Season::Sakura));
        assert_eq!(season_on(6, 1), None);
    }

    #[test]
    fn month_day_round_trips() {
        for (year, month, day) in [(2026, 10, 21), (2024, 2, 29), (2027, 1, 1), (2026, 12, 31)] {
            let days = clock::days_from_civil(year, month, day);
            assert_eq!(month_day(days), (month as u32, day as u32));
        }
    }

    #[test]
    fn black_theme_stays_black_in_season() {
        let colors = with_season(base(Kind::Black), Season::Halloween, true);
        assert_eq!(colors.background, hex(0x000000));
        assert!(colors.accent.is_some());
        let dark = with_season(base(Kind::Dark), Season::Halloween, false);
        assert_ne!(dark.background, base(Kind::Dark).background, "тёмная слегка подкрашена");
    }

    #[test]
    fn oklab_round_trips() {
        for rgb in [[0.2, 0.5, 0.8], [1.0, 0.0, 0.0], [0.9, 0.9, 0.1]] {
            let back = srgb(oklab(rgb));
            for (a, b) in rgb.iter().zip(back) {
                assert!((a - b).abs() < 1e-3, "{rgb:?} → {back:?}");
            }
        }
    }

    fn image(colors: &[([u8; 3], usize)]) -> Vec<u8> {
        let mut pixels = Vec::new();
        for (rgb, count) in colors {
            for _ in 0..*count {
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        pixels
    }

    /// Красная обложка — красный акцент и тёмный фон, серая — без своего акцента.
    #[test]
    fn cover_colors_follow_the_picture() {
        let red = cover_colors(&image(&[([200, 30, 40], 600), ([20, 20, 20], 400)]));
        let (accent, _) = red.accent.expect("яркая обложка — свой акцент");
        assert!(accent[0] > accent[1] + 0.2 && accent[0] > accent[2] + 0.2, "{accent:?}");
        assert!(red.background[0] < 0.25 && red.background[0] > red.background[2], "{:?}", red.background);
        assert!(red.dark);
        let grey = cover_colors(&image(&[([128, 128, 128], 500), ([30, 30, 30], 500)]));
        assert_eq!(grey.accent, None);
        // Синяя — синий акцент.
        let blue = cover_colors(&image(&[([30, 60, 210], 700), ([240, 240, 240], 300)]));
        let (accent, _) = blue.accent.unwrap();
        assert!(accent[2] > accent[0] + 0.2, "{accent:?}");
    }

    #[test]
    fn scale_multiplies() {
        assert!((scale_factor(2.0, 100) - 2.0 * BASE_SCALE).abs() < 1e-6);
        assert!((scale_factor(1.0, 150) - 1.5 * BASE_SCALE).abs() < 1e-6);
        assert!((scale_factor(1.0, 10) - 0.8 * BASE_SCALE).abs() < 1e-6, "не меньше 80 %");
    }
}
