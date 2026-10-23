//! Поток движка: очередь, декодирование с опережением, переходы без пауз.
//! `cursor` — что отдано декодеру, `current` — что звучит; границы треков — в `segments`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::convert::{Converter, Speed};
use crate::eq::{EqCoefficients, Equalizer};
use crate::output::{Capture, Output, Shared};
use crate::queue::{Queue, QueueItem};
use crate::source::{OpenOptions, ReplayGain, TrackDecoder, TrackError, TrackInfo, guard};

/// Сколько времени подряд движок декодирует, прежде чем снова посмотреть на команды.
const PUMP_BUDGET: Duration = Duration::from_millis(30);

/// Через сколько после паузы или остановки звуковой поток усыпляется.
const IDLE_STREAM_TIMEOUT: Duration = Duration::from_secs(3);

/// Сколько ждём, пока звуковой поток выбросит буфер.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(400);

/// «Назад» в первые секунды трека ведёт к предыдущему, позже — в начало текущего.
pub const PREVIOUS_RESTARTS_AFTER: f64 = 3.0;

const LONG_WAIT: Duration = Duration::from_secs(3600);

/// Как часто во время игры сверять устройство вывода с устройством по умолчанию.
const DEVICE_CHECK_EVERY: Duration = Duration::from_secs(2);
/// С паузой при смене устройства проверяем чаще — меньше звука успеет в динамики.
const DEVICE_CHECK_PAUSING: Duration = Duration::from_millis(500);

/// Следит, не сменилось ли устройство или порт, пока играет. Первое наблюдение — отсчёт.
#[derive(Default)]
struct SwitchWatch {
    heard: Option<String>,
}

impl SwitchWatch {
    /// Новое наблюдение; `true` — сменилось с прошлого раза.
    fn observe(&mut self, now: Option<String>) -> bool {
        let Some(now) = now else {
            return false;
        };
        let changed = self.heard.as_ref().is_some_and(|heard| *heard != now);
        self.heard = Some(now);
        changed
    }

    /// Музыка не играет: смена в это время — не повод для паузы.
    fn forget(&mut self) {
        self.heard = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

/// Выравнивание громкости по тегам ReplayGain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Loudness {
    #[default]
    Off,
    /// По громкости каждого трека.
    Track,
    /// По громкости альбома: тихие треки остаются тихими.
    Album,
}

impl Loudness {
    /// Множитель громкости по тегам (без тегов — 1), не выше пика.
    pub fn factor(self, tags: &ReplayGain) -> f32 {
        let (gain, peak) = match self {
            Loudness::Off => return 1.0,
            Loudness::Track => (
                tags.track_gain_db.or(tags.album_gain_db),
                tags.track_peak.or(tags.album_peak),
            ),
            Loudness::Album => (
                tags.album_gain_db.or(tags.track_gain_db),
                tags.album_peak.or(tags.track_peak),
            ),
        };
        let Some(gain) = gain else {
            return 1.0;
        };
        let mut factor = 10f32.powf(gain / 20.0);
        if let Some(peak) = peak.filter(|peak| *peak > 0.0) {
            factor = factor.min(1.0 / peak);
        }
        factor.clamp(0.0, 4.0)
    }
}

/// Куда движок выводит звук.
#[derive(Clone, Default)]
pub enum OutputTarget {
    /// Устройство вывода по умолчанию.
    #[default]
    Device,
    /// Память: звук никуда не играется, а накапливается в [`Capture`].
    Capture(Capture),
}

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub id: u64,
    pub path: PathBuf,
    pub info: TrackInfo,
}

#[derive(Debug)]
pub enum Event {
    /// До слушателя дошёл первый кадр трека.
    TrackStarted(NowPlaying),
    /// Трек не открылся или оборвался; движок перешёл к следующему.
    TrackFailed {
        id: u64,
        path: PathBuf,
        error: TrackError,
    },
    /// Доигран, но повреждён: `decode_errors` мест заглушено, `missing` — нехватка в конце.
    TrackDamaged {
        id: u64,
        path: PathBuf,
        decode_errors: u64,
        missing: Option<Duration>,
    },
    StateChanged(State),
    QueueChanged,
    /// Очередь доиграна до конца.
    Finished,
    OutputOpened {
        device: String,
        rate: u32,
        channels: usize,
        /// Эксклюзивный режим: значащих бит у устройства. `None` — общий режим.
        exclusive_bits: Option<u16>,
    },
    /// Устройство вывода пропало; движок попробует открыть его заново.
    OutputLost,
    /// Устройство сменилось или пропало во время игры — пауза (если включено).
    PausedForDevice,
    OutputFailed(String),
    /// Эксклюзивный вывод не открылся, звук идёт в общем режиме.
    ExclusiveUnavailable(String),
}

/// Зовётся после каждого события — интерфейсу не надо опрашивать.
type Waker = Box<dyn Fn() + Send + Sync>;

enum Command {
    SetQueue(Vec<PathBuf>),
    Cue(Vec<PathBuf>, usize, Duration),
    PlayNow(Vec<PathBuf>),
    Move(u64, usize),
    Reorder(Vec<u64>),
    PlayList(Vec<PathBuf>, usize),
    Append(Vec<PathBuf>),
    InsertNext(Vec<PathBuf>),
    Remove(u64),
    ClearQueue,
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    JumpTo(u64),
    Seek(Duration),
    SetRepeat(Repeat),
    SetLoudness(Loudness),
    SetSpeed(Speed),
    SetEqualizer(Equalizer),
    SetCrossfade(Option<Duration>),
    SetExclusive(bool),
    SetPauseOnDeviceChange(bool),
    SetDeviceVolumes(HashMap<String, f32>),
    StopAfterCurrent(bool),
    Shutdown,
}

struct Playhead {
    now: NowPlaying,
    start_abs: u64,
    base: f64,
    rate: u32,
    speed: f64,
}

struct Status {
    state: State,
    playhead: Option<Playhead>,
    /// Подготовленный элемент (см. [`Engine::cue`]).
    cued: Option<u64>,
}

/// Ручка движка: методы возвращаются сразу, результат — событиями.
pub struct Engine {
    commands: Sender<Command>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    status: Arc<Mutex<Status>>,
    queue: Arc<Mutex<Queue>>,
    waker: Arc<Mutex<Option<Waker>>>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(target: OutputTarget) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let shared = Arc::new(Shared::new());
        let status = Arc::new(Mutex::new(Status {
            state: State::Stopped,
            playhead: None,
            cued: None,
        }));
        let queue = Arc::new(Mutex::new(Queue::default()));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::default();

        // Звуковой поток нельзя передавать между потоками — создаём внутри своего.
        let thread = {
            let (shared, status, queue) = (shared.clone(), status.clone(), queue.clone());
            let notify = waker.clone();
            thread::Builder::new()
                .name("nyanoto-engine".into())
                .spawn(move || {
                    let mut worker = Worker::new(command_rx, event_tx, shared, status, queue, target);
                    worker.waker = notify;
                    worker.run()
                })
                .ok()
        };

        Self {
            commands,
            events,
            shared,
            status,
            queue,
            waker,
            thread,
        }
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Задаёт функцию, которую поток движка вызывает после каждого события.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *lock(&self.waker) = Some(Box::new(waker));
    }

    /// Заменяет очередь и начинает играть её элемент с номером `start`.
    pub fn play_list(&self, paths: Vec<PathBuf>, start: usize) {
        self.send(Command::PlayList(paths, start));
    }

    /// Заменяет очередь и останавливает воспроизведение.
    pub fn set_queue(&self, paths: Vec<PathBuf>) {
        self.send(Command::SetQueue(paths));
    }

    /// Новая очередь, готов элемент `index` с `position`, без звука (восстановление после запуска).
    pub fn cue(&self, paths: Vec<PathBuf>, index: usize, position: Duration) {
        self.send(Command::Cue(paths, index, position));
    }

    /// Сыграть сейчас, вставив сразу после текущего.
    pub fn play_now(&self, paths: Vec<PathBuf>) {
        self.send(Command::PlayNow(paths));
    }

    /// Переставляет элемент очереди на место `index`.
    pub fn move_item(&self, id: u64, index: usize) {
        self.send(Command::Move(id, index));
    }

    /// Переставить очередь по `ids`; играющий трек продолжает.
    pub fn reorder(&self, ids: Vec<u64>) {
        self.send(Command::Reorder(ids));
    }

    /// Где в очереди текущий элемент и сколько в ней всего — без копии очереди.
    pub fn queue_position(&self) -> (Option<usize>, usize) {
        let current = self.current_id();
        let queue = lock(&self.queue);
        (current.and_then(|id| queue.index_of(id)), queue.len())
    }

    /// Элемент очереди, который играет, стоит на паузе или подготовлен.
    pub fn current_id(&self) -> Option<u64> {
        let status = lock(&self.status);
        status.playhead.as_ref().map(|head| head.now.id).or(status.cued)
    }

    pub fn append(&self, paths: Vec<PathBuf>) {
        self.send(Command::Append(paths));
    }

    /// Ставит треки сразу после того, что сейчас играет.
    pub fn insert_next(&self, paths: Vec<PathBuf>) {
        self.send(Command::InsertNext(paths));
    }

    pub fn remove(&self, id: u64) {
        self.send(Command::Remove(id));
    }

    pub fn clear_queue(&self) {
        self.send(Command::ClearQueue);
    }

    pub fn queue(&self) -> Vec<QueueItem> {
        lock(&self.queue).items().to_vec()
    }

    pub fn play(&self) {
        self.send(Command::Play);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn next(&self) {
        self.send(Command::Next);
    }

    pub fn previous(&self) {
        self.send(Command::Previous);
    }

    /// Начинает играть элемент очереди с этим `id`.
    pub fn jump_to(&self, id: u64) {
        self.send(Command::JumpTo(id));
    }

    pub fn seek(&self, position: Duration) {
        self.send(Command::Seek(position));
    }

    /// Линейный множитель громкости от 0 до 1.
    pub fn set_volume(&self, volume: f32) {
        let volume = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 1.0 };
        self.shared.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume.load(Ordering::Relaxed))
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        self.send(Command::SetRepeat(repeat));
    }

    /// Выравнивание громкости; играющий трек перезапускается с того же места.
    pub fn set_loudness(&self, loudness: Loudness) {
        self.send(Command::SetLoudness(loudness));
    }

    /// Скорость; играющий трек перезапускается с того же места.
    pub fn set_speed(&self, speed: Speed) {
        self.send(Command::SetSpeed(speed));
    }

    /// Кроссфейд между папками; внутри альбома — встык.
    pub fn set_crossfade(&self, duration: Option<Duration>) {
        self.send(Command::SetCrossfade(duration));
    }

    /// Эксклюзивный вывод в формате файла; трек продолжается в новом режиме.
    pub fn set_exclusive(&self, on: bool) {
        self.send(Command::SetExclusive(on));
    }

    /// Пауза при смене устройства или порта. По умолчанию выключено.
    pub fn set_pause_on_device_change(&self, on: bool) {
        self.send(Command::SetPauseOnDeviceChange(on));
    }

    /// Громкость по имени устройства — ставится до того, как оно зазвучит.
    pub fn set_device_volumes(&self, volumes: HashMap<String, f32>) {
        self.send(Command::SetDeviceVolumes(volumes));
    }

    /// Эквалайзер. Применяется сразу, без перезапуска трека.
    pub fn set_equalizer(&self, equalizer: Equalizer) {
        self.send(Command::SetEqualizer(equalizer));
    }

    /// Во сколько раз быстрее записи звучит текущий трек.
    pub fn speed(&self) -> f64 {
        lock(&self.status).playhead.as_ref().map_or(1.0, |head| head.speed)
    }

    pub fn set_stop_after_current(&self, on: bool) {
        self.send(Command::StopAfterCurrent(on));
    }

    pub fn state(&self) -> State {
        lock(&self.status).state
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        lock(&self.status).playhead.as_ref().map(|head| head.now.clone())
    }

    /// Слышимая позиция, а не позиция декодера.
    pub fn position(&self) -> Option<Duration> {
        let status = lock(&self.status);
        let head = status.playhead.as_ref()?;
        let played = self.shared.frames_played.load(Ordering::Acquire);
        Some(Duration::from_secs_f64(playhead_seconds(
            head.base,
            head.start_abs,
            played,
            head.rate,
            head.speed,
        )))
    }

    /// Позиция, если сейчас звучит именно `path` (на стыке треков интерфейс ещё не в курсе).
    pub fn position_in(&self, path: &Path) -> Option<Duration> {
        let status = lock(&self.status);
        let head = status.playhead.as_ref().filter(|head| head.now.path == path)?;
        let played = self.shared.frames_played.load(Ordering::Acquire);
        Some(Duration::from_secs_f64(playhead_seconds(
            head.base,
            head.start_abs,
            played,
            head.rate,
            head.speed,
        )))
    }

    /// Сколько раз устройство осталось без звука, пока движок ещё декодировал.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(OutputTarget::Device)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Отравленный мьютекс — не повод падать: данные простые.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Частота, каналы и разрядность файла — для эксклюзивного вывода.
fn format_of(info: &TrackInfo) -> (u32, usize, u32) {
    (
        info.sample_rate.unwrap_or(44_100),
        info.channels.unwrap_or(2).max(1),
        info.bits_per_sample.unwrap_or(16),
    )
}

/// Позиция в треке: кадры вывода переводятся в секунды записи с учётом скорости.
fn playhead_seconds(base: f64, start_abs: u64, played: u64, rate: u32, speed: f64) -> f64 {
    if rate == 0 {
        return base;
    }
    base + played.saturating_sub(start_abs) as f64 / f64::from(rate) * speed
}

fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / f64::from(rate.max(1)))
}

/// Файл в работе у декодера.
struct Decoding {
    item: QueueItem,
    /// Номер открытия: при повторе тот же трек открывается заново.
    seq: u64,
    decoder: TrackDecoder,
    converter: Option<Converter>,
    /// Первый кусок звука уже отдан, отрезок трека заведён.
    started: bool,
    /// Позиция в треке, с которой начнётся звук (не ноль после перемотки).
    base: f64,
    /// Сообщать о начале трека (после перемотки не надо).
    announce: bool,
    /// Множитель громкости по ReplayGain.
    gain: f32,
}

/// Отрезок вывода, принадлежащий одному треку.
struct Segment {
    item: QueueItem,
    seq: u64,
    info: TrackInfo,
    start_abs: u64,
    base: f64,
    announce: bool,
    /// Во сколько раз быстрее записи звучит отрезок.
    speed: f64,
}

/// Последний отданный декодеру элемент; место — на случай, если его уберут.
struct Cursor {
    id: u64,
    index: usize,
}

enum Step {
    Audio { boundary: Option<usize>, fresh: usize, speed: f64 },
    End,
}

/// Начало нового трека накладывается на придержанный хвост прошлого.
struct Mixing {
    tail: VecDeque<f32>,
    /// Длина хвоста в кадрах и сколько кадров уже смешано.
    total: usize,
    done: usize,
}

struct Worker {
    commands: Receiver<Command>,
    events: Sender<Event>,
    shared: Arc<Shared>,
    status: Arc<Mutex<Status>>,
    queue: Arc<Mutex<Queue>>,
    target: OutputTarget,
    output: Option<Output>,
    state: State,
    repeat: Repeat,
    loudness: Loudness,
    speed: Speed,
    equalizer: Equalizer,
    crossfade: Option<Duration>,
    exclusive: bool,
    /// Громкость известных устройств — до открытия нового.
    device_volumes: HashMap<String, f32>,
    /// Имя последнего открытого устройства.
    last_device: Option<String>,
    /// Частоты, которые устройство не берёт эксклюзивно.
    exclusive_unsupported: std::collections::HashSet<u32>,
    /// Эксклюзив тут не открылся — не пробуем снова до смены устройства.
    exclusive_failed: bool,
    /// Следующий трек с другой частотой ждёт, пока доиграет буфер.
    switch_to: Option<Decoding>,
    /// Хвост текущего трека — для перехода.
    held: VecDeque<f32>,
    /// Хвост закончившегося трека и его папка — ждёт начала следующего.
    fade: Option<(VecDeque<f32>, Option<PathBuf>)>,
    mixing: Option<Mixing>,
    stop_after_current: bool,
    /// Подготовленный элемент и позиция для «играть» из остановки.
    cued: Option<(u64, f64)>,
    decoding: Option<Decoding>,
    /// Преобразователь прошлого трека: тот же формат — ресемплер не заметит стыка.
    carry: Option<Converter>,
    cursor: Option<Cursor>,
    /// Готовый звук, который ещё не поместился в кольцевой буфер.
    pending: Vec<f32>,
    pending_pos: usize,
    /// Кадров записано в буфер (в единицах `Shared::frames_played`).
    frames_pushed: u64,
    segments: VecDeque<Segment>,
    current: Option<Segment>,
    /// Кадр, на котором кончается звук, когда декодировать больше нечего.
    end_at: Option<u64>,
    failed_in_row: usize,
    next_seq: u64,
    /// Декодеру сейчас нечего делать: буфер полон или всё декодировано.
    decode_idle: bool,
    idle_since: Option<Instant>,
    last_open: Option<Instant>,
    /// Когда последний раз сверялись с устройством по умолчанию.
    device_checked: Option<Instant>,
    /// Пауза при смене устройства: включена ли, когда сверялись с портом и что звучало.
    pause_on_switch: bool,
    switch_checked: Option<Instant>,
    switch_watch: SwitchWatch,
    /// Вывод пересоздаётся из-за смены устройства по умолчанию, а не из-за сбоя.
    device_switch: bool,
    waker: Arc<Mutex<Option<Waker>>>,
    /// Подключение к звуковой системе (в Linux — сеанс PipeWire/PulseAudio, один на всё).
    audio: Option<crate::output::Audio>,
}

impl Worker {
    fn new(
        commands: Receiver<Command>,
        events: Sender<Event>,
        shared: Arc<Shared>,
        status: Arc<Mutex<Status>>,
        queue: Arc<Mutex<Queue>>,
        target: OutputTarget,
    ) -> Self {
        Self {
            commands,
            events,
            shared,
            status,
            queue,
            target,
            output: None,
            state: State::Stopped,
            repeat: Repeat::Off,
            loudness: Loudness::Off,
            speed: Speed::NORMAL,
            equalizer: Equalizer::default(),
            crossfade: None,
            exclusive: false,
            device_volumes: HashMap::new(),
            last_device: None,
            exclusive_unsupported: std::collections::HashSet::new(),
            exclusive_failed: false,
            switch_to: None,
            held: VecDeque::new(),
            fade: None,
            mixing: None,
            stop_after_current: false,
            cued: None,
            decoding: None,
            carry: None,
            cursor: None,
            pending: Vec::new(),
            pending_pos: 0,
            frames_pushed: 0,
            segments: VecDeque::new(),
            current: None,
            end_at: None,
            failed_in_row: 0,
            next_seq: 0,
            decode_idle: true,
            idle_since: None,
            last_open: None,
            device_checked: None,
            pause_on_switch: false,
            switch_checked: None,
            switch_watch: SwitchWatch::default(),
            device_switch: false,
            waker: Arc::default(),
            audio: None,
        }
    }

    fn audio(&mut self) -> &crate::output::Audio {
        self.audio.get_or_insert_with(crate::output::Audio::new)
    }

    /// Устройство по умолчанию; не вышло — переподключаемся к звуковой системе.
    fn open_shared_device(&mut self) -> Result<Output, String> {
        let shared = self.shared.clone();
        Output::open_device(&shared, self.audio()).or_else(|_| {
            self.audio = None;
            Output::open_device(&shared, self.audio())
        })
    }

    fn run(mut self) {
        loop {
            let wait = self.wait_time();
            let first = if wait.is_zero() {
                match self.commands.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match self.commands.recv_timeout(wait) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            if let Some(command) = first {
                if !self.handle(command) {
                    return;
                }
                while let Ok(command) = self.commands.try_recv() {
                    if !self.handle(command) {
                        return;
                    }
                }
            }
            self.follow_default_device();
            self.check_output();
            self.update_playhead();
            self.pump();
            self.update_playhead();
            self.manage_idle();
        }
    }

    /// Сколько спать до следующего дела; на паузе — не просыпаться.
    fn wait_time(&self) -> Duration {
        if !self.decode_idle {
            return Duration::ZERO;
        }
        let Some(output) = self.output.as_ref() else {
            return LONG_WAIT;
        };
        if self.state != State::Playing {
            return match self.idle_since {
                Some(since) if output.is_running() => IDLE_STREAM_TIMEOUT
                    .saturating_sub(since.elapsed())
                    .max(Duration::from_millis(1)),
                _ => LONG_WAIT,
            };
        }

        let filling = self.has_pending() || self.decoding.is_some();
        if filling && !output.is_realtime() {
            return Duration::from_millis(1);
        }
        // Буфер полон: просыпаемся, когда он опустеет наполовину.
        let mut wait = if filling {
            let over_half = output
                .buffered_frames()
                .saturating_sub(output.capacity_frames() / 2);
            frames_to_duration(over_half as u64, output.rate)
        } else {
            Duration::from_millis(500)
        };
        // И не позже, чем звук дойдёт до границы трека или до конца.
        let boundary = self.segments.front().map(|segment| segment.start_abs).or(self.end_at);
        if let Some(boundary) = boundary {
            let played = self.shared.frames_played.load(Ordering::Acquire);
            wait = wait.min(frames_to_duration(boundary.saturating_sub(played), output.rate));
        }
        wait.clamp(Duration::from_millis(2), Duration::from_millis(500))
    }

    /// Выполняет команду. Возвращает `false`, когда движок должен завершиться.
    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::SetQueue(paths) => {
                self.stop();
                lock(&self.queue).replace(paths);
                self.emit(Event::QueueChanged);
            }
            Command::Cue(paths, index, position) => {
                self.stop();
                let id = {
                    let mut queue = lock(&self.queue);
                    queue.replace(paths);
                    queue.get(index).map(|item| item.id)
                };
                self.set_cued(id.map(|id| (id, position.as_secs_f64())));
                self.emit(Event::QueueChanged);
            }
            Command::PlayNow(paths) => {
                let first = {
                    let at = self.playing_index().map_or(0, |index| index + 1);
                    let mut queue = lock(&self.queue);
                    let at = at.min(queue.len());
                    queue.insert_at(at, paths);
                    queue.get(at).map(|item| item.id)
                };
                self.emit(Event::QueueChanged);
                if let Some(id) = first {
                    self.start_playing(id);
                }
            }
            Command::Move(id, index) => {
                if lock(&self.queue).move_to(id, index) {
                    self.emit(Event::QueueChanged);
                    self.after_queue_edit();
                }
            }
            Command::Reorder(ids) => {
                lock(&self.queue).reorder(&ids);
                self.emit(Event::QueueChanged);
                self.after_queue_edit();
            }
            Command::PlayList(paths, start) => {
                self.stop();
                let first = {
                    let mut queue = lock(&self.queue);
                    queue.replace(paths);
                    queue.get(start).or_else(|| queue.get(0)).map(|item| item.id)
                };
                self.emit(Event::QueueChanged);
                if let Some(id) = first {
                    self.start_playing(id);
                }
            }
            Command::Append(paths) => {
                lock(&self.queue).append(paths);
                self.emit(Event::QueueChanged);
                self.after_queue_edit();
            }
            Command::InsertNext(paths) => {
                let at = self.playing_index().map_or(0, |index| index + 1);
                lock(&self.queue).insert_at(at, paths);
                self.emit(Event::QueueChanged);
                self.after_queue_edit();
            }
            Command::Remove(id) => self.remove(id),
            Command::ClearQueue => {
                self.stop();
                lock(&self.queue).clear();
                self.emit(Event::QueueChanged);
            }
            Command::Play => self.play(),
            Command::Pause => {
                if self.state == State::Playing {
                    self.set_state(State::Paused);
                }
            }
            Command::Toggle => match self.state {
                State::Playing => self.set_state(State::Paused),
                State::Paused | State::Stopped => self.play(),
            },
            Command::Stop => self.stop(),
            Command::Next => self.next(),
            Command::Previous => self.previous(),
            Command::JumpTo(id) => self.start_playing(id),
            Command::Seek(position) => self.seek(position),
            Command::SetLoudness(loudness) => {
                self.loudness = loudness;
                if self.state != State::Stopped
                    && let Some(id) = self.current.as_ref().map(|current| current.item.id)
                {
                    let position = self.position_seconds();
                    self.start(id, position, false);
                }
            }
            Command::SetSpeed(speed) => {
                // На обычной скорости звук не обрабатывается — переключать нечего.
                let same = speed == self.speed || (speed.is_normal() && self.speed.is_normal());
                self.speed = speed;
                if !same
                    && self.state != State::Stopped
                    && let Some(id) = self.current.as_ref().map(|current| current.item.id)
                {
                    let position = self.position_seconds();
                    self.start(id, position, false);
                }
            }
            Command::SetDeviceVolumes(volumes) => self.device_volumes = volumes,
            Command::SetPauseOnDeviceChange(on) => {
                self.pause_on_switch = on;
                self.switch_watch.forget();
            }
            Command::SetExclusive(on) => {
                if on != self.exclusive {
                    self.exclusive = on;
                    self.exclusive_failed = false;
                    self.exclusive_unsupported.clear();
                    let resume = self
                        .current
                        .as_ref()
                        .map(|current| (current.item.id, self.position_seconds()))
                        .filter(|_| self.state != State::Stopped);
                    // Устройство открывается заново в новом режиме.
                    self.flush();
                    self.output = None;
                    if let Some((id, position)) = resume {
                        self.start(id, position, false);
                    }
                }
            }
            Command::SetCrossfade(duration) => {
                self.crossfade = duration.filter(|duration| !duration.is_zero());
                if self.crossfade.is_none() {
                    // Придержанное больше не нужно: отдаём как есть.
                    self.pending.extend(self.held.drain(..));
                    if let Some((tail, _)) = self.fade.take() {
                        self.pending.extend(tail);
                    }
                }
            }
            Command::SetEqualizer(equalizer) => {
                self.equalizer = equalizer;
                self.push_equalizer();
            }
            Command::SetRepeat(repeat) => {
                self.repeat = repeat;
                self.after_queue_edit();
            }
            Command::StopAfterCurrent(on) => {
                self.stop_after_current = on;
                self.after_queue_edit();
            }
            Command::Shutdown => return false,
        }
        true
    }

    fn play(&mut self) {
        match self.state {
            State::Playing => {}
            State::Paused => self.set_state(State::Playing),
            State::Stopped => {
                if let Some((id, position)) = self.cued {
                    if self.start(id, position, true) {
                        self.set_state(State::Playing);
                    }
                    return;
                }
                let first = lock(&self.queue).get(0).map(|item| item.id);
                if let Some(id) = first {
                    self.start_playing(id);
                }
            }
        }
    }

    fn start_playing(&mut self, id: u64) {
        if self.start(id, 0.0, true) {
            self.set_state(State::Playing);
        }
    }

    /// Запоминает подготовленный элемент и показывает его снаружи.
    fn set_cued(&mut self, cued: Option<(u64, f64)>) {
        self.cued = cued;
        lock(&self.status).cued = cued.map(|(id, _)| id);
    }

    /// Сдвинуть подготовленный элемент без звука.
    fn move_cue(&mut self, id: u64) -> bool {
        if self.cued.is_none() || self.current.is_some() || self.decoding.is_some() {
            return false;
        }
        self.set_cued(Some((id, 0.0)));
        self.emit(Event::QueueChanged);
        true
    }

    fn stop(&mut self) {
        self.set_cued(None);
        self.flush();
        self.clear_pipeline();
        self.cursor = None;
        self.current = None;
        lock(&self.status).playhead = None;
        self.set_state(State::Stopped);
    }

    fn next(&mut self) {
        let target = {
            let queue = lock(&self.queue);
            let item = match self.playing_id().and_then(|id| queue.index_of(id)) {
                Some(index) if index + 1 < queue.len() => queue.get(index + 1),
                Some(_) if self.repeat == Repeat::All => queue.get(0),
                Some(_) => None,
                None => queue.get(0),
            };
            item.map(|item| item.id)
        };
        match target {
            Some(id) if self.move_cue(id) => {}
            Some(id) => self.jump_keeping_pause(id),
            None if self.cued.is_some() => {}
            None => self.stop(),
        }
    }

    fn previous(&mut self) {
        if self.current.is_some() && self.position_seconds() > PREVIOUS_RESTARTS_AFTER {
            self.seek(Duration::ZERO);
            return;
        }
        let target = {
            let queue = lock(&self.queue);
            self.playing_id()
                .and_then(|id| queue.index_of(id))
                .and_then(|index| queue.get(index.saturating_sub(1)))
                .map(|item| item.id)
        };
        if let Some(id) = target
            && !self.move_cue(id)
        {
            self.jump_keeping_pause(id);
        }
    }

    /// Переходит к треку: из остановки начинает играть, пауза остаётся паузой.
    fn jump_keeping_pause(&mut self, id: u64) {
        if self.start(id, 0.0, true) && self.state == State::Stopped {
            self.set_state(State::Playing);
        }
    }

    fn seek(&mut self, position: Duration) {
        // Трек только запущен и ещё не зазвучал — перематываем его же.
        let pending = self
            .decoding
            .as_ref()
            .filter(|_| self.current.is_none())
            .map(|decoding| (decoding.item.id, decoding.announce));
        if let Some((id, announce)) = pending {
            self.start(id, position.as_secs_f64(), announce);
            return;
        }
        if self.current.is_none()
            && let Some((id, _)) = self.cued
        {
            self.set_cued(Some((id, position.as_secs_f64())));
            return;
        }
        let Some(current) = self.current.as_ref() else {
            return;
        };
        let id = current.item.id;
        if current.info.duration.is_some_and(|duration| position >= duration) {
            self.next();
            return;
        }
        self.start(id, position.as_secs_f64(), false);
    }

    fn remove(&mut self, id: u64) {
        let was_current = self.current.as_ref().is_some_and(|current| current.item.id == id);
        let Some(index) = lock(&self.queue).remove(id) else {
            return;
        };
        if self.cued.is_some_and(|(cued, _)| cued == id) {
            let heir = lock(&self.queue).get(index).map(|item| item.id);
            self.set_cued(heir.map(|heir| (heir, 0.0)));
        }
        self.emit(Event::QueueChanged);
        if !was_current {
            self.after_queue_edit();
            return;
        }
        // Убрали то, что играет: звучать начинает трек, занявший его место.
        let heir = lock(&self.queue).get(index).map(|item| item.id);
        match heir {
            Some(heir) => {
                self.start(heir, 0.0, true);
            }
            None => self.stop(),
        }
    }

    /// Декодировать элемент с позиции. Пауза остаётся паузой. `false` — нечем играть.
    fn start(&mut self, id: u64, position: f64, announce: bool) -> bool {
        let found = {
            let queue = lock(&self.queue);
            queue
                .index_of(id)
                .and_then(|index| queue.get(index).cloned().map(|item| (index, item)))
        };
        let Some((index, item)) = found else {
            return false;
        };
        self.set_cued(None);
        // Перемотка внутри уже открытого файла дешевле, чем открытие заново.
        let reused = if position > 0.0 {
            self.decoding.take().filter(|decoding| decoding.item.id == id)
        } else {
            None
        };

        self.flush();
        self.clear_pipeline();
        self.cursor = Some(Cursor { id, index });
        let opened = match reused {
            Some(decoding) => Ok(decoding.decoder),
            None => guard(|| TrackDecoder::open(&item.path, OpenOptions::default())),
        };
        // В эксклюзивном режиме устройство открывается под формат файла.
        let wanted = opened.as_ref().ok().map(|decoder| format_of(decoder.info()));
        if self.exclusive
            && !self.exclusive_failed
            && let Some((rate, _, _)) = wanted
            && self.output.as_ref().is_some_and(|output| {
                output.exclusive.is_none_or(|format| format.rate != rate && !self.exclusive_unsupported.contains(&rate))
            })
        {
            self.output = None;
        }
        if self.output.is_none() && !self.open_output(wanted) {
            self.current = None;
            lock(&self.status).playhead = None;
            self.set_state(State::Stopped);
            return false;
        }
        self.shared.expect_data.store(true, Ordering::Relaxed);
        let mut decoder = match opened {
            Ok(decoder) => decoder,
            Err(error) => {
                // Курсор уже стоит на этом треке, так что движок пойдёт дальше по очереди.
                self.failed_in_row += 1;
                self.emit(Event::TrackFailed {
                    id,
                    path: item.path,
                    error,
                });
                return true;
            }
        };
        let mut base = 0.0;
        if position > 0.0 {
            match guard(|| decoder.seek(Duration::from_secs_f64(position))) {
                Ok(landed) => base = landed.as_secs_f64(),
                // За концом файла или перемотка не умеет — считаем доигранным.
                Err(_) => return true,
            }
        }
        let seq = self.bump_seq();
        let gain = self.loudness.factor(&decoder.info().replay_gain);
        self.decoding = Some(Decoding {
            item,
            seq,
            decoder,
            converter: None,
            started: false,
            base,
            announce,
            gain,
        });
        true
    }

    /// Очередь поменялась, а декодер уже ушёл не туда — перезапуск с той же позиции.
    fn after_queue_edit(&mut self) {
        self.failed_in_row = 0;
        if self.state == State::Stopped {
            return;
        }
        let Some((current_id, current_seq)) =
            self.current.as_ref().map(|current| (current.item.id, current.seq))
        else {
            return;
        };
        let ahead = self
            .segments
            .front()
            .map(|segment| segment.item.id)
            .or_else(|| {
                self.decoding
                    .as_ref()
                    .filter(|decoding| decoding.seq != current_seq)
                    .map(|decoding| decoding.item.id)
            });
        let expected = self.next_after(current_id);
        match ahead {
            Some(ahead) if Some(ahead) != expected => {
                let position = self.position_seconds();
                self.start(current_id, position, false);
            }
            // Декодер уже дошёл до конца очереди, а у неё появилось продолжение.
            None if self.end_at.is_some() && expected.is_some() => {
                self.end_at = None;
                self.shared.expect_data.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Что должно играть после элемента `id` по текущим правилам.
    fn next_after(&self, id: u64) -> Option<u64> {
        if self.stop_after_current {
            return None;
        }
        let queue = lock(&self.queue);
        let index = queue.index_of(id)?;
        let next = if self.repeat == Repeat::One { index } else { index + 1 };
        let next = if next < queue.len() {
            next
        } else if self.repeat == Repeat::All {
            0
        } else {
            return None;
        };
        queue.get(next).map(|item| item.id)
    }

    /// Следующий элемент для декодера относительно курсора.
    fn next_item(&self) -> Option<(usize, QueueItem)> {
        let queue = lock(&self.queue);
        let index = match &self.cursor {
            None => 0,
            Some(cursor) => match queue.index_of(cursor.id) {
                Some(index) if self.repeat == Repeat::One => index,
                Some(index) => index + 1,
                // Элемент убрали из очереди: на его месте уже стоит следующий.
                None => cursor.index,
            },
        };
        let index = if index < queue.len() {
            index
        } else if self.repeat == Repeat::All {
            0
        } else {
            return None;
        };
        queue.get(index).cloned().map(|item| (index, item))
    }

    /// Следующий трек; не открылся — пропуск. `false` — играть нечего.
    fn advance(&mut self) -> bool {
        loop {
            let queue_len = lock(&self.queue).len();
            // Если подряд не открылась вся очередь, дальше крутить её незачем.
            if queue_len == 0 || self.failed_in_row >= queue_len {
                return false;
            }
            if self.stop_after_current && self.cursor.is_some() {
                return false;
            }
            let Some((index, item)) = self.next_item() else {
                return false;
            };
            self.cursor = Some(Cursor { id: item.id, index });
            match guard(|| TrackDecoder::open(&item.path, OpenOptions::default())) {
                Ok(decoder) => {
                    let seq = self.bump_seq();
                    let gain = self.loudness.factor(&decoder.info().replay_gain);
                    // Эксклюзив и другая частота: сначала доиграть буфер, потом переоткрыть устройство.
                    let other_rate = self.exclusive
                        && self.output.as_ref().is_some_and(|output| {
                            output.exclusive.is_some_and(|format| {
                                decoder.info().sample_rate.is_some_and(|rate| {
                                    rate != format.rate && !self.exclusive_unsupported.contains(&rate)
                                })
                            })
                        });
                    if other_rate {
                        self.switch_to = Some(Decoding {
                            item,
                            seq,
                            decoder,
                            converter: None,
                            started: false,
                            base: 0.0,
                            announce: true,
                            gain,
                        });
                        return false;
                    }
                    self.decoding = Some(Decoding {
                        item,
                        seq,
                        decoder,
                        converter: None,
                        started: false,
                        base: 0.0,
                        announce: true,
                        gain,
                    });
                    return true;
                }
                Err(error) => {
                    self.failed_in_row += 1;
                    self.emit(Event::TrackFailed {
                        id: item.id,
                        path: item.path,
                        error,
                    });
                }
            }
        }
    }

    /// Декодировать в буфер, пока он не полон или не вышло время.
    fn pump(&mut self) {
        self.decode_idle = true;
        if self.state == State::Stopped || self.output.is_none() {
            return;
        }
        let deadline = Instant::now() + PUMP_BUDGET;
        loop {
            if !self.push_pending() || self.end_at.is_some() {
                return;
            }
            if self.decoding.is_none() && !self.advance() {
                self.finish_decoding();
                continue;
            }
            self.decode_step();
            if Instant::now() >= deadline {
                self.decode_idle = false;
                return;
            }
        }
    }

    /// Переносит готовый звук в кольцевой буфер. `true`, если перенесено всё.
    fn push_pending(&mut self) -> bool {
        if self.has_pending() {
            let Some(output) = self.output.as_mut() else {
                return false;
            };
            let written = output.write(&self.pending[self.pending_pos..]);
            self.pending_pos += written;
            self.frames_pushed += (written / output.channels) as u64;
        }
        if self.has_pending() {
            return false;
        }
        self.pending.clear();
        self.pending_pos = 0;
        true
    }

    fn has_pending(&self) -> bool {
        self.pending_pos < self.pending.len()
    }

    /// Декодирует один кусок текущего файла и добавляет его к готовому звуку.
    fn decode_step(&mut self) {
        let Some((out_rate, out_channels)) =
            self.output.as_ref().map(|output| (output.rate, output.channels))
        else {
            return;
        };
        let Some(mut decoding) = self.decoding.take() else {
            return;
        };
        if !decoding.started
            && let Some((tail, folder)) = self.fade.take()
        {
            let other_album = decoding.item.path.parent().map(PathBuf::from) != folder;
            if self.crossfade.is_some() && other_album && !tail.is_empty() {
                let total = tail.len() / out_channels;
                self.mixing = Some(Mixing { tail, total, done: 0 });
            } else {
                self.pending.extend(tail);
            }
        }
        let before = self.pending.len();
        let pending = &mut self.pending;
        let carry = &mut self.carry;
        let speed = self.speed;

        let step = guard(|| {
            let Some(chunk) = decoding.decoder.next_chunk()? else {
                return Ok(Step::End);
            };
            let fits = decoding
                .converter
                .as_ref()
                .is_some_and(|converter| converter.matches(chunk.rate, chunk.channels));
            if !fits {
                // Формат сменился посреди файла: хвост старого преобразователя доигрываем.
                if let Some(mut old) = decoding.converter.take() {
                    old.flush(pending)?;
                }
                decoding.converter = Some(match carry.take() {
                    Some(inherited) if inherited.matches(chunk.rate, chunk.channels) => inherited,
                    other => {
                        if let Some(mut previous) = other {
                            previous.flush(pending)?;
                        }
                        Converter::new(chunk.rate, chunk.channels, out_rate, out_channels, speed)?
                    }
                });
            }
            // Всё, что лежит в `pending` до этой точки, принадлежит предыдущему треку.
            let boundary = (!decoding.started).then_some(pending.len());
            let fresh = pending.len();
            if let Some(converter) = decoding.converter.as_mut() {
                converter.process(chunk.samples, pending)?;
            }
            if decoding.gain != 1.0 {
                for sample in &mut pending[fresh..] {
                    *sample *= decoding.gain;
                }
            }
            let speed = decoding.converter.as_ref().map_or(1.0, Converter::speed);
            Ok(Step::Audio { boundary, fresh, speed })
        });

        match step {
            Ok(Step::Audio { boundary, fresh, speed }) => {
                self.mix_and_hold(fresh, out_channels);
                if let Some(boundary) = boundary {
                    decoding.started = true;
                    self.failed_in_row = 0;
                    let ahead = boundary.saturating_sub(self.pending_pos) / out_channels;
                    self.segments.push_back(Segment {
                        item: decoding.item.clone(),
                        seq: decoding.seq,
                        info: decoding.decoder.info().clone(),
                        start_abs: self.frames_pushed + ahead as u64,
                        base: decoding.base,
                        announce: decoding.announce,
                        speed,
                    });
                }
                self.decoding = Some(decoding);
            }
            Ok(Step::End) => self.finish_track(decoding, None),
            Err(error) => {
                // Сбой мог оставить в `pending` недописанный кадр.
                self.pending.truncate(before);
                self.finish_track(decoding, Some(error));
            }
        }
    }

    fn finish_track(&mut self, mut decoding: Decoding, error: Option<TrackError>) {
        let QueueItem { id, path, .. } = decoding.item;
        let folder = path.parent().map(PathBuf::from);
        let error = match error {
            Some(error) => Some(error),
            None if !decoding.started => Some(TrackError::Malformed("в файле нет звука".into())),
            None => None,
        };
        match error {
            Some(error) => {
                if !decoding.started {
                    self.failed_in_row += 1;
                }
                self.emit(Event::TrackFailed { id, path, error });
            }
            None => {
                let decode_errors = decoding.decoder.decode_errors();
                let missing = decoding.decoder.missing();
                if decode_errors > 0 || missing.is_some() {
                    self.emit(Event::TrackDamaged {
                        id,
                        path,
                        decode_errors,
                        missing,
                    });
                }
            }
        }
        if let Some(mut converter) = decoding.converter.take() {
            if converter.speed() == 1.0 && self.crossfade.is_none() {
                self.carry = Some(converter);
            } else {
                // Растянутый трек дописывается целиком, иначе съедут границы.
                let before = self.pending.len();
                let pending = &mut self.pending;
                if guard(|| converter.flush(pending)).is_err() {
                    self.pending.truncate(before);
                }
                let channels = self.output.as_ref().map_or(1, |output| output.channels);
                self.mix_and_hold(before, channels);
            }
        }
        if self.crossfade.is_some() && decoding.started {
            // Трек короче перехода: остаток прошлого хвоста угасает сам.
            if let Some(mixing) = self.mixing.take() {
                let channels = self.output.as_ref().map_or(1, |output| output.channels);
                let rest: Vec<f32> = mixing.tail.into_iter().collect();
                for (index, sample) in rest.into_iter().enumerate() {
                    let frame = mixing.done + index / channels;
                    let t = (frame as f32 + 0.5) / mixing.total.max(1) as f32;
                    self.held.push_back(sample * (t * std::f32::consts::FRAC_PI_2).cos());
                }
            }
            let tail = std::mem::take(&mut self.held);
            self.fade = Some((tail, folder));
        }
    }

    /// Смешать начало нового трека с хвостом прошлого (косинус/синус), придержать хвост.
    /// `fresh` — где начинаются новые сэмплы.
    fn mix_and_hold(&mut self, fresh: usize, channels: usize) {
        let fresh = fresh.min(self.pending.len());
        if let Some(mixing) = self.mixing.as_mut() {
            for frame in self.pending[fresh..].chunks_exact_mut(channels.max(1)) {
                if mixing.tail.len() < frame.len() {
                    break;
                }
                let t = (mixing.done as f32 + 0.5) / mixing.total.max(1) as f32;
                let (fade_out, fade_in) = {
                    let angle = t.min(1.0) * std::f32::consts::FRAC_PI_2;
                    (angle.cos(), angle.sin())
                };
                for sample in frame.iter_mut() {
                    let old = mixing.tail.pop_front().unwrap_or(0.0);
                    *sample = old * fade_out + *sample * fade_in;
                }
                mixing.done += 1;
            }
            if mixing.tail.len() < channels.max(1) {
                self.mixing = None;
            }
        }
        let Some(duration) = self.crossfade else {
            return;
        };
        let rate = self.output.as_ref().map_or(48_000, |output| output.rate);
        let limit = (duration.as_secs_f64() * f64::from(rate)) as usize * channels.max(1);
        self.held.extend(self.pending.drain(fresh..));
        if self.held.len() > limit {
            let excess = self.held.len() - limit;
            self.pending.extend(self.held.drain(..excess));
        }
    }

    /// Декодировать нечего: хвост ресемплера и кадр конца.
    fn finish_decoding(&mut self) {
        if let Some(mut converter) = self.carry.take() {
            let before = self.pending.len();
            let pending = &mut self.pending;
            if guard(|| converter.flush(pending)).is_err() {
                self.pending.truncate(before);
            }
        }
        // Переходить больше некуда: придержанное звучит как есть.
        if let Some((tail, _)) = self.fade.take() {
            self.pending.extend(tail);
        }
        if let Some(mixing) = self.mixing.take() {
            self.pending.extend(mixing.tail);
        }
        self.pending.extend(self.held.drain(..));
        let channels = self.output.as_ref().map_or(1, |output| output.channels);
        let ahead = (self.pending.len() - self.pending_pos) / channels;
        self.end_at = Some(self.frames_pushed + ahead as u64);
        self.shared.expect_data.store(false, Ordering::Relaxed);
    }

    /// Сверить отрезки с прозвучавшим и сообщить о стыках и конце очереди.
    fn update_playhead(&mut self) {
        let played = self.shared.frames_played.load(Ordering::Acquire);
        while self
            .segments
            .front()
            .is_some_and(|segment| segment.start_abs <= played)
        {
            let Some(segment) = self.segments.pop_front() else {
                break;
            };
            let now = NowPlaying {
                id: segment.item.id,
                path: segment.item.path.clone(),
                info: segment.info.clone(),
            };
            lock(&self.status).playhead = Some(Playhead {
                now: now.clone(),
                start_abs: segment.start_abs,
                base: segment.base,
                rate: self.output.as_ref().map_or(0, |output| output.rate),
                speed: segment.speed,
            });
            if segment.announce {
                self.emit(Event::TrackStarted(now));
            }
            self.current = Some(segment);
        }

        if let Some(end) = self.end_at
            && played >= end
            && self.segments.is_empty()
            && !self.has_pending()
            && let Some(next) = self.switch_to.take()
        {
            self.switch_rate(next);
            return;
        }
        if let Some(end) = self.end_at
            && played >= end
            && self.segments.is_empty()
            && !self.has_pending()
        {
            self.clear_pipeline();
            self.cursor = None;
            self.current = None;
            self.stop_after_current = false;
            lock(&self.status).playhead = None;
            self.set_state(State::Stopped);
            self.emit(Event::Finished);
        }
    }

    /// Просит звуковой поток выбросить буфер и ждёт, пока он это сделает.
    fn flush(&mut self) {
        self.pending.clear();
        self.pending_pos = 0;
        let Some(output) = self.output.as_mut() else {
            return;
        };
        // Спящий поток просьбу не увидит, поэтому сначала будим его.
        output.set_running(true);
        if self.state != State::Playing {
            self.idle_since = Some(Instant::now());
        }
        self.shared.flush.store(true, Ordering::Release);
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        while self.shared.flush.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                // Поток не отвечает: считаем устройство потерянным.
                self.shared.failed.store(true, Ordering::Release);
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.frames_pushed = self.shared.frames_played.load(Ordering::Acquire);
    }

    fn clear_pipeline(&mut self) {
        self.decoding = None;
        self.carry = None;
        self.segments.clear();
        self.end_at = None;
        self.pending.clear();
        self.pending_pos = 0;
        self.failed_in_row = 0;
        self.held.clear();
        self.fade = None;
        self.mixing = None;
        self.switch_to = None;
    }

    /// Буфер доиграл — переоткрыть устройство под частоту следующего.
    fn switch_rate(&mut self, next: Decoding) {
        let wanted = format_of(next.decoder.info());
        self.end_at = None;
        self.output = None;
        if !self.open_output(Some(wanted)) {
            self.clear_pipeline();
            self.current = None;
            lock(&self.status).playhead = None;
            self.set_state(State::Stopped);
            return;
        }
        self.decoding = Some(next);
        self.shared.expect_data.store(true, Ordering::Relaxed);
    }

    /// `wanted` — частота, каналы и разрядность файла (для эксклюзива).
    fn open_output(&mut self, wanted: Option<(u32, usize, u32)>) -> bool {
        self.last_open = Some(Instant::now());
        if matches!(self.target, OutputTarget::Device) && !self.device_volumes.is_empty() {
            let name = self.audio().default_name();
            if name != self.last_device
                && let Some(volume) = name.and_then(|name| self.device_volumes.get(&name).copied())
            {
                self.shared.volume.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            }
        }
        let opened = match (&self.target, self.exclusive, wanted) {
            (OutputTarget::Device, true, Some((rate, channels, bits))) if !self.exclusive_failed => {
                match Output::open_exclusive(&self.shared, rate, channels, bits) {
                    Ok(output) => {
                        if output.rate != rate {
                            self.exclusive_unsupported.insert(rate);
                        }
                        Ok(output)
                    }
                    Err(error) => {
                        // Не вышло — играем в общем режиме, но говорим почему.
                        self.exclusive_failed = true;
                        self.emit(Event::ExclusiveUnavailable(error));
                        self.open_shared_device()
                    }
                }
            }
            (OutputTarget::Device, _, _) => self.open_shared_device(),
            (OutputTarget::Capture(capture), _, _) => Output::open_capture(&self.shared, capture),
        };
        match opened {
            Ok(output) => {
                self.last_device = Some(output.device.clone());
                self.emit(Event::OutputOpened {
                    device: output.device.clone(),
                    rate: output.rate,
                    channels: output.channels,
                    exclusive_bits: output.exclusive.map(|format| format.bits),
                });
                self.frames_pushed = self.shared.frames_played.load(Ordering::Acquire);
                self.output = Some(output);
                self.push_equalizer();
                if self.state != State::Playing {
                    self.idle_since = Some(Instant::now());
                }
                true
            }
            Err(message) => {
                self.emit(Event::OutputFailed(message));
                false
            }
        }
    }

    /// Сменилось устройство по умолчанию — переходим на него с того же места (только при игре).
    fn follow_default_device(&mut self) {
        if self.state != State::Playing || !matches!(self.target, OutputTarget::Device) {
            self.switch_watch.forget();
            return;
        }
        // Пауза при смене: проверяем чаще, сменилось — сразу пауза.
        if self.pause_on_switch && self.switch_checked.is_none_or(|checked| checked.elapsed() >= DEVICE_CHECK_PAUSING) {
            self.switch_checked = Some(Instant::now());
            let identity = self.audio().default_identity();
            if self.switch_watch.observe(identity) {
                self.set_state(State::Paused);
                self.emit(Event::PausedForDevice);
                self.device_checked = None;
            }
        }
        if self.device_checked.is_some_and(|checked| checked.elapsed() < DEVICE_CHECK_EVERY) {
            return;
        }
        self.device_checked = Some(Instant::now());
        let Some(current) = self.output.as_ref().map(|output| output.device.clone()) else {
            return;
        };
        if let Some(default) = self.audio().default_name()
            && default != current
        {
            // Звуковой сервер уже перенёс звук — только громкость и событие.
            if self.audio().follows_default()
                && let Some(output) = self.output.as_mut().filter(|output| output.exclusive.is_none())
            {
                output.device = default.clone();
                let (rate, channels) = (output.rate, output.channels);
                if let Some(volume) = self.device_volumes.get(&default).copied() {
                    self.shared.volume.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                }
                self.last_device = Some(default.clone());
                self.emit(Event::OutputOpened { device: default, rate, channels, exclusive_bits: None });
                return;
            }
            self.device_switch = true;
            self.exclusive_failed = false;
            self.exclusive_unsupported.clear();
            self.shared.failed.store(true, Ordering::Release);
        }
    }

    /// Ошибка устройства — открыть заново с последнего слышного места.
    fn check_output(&mut self) {
        if !self.shared.failed.swap(false, Ordering::AcqRel) {
            return;
        }
        let resume = self
            .current
            .as_ref()
            .map(|current| (current.item.id, self.position_seconds()))
            .or_else(|| self.segments.front().map(|segment| (segment.item.id, segment.base)))
            .or_else(|| self.decoding.as_ref().map(|decoding| (decoding.item.id, decoding.base)));

        self.output = None;
        self.shared.flush.store(false, Ordering::Release);
        self.clear_pipeline();
        self.frames_pushed = self.shared.frames_played.load(Ordering::Acquire);
        if !std::mem::take(&mut self.device_switch) {
            self.emit(Event::OutputLost);
            // Устройство пропало во время игры — с паузой при смене продолжаем на паузе.
            if self.pause_on_switch && self.state == State::Playing && matches!(self.target, OutputTarget::Device) {
                self.set_state(State::Paused);
                self.emit(Event::PausedForDevice);
            }
        }
        if self.state == State::Stopped {
            return;
        }
        // Устройство падает раз за разом: останавливаемся, а не крутимся в цикле.
        let failing = self
            .last_open
            .is_some_and(|opened| opened.elapsed() < Duration::from_secs(2));
        match resume {
            Some((id, position)) if !failing => {
                self.start(id, position, false);
            }
            _ => self.stop(),
        }
    }

    /// Усыпляет звуковой поток, если плеер давно стоит.
    fn manage_idle(&mut self) {
        if self.state == State::Playing {
            return;
        }
        if let Some(since) = self.idle_since
            && since.elapsed() >= IDLE_STREAM_TIMEOUT
        {
            if let Some(output) = self.output.as_mut() {
                output.set_running(false);
            }
            self.idle_since = None;
        }
    }

    fn set_state(&mut self, state: State) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.shared.paused.store(state == State::Paused, Ordering::Release);
        lock(&self.status).state = state;
        if state == State::Playing {
            self.idle_since = None;
            if let Some(output) = self.output.as_mut() {
                output.set_running(true);
            }
        } else {
            self.idle_since = Some(Instant::now());
        }
        self.emit(Event::StateChanged(state));
    }

    /// Что звучит сейчас, а если ещё ничего — что вот-вот зазвучит.
    fn playing_id(&self) -> Option<u64> {
        self.current
            .as_ref()
            .map(|current| current.item.id)
            .or_else(|| self.segments.front().map(|segment| segment.item.id))
            .or_else(|| self.decoding.as_ref().map(|decoding| decoding.item.id))
            .or_else(|| self.cursor.as_ref().map(|cursor| cursor.id))
            .or_else(|| self.cued.map(|(id, _)| id))
    }

    fn playing_index(&self) -> Option<usize> {
        let id = self.playing_id()?;
        lock(&self.queue).index_of(id)
    }

    fn position_seconds(&self) -> f64 {
        let Some(current) = self.current.as_ref() else {
            return 0.0;
        };
        playhead_seconds(
            current.base,
            current.start_abs,
            self.shared.frames_played.load(Ordering::Acquire),
            self.output.as_ref().map_or(0, |output| output.rate),
            current.speed,
        )
    }

    /// Коэффициенты эквалайзера под частоту устройства.
    fn push_equalizer(&self) {
        let Some(output) = self.output.as_ref() else {
            return;
        };
        *lock(&self.shared.eq) = EqCoefficients::new(&self.equalizer, output.rate);
        self.shared.eq_version.fetch_add(1, Ordering::AcqRel);
    }

    fn bump_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        if let Some(wake) = lock(&self.waker).as_ref() {
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SwitchWatch;

    /// Другой порт того же выхода — смена; первое наблюдение и смена на паузе — нет.
    #[test]
    fn switch_watch_sees_device_and_port_changes() {
        let mut watch = SwitchWatch::default();
        let built_in = |port: &str| Some(format!("alsa_output.pci.analog-stereo|{port}"));
        assert!(!watch.observe(built_in("analog-output-headphones")));
        assert!(!watch.observe(built_in("analog-output-headphones")));
        assert!(watch.observe(built_in("analog-output-speaker")));
        // Сервер не ответил — не повод.
        assert!(!watch.observe(None));
        assert!(watch.observe(Some("bluez_output.80_99.1|headset-output".into())));
        // Пауза: пока не играет, всё забывается.
        watch.forget();
        assert!(!watch.observe(built_in("analog-output-speaker")));
    }
}

