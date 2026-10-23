//! Эквалайзер в окне: полосы, профили, AutoEQ, профили по устройству.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::path::Path;
use std::time::Duration;

use nyanoto_core::{EqBand, Equalizer, FilterKind, MAX_BANDS};
use serde::{Deserialize, Serialize};
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, TimerMode, VecModel};

use crate::{App, EqRow, with_app};

pub const SETTING_EQ: &str = "eq";
pub const SETTING_DEVICE_PROFILES: &str = "device_profiles";
const SETTING_EQ_STATE: &str = "eq_state";
const SETTING_EQ_PROFILES: &str = "eq_profiles";
const SETTING_EQ_PROFILE: &str = "eq_profile";
const SETTING_DEVICE_MAP: &str = "device_map";

const CURVE_WIDTH: u32 = 640;
const CURVE_HEIGHT: u32 = 120;
/// Пределы графика по вертикали, дБ.
const CURVE_RANGE_DB: f32 = 15.0;

#[derive(Serialize, Deserialize, Clone)]
struct StoredBand {
    kind: String,
    freq: f32,
    gain_db: f32,
    q: f32,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct StoredEq {
    enabled: bool,
    preamp_db: f32,
    bands: Vec<StoredBand>,
}

/// Что запомнено для устройства вывода.
#[derive(Serialize, Deserialize, Clone)]
pub struct DeviceProfile {
    profile: String,
    volume: f32,
}

const KINDS: [(FilterKind, &str); 5] = [
    (FilterKind::Peaking, "PK"),
    (FilterKind::LowShelf, "LSC"),
    (FilterKind::HighShelf, "HSC"),
    (FilterKind::LowPass, "LP"),
    (FilterKind::HighPass, "HP"),
];

fn kind_index(kind: FilterKind) -> i32 {
    KINDS.iter().position(|(item, _)| *item == kind).unwrap_or(0) as i32
}

fn kind_at(index: i32) -> FilterKind {
    KINDS.get(index.max(0) as usize).map_or(FilterKind::Peaking, |(kind, _)| *kind)
}

impl From<&Equalizer> for StoredEq {
    fn from(eq: &Equalizer) -> Self {
        StoredEq {
            enabled: eq.enabled,
            preamp_db: eq.preamp_db,
            bands: eq
                .bands
                .iter()
                .map(|band| StoredBand {
                    kind: KINDS[kind_index(band.kind) as usize].1.to_string(),
                    freq: band.freq,
                    gain_db: band.gain_db,
                    q: band.q,
                })
                .collect(),
        }
    }
}

impl From<StoredEq> for Equalizer {
    fn from(stored: StoredEq) -> Self {
        Equalizer {
            enabled: stored.enabled,
            preamp_db: stored.preamp_db,
            bands: stored
                .bands
                .into_iter()
                .take(MAX_BANDS)
                .map(|band| EqBand {
                    kind: KINDS.iter().find(|(_, code)| *code == band.kind).map_or(FilterKind::Peaking, |(kind, _)| *kind),
                    freq: band.freq,
                    gain_db: band.gain_db,
                    q: band.q,
                })
                .collect(),
        }
    }
}

/// АЧХ для окна: маска, линия нуля бледнее, кривая плотнее.
fn curve(eq: &Equalizer) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(CURVE_WIDTH, CURVE_HEIGHT);
    let pixels = buffer.make_mut_slice();
    let to_row = |db: f32| {
        let share = 0.5 - db.clamp(-CURVE_RANGE_DB, CURVE_RANGE_DB) / (2.0 * CURVE_RANGE_DB);
        (share * (CURVE_HEIGHT - 1) as f32).round() as i32
    };
    let zero = to_row(0.0);
    for column in 0..CURVE_WIDTH as usize {
        pixels[zero as usize * CURVE_WIDTH as usize + column] = Rgba8Pixel { r: 255, g: 255, b: 255, a: 70 };
    }
    let mut previous: Option<i32> = None;
    for column in 0..CURVE_WIDTH as usize {
        // От 20 Гц до 20 кГц по логарифмической шкале.
        let freq = 20.0 * 1000f32.powf(column as f32 / (CURVE_WIDTH - 1) as f32);
        let row = to_row(eq.response_db(freq, 48_000));
        let (from, to) = match previous {
            Some(last) => (last.min(row), last.max(row)),
            None => (row, row),
        };
        for y in from - 1..=to + 1 {
            if (0..CURVE_HEIGHT as i32).contains(&y) {
                pixels[y as usize * CURVE_WIDTH as usize + column] = Rgba8Pixel { r: 255, g: 255, b: 255, a: 255 };
            }
        }
        previous = Some(row);
    }
    buffer
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

impl App {
    /// Читает настройки эквалайзера и профилей из базы.
    pub(crate) fn load_equalizer(&self) {
        let setting = |key: &str| self.library.as_ref().and_then(|library| library.setting(key).ok().flatten());
        // Эквалайзер и профили — в разработке: выключены всегда.
        self.eq_on.set(!crate::UNFINISHED && setting(SETTING_EQ).as_deref() == Some("1"));
        self.profiles_on.set(!crate::UNFINISHED && setting(SETTING_DEVICE_PROFILES).as_deref() == Some("1"));
        let eq = setting(SETTING_EQ_STATE)
            .and_then(|text| serde_json::from_str::<StoredEq>(&text).ok())
            .map(Equalizer::from)
            .unwrap_or_else(Equalizer::ten_band);
        *self.eq.borrow_mut() = eq;
        let profiles: BTreeMap<String, StoredEq> = setting(SETTING_EQ_PROFILES)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        *self.eq_profiles.borrow_mut() = profiles.into_iter().map(|(name, eq)| (name, eq.into())).collect();
        *self.eq_profile.borrow_mut() = setting(SETTING_EQ_PROFILE).unwrap_or_default();
        *self.device_map.borrow_mut() = setting(SETTING_DEVICE_MAP)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        self.window.set_eq_enabled(self.eq_on.get());
        self.window.set_device_profiles(self.profiles_on.get());
        // Громкость устройства, на котором сейчас заиграет, видна сразу.
        let known = nyanoto_core::default_device_name().and_then(|name| self.device_map.borrow().get(&name).cloned());
        if self.profiles_on.get()
            && let Some(profile) = known
        {
            self.window.set_volume(profile.volume);
        }
        self.send_device_volumes();
        self.apply_equalizer();
    }

    /// Громкости устройств — движку, чтобы ставил до первого звука.
    pub(crate) fn send_device_volumes(&self) {
        let volumes = if self.profiles_on.get() {
            let map = self.device_map.borrow();
            map.iter().map(|(name, profile)| (name.clone(), profile.volume * profile.volume)).collect()
        } else {
            std::collections::HashMap::new()
        };
        self.engine.set_device_volumes(volumes);
    }

    /// Отдаёт эквалайзер движку; без модуля — выключенный.
    pub(crate) fn apply_equalizer(&self) {
        let mut eq = self.eq.borrow().clone();
        eq.enabled &= self.eq_on.get();
        self.window.set_eq_active(eq.enabled);
        self.engine.set_equalizer(eq);
        self.show_signal_path();
    }

    /// Правка полосы или предусилителя: сразу в движок, в базу — через полсекунды.
    fn eq_changed(&self, redraw_rows: bool) {
        self.apply_equalizer();
        self.show_eq_curve();
        if redraw_rows {
            self.show_eq_rows();
        }
        self.eq_save_timer.start(TimerMode::SingleShot, Duration::from_millis(500), || {
            with_app(|app| {
                let stored = StoredEq::from(&*app.eq.borrow());
                app.save_setting(SETTING_EQ_STATE, &json(&stored));
            });
        });
    }

    pub(crate) fn open_eq(&self) {
        self.show_eq_rows();
        self.show_eq_curve();
        self.show_eq_profiles();
        let device = self.device.borrow().clone();
        self.window.set_eq_device(device.into());
        self.window.set_eq_message(SharedString::new());
        self.window.set_eq_open(true);
    }

    fn show_eq_rows(&self) {
        let eq = self.eq.borrow();
        let rows: Vec<EqRow> = eq
            .bands
            .iter()
            .map(|band| EqRow {
                kind: kind_index(band.kind),
                freq: format!("{}", band.freq).into(),
                gain: band.gain_db,
                q: format!("{:.2}", band.q).into(),
            })
            .collect();
        self.eq_rows.set_vec(rows);
        self.window.set_eq_preamp(eq.preamp_db);
    }

    fn show_eq_curve(&self) {
        let mut eq = self.eq.borrow().clone();
        eq.enabled = true;
        self.window.set_eq_curve(Image::from_rgba8(curve(&eq)));
    }

    fn show_eq_profiles(&self) {
        let names: Vec<SharedString> = self.eq_profiles.borrow().keys().map(SharedString::from).collect();
        let current = self.eq_profile.borrow().clone();
        let index = names.iter().position(|name| name.as_str() == current).map_or(-1, |index| index as i32);
        self.window.set_eq_profiles(ModelRc::from(Rc::new(VecModel::from(names))));
        self.window.set_eq_profile_index(index);
        self.window.set_eq_profile_name(current.into());
    }

    pub(crate) fn eq_set_active(&self, on: bool) {
        self.eq.borrow_mut().enabled = on;
        self.eq_changed(false);
    }

    pub(crate) fn eq_set_preamp(&self, db: f32) {
        self.eq.borrow_mut().preamp_db = (db * 10.0).round() / 10.0;
        self.eq_changed(false);
    }

    pub(crate) fn eq_edit(&self, index: usize, edit: impl FnOnce(&mut EqBand)) {
        if let Some(band) = self.eq.borrow_mut().bands.get_mut(index) {
            edit(band);
        }
        self.eq_changed(false);
    }

    pub(crate) fn eq_set_kind(&self, index: usize, kind: i32) {
        self.eq_edit(index, |band| band.kind = kind_at(kind));
        if let Some(mut row) = self.eq_rows.row_data(index) {
            row.kind = kind;
            self.eq_rows.set_row_data(index, row);
        }
    }

    pub(crate) fn eq_add(&self) {
        {
            let mut eq = self.eq.borrow_mut();
            if eq.bands.len() >= MAX_BANDS {
                return;
            }
            eq.bands.push(EqBand { kind: FilterKind::Peaking, freq: 1000.0, gain_db: 0.0, q: 1.0 });
        }
        self.eq_changed(true);
    }

    pub(crate) fn eq_remove(&self, index: usize) {
        {
            let mut eq = self.eq.borrow_mut();
            if index < eq.bands.len() {
                eq.bands.remove(index);
            }
        }
        self.eq_changed(true);
    }

    pub(crate) fn eq_reset(&self) {
        let enabled = self.eq.borrow().enabled;
        *self.eq.borrow_mut() = Equalizer { enabled, ..Equalizer::ten_band() };
        self.eq_changed(true);
    }

    pub(crate) fn eq_choose_profile(&self, index: usize) {
        let chosen = self.eq_profiles.borrow().iter().nth(index).map(|(name, eq)| (name.clone(), eq.clone()));
        let Some((name, eq)) = chosen else {
            return;
        };
        *self.eq.borrow_mut() = Equalizer { enabled: true, ..eq };
        *self.eq_profile.borrow_mut() = name.clone();
        self.save_setting(SETTING_EQ_PROFILE, &name);
        self.window.set_eq_profile_name(name.into());
        self.eq_changed(true);
        self.remember_device();
    }

    pub(crate) fn eq_save_profile(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let eq = self.eq.borrow().clone();
        self.eq_profiles.borrow_mut().insert(name.to_string(), eq);
        *self.eq_profile.borrow_mut() = name.to_string();
        self.save_profiles();
        self.save_setting(SETTING_EQ_PROFILE, name);
        self.show_eq_profiles();
        self.window.set_eq_message(format!("{}: {name}", self.lex().eq_saved).into());
        self.remember_device();
    }

    pub(crate) fn eq_delete_profile(&self) {
        let name = self.eq_profile.borrow().clone();
        if self.eq_profiles.borrow_mut().remove(&name).is_some() {
            self.eq_profile.borrow_mut().clear();
            self.save_profiles();
            self.save_setting(SETTING_EQ_PROFILE, "");
            self.show_eq_profiles();
        }
    }

    fn save_profiles(&self) {
        let stored: BTreeMap<String, StoredEq> =
            self.eq_profiles.borrow().iter().map(|(name, eq)| (name.clone(), StoredEq::from(eq))).collect();
        self.save_setting(SETTING_EQ_PROFILES, &json(&stored));
    }

    /// AutoEQ становится текущим и сохраняется под именем файла.
    pub(crate) fn eq_import(&self) {
        self.choose(crate::choose::Want::File("AutoEQ", &["txt"]), "", |app, path| app.eq_import_file(&path));
    }

    fn eq_import_file(&self, path: &Path) {
        let lex = self.lex();
        let text = std::fs::read(path).map(|bytes| nyanoto_library::decode_text(&bytes).0);
        match text.map_err(|error| error.to_string()).and_then(|text| Equalizer::parse_autoeq(&text)) {
            Ok(eq) => {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().replace(" ParametricEQ", ""))
                    .unwrap_or_else(|| "AutoEQ".into());
                *self.eq.borrow_mut() = eq;
                self.eq_save_profile(&name);
                self.eq_changed(true);
                self.window.set_eq_message(format!("{}: {name}", lex.eq_imported).into());
            }
            Err(error) => self
                .window
                .set_eq_message(format!("{}: {error}", lex.eq_import_failed).into()),
        }
    }

    // ───────────────────── профили по устройству вывода ─────────────────────

    /// Своё у устройства — ставим, новое — запоминаем текущее.
    pub(crate) fn device_opened(&self, device: String) {
        let previous = self.device.replace(device.clone());
        self.window.set_eq_device(device.clone().into());
        if !self.profiles_on.get() || previous == device {
            return;
        }
        if !previous.is_empty() {
            let volume = self.window.get_volume();
            if let Some(entry) = self.device_map.borrow_mut().get_mut(&previous) {
                entry.volume = volume;
            }
        }
        let known = self.device_map.borrow().get(&device).cloned();
        match known {
            Some(profile) => {
                self.window.set_volume(profile.volume);
                self.set_volume(profile.volume);
                let eq = self.eq_profiles.borrow().get(&profile.profile).cloned();
                if let Some(eq) = eq {
                    *self.eq.borrow_mut() = Equalizer { enabled: true, ..eq };
                    *self.eq_profile.borrow_mut() = profile.profile.clone();
                    self.eq_changed(true);
                }
                let lex = self.lex();
                self.window.set_status(
                    format!("{}: {device} · {}", lex.eq_device, if profile.profile.is_empty() { "—" } else { &profile.profile }).into(),
                );
            }
            None => self.remember_device(),
        }
        self.save_device_map();
    }

    /// Текущие профиль и громкость — для текущего устройства.
    pub(crate) fn remember_device(&self) {
        let device = self.device.borrow().clone();
        if !self.profiles_on.get() || device.is_empty() {
            return;
        }
        let profile = DeviceProfile { profile: self.eq_profile.borrow().clone(), volume: self.window.get_volume() };
        self.device_map.borrow_mut().insert(device, profile);
        self.save_device_map();
    }

    pub(crate) fn save_device_map(&self) {
        if !self.profiles_on.get() {
            return;
        }
        let volume = self.window.get_volume();
        let device = self.device.borrow().clone();
        if let Some(entry) = self.device_map.borrow_mut().get_mut(&device) {
            entry.volume = volume;
        }
        let text = json(&*self.device_map.borrow());
        self.save_setting(SETTING_DEVICE_MAP, &text);
        self.send_device_volumes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_form_round_trips() {
        let mut eq = Equalizer::ten_band();
        eq.preamp_db = -4.5;
        eq.bands[0].kind = FilterKind::LowShelf;
        eq.bands[9].kind = FilterKind::HighPass;
        eq.bands[3].gain_db = 2.5;
        let text = json(&StoredEq::from(&eq));
        let back: Equalizer = serde_json::from_str::<StoredEq>(&text).unwrap().into();
        assert_eq!(back, eq);
    }

    #[test]
    fn curve_shows_a_lifted_band() {
        let eq = Equalizer {
            enabled: true,
            preamp_db: 0.0,
            bands: vec![EqBand { kind: FilterKind::Peaking, freq: 1000.0, gain_db: 12.0, q: 1.0 }],
        };
        let image = curve(&eq);
        let column_of = |freq: f32| ((freq / 20.0).log10() / 3.0 * (CURVE_WIDTH - 1) as f32).round() as u32;
        let top_row = |column: u32| {
            (0..CURVE_HEIGHT).find(|row| image.as_slice()[(row * CURVE_WIDTH + column) as usize].a == 255).unwrap()
        };
        assert!(top_row(column_of(1000.0)) < top_row(column_of(50.0)), "на 1 кГц кривая выше");
    }
}
