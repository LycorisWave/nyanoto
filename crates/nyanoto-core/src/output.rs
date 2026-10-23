//! Вывод звука: кольцо между движком и звуковым потоком.
//! Звуковой поток не блокируется и не выделяет память — только забирает и умножает на громкость.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::eq::{EqCoefficients, EqRuntime};
use crate::exclusive::{DeviceFormat, Exclusive};

/// Запас в буфере. На отклик не влияет: громкость и пауза — уже на выходе.
const RING_SECONDS: f64 = 2.0;

/// Длительность плавного изменения громкости, чтобы пауза и регулировка не щёлкали.
const RAMP_SECONDS: f32 = 0.012;

/// Кусок для Linux. С 25 мс PipeWire крутил граф 40+ раз в секунду, с 50 мс — вдвое реже.
/// Пауза слышна с задержкой до 0,1 с.
#[cfg(target_os = "linux")]
const DEVICE_PERIOD_SECONDS: f64 = 0.05;

/// Состояние, общее для движка и звукового потока.
pub(crate) struct Shared {
    /// Сколько кадров уже ушло на устройство с момента запуска движка.
    pub frames_played: AtomicU64,
    pub paused: AtomicBool,
    /// Просьба сбросить буфер; звуковой поток снимает флаг, когда сделал.
    pub flush: AtomicBool,
    /// Громкость как биты `f32`.
    pub volume: AtomicU32,
    /// Движок ещё декодирует: пустой буфер в этот момент — это заикание.
    pub expect_data: AtomicBool,
    pub underruns: AtomicU64,
    /// Устройство сообщило об ошибке, поток надо пересоздать.
    pub failed: AtomicBool,
    /// Коэффициенты EQ и версия: поток берёт новые, когда версия меняется.
    pub eq: Mutex<EqCoefficients>,
    pub eq_version: AtomicU64,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            frames_played: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            expect_data: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            eq: Mutex::new(EqCoefficients::default()),
            eq_version: AtomicU64::new(0),
        }
    }
}

/// Приёмник в память вместо устройства — для тестов и конвертации.
#[derive(Clone)]
pub struct Capture {
    rate: u32,
    channels: usize,
    paced: bool,
    samples: Arc<Mutex<Vec<f32>>>,
}

impl Capture {
    /// Приёмник, который забирает звук так быстро, как движок его готовит.
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            rate,
            channels,
            paced: false,
            samples: Arc::default(),
        }
    }

    /// Приёмник, который забирает звук в темпе воспроизведения, как устройство.
    pub fn paced(rate: u32, channels: usize) -> Self {
        Self {
            paced: true,
            ..Self::new(rate, channels)
        }
    }

    /// Забирает всё, что успело проиграться.
    pub fn take(&self) -> Vec<f32> {
        std::mem::take(&mut *self.samples.lock().unwrap_or_else(|error| error.into_inner()))
    }
}

enum Backend {
    Device(cpal::Stream),
    /// Эксклюзивный WASAPI: поток вывода живёт в `Exclusive`.
    Exclusive(Exclusive),
    Capture {
        paced: bool,
        running: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    },
}

pub(crate) struct Output {
    backend: Backend,
    producer: Producer<f32>,
    pub rate: u32,
    pub channels: usize,
    pub device: String,
    /// Формат эксклюзивного вывода; `None` — общий режим или захват.
    pub exclusive: Option<DeviceFormat>,
    capacity: usize,
    running: bool,
}

/// Звуковая система ОС.
///
/// В Linux — ALSA, дальше ведёт PipeWire: дешевле всего, без посредника (проверено замером).
/// Но ALSA видит только `default`, поэтому настоящее имя выхода спрашиваем у сервера PulseAudio.
pub(crate) struct Audio {
    host: cpal::Host,
    #[cfg(target_os = "linux")]
    pulse: Option<pulseaudio::Client>,
}

/// Рукопожатие PulseAudio ждёт без срока — ждём не больше двух секунд.
#[cfg(target_os = "linux")]
fn connect_pulse() -> Option<pulseaudio::Client> {
    pulseaudio::socket_path_from_env()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("nyanoto-pulse".into())
        .spawn(move || {
            let _ = sender.send(pulseaudio::Client::from_env(c"Nyanoto"));
        })
        .ok()?;
    receiver.recv_timeout(Duration::from_secs(2)).ok()?.ok()
}

impl Audio {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        if let Ok(host) = cpal::host_from_id(cpal::HostId::Alsa) {
            return Self { host, pulse: connect_pulse() };
        }
        Self {
            host: cpal::default_host(),
            #[cfg(target_os = "linux")]
            pulse: None,
        }
    }

    #[cfg(target_os = "linux")]
    fn default_sink(&self) -> Option<pulseaudio::protocol::SinkInfo> {
        let client = self.pulse.as_ref()?;
        futures::executor::block_on(client.sink_info_by_name(pulseaudio::protocol::DEFAULT_SINK.to_owned())).ok()
    }

    /// Имя устройства вывода по умолчанию.
    pub fn default_name(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        if self.pulse.is_some() {
            let sink = self.default_sink()?;
            let name = sink.description.as_ref().unwrap_or(&sink.name);
            return Some(String::from_utf8_lossy(name.as_bytes()).into_owned());
        }
        device_name_on(&self.host)
    }

    /// Что звучит на самом деле: устройство и активный порт.
    /// Наушники в разъёме — тот же выход, меняется только порт.
    pub fn default_identity(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        if self.pulse.is_some() {
            let sink = self.default_sink()?;
            let port = sink.ports.get(sink.active_port).map(|port| String::from_utf8_lossy(port.name.as_bytes()).into_owned());
            return Some(format!("{}|{}", String::from_utf8_lossy(sink.name.as_bytes()), port.unwrap_or_default()));
        }
        self.default_name()
    }

    /// Звук сам переходит на новое устройство по умолчанию, переоткрывать не надо.
    pub fn follows_default(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.pulse.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }
}

impl Output {
    /// Открывает устройство вывода по умолчанию в его собственном формате.
    pub fn open_device(shared: &Arc<Shared>, audio: &Audio) -> Result<Self, String> {
        let device = audio
            .host
            .default_output_device()
            .ok_or_else(|| "нет устройства вывода".to_string())?;
        let name = audio.default_name().unwrap_or_else(|| device_name(&device));
        let supported = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let format = supported.sample_format();
        let config = supported.config();
        let rate = config.sample_rate;
        let channels = usize::from(config.channels);
        if rate == 0 || channels == 0 {
            return Err(format!("устройство сообщило формат {rate} Гц, {channels} каналов"));
        }

        let open = |config: cpal::StreamConfig| {
            let (producer, consumer, capacity) = ring(rate, channels);
            let stream = match format {
                SampleFormat::F32 => build::<f32>(&device, config, consumer, shared.clone()),
                SampleFormat::F64 => build::<f64>(&device, config, consumer, shared.clone()),
                SampleFormat::I16 => build::<i16>(&device, config, consumer, shared.clone()),
                SampleFormat::I24 => build::<cpal::I24>(&device, config, consumer, shared.clone()),
                SampleFormat::I32 => build::<i32>(&device, config, consumer, shared.clone()),
                SampleFormat::U16 => build::<u16>(&device, config, consumer, shared.clone()),
                other => return Err(format!("формат устройства {other:?} не поддерживается")),
            }
            .map_err(|error| error.to_string())?;
            stream.play().map_err(|error| error.to_string())?;
            Ok((stream, producer, capacity))
        };
        // Крупный кусок, а если устройство его не принимает — какой оно даёт.
        let roomy = roomy_buffer(&supported, rate).map(|buffer_size| cpal::StreamConfig { buffer_size, ..config });
        let (stream, producer, capacity) = match roomy.map(open) {
            Some(Ok(opened)) => opened,
            _ => open(config)?,
        };

        Ok(Self {
            backend: Backend::Device(stream),
            producer,
            rate,
            channels,
            device: name,
            exclusive: None,
            capacity,
            running: true,
        })
    }

    /// Открывает устройство по умолчанию эксклюзивно, в формате файла.
    pub fn open_exclusive(shared: &Arc<Shared>, rate: u32, channels: usize, bits: u32) -> Result<Self, String> {
        let opened = crate::exclusive::open(shared, rate, channels, bits)?;
        Ok(Self {
            backend: Backend::Exclusive(opened.exclusive),
            producer: opened.producer,
            rate: opened.format.rate,
            channels: opened.format.channels,
            device: default_device_name().unwrap_or_else(|| "?".into()),
            exclusive: Some(opened.format),
            capacity: opened.capacity,
            running: true,
        })
    }

    /// Забирает звук так быстро, как может, в `capture`. Путь сэмплов тот же.
    pub fn open_capture(shared: &Arc<Shared>, capture: &Capture) -> Result<Self, String> {
        let (rate, channels) = (capture.rate, capture.channels);
        if rate == 0 || channels == 0 {
            return Err(format!("неверный формат захвата: {rate} Гц, {channels} каналов"));
        }
        let (producer, mut consumer, capacity) = ring(rate, channels);
        let running = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let (shared, running, stop) = (shared.clone(), running.clone(), stop.clone());
            let sink = capture.samples.clone();
            let step = ramp_step(rate);
            let paced = capture.paced;
            thread::Builder::new()
                .name("nyanoto-capture".into())
                .spawn(move || {
                    let mut block = vec![0.0f32; 512 * channels];
                    let mut gain = 0.0f32;
                    let mut eq = EqRuntime::new();
                    // Для темпа: начало отрезка и сколько кадров забрано.
                    let mut run: Option<(Instant, u64)> = None;
                    while !stop.load(Ordering::Acquire) {
                        // Не больше, чем лежит: у захвата нет реального времени, пусто — не заикание.
                        let ready = (consumer.slots() / channels * channels).min(block.len());
                        let wanted = ready > 0 || shared.flush.load(Ordering::Acquire);
                        let used = if wanted && running.load(Ordering::Acquire) {
                            let block = &mut block[..ready.max(channels)];
                            render(block, &mut consumer, &shared, channels, &mut gain, &mut eq, step)
                        } else {
                            0
                        };
                        if used == 0 {
                            run = None;
                            thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        sink.lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .extend_from_slice(&block[..used]);
                        if paced {
                            let (since, frames) = run.get_or_insert((Instant::now(), 0));
                            *frames += (used / channels) as u64;
                            let due = Duration::from_secs_f64(*frames as f64 / f64::from(rate));
                            thread::sleep(due.saturating_sub(since.elapsed()));
                        }
                    }
                })
                .map_err(|error| error.to_string())?
        };

        Ok(Self {
            backend: Backend::Capture {
                paced: capture.paced,
                running,
                stop,
                thread: Some(thread),
            },
            producer,
            rate,
            channels,
            device: "захват в память".into(),
            exclusive: None,
            capacity,
            running: true,
        })
    }

    /// Пишет целые кадры, сколько влезет; возвращает число сэмплов.
    pub fn write(&mut self, samples: &[f32]) -> usize {
        let fits = samples.len().min(self.producer.slots());
        let count = fits - fits % self.channels;
        if count == 0 {
            return 0;
        }
        match self.producer.write_chunk_uninit(count) {
            Ok(chunk) => chunk.fill_from_iter(samples[..count].iter().copied()),
            Err(_) => 0,
        }
    }

    pub fn buffered_frames(&self) -> usize {
        (self.capacity - self.producer.slots().min(self.capacity)) / self.channels
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity / self.channels
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Захват в память опустошает буфер сразу — ждать «проигрыша» незачем.
    pub fn is_realtime(&self) -> bool {
        match &self.backend {
            Backend::Device(_) | Backend::Exclusive(_) => true,
            Backend::Capture { paced, .. } => *paced,
        }
    }

    /// Остановленный поток не будит звуковую карту на паузе.
    pub fn set_running(&mut self, run: bool) -> bool {
        if self.running == run {
            return true;
        }
        let done = match &self.backend {
            Backend::Device(stream) if run => stream.play().is_ok(),
            Backend::Device(stream) => stream.pause().is_ok(),
            Backend::Capture { running, .. } => {
                running.store(run, Ordering::Release);
                true
            }
            Backend::Exclusive(exclusive) => {
                exclusive.running.store(run, Ordering::Release);
                true
            }
        };
        if done {
            self.running = run;
        }
        done
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Backend::Capture { stop, thread, .. } = &mut self.backend {
            stop.store(true, Ordering::Release);
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// Кусок звука для устройства, если плеер хочет не тот, что даёт cpal.
#[cfg(target_os = "linux")]
fn roomy_buffer(supported: &cpal::SupportedStreamConfig, rate: u32) -> Option<cpal::BufferSize> {
    let wanted = (f64::from(rate) * DEVICE_PERIOD_SECONDS) as u32;
    let frames = match *supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => wanted.clamp(min, max),
        cpal::SupportedBufferSize::Unknown => wanted,
    };
    Some(cpal::BufferSize::Fixed(frames))
}

#[cfg(not(target_os = "linux"))]
fn roomy_buffer(_supported: &cpal::SupportedStreamConfig, _rate: u32) -> Option<cpal::BufferSize> {
    None
}

/// Имя выхода по умолчанию. Подключается каждый раз заново, у движка своё ([`Audio`]).
pub fn default_device_name() -> Option<String> {
    Audio::new().default_name()
}

/// Имя устройства вывода по умолчанию на уже открытом подключении.
pub(crate) fn device_name_on(host: &cpal::Host) -> Option<String> {
    host.default_output_device().map(|device| device_name(&device))
}

/// Имя для людей: «Наушники Arctis», «Встроенное аудио Аналоговый стерео».
fn device_name(device: &cpal::Device) -> String {
    device.description().map_or_else(|_| "?".into(), |description| description.name().to_string())
}

pub(crate) fn ring(rate: u32, channels: usize) -> (Producer<f32>, Consumer<f32>, usize) {
    let capacity = (f64::from(rate) * RING_SECONDS) as usize * channels;
    let (producer, consumer) = RingBuffer::new(capacity);
    (producer, consumer, capacity)
}

pub(crate) fn ramp_step(rate: u32) -> f32 {
    1.0 / (rate as f32 * RAMP_SECONDS)
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut consumer: Consumer<f32>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let step = ramp_step(config.sample_rate);
    let mut gain = 0.0f32;
    let mut eq = EqRuntime::new();
    let on_error = {
        let shared = shared.clone();
        // Недолив — заикание, а не пропажа устройства.
        move |error: cpal::Error| match error.kind() {
            cpal::ErrorKind::Xrun => {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            _ => shared.failed.store(true, Ordering::Release),
        }
    };
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            render(data, &mut consumer, &shared, channels, &mut gain, &mut eq, step);
        },
        on_error,
        None,
    )
}

/// Заполняет `data` из буфера, недостающее — тишиной. Возвращает число взятых сэмплов.
pub(crate) fn render<T>(
    data: &mut [T],
    consumer: &mut Consumer<f32>,
    shared: &Shared,
    channels: usize,
    gain: &mut f32,
    eq: &mut EqRuntime,
    step: f32,
) -> usize
where
    T: Sample + FromSample<f32>,
{
    let mut used = 0;
    // Новые настройки эквалайзера: забираем, только если движок их сейчас не пишет.
    let version = shared.eq_version.load(Ordering::Acquire);
    if version != eq.version
        && let Ok(coefficients) = shared.eq.try_lock()
    {
        eq.update(*coefficients, version);
    }
    if shared.flush.load(Ordering::Acquire) {
        if let Ok(chunk) = consumer.read_chunk(consumer.slots()) {
            chunk.commit_all();
        }
        // После сброса звук начинается с другого места: вводим его плавно.
        *gain = 0.0;
        eq.reset();
        shared.flush.store(false, Ordering::Release);
    } else {
        let paused = shared.paused.load(Ordering::Relaxed);
        if !(paused && *gain <= 0.0) {
            let target = if paused {
                0.0
            } else {
                f32::from_bits(shared.volume.load(Ordering::Relaxed))
            };
            let frames = (data.len() / channels).min(consumer.slots() / channels);
            if let Ok(chunk) = consumer.read_chunk(frames * channels) {
                let (head, tail) = chunk.as_slices();
                let mut level = *gain;
                for (index, sample) in head.iter().chain(tail).enumerate() {
                    if index % channels == 0 {
                        // На паузе громкость дошла до нуля: остальное оставляем в буфере.
                        if paused && level <= 0.0 {
                            break;
                        }
                        level = if level < target {
                            (level + step).min(target)
                        } else {
                            (level - step).max(target)
                        };
                    }
                    data[index] = T::from_sample(eq.process(index % channels, *sample) * level);
                    used = index + 1;
                }
                *gain = level;
                chunk.commit(used);
                shared
                    .frames_played
                    .fetch_add((used / channels) as u64, Ordering::AcqRel);
            }
            if !paused && used < data.len() && shared.expect_data.load(Ordering::Relaxed) {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    for sample in &mut data[used..] {
        *sample = T::EQUILIBRIUM;
    }
    used
}

#[cfg(test)]
mod tests {
    use super::Audio;

    /// Живая проверка: `cargo test -p nyanoto-core default_identity_is_read -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn default_identity_is_read() {
        let audio = Audio::new();
        println!("имя: {:?}", audio.default_name());
        println!("устройство и порт: {:?}", audio.default_identity());
        let started = std::time::Instant::now();
        for _ in 0..100 {
            let _ = audio.default_identity();
        }
        println!("один запрос: {:?}", started.elapsed() / 100);
    }
}

