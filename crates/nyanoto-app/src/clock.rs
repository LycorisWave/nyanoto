//! Местное время для подписей. Смещение — на каждый момент: летнее время тогда было своё.

use std::time::{SystemTime, UNIX_EPOCH};

/// Смещение местного времени от UTC в секундах в момент `unix`.
#[cfg(unix)]
pub fn offset_at(unix: i64) -> i64 {
    // time_t бывает 32-битным (старые 32-битные системы) — тогда проверка нужна.
    #[allow(irrefutable_let_patterns)]
    let Ok(time) = libc::time_t::try_from(unix) else {
        return 0;
    };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r пишет только в переданную структуру и потокобезопасна.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return 0;
    }
    // На 32-битных системах (Android armv7) это i32, на 64-битных — уже i64.
    #[allow(clippy::useless_conversion)]
    i64::from(tm.tm_gmtoff)
}

/// Смещение от UTC в момент `unix` — Windows сама учтёт летнее время.
#[cfg(windows)]
pub fn offset_at(unix: i64) -> i64 {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME — сотни наносекунд от 1601 года.
    let Some(ticks) = unix.checked_add(11_644_473_600).and_then(|seconds| seconds.checked_mul(10_000_000)) else {
        return 0;
    };
    let Ok(ticks) = u64::try_from(ticks) else {
        return 0;
    };
    let file = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    let converted = unsafe {
        FileTimeToSystemTime(&file, &mut utc).and_then(|()| SystemTimeToTzSpecificLocalTime(None, &utc, &mut local))
    };
    if converted.is_err() {
        return 0;
    }
    let as_seconds = |time: &SYSTEMTIME| {
        days_from_civil(i64::from(time.wYear), i64::from(time.wMonth), i64::from(time.wDay)) * 86_400
            + i64::from(time.wHour) * 3600
            + i64::from(time.wMinute) * 60
            + i64::from(time.wSecond)
    };
    as_seconds(&local) - as_seconds(&utc)
}

#[cfg(not(any(unix, windows)))]
pub fn offset_at(_unix: i64) -> i64 {
    0
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// Номер местного дня (дни от 1970-01-01).
pub fn local_day(unix: i64) -> i64 {
    (unix + offset_at(unix)).div_euclid(86_400)
}

/// «21:14» по местному времени.
pub fn time_of_day(unix: i64) -> String {
    let seconds = (unix + offset_at(unix)).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

/// Номер дня от 1970-01-01 по дате (алгоритм Говарда Хиннанта).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 10, 7), 1_791_331_200 / 86_400);
        assert_eq!(days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28), 2, "2000 — високосный");
        assert_eq!(days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28), 1, "1900 — нет");
    }

    #[test]
    fn offset_is_sane_and_follows_the_moment() {
        // Смещения на Земле — от −12 до +14 часов, кратные четверти часа.
        for moment in [0, 1_700_000_000, 1_720_000_000, now()] {
            let offset = offset_at(moment);
            assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
            assert_eq!(offset % 900, 0, "{offset}");
        }
        assert_eq!(time_of_day(1_791_331_200 - offset_at(1_791_331_200) + 21 * 3600 + 14 * 60), "21:14");
        // Летнее время: проверяется при запуске с `TZ=Europe/Berlin`.
        if std::env::var("TZ").as_deref() == Ok("Europe/Berlin") {
            assert_eq!(offset_at(1_720_000_000), 7200, "июль — летнее время");
            assert_eq!(offset_at(1_700_000_000), 3600, "ноябрь — зимнее");
        }
    }
}
