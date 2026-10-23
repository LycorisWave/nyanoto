//! Эксклюзивный WASAPI: устройство целиком наше, частота и разрядность — как у файла.
//! COM живёт в своём потоке, так что всё устройство — в потоке вывода.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use rtrb::Producer;

use crate::output::Shared;

/// Формат, в котором открылось устройство.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceFormat {
    pub rate: u32,
    pub channels: usize,
    /// Значащих бит в сэмпле.
    pub bits: u16,
    /// Сэмпл с плавающей точкой (32 бита).
    pub float: bool,
    /// Байт на сэмпл в буфере устройства.
    pub container: u16,
}

/// Открытое устройство: поток вывода, формат и вход кольцевого буфера.
pub(crate) struct Opened {
    pub exclusive: Exclusive,
    pub format: DeviceFormat,
    pub producer: Producer<f32>,
    pub capacity: usize,
}

/// Что поток вывода передаёт после открытия устройства.
#[cfg(windows)]
type Handover = Result<(DeviceFormat, Producer<f32>, usize), String>;

pub(crate) struct Exclusive {
    pub thread: Option<JoinHandle<()>>,
    pub stop: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Сэмпл в формат устройства. Для нетронутых 16/24 бит — точно обратно декодеру.
/// Проверяется тестами везде, хоть эксклюзив пока только в Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn encode(sample: f32, format: &DeviceFormat, out: &mut [u8]) {
    if format.float {
        out[..4].copy_from_slice(&sample.to_le_bytes());
        return;
    }
    match format.container {
        2 => {
            let value = (f64::from(sample) * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16;
            out[..2].copy_from_slice(&value.to_le_bytes());
        }
        3 => {
            let value = (f64::from(sample) * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
            out[..3].copy_from_slice(&value.to_le_bytes()[..3]);
        }
        _ => {
            let value = (f64::from(sample) * 2_147_483_648.0).round().clamp(-2_147_483_648.0, 2_147_483_647.0) as i32;
            out[..4].copy_from_slice(&value.to_le_bytes());
        }
    }
}

/// Форматы по порядку: свой, потом шире (биты, байты, float).
#[cfg_attr(not(windows), allow(dead_code))]
fn candidates(bits: u32) -> Vec<(u16, u16, bool)> {
    match bits {
        0..=16 => vec![(16, 2, false), (24, 4, false), (32, 4, false), (24, 3, false), (32, 4, true)],
        17..=24 => vec![(24, 4, false), (24, 3, false), (32, 4, false), (32, 4, true), (16, 2, false)],
        _ => vec![(32, 4, false), (32, 4, true), (24, 4, false), (24, 3, false), (16, 2, false)],
    }
}

#[cfg(not(windows))]
pub(crate) fn open(
    _shared: &Arc<Shared>,
    _rate: u32,
    _channels: usize,
    _bits: u32,
) -> Result<Opened, String> {
    Err("эксклюзивный вывод есть только в Windows".into())
}

#[cfg(windows)]
pub(crate) fn open(shared: &Arc<Shared>, rate: u32, channels: usize, bits: u32) -> Result<Opened, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let thread = {
        let (shared, stop, running) = (shared.clone(), stop.clone(), running.clone());
        std::thread::Builder::new()
            .name("nyanoto-exclusive".into())
            .spawn(move || wasapi::run(shared, rate, channels, bits, stop, running, opened_tx))
            .map_err(|error| error.to_string())?
    };
    let exclusive = Exclusive { thread: Some(thread), stop, running };
    match opened_rx.recv() {
        Ok(Ok((format, producer, capacity))) => Ok(Opened { exclusive, format, producer, capacity }),
        Ok(Err(error)) => Err(error),
        Err(_) => Err("поток вывода завершился".into()),
    }
}

#[cfg(windows)]
mod wasapi {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;

    use rtrb::Consumer;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Media::Audio::{
        AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
        WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
    };
    use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
    use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::Threading::{CreateEventA, WaitForSingleObject};
    use windows::core::PCSTR;

    use super::{DeviceFormat, candidates, encode};
    use crate::eq::EqRuntime;
    use crate::output::{Shared, ramp_step, render, ring};

    fn wave_format(format: &DeviceFormat) -> WAVEFORMATEXTENSIBLE {
        let block = format.container * format.channels as u16;
        let mask = match format.channels {
            1 => 0x4,
            2 => 0x3,
            _ => 0,
        };
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: format.channels as u16,
                nSamplesPerSec: format.rate,
                nAvgBytesPerSec: format.rate * u32::from(block),
                nBlockAlign: block,
                wBitsPerSample: format.container * 8,
                cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: format.bits },
            dwChannelMask: mask,
            SubFormat: if format.float { KSDATAFORMAT_SUBTYPE_IEEE_FLOAT } else { KSDATAFORMAT_SUBTYPE_PCM },
        }
    }

    struct Device {
        client: IAudioClient,
        render: IAudioRenderClient,
        event: HANDLE,
        format: DeviceFormat,
        frames: u32,
    }

    /// Открывает устройство по умолчанию в первом подходящем формате.
    unsafe fn open(rate: u32, channels: usize, bits: u32) -> Result<Device, String> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|error| error.to_string())?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(|error| error.to_string())?;
        let activate = || -> Result<IAudioClient, String> {
            unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }.map_err(|error| error.to_string())
        };
        let mut client = activate()?;

        let channel_options = if channels == 2 { vec![2] } else { vec![channels, 2] };
        // Сначала частота файла, не берёт (USB-гарнитуры на 48 кГц) — ближайшая с ресемплингом.
        let mut rates: Vec<u32> = Vec::new();
        for candidate in [rate, 48_000, 96_000, 44_100, 88_200, 192_000, 176_400] {
            if !rates.contains(&candidate) {
                rates.push(candidate);
            }
        }
        let mut chosen = None;
        let mut last = windows::core::HRESULT(0);
        'search: for rate in rates {
            for channels in channel_options.iter().copied() {
            for (valid, container, float) in candidates(bits) {
                let format = DeviceFormat { rate, channels, bits: valid, float, container };
                let wave = wave_format(&format);
                let result = unsafe {
                    client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &wave.Format as *const WAVEFORMATEX, None)
                };
                if result.is_ok() && result.0 == 0 {
                    chosen = Some(format);
                    break 'search;
                }
                last = result;
            }
            }
        }
        let format = chosen.ok_or_else(|| match last.0 as u32 {
            0x8889_000E => "в настройках звука Windows у устройства запрещён монопольный режим для приложений".to_string(),
            0x8889_000A => "устройство занято другой программой в монопольном режиме".to_string(),
            code => format!("устройство не принимает {rate} Гц в эксклюзивном режиме (код {code:#010x})"),
        })?;
        let wave = wave_format(&format);

        let mut period = 0i64;
        unsafe { client.GetDevicePeriod(Some(&mut period), None) }.map_err(|error| error.to_string())?;
        let init = |client: &IAudioClient, period: i64| unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                period,
                period,
                &wave.Format as *const WAVEFORMATEX,
                None,
            )
        };
        if let Err(error) = init(&client, period) {
            if error.code() != AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
                return Err(error.to_string());
            }
            // Период надо выровнять по размеру буфера, который предложило устройство.
            let frames = unsafe { client.GetBufferSize() }.map_err(|error| error.to_string())?;
            period = (10_000_000.0 * f64::from(frames) / f64::from(format.rate) + 0.5) as i64;
            client = activate()?;
            init(&client, period).map_err(|error| error.to_string())?;
        }
        let event = unsafe { CreateEventA(None, false, false, PCSTR::null()) }.map_err(|error| error.to_string())?;
        unsafe { client.SetEventHandle(event) }.map_err(|error| error.to_string())?;
        let render: IAudioRenderClient = unsafe { client.GetService() }.map_err(|error| error.to_string())?;
        let frames = unsafe { client.GetBufferSize() }.map_err(|error| error.to_string())?;
        Ok(Device { client, render, event, format, frames })
    }

    /// Заполняет буфер устройства; `false` — устройство пропало.
    unsafe fn fill(
        device: &Device,
        scratch: &mut [f32],
        consumer: &mut Consumer<f32>,
        shared: &Shared,
        gain: &mut f32,
        eq: &mut EqRuntime,
        step: f32,
    ) -> bool {
        let channels = device.format.channels;
        let samples = device.frames as usize * channels;
        let scratch = &mut scratch[..samples];
        render(scratch, consumer, shared, channels, gain, eq, step);
        let Ok(buffer) = (unsafe { device.render.GetBuffer(device.frames) }) else {
            return false;
        };
        let size = usize::from(device.format.container);
        let bytes = unsafe { std::slice::from_raw_parts_mut(buffer, samples * size) };
        for (sample, out) in scratch.iter().zip(bytes.chunks_exact_mut(size)) {
            encode(*sample, &device.format, out);
        }
        unsafe { device.render.ReleaseBuffer(device.frames, 0) }.is_ok()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run(
        shared: Arc<Shared>,
        rate: u32,
        channels: usize,
        bits: u32,
        stop: Arc<AtomicBool>,
        running: Arc<AtomicBool>,
        opened: Sender<super::Handover>,
    ) {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let device = match unsafe { open(rate, channels, bits) } {
            Ok(device) => device,
            Err(error) => {
                let _ = opened.send(Err(error));
                if initialized {
                    unsafe { CoUninitialize() };
                }
                return;
            }
        };
        // Кольцевой буфер — под формат, который приняло устройство.
        let (producer, mut consumer, capacity) = ring(device.format.rate, device.format.channels);
        let _ = opened.send(Ok((device.format, producer, capacity)));

        let mut scratch = vec![0.0f32; device.frames as usize * device.format.channels];
        let mut gain = 0.0f32;
        let mut eq = EqRuntime::new();
        let step = ramp_step(device.format.rate);
        let mut started = false;
        while !stop.load(Ordering::Acquire) {
            let wanted = running.load(Ordering::Acquire);
            if wanted && !started {
                // Перед запуском буфер должен быть заполнен.
                if !unsafe { fill(&device, &mut scratch, &mut consumer, &shared, &mut gain, &mut eq, step) }
                    || unsafe { device.client.Start() }.is_err()
                {
                    shared.failed.store(true, Ordering::Release);
                    break;
                }
                started = true;
            } else if !wanted && started {
                let _ = unsafe { device.client.Stop() };
                started = false;
            }
            if !started {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            unsafe { WaitForSingleObject(device.event, 200) };
            if !unsafe { fill(&device, &mut scratch, &mut consumer, &shared, &mut gain, &mut eq, step) } {
                shared.failed.store(true, Ordering::Release);
                break;
            }
        }
        unsafe {
            let _ = device.client.Stop();
            let _ = CloseHandle(device.event);
        }
        drop(device);
        if initialized {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_samples_come_back_exactly() {
        let sixteen = DeviceFormat { rate: 44_100, channels: 2, bits: 16, float: false, container: 2 };
        for value in [-32_768i32, -1, 0, 1, 12_345, 32_767] {
            let mut out = [0u8; 2];
            encode(value as f32 / 32_768.0, &sixteen, &mut out);
            assert_eq!(i16::from_le_bytes(out) as i32, value);
        }
        let in_32 = DeviceFormat { bits: 24, container: 4, ..sixteen };
        let packed = DeviceFormat { bits: 24, container: 3, ..sixteen };
        for value in [-8_388_608i32, -1, 0, 1, 4_660_000, 8_388_607] {
            let sample = value as f32 / 8_388_608.0;
            let mut out = [0u8; 4];
            encode(sample, &in_32, &mut out);
            assert_eq!(i32::from_le_bytes(out), value << 8);
            let mut out = [0u8; 3];
            encode(sample, &packed, &mut out);
            let back = i32::from_le_bytes([out[0], out[1], out[2], if out[2] & 0x80 != 0 { 0xFF } else { 0 }]);
            assert_eq!(back, value);
        }
    }

    #[test]
    fn formats_are_tried_from_the_closest() {
        assert_eq!(candidates(16)[0], (16, 2, false));
        assert_eq!(candidates(24)[0], (24, 4, false));
        assert_eq!(candidates(32)[0], (32, 4, false));
    }
}
