//! Тихий режим фоновой работы: низший приоритет, отдых втрое дольше работы,
//! а занята система — ждём. Дольше, зато незаметно.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// Нагрузка на процессор, выше которой фоновая работа ждёт.
const BUSY: f32 = 0.5;
/// Во сколько раз отдых длиннее работы.
const REST_FACTOR: u32 = 3;
/// Шаг ожидания, пока система занята, и сколько таких шагов подряд допустимо.
const BUSY_STEP: Duration = Duration::from_secs(2);
const BUSY_STEPS: u32 = 30;

/// Самый низкий приоритет процессора и диска для этого потока.
pub fn enter_background() {
    #[cfg(windows)]
    windows::enter_background();
    #[cfg(target_os = "linux")]
    linux::enter_background();
}

/// Счётчики времени процессора: сколько всего и сколько из этого простой.
#[derive(Clone, Copy)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

fn cpu_times() -> Option<CpuTimes> {
    #[cfg(windows)]
    return windows::cpu_times();
    #[cfg(target_os = "linux")]
    return linux::cpu_times();
    #[allow(unreachable_code)]
    None
}

/// Отдых после `worked`, при занятой системе — дольше. Попросили стоп — сразу выходим.
pub fn rest(worked: Duration, stop: &AtomicBool) {
    let pause = (worked * REST_FACTOR).clamp(Duration::from_millis(20), Duration::from_secs(2));
    let mut before = cpu_times();
    sleep_unless_stopped(pause, stop);
    for _ in 0..BUSY_STEPS {
        let now = cpu_times();
        let load = match (before, now) {
            (Some(before), Some(now)) => {
                let total = now.total.saturating_sub(before.total);
                let idle = now.idle.saturating_sub(before.idle);
                (total > 0).then(|| 1.0 - idle as f32 / total as f32)
            }
            _ => None,
        };
        if load.is_none_or(|load| load < BUSY) || stop.load(Ordering::Relaxed) {
            return;
        }
        before = now;
        sleep_unless_stopped(BUSY_STEP, stop);
    }
}

fn sleep_unless_stopped(duration: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut left = duration;
    while !left.is_zero() && !stop.load(Ordering::Relaxed) {
        let now = left.min(step);
        thread::sleep(now);
        left -= now;
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;

    use super::CpuTimes;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        fn ticks(self) -> u64 {
            u64::from(self.high) << 32 | u64::from(self.low)
        }
    }

    /// Фоновый режим потока: низкий приоритет процессора, диска и памяти.
    const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
        fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    }

    pub fn enter_background() {
        // Вызов меняет только текущий поток и не трогает память программы.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }

    pub fn cpu_times() -> Option<CpuTimes> {
        let (mut idle, mut kernel, mut user) = (FileTime::default(), FileTime::default(), FileTime::default());
        // Функция только заполняет три переданные структуры.
        let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        // Время ядра уже включает простой.
        (ok != 0).then(|| CpuTimes {
            total: kernel.ticks() + user.ticks(),
            idle: idle.ticks(),
        })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::CpuTimes;

    #[repr(C)]
    struct SchedParam {
        priority: i32,
    }

    unsafe extern "C" {
        fn nice(increment: i32) -> i32;
        fn sched_setscheduler(pid: i32, policy: i32, param: *const SchedParam) -> i32;
        fn syscall(number: i64, ...) -> i64;
    }

    /// Поток получает процессор, только когда он больше никому не нужен.
    const SCHED_IDLE: i32 = 5;
    /// Диск — только когда к нему никто больше не обращается.
    const IOPRIO_WHO_PROCESS: i64 = 1;
    const IOPRIO_CLASS_IDLE: i64 = 3;
    const IOPRIO_CLASS_SHIFT: i64 = 13;
    #[cfg(target_arch = "x86_64")]
    const SYS_IOPRIO_SET: Option<i64> = Some(251);
    #[cfg(target_arch = "aarch64")]
    const SYS_IOPRIO_SET: Option<i64> = Some(30);
    #[cfg(target_arch = "x86")]
    const SYS_IOPRIO_SET: Option<i64> = Some(289);
    #[cfg(target_arch = "arm")]
    const SYS_IOPRIO_SET: Option<i64> = Some(314);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "x86", target_arch = "arm")))]
    const SYS_IOPRIO_SET: Option<i64> = None;

    pub fn enter_background() {
        // В Linux приоритет у потока свой: 0 — вызывающий поток.
        unsafe {
            if sched_setscheduler(0, SCHED_IDLE, &SchedParam { priority: 0 }) != 0 {
                nice(19);
            }
            if let Some(number) = SYS_IOPRIO_SET {
                syscall(number, IOPRIO_WHO_PROCESS, 0i64, IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT);
            }
        }
    }

    pub fn cpu_times() -> Option<CpuTimes> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let values: Vec<u64> = stat
            .lines()
            .next()?
            .split_whitespace()
            .skip(1)
            .filter_map(|value| value.parse().ok())
            .collect();
        // user nice system idle iowait irq softirq steal …
        let idle = values.get(3)? + values.get(4).copied().unwrap_or(0);
        Some(CpuTimes { total: values.iter().sum(), idle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_interrupts_rest() {
        let stop = AtomicBool::new(true);
        let started = std::time::Instant::now();
        rest(Duration::from_secs(10), &stop);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// SCHED_IDLE только для этого потока.
    #[cfg(target_os = "linux")]
    #[test]
    fn background_touches_only_this_thread() {
        // Политика — 41-е поле `stat`, счёт после имени в скобках с 3-го.
        let policy = || {
            let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
            let after = stat[stat.rfind(')').unwrap() + 2..].to_string();
            after.split_whitespace().nth(41 - 3).unwrap().to_string()
        };
        let worker = std::thread::spawn(move || {
            enter_background();
            policy()
        });
        assert_eq!(worker.join().unwrap(), "5", "поток не стал SCHED_IDLE");
        assert_eq!(policy(), "0", "задет вызывающий поток");
    }

    #[test]
    fn cpu_times_are_readable_here() {
        if cfg!(any(windows, target_os = "linux")) {
            let times = cpu_times().expect("счётчики процессора недоступны");
            assert!(times.total >= times.idle);
        }
    }
}
