//! Нагрузка и энергосбережение — чтобы «Современные» сцены уступали «Обычным».
//! Нагрузка — доля процессора у других программ (свою вычитаем, иначе сцены выключат сами себя).
//! Linux/Android — `/proc/stat`, Windows — `GetSystemTimes`. Энергосбережение — power-profiles-daemon,
//! `platform_profile` или «экономия заряда».

use std::time::{Duration, Instant};

/// Когда уступать и когда возвращать.
const BUSY_FROM: f32 = 0.7;
const BUSY_FOR: Duration = Duration::from_secs(5);
const CALM_BELOW: f32 = 0.5;
const CALM_FOR: Duration = Duration::from_secs(30);

/// Почему сейчас «Обычные».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Load,
    Power,
}

/// Счётчики времени процессора: всё, простой и своё (в одних единицах).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Times {
    pub total: u64,
    pub idle: u64,
    pub own: u64,
}

/// Доля процессора, занятая другими между двумя замерами.
pub fn others_share(before: Times, after: Times) -> Option<f32> {
    let total = after.total.checked_sub(before.total)?;
    if total == 0 {
        return None;
    }
    let idle = after.idle.saturating_sub(before.idle);
    let own = after.own.saturating_sub(before.own);
    let others = total.saturating_sub(idle).saturating_sub(own);
    Some((others as f32 / total as f32).clamp(0.0, 1.0))
}

/// Занято дольше `BUSY_FOR` — уступить, спокойно дольше `CALM_FOR` — вернуть.
#[derive(Default)]
pub struct Watch {
    last: Option<Times>,
    busy_since: Option<Instant>,
    calm_since: Option<Instant>,
    pub busy: bool,
}

impl Watch {
    /// Очередной замер. `share` — доля других (если её удалось узнать).
    pub fn feed(&mut self, share: Option<f32>, now: Instant) -> bool {
        let Some(share) = share else {
            return self.busy;
        };
        if share >= BUSY_FROM {
            self.calm_since = None;
            let since = *self.busy_since.get_or_insert(now);
            if now.duration_since(since) >= BUSY_FOR {
                self.busy = true;
            }
        } else {
            self.busy_since = None;
            if share < CALM_BELOW {
                let since = *self.calm_since.get_or_insert(now);
                if now.duration_since(since) >= CALM_FOR {
                    self.busy = false;
                }
            } else {
                self.calm_since = None;
            }
        }
        self.busy
    }

    /// Замер по системным счётчикам.
    pub fn sample(&mut self, now: Instant) -> bool {
        let times = read_times();
        let share = match (self.last, times) {
            (Some(before), Some(after)) => others_share(before, after),
            _ => None,
        };
        self.last = times;
        self.feed(share, now)
    }

    /// Сцены не видны: начать заново, когда снова понадобятся.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_times() -> Option<Times> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let fields: Vec<u64> = line.split_whitespace().skip(1).take(8).filter_map(|field| field.parse().ok()).collect();
    if fields.len() < 5 {
        return None;
    }
    // user nice system idle iowait irq softirq steal
    let total: u64 = fields.iter().sum();
    let idle = fields[3] + fields[4];
    // Своё время: utime и stime в /proc/self/stat (после имени в скобках).
    let own_stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after_name = own_stat.rsplit_once(')')?.1;
    let own_fields: Vec<&str> = after_name.split_whitespace().collect();
    let own = own_fields.get(11)?.parse::<u64>().ok()? + own_fields.get(12)?.parse::<u64>().ok()?;
    Some(Times { total, idle, own })
}

#[cfg(windows)]
fn read_times() -> Option<Times> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, GetSystemTimes};
    let ticks = |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    let (mut created, mut exited, mut own_kernel, mut own_user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: указатели на свои переменные, функции только пишут в них.
    unsafe {
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
        GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut own_kernel, &mut own_user).ok()?;
    }
    // Время ядра в Windows включает простой.
    Some(Times {
        total: ticks(kernel) + ticks(user),
        idle: ticks(idle),
        own: ticks(own_kernel) + ticks(own_user),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
fn read_times() -> Option<Times> {
    None
}

/// `None` — узнать нельзя. Может ждать D-Bus — не из потока окна.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
pub fn power_saving() -> Option<bool> {
    // power-profiles-daemon: новое имя и прежнее.
    if let Ok(connection) = zbus::blocking::Connection::system() {
        for (service, path) in [
            ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
            ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
        ] {
            let profile = zbus::blocking::Proxy::new(&connection, service, path, service)
                .and_then(|proxy| proxy.get_property::<String>("ActiveProfile"));
            if let Ok(profile) = profile {
                return Some(profile == "power-saver");
            }
        }
    }
    // Без демона — профиль платформы в ядре (ноутбуки с ACPI).
    std::fs::read_to_string("/sys/firmware/acpi/platform_profile")
        .ok()
        .map(|profile| matches!(profile.trim(), "low-power" | "quiet" | "cool"))
}

#[cfg(windows)]
pub fn power_saving() -> Option<bool> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: указатель на свою переменную.
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    Some(status.SystemStatusFlag & 1 == 1)
}

#[cfg(not(any(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))), windows)))]
pub fn power_saving() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_time_is_not_counted_as_load() {
        let before = Times { total: 1000, idle: 600, own: 100 };
        // Из 400 тиков 300 простой, 80 — своё: другим осталось 20.
        let after = Times { total: 1400, idle: 900, own: 180 };
        let share = others_share(before, after).unwrap();
        assert!((share - 0.05).abs() < 1e-6, "{share}");
        assert_eq!(others_share(after, after), None);
    }

    #[test]
    fn busy_after_five_seconds_back_after_half_a_minute_of_calm() {
        let mut watch = Watch::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        // Короткий всплеск не считается.
        assert!(!watch.feed(Some(0.9), at(0)));
        assert!(!watch.feed(Some(0.9), at(4)));
        assert!(!watch.feed(Some(0.2), at(5)));
        // Долгая нагрузка — уступаем.
        assert!(!watch.feed(Some(0.8), at(10)));
        assert!(watch.feed(Some(0.8), at(16)));
        // Средняя нагрузка не возвращает, спокойствие — только через 30 с.
        assert!(watch.feed(Some(0.6), at(20)));
        assert!(watch.feed(Some(0.1), at(22)));
        assert!(watch.feed(Some(0.1), at(40)));
        assert!(!watch.feed(Some(0.1), at(53)));
        // Замера нет — ничего не меняется.
        assert!(!watch.feed(None, at(60)));
    }

    /// `cargo test -p nyanoto-app power_saving_is_read -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn power_saving_is_read() {
        println!("энергосбережение: {:?}", power_saving());
    }
}
