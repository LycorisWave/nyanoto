//! Плеер с окном. Окно — в главном потоке, движок и сканер — в своих, результаты
//! приходят через `invoke_from_event_loop`. Таймер полосы перемотки идёт только при игре.
//! Запускают его `main.rs` (компьютер) и `nyanoto-android` (телефон).

#[cfg(target_os = "android")]
mod android;
mod avatars;
mod chain;
mod chapters;
mod choose;
mod clock;
mod conversion;
mod covers;
mod daily;
mod equalizer;
mod gestures;
mod graph;
mod health;
mod keys;
mod lang;
mod link;
mod load;
mod lrc;
mod lyrics;
mod quiet;
mod rows;
mod signal;
mod searcher;
mod sleep;
mod smart;
mod stage;
mod theme;
mod system_media;
mod wavebar;
mod wave;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nyanoto_core::{Engine, Event, Loudness, NowPlaying, OutputTarget, PREVIOUS_RESTARTS_AFTER, Peaks, Repeat, Speed, State, TrackError};
use nyanoto_library::{
    Album, Artist, Authenticity, Check, CheckLevel, CheckRecord, Grouping, Library, PLAY_CHOSEN, PLAY_DAILY,
    PLAY_SLEEPY, Playlist, QualityOverride, ScanProgress,
    ScanReport, TagField, Track, default_database, quality, read_fields,
    write_fields,
};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use lang::{Lang, Lex, Noun};
use rows::{RowContext, artist_name, artist_row, file_name, track_row};

slint::include_modules!();

const PAGE_ARTISTS: i32 = 0;
const PAGE_ALBUMS: i32 = 1;
const PAGE_TRACKS: i32 = 2;
const PAGE_SEARCH: i32 = 3;
const PAGE_PLAYLISTS: i32 = 4;
const PAGE_PLAYLIST: i32 = 5;
const PAGE_STATS: i32 = 6;
const PAGE_SETTINGS: i32 = 7;
const PAGE_HEALTH: i32 = 8;
const PAGE_QUEUE: i32 = 9;
/// Папки с музыкой — своя страница (не в настройках).
const PAGE_FOLDERS: i32 = 10;

/// Очередь сохраняется не на каждое изменение, а когда правки затихли.
const QUEUE_SAVE_DELAY: Duration = Duration::from_secs(2);
/// Поиск треков начинается, когда набор затих на это время.
const SEARCH_DELAY: Duration = Duration::from_millis(100);

/// Обложка в полноэкранном плеере (сторона в пикселях) и размытый фон за ней.
const FULL_COVER: u32 = 1024;
/// Сколько записей журнала «что играло» показывать.
const HISTORY_SHOWN: usize = 500;

/// Шаг громкости стрелками и перемотки стрелками (с Shift — длинной).
const VOLUME_STEP: f32 = 0.05;
const SEEK_STEP: f32 = 5.0;
const SEEK_LONG_STEP: f32 = 30.0;

/// Списки треков: номер приходит из окна вместе с номером строки.
const LIST_ALBUM: usize = 0;
const LIST_SEARCH: usize = 1;
const LIST_PLAYLIST: usize = 2;
const LIST_STATS: usize = 3;

/// Размеры обложек в точках: в списке альбомов, в шапке альбома, в панели.
const LIST_COVER: u32 = 120;
const HEADER_COVER: u32 = 240;
/// Обложки и картинки в сетках (до 180 точек при двойной плотности).
const GRID_COVER: u32 = 384;
const GRID_PICTURE: f32 = 150.0;
const BAR_COVER: u32 = 128;
/// Сколько обложек держать в памяти (каждая 60–230 КБ).
const COVERS_IN_MEMORY: usize = 300;
/// И не больше этого по объёму пикселей: 300 обложек шапки альбома заняли бы ~70 МБ.
const COVER_MEMORY_BYTES: usize = 24 * 1024 * 1024;
/// Картинки крупнее в памяти не держатся: нужны по одной.
const REMEMBERED_SIDE: u32 = 256;

/// Прослушивание засчитывается после 30 секунд или половины короткого трека.
const COUNTED_AFTER: f32 = 30.0;

const SETTING_LANGUAGE: &str = "language";
const SETTING_CLASSICAL: &str = "classical";
const SETTING_LONG: &str = "long_records";
const SETTING_SMART: &str = "smart";
const SETTING_HEALTH: &str = "health";
const SETTING_WAVEBAR: &str = "waveform";
const SETTING_CROSSFADE: &str = "crossfade";
const SETTING_EXCLUSIVE: &str = "exclusive";
/// Пауза при смене устройства вывода — включена, пока её не выключили («0»).
const SETTING_DEVICE_PAUSE: &str = "device_pause";
/// Модули «в разработке»: включить нельзя, сохранённое включение не действует.
pub(crate) const UNFINISHED: bool = true;
/// Чем рисовать окно: "gpu" — видеокартой, иначе — процессором.
const SETTING_RENDERER: &str = "renderer";
/// Метка «видеокарта ещё не нарисовала первый кадр». Осталась — прошлый запуск упал.
const RENDERER_TRIAL: &str = "renderer-trial";
const SETTING_CROSSFADE_SECS: &str = "crossfade_secs";
/// Длительности кроссфейда в списке настроек, секунды.
const CROSSFADE_SECONDS: [u64; 5] = [2, 3, 5, 8, 12];
const SETTING_SHUFFLE: &str = "shuffle";

/// С какой длины трек считается длинной записью, мс.
const LONG_TRACK_MS: i64 = 20 * 60 * 1000;
/// Как часто запоминать место в длинной записи.
const POSITION_SAVE_EVERY: Duration = Duration::from_secs(10);
const SETTING_ADVANCED: &str = "advanced";
const SETTING_STATISTICS: &str = "statistics";
const SETTING_AUTHENTICITY: &str = "authenticity";
const SETTING_AUTHENTICITY_LEVEL: &str = "authenticity_level";
/// Кэш обложек, копии на диске и их качество. Включены, пока их не выключили.
const SETTING_COVER_CACHE: &str = "cover_cache";
const SETTING_COVER_DISK: &str = "cover_disk";
const SETTING_COVER_QUALITY: &str = "cover_quality";
/// Редкие форматы через FFmpeg; включено, пока не выключили.
const SETTING_FFMPEG: &str = "ffmpeg";
const SETTING_SYSTEM_MEDIA: &str = "system_media";
/// Жесты тачпада — включены, пока их не выключили («0»).
const SETTING_GESTURES: &str = "gestures";
/// Вид исполнителей и альбомов: «list» — списком, иначе сеткой.
const SETTING_VIEW_ARTISTS: &str = "view_artists";
const SETTING_VIEW_ALBUMS: &str = "view_albums";
/// Высота нижней панели: жест над ней — по треку.
const BAR_HEIGHT: f32 = 92.0;
/// Сколько точек снизу в полном плеере занимают название и кнопки.
const FULL_LYRICS_BOTTOM: f32 = 270.0;
/// Без явного конца жеста (X11, Windows) — конец, когда прокрутка стихла на столько.
const GESTURE_IDLE: Duration = Duration::from_millis(160);
/// Сколько мест помнит «Вперёд».
const FORWARD_LIMIT: usize = 20;
/// `sleep_ab` — старая общая настройка сна и A–B, читается, пока нет новых.
const SETTING_SLEEP_TIMER: &str = "sleep_timer";
const SETTING_AB_REPEAT: &str = "ab_repeat";
const SETTING_OLD_SLEEP_AB: &str = "sleep_ab";
const SETTING_REPLAYGAIN: &str = "replaygain";
const SETTING_REPLAYGAIN_MODE: &str = "replaygain_mode";
const SETTING_LYRICS: &str = "lyrics";
/// На сколько мс раньше подсвечивать строку текста.
const SETTING_LYRICS_LEAD: &str = "lyrics_lead_ms";
const LYRICS_LEAD_MS: u32 = 150;
/// Вид текста: обычные строки или сцены.
const SETTING_LYRICS_STYLE: &str = "lyrics_style";
const LYRICS_STYLES: [&str; 2] = ["column", "scenes"];
/// Какая сцена: два авторежима, затем стили по порядку `stage::Kind::ALL`.
const SETTING_STAGE_SCENE: &str = "stage_scene";
/// Сколько кадров в секунду у сцен: 60 или 30.
const SETTING_STAGE_FPS: &str = "stage_fps";
/// Сцены → обычные строки под нагрузкой и при энергосбережении.
const SETTING_STAGE_FALLBACK_LOAD: &str = "stage_fallback_load";
const SETTING_STAGE_FALLBACK_POWER: &str = "stage_fallback_power";
/// Как часто мерить нагрузку, пока видны сцены, и спрашивать систему об энергосбережении.
const LOAD_EVERY: Duration = Duration::from_secs(2);
const POWER_EVERY: Duration = Duration::from_secs(10);
/// Вид текста «сцены» в окне (0 — колонка).
const LOOK_STAGE: i32 = 1;
/// Нажатие на строку перематывает чуть раньше её начала.
const LYRIC_SEEK_LEAD: Duration = Duration::from_millis(1500);
/// Кадр сцены на паузе: время (биты f64), размер сцены, полноэкранный ли.
type PausedFrame = (u64, (f32, f32), bool);

/// Код сцены для настроек: «song», «moment» или код стиля.
fn stage_scene_code(index: usize) -> String {
    match index {
        0 => "song".into(),
        1 => "moment".into(),
        _ => stage::Kind::ALL.get(index - 2).map_or("song", |kind| kind.code()).into(),
    }
}

fn stage_scene_index(code: &str) -> usize {
    match code {
        "song" => 0,
        "moment" => 1,
        _ => stage::Kind::from_code(code).and_then(|kind| stage::Kind::ALL.iter().position(|known| *known == kind)).map_or(0, |index| index + 2),
    }
}

const SETTING_TAP_OFFSET: &str = "lrc_tap_offset";
const SETTING_SPEED: &str = "speed";
const SETTING_KEEP_PITCH: &str = "keep_pitch";
/// Громкость, 0…1.
const SETTING_VOLUME: &str = "volume";
/// Через сколько после последнего движения ползунка громкость пишется в базу.
const VOLUME_SETTLE: Duration = Duration::from_secs(1);

/// Через сколько после последнего движения ползунка скорость применяется.
const SPEED_SETTLE: Duration = Duration::from_millis(300);

/// Крупная волна редактора показывает столько миллисекунд до и после текущего места.
const WAVE_HALF_MS: i64 = 5000;
/// Волна рисуется полосой втрое шире видимого — меньше перерисовок.
const WAVE_STRIP_MS: i64 = 6 * WAVE_HALF_MS;

/// С какого запаса до строки редактор включает её прослушивание, мс.
const PREROLL_MS: i64 = 1000;

/// Пропуск короче секунды — трек и не прозвучал.
const SKIP_FROM: f32 = 1.0;
const SLEEP_DEFAULT_MINUTES: i64 = 30;
const SLEEP_FADE: Duration = Duration::from_secs(30);
/// Сколько строк текста показывать до и после текущей.
const SETTING_AUTO_NEXT: &str = "auto_next";
const SETTING_AUTO_NEXT_ORDER: &str = "auto_next_order";

/// Виды значка качества, как их рисует окно.
use rows::{BADGE_GREY, BADGE_RED, quality_tier};

/// Подписи вместо красного значка (порядок как в окне подлинности).
const QUALITY_LABELS: &[&str] = &["Hi-Res", "CD", "MP3", "AAC", "Lossy"];

/// О чём окно предупреждения (номер = `warning-kind`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Warning {
    Authenticity = 0,
    Covers = 1,
}

/// Сколько треков проверка подлинности берёт за один заход.
const CHECK_BATCH: usize = 64;

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Выполняет `work` с приложением в главном потоке.
fn with_app(work: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|cell| cell.borrow().clone());
    if let Some(app) = app {
        work(&app);
    }
}

/// Отправляет работу в главный поток из любого другого.
fn in_ui(work: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(work));
}

#[cfg(not(target_os = "android"))]
thread_local! {
    static DIALOG_OPEN: Cell<bool> = const { Cell::new(false) };
}

/// Системный диалог выбора, который не держит окно (иначе GNOME считает программу
/// зависшей). Второй, пока открыт первый, не открывается.
#[cfg(not(target_os = "android"))]
fn pick<F: Future<Output = Option<rfd::FileHandle>> + 'static>(
    dialog: impl FnOnce() -> F,
    then: impl FnOnce(&Rc<App>, PathBuf) + 'static,
) {
    if DIALOG_OPEN.with(|open| open.replace(true)) {
        return;
    }
    let chosen = dialog();
    let _ = slint::spawn_local(async move {
        let chosen = chosen.await;
        DIALOG_OPEN.with(|open| open.set(false));
        if let Some(chosen) = chosen {
            let path = chosen.path().to_path_buf();
            with_app(|app| then(app, path));
        }
    });
}

/// Куда вернёт «Вперёд»: место, откуда ушли «Назад».
#[derive(Clone)]
enum Place {
    Albums(Artist),
    Tracks(Album),
    Playlist(OpenList),
    Search(String),
    Queue,
    Page(i32),
}

/// Какой плейлист открыт.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenList {
    Liked,
    Playlist(i64),
    /// Плейлист дня.
    Daily,
}

/// Что сейчас в очереди: от этого зависит подпись кнопки повтора.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueKind {
    Album,
    Playlist,
    List,
}

/// Таймер сна.
#[derive(Clone, Copy, PartialEq)]
enum Sleep {
    Off,
    /// Остановиться в этот момент; число — выбранные минуты.
    Until(Instant, u32),
    /// Остановиться, когда доиграет текущий трек.
    AfterTrack,
}

/// Картинка в памяти: файл или папка, откуда она, сторона в точках и форма.
type PictureKey = (PathBuf, u32, covers::Shape);

/// Список исполнителей в окне: в «Музыке» или в найденном.
#[derive(Clone, Copy)]
enum ArtistList {
    Library,
    Found,
}

impl ArtistList {
    fn rows(self, app: &App) -> &Rc<VecModel<ArtistRow>> {
        match self {
            ArtistList::Library => &app.artist_rows,
            ArtistList::Found => &app.found_artist_rows,
        }
    }

    fn artists(self, app: &App) -> &RefCell<Vec<Artist>> {
        match self {
            ArtistList::Library => &app.artists,
            ArtistList::Found => &app.found_artists,
        }
    }
}

/// Сторона картинки ровно в пикселях экрана: отрисовка уменьшает без сглаживания.
fn picture_side(points: f32, scale: f32) -> u32 {
    ((points * scale).round() as u32).clamp(8, avatars::SIDE)
}

/// Что сделать, если пользователь согласится в окне подтверждения.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    /// Включить исправление кодировки тегов и перечитать их.
    FixEncoding,
    /// Убрать папку музыки с этим номером.
    RemoveFolder(i64),
}

/// Глава или закладка в списке длинной записи.
#[derive(Clone)]
struct ChapterItem {
    at_ms: i64,
    name: String,
    /// Номер закладки в базе; у главы его нет.
    bookmark: Option<i64>,
}

/// Трек, который сейчас слушают: из этого складывается статистика.
struct Listening {
    track: i64,
    seconds: f32,
    duration: f32,
    /// Отметки для плейлиста дня: выбран вручную, из него самого, под таймер сна.
    flags: u8,
}

/// Окно сведений о треке.
struct Info {
    track: Track,
    /// Теги, как они записаны в файле сейчас.
    original: Vec<TagField>,
    /// Теги с правками пользователя.
    edited: Vec<TagField>,
    /// Что было в файле до последней записи: для «Вернуть как было».
    undo: Option<Vec<TagField>>,
}

/// Редактор текста песни.
struct Editor {
    track: Track,
    /// Куда запишется `.lrc`.
    path: PathBuf,
    draft: lrc::Draft,
    /// Выбранная строка: её сдвигают стрелки и слушает Enter.
    selected: usize,
    /// Текст шага «Файл LRC», если его правили руками.
    raw: Option<String>,
    /// Файл, каким он был при открытии редактора; `None` — файла не было.
    original: Option<Vec<u8>>,
    /// Что лежит в файле сейчас.
    saved: Option<Vec<u8>>,
    dirty: bool,
    /// Строка, подсвеченная как звучащая.
    current: Option<usize>,
}

struct App {
    window: AppWindow,
    library: Option<Library>,
    database: Option<PathBuf>,
    engine: Engine,
    covers: covers::Loader,
    cover_cache: RefCell<HashMap<PictureKey, Image>>,
    /// Порядок, в котором обложки попали в память: первые вытесняются первыми.
    cover_order: RefCell<std::collections::VecDeque<PictureKey>>,
    /// Сколько байт пикселей в памяти у картинок кэша.
    cover_bytes: Cell<usize>,
    /// Аватары исполнителей, которые выбрал пользователь.
    avatars: RefCell<avatars::Avatars>,
    /// Кэш обложек: без него обложки не держатся и в памяти.
    cover_cache_on: Cell<bool>,
    cover_disk_on: Cell<bool>,
    cover_quality: Cell<covers::Quality>,

    lang: Cell<Lang>,
    grouping: Cell<Grouping>,
    advanced: Cell<bool>,
    statistics: Cell<bool>,
    authenticity: Cell<bool>,
    level: Cell<CheckLevel>,
    warning: Cell<Warning>,
    /// Карта прослушиваний: модель графа, её данные, узлы для окна и шаги анимации.
    graph: RefCell<Option<graph::Layout>>,
    graph_artists: RefCell<Vec<nyanoto_library::GraphArtist>>,
    graph_rows: Rc<VecModel<GraphNode>>,
    /// Номер построения карты: поздние картинки старой карты не попадут в новую.
    graph_view: Cell<u64>,
    graph_timer: Timer,
    /// Где взяли узел и на сколько его с тех пор протащили: отличить нажатие от перетаскивания.
    graph_press: Cell<(f32, f32)>,
    graph_travel: Cell<f32>,
    auto_next: Cell<bool>,
    auto_next_random: Cell<bool>,
    sleep_on: Cell<bool>,
    ab_on: Cell<bool>,
    /// Исходный порядок очереди (пути) — вернуть, когда перемешивание выключат.
    shuffle_original: RefCell<Option<Vec<PathBuf>>>,
    replaygain: Cell<bool>,
    replaygain_album: Cell<bool>,
    lyrics_on: Cell<bool>,
    classical: Cell<bool>,
    long_on: Cell<bool>,
    smart_on: Cell<bool>,
    health_on: Cell<bool>,
    wavebar_on: Cell<bool>,
    eq_on: Cell<bool>,
    exclusive_on: Cell<bool>,
    sync_on: Cell<bool>,
    convert_on: Cell<bool>,
    converting: Cell<bool>,
    stop_convert: RefCell<Arc<AtomicBool>>,
    /// Что конвертировать: альбом или плейлист, открытые при нажатии.
    convert_tracks: RefCell<Vec<Track>>,
    /// Связка уже была в этом запуске.
    synced_once: Cell<bool>,
    output_info: RefCell<Option<signal::OutputInfo>>,
    profiles_on: Cell<bool>,
    /// Настройки эквалайзера, как в окне.
    eq: RefCell<nyanoto_core::Equalizer>,
    eq_profiles: RefCell<std::collections::BTreeMap<String, nyanoto_core::Equalizer>>,
    eq_profile: RefCell<String>,
    eq_rows: Rc<VecModel<EqRow>>,
    eq_save_timer: Timer,
    /// Открытое устройство вывода и что для каждого устройства запомнено.
    device: RefCell<String>,
    device_map: RefCell<std::collections::BTreeMap<String, equalizer::DeviceProfile>>,
    wavebar_stop: RefCell<Arc<AtomicBool>>,
    health_checking: Cell<bool>,
    health_urgent: Cell<bool>,
    stop_health: Arc<AtomicBool>,
    stop_health_urgent: Arc<AtomicBool>,
    /// Треки строк страницы «Здоровье»: по ним клик открывает сведения.
    health_tracks: RefCell<Vec<Option<Track>>>,
    shuffle: Cell<smart::Shuffle>,
    /// Какой умный плейлист правится; `None` — создаётся новый.
    smart_editing: Cell<Option<i64>>,
    /// Условия в редакторе: вид и значение, как их ввели.
    smart_draft: RefCell<Vec<(i32, String)>>,
    smart_rows: Rc<VecModel<SmartRow>>,
    smart_timer: Timer,
    /// Главы и закладки играющего трека, по времени.
    chapters: RefCell<Vec<ChapterItem>>,
    /// Когда место длинной записи запоминалось последний раз.
    position_saved: Cell<Option<Instant>>,
    /// Действие, которого ждёт открытое окно подтверждения.
    confirm: Cell<Option<Confirm>>,
    sleep: Cell<Sleep>,
    /// Меню сна: цель, круг, угол пальца, таймер, затухание.
    sleep_target: Cell<i64>,
    sleep_raw: Cell<f32>,
    sleep_angle: Cell<f32>,
    sleep_timer: Timer,
    sleep_fading: Cell<bool>,
    /// Начало и конец отрезка A–B в секундах.
    ab: Cell<(Option<f32>, Option<f32>)>,
    lyrics: RefCell<Option<lyrics::Lyrics>>,
    /// Идёт немедленная проверка подлинности.
    urgent: Cell<bool>,
    stop_urgent: Arc<AtomicBool>,
    liked: RefCell<HashSet<i64>>,
    /// Треки, которые проверка сочла подозрительными.
    suspicious: RefCell<HashMap<i64, Check>>,
    /// Решения пользователя по подозрительным трекам.
    overrides: RefCell<HashMap<i64, QualityOverride>>,
    checking: Cell<bool>,
    stop_checking: Arc<AtomicBool>,
    /// Трек, открытый в окне подлинности.
    quality_track: RefCell<Option<Track>>,
    /// Альбомы, сыгранные подряд в режиме «следующий альбом исполнителя».
    chain: RefCell<HashSet<Option<String>>>,
    /// Откуда перешли в следующий альбом — «предыдущий» вернёт туда.
    album_trail: RefCell<Vec<PathBuf>>,
    random: Cell<u64>,

    artists: RefCell<Vec<Artist>>,
    artist: RefCell<Option<Artist>>,
    albums: RefCell<Vec<Album>>,
    album: RefCell<Option<Album>>,
    found_artists: RefCell<Vec<Artist>>,
    /// Треки четырёх списков: альбом, поиск, плейлист, статистика.
    lists: [RefCell<Vec<Track>>; 4],
    /// Пояснения к строкам статистики: сколько раз слушали.
    stat_counts: RefCell<Vec<u32>>,
    playlists: RefCell<Vec<Playlist>>,
    open_list: Cell<OpenList>,
    /// Места треков в открытом плейлисте: по ним треки убираются.
    positions: RefCell<Vec<i64>>,
    /// Треки, которые ждут выбора плейлиста.
    picking: RefCell<Vec<Track>>,
    /// Очередь в окне: строки, `id` движка, откуда открыли, отложенное сохранение.
    queue_rows: Rc<VecModel<QueueRow>>,
    queue_ids: RefCell<Vec<u64>>,
    queue_before: Cell<i32>,
    queue_timer: Timer,
    /// Файл подготовленного трека, показанного в панели (очередь после перезапуска).
    shown_cue: RefCell<Option<PathBuf>>,
    /// Очередь менялась с последнего сохранения.
    queue_dirty: Cell<bool>,
    /// Текст песни: строки, текущая, сдвиг подсветки.
    lyrics_rows: RefCell<Option<Rc<VecModel<LyricRow>>>>,
    lyrics_current: Cell<Option<usize>>,
    lyrics_lead: Cell<u32>,
    /// Вид текста из настроек (номер в [`LYRICS_STYLES`]).
    lyrics_style: Cell<usize>,
    /// Сцены текста: кадры рисуются по своему таймеру, пока сцену видно.
    stage: RefCell<stage::Stage>,
    stage_timer: Timer,
    /// Возврат на обычные строки: настройки, замер нагрузки, энергосбережение.
    stage_fallback_load: Cell<bool>,
    stage_fallback_power: Cell<bool>,
    load_watch: RefCell<load::Watch>,
    load_timer: Timer,
    power_saving: Arc<AtomicBool>,
    power_wanted: Arc<AtomicBool>,
    stage_fallback: Cell<Option<load::Fallback>>,
    /// Размытый фон полного плеера, готовый заранее: папка, размер, картинка.
    backdrop: RefCell<Option<(PathBuf, u32, u32, Image)>>,
    /// Полный плеер открывали в этом сеансе — фон готовится при смене трека.
    full_used: Cell<bool>,
    /// Крупная обложка для полного плеера, готовая заранее: папка, сторона.
    full_ready: RefCell<Option<(PathBuf, u32, Image)>>,
    /// Жесты: включены ли, текущий, где указатель.
    gestures_on: Cell<bool>,
    /// Сетка исполнителей: чьи картинки уже запрошены.
    grid_pictures: RefCell<HashSet<String>>,
    gesture: RefCell<gestures::Tracker>,
    gesture_edges: Cell<bool>,
    pointer: Cell<(f32, f32)>,
    /// Пальцы на сенсорном экране.
    touches: RefCell<gestures::Touches>,
    gesture_idle: Timer,
    /// «Вперёд»: места, откуда ушли «Назад»; любой другой переход их забывает.
    forward_places: RefCell<Vec<Place>>,
    history_moving: Cell<bool>,
    stage_clock: RefCell<stage::Clock>,
    /// Размер сцены в окне текста и в полноэкранном плеере.
    stage_sizes: Cell<[(f32, f32); 2]>,
    /// Случайность показа: у колонки каждый раз по-новому.
    stage_salt: Cell<u64>,
    /// Сцена из настроек и кадров в секунду.
    stage_scene: Cell<usize>,
    stage_fps: Cell<u32>,
    /// Настроение песни по звуку: для какого файла, и идёт ли разбор.
    stage_mood: RefCell<Option<(PathBuf, stage::feel::Mood)>>,
    stage_mood_stop: RefCell<Arc<AtomicBool>>,
    /// С чем нарисован кадр на паузе: то же самое не перерисовывается.
    stage_paused: Cell<Option<PausedFrame>>,
    /// Для журнала `NYANOTO_TRACE`: сколько кадров нарисовано и за сколько.
    stage_cost: Cell<(u32, Duration)>,
    /// Громкость до «выключить звук» клавишей M.
    muted: Cell<Option<f32>>,
    /// Журнал «что играло»: треки строк и сами строки.
    history_tracks: RefCell<Vec<Track>>,
    history_rows: Rc<VecModel<HistoryRow>>,
    /// Поиск в своём потоке, номер запроса, задержка после буквы.
    searcher: searcher::Searcher,
    search_id: Cell<u64>,
    search_timer: Timer,
    /// Номера папок музыки в том порядке, в каком они в настройках.
    root_ids: RefCell<Vec<i64>>,
    /// Папки, выбранные во время сканирования, и просьба просканировать ещё раз.
    pending_roots: RefCell<Vec<(PathBuf, Grouping)>>,
    /// Выбранная папка музыки ждёт ответа «по папкам или по тегам».
    choosing_root: RefCell<Option<PathBuf>>,
    rescan_pending: Cell<bool>,
    /// Медиаклавиши и виджет плеера системы, если включены.
    media: RefCell<Option<system_media::SystemMedia>>,
    /// Маленькая копия обложки для системы: папка альбома и файл (пока делается — `None`).
    system_art: RefCell<Option<(PathBuf, Option<PathBuf>)>>,
    system_art_dir: Option<PathBuf>,

    artist_rows: Rc<VecModel<ArtistRow>>,
    album_rows: Rc<VecModel<AlbumRow>>,
    track_rows: [Rc<VecModel<TrackRow>>; 4],
    found_artist_rows: Rc<VecModel<ArtistRow>>,
    playlist_rows: Rc<VecModel<ArtistRow>>,
    info_rows: Rc<VecModel<InfoRow>>,
    tag_rows: Rc<VecModel<TagRow>>,

    browse_page: Cell<i32>,
    /// Номер списка альбомов: поздние обложки старого списка не лезут в новый.
    view: Cell<u64>,
    /// Что стоит в очереди движка: по пути находим трек.
    queue: RefCell<HashMap<PathBuf, Track>>,
    queue_kind: Cell<QueueKind>,
    current: RefCell<Option<Track>>,
    repeat: Cell<Repeat>,
    listening: RefCell<Option<Listening>>,
    /// Трек, выбранный руками: его прослушивание весит больше.
    chosen: RefCell<Option<PathBuf>>,
    /// Пользователь сам переключил трек: недослушанный — пропуск.
    leaving: Cell<bool>,
    daily: daily::State,
    /// Оформление: тема, сезон, масштаб.
    look: theme::State,
    info: RefCell<Option<Info>>,
    editor: RefCell<Option<Editor>>,
    editor_rows: Rc<VecModel<EditorLine>>,
    speed_module: Cell<bool>,
    keep_pitch: Cell<bool>,
    /// Скорость с ползунка панели; при запуске всегда 1.
    normal_speed: Cell<f64>,
    /// Скорость в редакторе текстов.
    editor_speed: Cell<f64>,
    speed_timer: Timer,
    volume_timer: Timer,
    /// Огибающая громкости трека из редактора.
    wave: RefCell<Option<(PathBuf, Arc<Peaks>)>>,
    wave_stop: RefCell<Arc<AtomicBool>>,
    /// Пока тянут метку: волна стоит, чтобы метка не уезжала из-под руки.
    drag_window: Cell<Option<i64>>,
    /// Начало нарисованной полосы крупной волны, мс.
    wave_strip: Cell<Option<i64>>,
    wave_marks: Rc<VecModel<WaveMark>>,
    overview_marks: Rc<VecModel<WaveMark>>,
    ticker: Timer,
    /// Срабатывает ровно к началу следующей строки текста.
    line_timer: Timer,
    scanning: Cell<bool>,
    /// Счётчики идущего сканирования и таймер их показа.
    scan_progress: RefCell<Option<Arc<ScanProgress>>>,
    scan_timer: Timer,
}

/// Скорость с шагом 0,05 в пределах ползунка.
fn round_speed(value: f64, min: f64, max: f64) -> f64 {
    ((value / 0.05).round() * 0.05).clamp(min, max)
}

fn models<T: Clone + 'static>() -> Rc<VecModel<T>> {
    Rc::new(VecModel::default())
}

/// `NYANOTO_TRACE=путь` — журнал шагов с временем. Без переменной почти бесплатен.
pub(crate) fn trace(step: &str) {
    use std::io::Write;
    static PATH: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
    if let Some(path) = PATH.get_or_init(|| std::env::var_os("NYANOTO_TRACE"))
        && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let _ = writeln!(file, "{} {step}", now.as_millis());
    }
}

/// Большие блоки — сразу обратно системе: иначе glibc держит десятки МБ свободной памяти
/// потоков обложек.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn return_big_blocks_to_system() {
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    const M_MMAP_THRESHOLD: i32 = -3;
    // Меняет только настройку распределителя памяти, данные не трогает.
    unsafe {
        mallopt(M_MMAP_THRESHOLD, 128 * 1024);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn return_big_blocks_to_system() {}

/// Значок, вшитый в программу.
fn app_icon() -> Image {
    let Ok(picture) = image::load_from_memory(include_bytes!("../assets/icon-256.png")) else {
        return Image::default();
    };
    let picture = picture.to_rgba8();
    let (width, height) = picture.dimensions();
    Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(picture.as_raw(), width, height))
}

/// На телефоне окно и отрисовку выбирает `slint::android::init`.
#[cfg(target_os = "android")]
fn select_renderer(_database: Option<&Path>) -> (bool, bool) {
    (false, false)
}

/// Чем рисовать: процессором или видеокартой (настройка, `NYANOTO_RENDERER`, `SLINT_BACKEND`).
/// Если видеокарта в прошлый раз не нарисовала окно — процессором.
#[cfg(not(target_os = "android"))]
fn select_renderer(database: Option<&Path>) -> (bool, bool) {
    let trial = database.and_then(Path::parent).map(|dir| dir.join(RENDERER_TRIAL));
    let wanted = match std::env::var("NYANOTO_RENDERER").as_deref() {
        Ok("gpu") => true,
        Ok("software") => false,
        _ => database.and_then(|database| nyanoto_library::peek_setting(database, SETTING_RENDERER)).as_deref() == Some("gpu"),
    };
    let failed = wanted && trial.as_ref().is_some_and(|path| path.exists());
    let gpu = wanted && !failed;
    if let Some(path) = &trial {
        let _ = if gpu { std::fs::write(path, b"") } else { std::fs::remove_file(path) };
    }
    let mut selector = slint::BackendSelector::new().backend_name("winit".into());
    if std::env::var_os("SLINT_BACKEND").is_none() {
        selector = selector.renderer_name(if gpu { "femtovg" } else { "software" }.into());
    }
    if let Err(error) = selector.select() {
        eprintln!("отрисовка: {error}");
    }
    (gpu, failed)
}

/// Открывает окно и крутит его до закрытия.
pub fn run() -> Result<(), slint::PlatformError> {
    return_big_blocks_to_system();
    trace("main");
    // Имя окна для панели задач и меню в Linux (Wayland app_id, X11 WM_CLASS).
    let _ = slint::set_xdg_app_id("nyanoto");
    let database = default_database();
    let (gpu, gpu_failed) = select_renderer(database.as_deref());
    let window = AppWindow::new()?;
    window.set_app_icon(app_icon());
    // Проверки окна: настройки сразу прокручены на столько точек вниз.
    #[cfg(debug_assertions)]
    if let Some(scroll) = std::env::var("NYANOTO_SETTINGS_SCROLL").ok().and_then(|value| value.parse::<f32>().ok()) {
        window.set_settings_scroll(-scroll);
    }
    // Проверки окна: замедленные анимации — и сворачивание плеера ждёт дольше.
    #[cfg(debug_assertions)]
    if let Some(slow) = std::env::var("SLINT_SLOW_ANIMATIONS").ok().and_then(|value| value.parse::<i64>().ok()) {
        window.set_full_close_time(420 * slow.max(1));
    }
    trace("window created");
    let (library, problem) = match database.as_deref().map(Library::open) {
        Some(Ok(library)) => (Some(library), None),
        Some(Err(error)) => (None, Some(error.to_string())),
        None => (None, Some("APPDATA / HOME ?".to_string())),
    };
    trace("library opened");
    let app_data_dir = database.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    let cover_copies = database.as_deref().and_then(Path::parent).map(|dir| dir.join("covers"));
    let avatar_folder = database.as_deref().and_then(Path::parent).map(|dir| dir.join("avatars"));
    let search_database = database.clone();
    let system_art_dir = database.as_deref().and_then(Path::parent).map(|dir| dir.join("mpris"));

    let app = Rc::new(App {
        window,
        library,
        database,
        engine: Engine::new(OutputTarget::Device),
        covers: covers::Loader::new(cover_copies),
        cover_cache: RefCell::default(),
        cover_order: RefCell::default(),
        cover_bytes: Cell::new(0),
        avatars: RefCell::new(avatars::Avatars::open(avatar_folder)),
        cover_cache_on: Cell::new(true),
        cover_disk_on: Cell::new(true),
        cover_quality: Cell::new(covers::Quality::Medium),
        lang: Cell::new(Lang::default()),
        grouping: Cell::new(Grouping::PerRoot),
        advanced: Cell::new(false),
        statistics: Cell::new(false),
        authenticity: Cell::new(false),
        level: Cell::new(CheckLevel::Low),
        warning: Cell::new(Warning::Authenticity),
        graph: RefCell::default(),
        graph_artists: RefCell::default(),
        graph_rows: Rc::new(VecModel::default()),
        graph_view: Cell::new(0),
        graph_timer: Timer::default(),
        graph_press: Cell::new((0.0, 0.0)),
        graph_travel: Cell::new(0.0),
        auto_next: Cell::new(true),
        auto_next_random: Cell::new(false),
        sleep_on: Cell::new(false),
        ab_on: Cell::new(false),
        shuffle_original: RefCell::default(),
        replaygain: Cell::new(false),
        replaygain_album: Cell::new(false),
        lyrics_on: Cell::new(false),
        classical: Cell::new(false),
        long_on: Cell::new(false),
        smart_on: Cell::new(false),
        health_on: Cell::new(false),
        wavebar_on: Cell::new(false),
        eq_on: Cell::new(false),
        exclusive_on: Cell::new(false),
        sync_on: Cell::new(false),
        convert_on: Cell::new(false),
        converting: Cell::new(false),
        stop_convert: RefCell::default(),
        convert_tracks: RefCell::default(),
        synced_once: Cell::new(false),
        output_info: RefCell::default(),
        profiles_on: Cell::new(false),
        eq: RefCell::default(),
        eq_profiles: RefCell::default(),
        eq_profile: RefCell::default(),
        eq_rows: models(),
        eq_save_timer: Timer::default(),
        device: RefCell::default(),
        device_map: RefCell::default(),
        wavebar_stop: RefCell::default(),
        health_checking: Cell::new(false),
        health_urgent: Cell::new(false),
        stop_health: Arc::default(),
        stop_health_urgent: Arc::default(),
        health_tracks: RefCell::default(),
        shuffle: Cell::new(smart::Shuffle::Off),
        smart_editing: Cell::new(None),
        smart_draft: RefCell::default(),
        smart_rows: models(),
        smart_timer: Timer::default(),
        chapters: RefCell::default(),
        position_saved: Cell::new(None),
        confirm: Cell::new(None),
        sleep: Cell::new(Sleep::Off),
        sleep_target: Cell::new(0),
        sleep_raw: Cell::new(0.0),
        sleep_angle: Cell::new(0.0),
        sleep_timer: Timer::default(),
        sleep_fading: Cell::new(false),
        ab: Cell::new((None, None)),
        lyrics: RefCell::default(),
        urgent: Cell::new(false),
        stop_urgent: Arc::default(),
        liked: RefCell::default(),
        suspicious: RefCell::default(),
        overrides: RefCell::default(),
        checking: Cell::new(false),
        stop_checking: Arc::default(),
        quality_track: RefCell::default(),
        chain: RefCell::default(),
        album_trail: RefCell::default(),
        random: Cell::new(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0x9E37_79B9, |since| since.as_nanos() as u64)
                | 1,
        ),
        artists: RefCell::default(),
        artist: RefCell::default(),
        albums: RefCell::default(),
        album: RefCell::default(),
        found_artists: RefCell::default(),
        lists: Default::default(),
        stat_counts: RefCell::default(),
        playlists: RefCell::default(),
        open_list: Cell::new(OpenList::Liked),
        positions: RefCell::default(),
        picking: RefCell::default(),
        queue_rows: Rc::new(VecModel::default()),
        queue_ids: RefCell::default(),
        queue_before: Cell::new(PAGE_ARTISTS),
        queue_timer: Timer::default(),
        shown_cue: RefCell::default(),
        muted: Cell::new(None),
        lyrics_rows: RefCell::default(),
        lyrics_current: Cell::new(None),
        lyrics_lead: Cell::new(LYRICS_LEAD_MS),
        lyrics_style: Cell::new(0),
        stage: RefCell::default(),
        stage_timer: Timer::default(),
        stage_fallback_load: Cell::new(true),
        stage_fallback_power: Cell::new(true),
        load_watch: RefCell::default(),
        load_timer: Timer::default(),
        power_saving: Arc::default(),
        power_wanted: Arc::default(),
        stage_fallback: Cell::new(None),
        backdrop: RefCell::default(),
        full_used: Cell::new(false),
        full_ready: RefCell::default(),
        gestures_on: Cell::new(true),
        grid_pictures: RefCell::default(),
        gesture: RefCell::default(),
        gesture_edges: Cell::new(false),
        pointer: Cell::new((0.0, 0.0)),
        touches: RefCell::default(),
        gesture_idle: Timer::default(),
        forward_places: RefCell::default(),
        history_moving: Cell::new(false),
        stage_clock: RefCell::default(),
        stage_sizes: Cell::new([(0.0, 0.0); 2]),
        stage_salt: Cell::new(0),
        stage_scene: Cell::new(0),
        stage_fps: Cell::new(60),
        stage_mood: RefCell::default(),
        stage_mood_stop: RefCell::default(),
        stage_paused: Cell::new(None),
        stage_cost: Cell::new((0, Duration::ZERO)),
        queue_dirty: Cell::new(false),
        history_tracks: RefCell::default(),
        history_rows: Rc::new(VecModel::default()),
        media: RefCell::default(),
        system_art: RefCell::default(),
        system_art_dir,
        searcher: searcher::Searcher::new(search_database, |found| in_ui(move |app| app.search_found(found))),
        search_id: Cell::new(0),
        search_timer: Timer::default(),
        root_ids: RefCell::default(),
        pending_roots: RefCell::default(),
        choosing_root: RefCell::default(),
        rescan_pending: Cell::new(false),
        artist_rows: models(),
        album_rows: models(),
        track_rows: [models(), models(), models(), models()],
        found_artist_rows: models(),
        playlist_rows: models(),
        info_rows: models(),
        tag_rows: models(),
        browse_page: Cell::new(PAGE_ARTISTS),
        view: Cell::new(0),
        queue: RefCell::default(),
        queue_kind: Cell::new(QueueKind::Album),
        current: RefCell::default(),
        repeat: Cell::new(Repeat::Off),
        listening: RefCell::default(),
        chosen: RefCell::default(),
        leaving: Cell::new(false),
        daily: daily::State::default(),
        look: theme::State::default(),
        info: RefCell::default(),
        editor: RefCell::default(),
        editor_rows: models(),
        speed_module: Cell::new(false),
        keep_pitch: Cell::new(true),
        normal_speed: Cell::new(1.0),
        editor_speed: Cell::new(1.0),
        speed_timer: Timer::default(),
        volume_timer: Timer::default(),
        wave: RefCell::default(),
        wave_stop: RefCell::default(),
        drag_window: Cell::new(None),
        wave_strip: Cell::new(None),
        wave_marks: models(),
        overview_marks: models(),
        ticker: Timer::default(),
        line_timer: Timer::default(),
        scanning: Cell::new(false),
        scan_progress: RefCell::default(),
        scan_timer: Timer::default(),
    });
    APP.with(|cell| *cell.borrow_mut() = Some(app.clone()));

    trace("app built");
    app.connect();
    keys::latin_shortcuts(
        app.window.window(),
        |key| with_app(|app| app.media_key(key)),
        |pointer| {
            let mut swallow = false;
            with_app(|app| swallow = app.pointer_event(pointer));
            swallow
        },
    );
    trace("connected");
    app.load_settings();
    if gpu_failed {
        app.renderer_failed();
    }
    trace("settings loaded");
    app.engine.set_waker(|| in_ui(|app| app.drain_events()));
    app.set_volume(app.window.get_volume());
    app.restore_queue();
    trace("queue restored");
    // Телефон: без доступа ко всем файлам музыку не прочитать — спросим в окне.
    #[cfg(target_os = "android")]
    {
        app.window.set_storage_denied(true);
        app.storage_changed(android::has_storage());
    }
    if let Some(problem) = problem {
        app.report(problem);
    }

    // Папка из командной строки добавляется в библиотеку.
    if let (Some(folder), Some(library)) = (std::env::args_os().nth(1), app.library.as_ref())
        && let Err(error) = library.add_root(Path::new(&folder))
    {
        app.report(error);
    }
    app.refresh_artists();
    trace("artists shown");
    app.start_scan();
    trace("scan started");
    // Масштаб и сезон — когда окно уже есть.
    Timer::single_shot(Duration::ZERO, || with_app(|app| app.look_start()));
    // Плейлист дня — когда окно уже открылось: запуск не ждёт подсчётов.
    Timer::single_shot(Duration::from_secs(2), || with_app(|app| app.daily_check()));
    // Проверки окна (только отладочная сборка): размер окна в точках, «ШxВ».
    #[cfg(debug_assertions)]
    if let Some((width, height)) = std::env::var("NYANOTO_WINDOW").ok().and_then(|size| {
        let (width, height) = size.split_once('x')?;
        Some((width.parse::<f32>().ok()?, height.parse::<f32>().ok()?))
    }) {
        app.window.window().set_size(slint::LogicalSize::new(width, height));
    }
    // Отладка: `NYANOTO_RESIZE="сек:ШxВ;…"` — размер окна на ходу.
    #[cfg(debug_assertions)]
    if let Ok(spec) = std::env::var("NYANOTO_RESIZE") {
        for part in spec.split(';') {
            if let Some((delay, size)) = part.split_once(':')
                && let Ok(delay) = delay.parse::<f32>()
                && let Some((width, height)) = size.split_once('x')
                && let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>())
            {
                Timer::single_shot(Duration::from_secs_f32(delay), move || {
                    with_app(|app| app.window.window().set_size(slint::LogicalSize::new(width, height)));
                });
            }
        }
    }
    // Отладка: `NYANOTO_ZOOM="сек:шаг;…"` — масштаб на ходу.
    #[cfg(debug_assertions)]
    if let Ok(spec) = std::env::var("NYANOTO_ZOOM") {
        for part in spec.split(';') {
            if let Some((delay, step)) = part.split_once(':')
                && let (Ok(delay), Ok(step)) = (delay.parse::<f32>(), step.parse::<i32>())
            {
                Timer::single_shot(Duration::from_secs_f32(delay), move || with_app(|app| app.zoom(step)));
            }
        }
    }
    // Отладка: `NYANOTO_SWIPE="сек:x,y,dx,dy,шагов,hold|end;…"` — жесты понарошку.
    #[cfg(debug_assertions)]
    if let Ok(spec) = std::env::var("NYANOTO_SWIPE") {
        for part in spec.split(';') {
            let Some((delay, rest)) = part.split_once(':') else {
                continue;
            };
            let values: Vec<f32> = rest.split(',').take(5).filter_map(|value| value.parse().ok()).collect();
            let (Ok(delay), &[x, y, dx, dy, steps]) = (delay.parse::<f32>(), values.as_slice()) else {
                continue;
            };
            let hold = rest.ends_with("hold");
            Timer::single_shot(Duration::from_secs_f32(delay), move || {
                with_app(|app| {
                    app.pointer_event(keys::Pointer::Moved(x, y));
                    for step in 0..steps as usize {
                        let edge = (step == 0).then_some(true);
                        app.pointer_event(keys::Pointer::Scroll { dx, dy, edge, precise: true });
                    }
                    if hold {
                        app.gesture_idle.stop();
                    } else {
                        app.pointer_event(keys::Pointer::Scroll { dx: 0.0, dy: 0.0, edge: Some(false), precise: true });
                    }
                });
            });
        }
    }

    // Панель Windows привязывается к окну, а окно появляется с началом цикла событий.
    Timer::single_shot(Duration::ZERO, || with_app(|app| app.apply_system_media()));

    let window = app.window.clone_strong();
    let first = Cell::new(false);
    // Видеокарта дорисовала первый кадр — метку предохранителя снимаем.
    let trial = app_data_dir.filter(|_| gpu).map(|dir| dir.join(RENDERER_TRIAL));
    let _ = window.window().set_rendering_notifier(move |state, _| {
        if matches!(state, slint::RenderingState::AfterRendering) && !first.replace(true) {
            trace("first frame");
            if let Some(path) = &trial {
                let _ = std::fs::remove_file(path);
            }
        }
    });
    let result = window.run();
    app.daily.stop();
    app.media.borrow_mut().take();
    app.save_queue_now();
    app.remember_position();
    app.save_volume();
    app.save_device_map();
    app.export_sync();
    result
}

impl App {
    fn lex(&self) -> &'static Lex {
        self.lang.get().lex()
    }

    /// Показывает ошибку в строке состояния.
    fn report(&self, error: impl Display) {
        self.window
            .set_status(format!("{}: {error}", self.lex().error).into());
    }

    fn connect(self: &Rc<Self>) {
        let window = &self.window;
        window.set_artists(ModelRc::from(self.artist_rows.clone()));
        window.set_albums(ModelRc::from(self.album_rows.clone()));
        window.set_tracks(ModelRc::from(self.track_rows[LIST_ALBUM].clone()));
        window.set_found_tracks(ModelRc::from(self.track_rows[LIST_SEARCH].clone()));
        window.set_playlist_tracks(ModelRc::from(self.track_rows[LIST_PLAYLIST].clone()));
        window.set_top_tracks(ModelRc::from(self.track_rows[LIST_STATS].clone()));
        window.set_found_artists(ModelRc::from(self.found_artist_rows.clone()));
        window.set_queue_rows(ModelRc::from(self.queue_rows.clone()));
        window.set_history_rows(ModelRc::from(self.history_rows.clone()));
        window.set_playlists(ModelRc::from(self.playlist_rows.clone()));
        window.set_info_rows(ModelRc::from(self.info_rows.clone()));
        window.set_info_tags(ModelRc::from(self.tag_rows.clone()));
        window.set_editor_lines(ModelRc::from(self.editor_rows.clone()));
        window.set_smart_conditions(ModelRc::from(self.smart_rows.clone()));
        window.set_eq_rows(ModelRc::from(self.eq_rows.clone()));
        window.set_editor_wave_marks(ModelRc::from(self.wave_marks.clone()));
        window.set_editor_overview_marks(ModelRc::from(self.overview_marks.clone()));

        // Колбэки окна держат слабую ссылку: окно принадлежит приложению.
        macro_rules! on {
            ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                window.$setter(move |$($arg),*| {
                    if let Some($app) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_search, |app, text| app.search(&text));
        on!(on_back, |app| app.back());
        on!(on_go, |app, page| app.go(page));
        on!(on_open_artist, |app, index| {
            let artist = app.artists.borrow().get(index as usize).cloned();
            if let Some(artist) = artist {
                app.open_artist(artist);
            }
        });
        on!(on_open_found_artist, |app, index| {
            let artist = app.found_artists.borrow().get(index as usize).cloned();
            if let Some(artist) = artist {
                app.window.set_query(SharedString::new());
                app.open_artist(artist);
            }
        });
        on!(on_open_album, |app, index| app.open_album(index as usize));
        on!(on_play_album, |app| app.play_list(LIST_ALBUM, 0, QueueKind::Album));
        on!(on_track_clicked, |app, list, index| app.track_clicked(list as usize, index as usize));
        on!(on_track_like, |app, list, index| {
            if let Some(track) = app.track_at(list as usize, index as usize) {
                app.toggle_like(track.id);
            }
        });
        on!(on_track_add, |app, list, index| {
            if let Some(track) = app.track_at(list as usize, index as usize) {
                app.open_picker(vec![track], true);
            }
        });
        on!(on_track_info, |app, list, index| {
            if let Some(track) = app.track_at(list as usize, index as usize) {
                app.open_info(track);
            }
        });
        on!(on_track_remove, |app, list, index| app.remove_from_list(list as usize, index as usize));
        on!(on_open_playlist, |app, index| app.open_playlist(index as usize));
        on!(on_create_playlist, |app, name| {
            app.create_playlist(&name);
        });
        on!(on_delete_playlist, |app| app.delete_playlist());
        on!(on_play_playlist, |app| app.play_list(LIST_PLAYLIST, 0, QueueKind::Playlist));
        on!(on_toggle, |app| app.engine.toggle());
        let weak = Rc::downgrade(self);
        window.on_hotkey(move |text, control, shift| weak.upgrade().is_some_and(|app| app.hotkey(&text, control, shift)));
        on!(on_queue_open, |app| app.toggle_queue_page());
        on!(on_queue_tab_changed, |app, _tab| app.render_queue_page());
        on!(on_history_play, |app, index| app.history_play(index as usize));
        on!(on_history_clear, |app| {
            if let Some(library) = app.library.as_ref()
                && let Err(error) = library.clear_history()
            {
                app.report(error);
            }
            app.render_history();
        });
        on!(on_open_now_artist, |app| app.open_now(false));
        on!(on_open_now_album, |app| app.open_now(true));
        on!(on_full_toggle, |app| app.toggle_full());
        on!(on_full_warmup, |app| {
            app.prepare_backdrop();
            app.prepare_full_cover();
        });
        on!(on_toggle_fullscreen, |app| app.toggle_os_fullscreen());
        on!(on_add_current, |app| {
            let current = app.current.borrow().clone();
            if let Some(track) = current {
                app.open_picker(vec![track], true);
            }
        });
        on!(on_queue_jump, |app, index| app.queue_jump(index as usize));
        on!(on_queue_remove, |app, index| app.queue_remove(index as usize));
        on!(on_queue_move, |app, index, step| app.queue_move(index as usize, step));
        on!(on_queue_clear, |app| app.queue_clear());
        on!(on_queue_save, |app| app.queue_save());
        on!(on_picker_queue, |app, next| {
            let tracks = std::mem::take(&mut *app.picking.borrow_mut());
            app.window.set_picker_open(false);
            app.enqueue(tracks, next);
        });
        on!(on_queue_album, |app| {
            let tracks = app.lists[LIST_ALBUM].borrow().clone();
            app.enqueue(tracks, false);
        });
        on!(on_queue_playlist, |app| {
            let tracks = app.lists[LIST_PLAYLIST].borrow().clone();
            app.enqueue(tracks, false);
        });
        on!(on_next, |app| app.go_next());
        on!(on_previous, |app| app.go_previous());
        on!(on_seek, |app, seconds| {
            app.engine.seek(Duration::from_secs_f32(seconds.max(0.0)));
            app.media_seeked(seconds);
        });
        on!(on_set_volume, |app, volume| {
            // Громкость выбрали руками — «вернуть звук» клавишей M больше не к чему.
            app.muted.set(None);
            app.set_volume(volume);
            app.update_system_media();
            app.volume_timer.start(TimerMode::SingleShot, VOLUME_SETTLE, || with_app(|app| app.save_volume()));
        });
        on!(on_cycle_repeat, |app| app.cycle_repeat());
        on!(on_like_current, |app| {
            let current = app.current.borrow().as_ref().map(|track| track.id);
            if let Some(id) = current {
                app.toggle_like(id);
            }
        });
        on!(on_info_current, |app| {
            let current = app.current.borrow().clone();
            if let Some(track) = current {
                app.open_info(track);
            }
        });
        on!(on_add_folder, |app| app.add_folder());
        on!(on_browse_enter, |app, index| app.browse_enter(index.max(0) as usize));
        on!(on_browse_up, |app| app.browse_up());
        on!(on_browse_pick, |app| app.browse_pick());
        on!(on_browse_close, |app| app.browse_close());
        on!(on_storage_ask, |_app| {
            #[cfg(target_os = "android")]
            crate::android::ask_storage();
        });
        on!(on_leave, |_app| {
            #[cfg(target_os = "android")]
            crate::android::leave();
        });
        on!(on_back_possible, |_app, on| {
            #[cfg(target_os = "android")]
            crate::android::set_back(on);
            #[cfg(not(target_os = "android"))]
            let _ = on;
        });
        on!(on_folder_choice, |app, choice| app.folder_chosen(choice));
        on!(on_root_grouping_changed, |app, index, choice| app.root_grouping_changed(index.max(0) as usize, choice));
        on!(on_remove_folder, |app, index| app.ask_remove_folder(index as usize));
        on!(on_rescan, |app| app.start_scan());
        on!(on_language_changed, |app, index| app.set_language(Lang::from_index(index)));
        on!(on_advanced_changed, |app, on| {
            app.advanced.set(on);
            app.save_flag(SETTING_ADVANCED, on);
            // Плейлист дня: в режиме продвинутого — без порогов.
            app.show_daily();
        });
        on!(on_statistics_changed, |app, on| {
            app.statistics.set(on);
            app.window.set_statistics_enabled(on);
            app.save_flag(SETTING_STATISTICS, on);
            app.show_daily();
        });
        on!(on_daily_open, |app| app.open_daily());
        on!(on_daily_play, |app| app.play_daily());
        on!(on_daily_enable, |app| app.daily_accept());
        on!(on_daily_dismiss, |app| app.daily_dismiss());
        on!(on_daily_rebuild, |app| app.daily_rebuild());
        on!(on_daily_enabled_changed, |app, on| app.daily_set_enabled(on));
        on!(on_daily_length_changed, |app, index| app.daily_set_length(index.max(0) as usize));
        on!(on_daily_fresh_changed, |app, value, done| app.daily_set_fresh(value, done));
        on!(on_daily_threshold_changed, |app, index| app.daily_set_threshold(index.max(0) as usize));
        on!(on_authenticity_changed, |app, on| app.request_authenticity(on));
        on!(on_authenticity_level_changed, |app, index| app.set_level(index));
        on!(on_cover_cache_changed, |app, on| app.request_cover_cache(on));
        on!(on_cover_disk_changed, |app, on| {
            app.cover_disk_on.set(on);
            app.save_flag(SETTING_COVER_DISK, on);
            app.apply_covers();
        });
        on!(on_stats_view_changed, |app, _view| app.wake_graph());
        on!(on_choose_avatar, |app| app.choose_avatar());
        on!(on_remove_avatar, |app| app.remove_avatar());
        on!(on_graph_grab, |app, index, x, y| {
            app.graph_press.set((x, y));
            app.graph_travel.set(0.0);
            if let Some(layout) = app.graph.borrow_mut().as_mut() {
                layout.grab(index as usize, x, y);
            }
            app.wake_graph();
        });
        on!(on_graph_drag, |app, index, x, y| {
            let (start_x, start_y) = app.graph_press.get();
            let travel = ((x - start_x).powi(2) + (y - start_y).powi(2)).sqrt();
            app.graph_travel.set(app.graph_travel.get().max(travel));
            if let Some(layout) = app.graph.borrow_mut().as_mut() {
                layout.drag_to(index as usize, x, y);
            }
            app.wake_graph();
        });
        on!(on_graph_release, |app, index| {
            if let Some(layout) = app.graph.borrow_mut().as_mut() {
                layout.release(index as usize);
            }
            // Почти не сдвинули — это нажатие: открыть исполнителя или альбом.
            if app.graph_travel.get() < 5.0 {
                app.open_graph_node(index as usize);
            }
            app.wake_graph();
        });
        on!(on_theme_changed, |app, index| app.set_theme(index.max(0) as usize));
        on!(on_season_preview, |app, index| app.season_preview(index));
        on!(on_season_play, |app| app.season_play());
        on!(on_view_changed, |app, kind, grid| app.set_view(kind, grid));
        on!(on_artist_tile_shown, |app, index| app.artist_picture(index.max(0) as usize));
        on!(on_seasons_changed, |app, on| app.set_seasons(on));
        on!(on_scale_changed, |app, percent, done| app.set_scale(percent, done));
        on!(on_zoom, |app, step| app.zoom(step));
        on!(on_gestures_enabled_changed, |app, on| {
            app.gestures_on.set(on);
            app.save_flag(SETTING_GESTURES, on);
            app.gesture_cancel();
        });
        on!(on_system_media_changed, |app, on| {
            app.save_flag(SETTING_SYSTEM_MEDIA, on);
            app.apply_system_media();
        });
        on!(on_ffmpeg_changed, |app, on| {
            nyanoto_core::set_ffmpeg_enabled(on);
            app.save_flag(SETTING_FFMPEG, on);
            app.show_ffmpeg_status();
        });
        on!(on_cover_quality_changed, |app, index| {
            let quality = covers::Quality::ALL.get(index as usize).copied().unwrap_or(covers::Quality::Medium);
            app.cover_quality.set(quality);
            app.save_setting(SETTING_COVER_QUALITY, quality.code());
            // Следующий показ берёт обложки уже из копий нового качества.
            app.forget_covers();
            app.apply_covers();
        });
        on!(on_warning_confirm, |app| {
            app.close_warning();
            match app.warning.get() {
                Warning::Authenticity => app.set_authenticity(true),
                Warning::Covers => app.set_cover_cache(false),
            }
        });
        on!(on_warning_cancel, |app| app.close_warning());
        on!(on_check_now, |app| app.check_now());
        on!(on_sleep_enabled_changed, |app, on| {
            app.sleep_on.set(on);
            app.save_flag(SETTING_SLEEP_TIMER, on);
            if !on {
                app.window.set_sleep_menu_open(false);
                app.set_sleep(Sleep::Off);
            }
        });
        on!(on_ab_enabled_changed, |app, on| {
            app.ab_on.set(on);
            app.save_flag(SETTING_AB_REPEAT, on);
            if !on {
                app.set_ab((None, None));
            }
        });

        on!(on_replaygain_changed, |app, on| {
            app.replaygain.set(on);
            app.save_flag(SETTING_REPLAYGAIN, on);
            app.apply_loudness();
        });
        on!(on_replaygain_mode_changed, |app, index| {
            app.replaygain_album.set(index == 1);
            app.save_setting(SETTING_REPLAYGAIN_MODE, if index == 1 { "album" } else { "track" });
            app.apply_loudness();
        });
        on!(on_lyrics_changed, |app, on| {
            app.lyrics_on.set(on);
            app.save_flag(SETTING_LYRICS, on);
            if on {
                app.load_lyrics();
            } else {
                app.window.set_lyrics_open(false);
                app.window.set_full_lyrics(false);
            }
        });
        on!(on_sleep_cycle, |app| app.toggle_sleep_menu());
        on!(on_sleep_step, |app, part, delta| {
            let unit = if part == 0 { 3600 } else { 60 };
            app.set_sleep_target(app.sleep_target.get() + i64::from(delta) * unit);
        });
        on!(on_sleep_dial, |app, x, y, start| app.sleep_dial(x, y, start));
        on!(on_sleep_wheel, |app, delta| {
            let (now_minute, minutes) = app.sleep_plan();
            let minutes = sleep::snap(now_minute, minutes + i64::from(delta) * sleep::DIAL_STEP);
            app.set_sleep_minutes(minutes);
        });
        on!(on_sleep_start, |app| {
            let left = (app.sleep_target.get() - clock::now()).max(60);
            app.set_sleep(Sleep::Until(Instant::now() + Duration::from_secs(left as u64), (left / 60) as u32));
            app.window.set_sleep_menu_open(false);
        });
        on!(on_sleep_stop, |app| {
            app.set_sleep(Sleep::Off);
            app.window.set_sleep_menu_open(false);
        });
        on!(on_sleep_after_toggle, |app| {
            let next = if app.sleep.get() == Sleep::AfterTrack { Sleep::Off } else { Sleep::AfterTrack };
            app.set_sleep(next);
        });
        on!(on_ab_cycle, |app| app.cycle_ab());
        on!(on_lyrics_toggle, |app| {
            let open = !app.window.get_lyrics_open();
            app.window.set_lyrics_open(open);
            if open {
                app.show_lyrics();
                app.schedule_line();
            }
        });
        on!(on_lyrics_close, |app| app.window.set_lyrics_open(false));
        on!(on_lyrics_seek, |app, index| app.seek_lyric_line(index));
        on!(on_lyrics_style_changed, |app, index| {
            let style = (index.max(0) as usize).min(LYRICS_STYLES.len() - 1);
            app.lyrics_style.set(style);
            app.save_setting(SETTING_LYRICS_STYLE, LYRICS_STYLES[style]);
            app.stage.borrow_mut().clear();
            app.stage_paused.set(None);
            if app.lyrics_visible() {
                app.show_lyrics();
                app.schedule_line();
            }
        });
        on!(on_stage_scene_changed, |app, index| {
            let scene = (index.max(0) as usize).min(stage::Kind::ALL.len() + 1);
            app.stage_scene.set(scene);
            app.save_setting(SETTING_STAGE_SCENE, &stage_scene_code(scene));
            app.stage.borrow_mut().clear();
            app.stage_paused.set(None);
            app.stage_tick();
        });
        on!(on_stage_fps_changed, |app, index| {
            let fps = if index == 1 { 30 } else { 60 };
            app.stage_fps.set(fps);
            app.save_setting(SETTING_STAGE_FPS, &fps.to_string());
        });
        on!(on_stage_fallback_load_changed, |app, on| {
            app.stage_fallback_load.set(on);
            app.save_setting(SETTING_STAGE_FALLBACK_LOAD, if on { "1" } else { "0" });
            app.watch_load();
            app.load_tick();
        });
        on!(on_stage_fallback_power_changed, |app, on| {
            app.stage_fallback_power.set(on);
            app.save_setting(SETTING_STAGE_FALLBACK_POWER, if on { "1" } else { "0" });
            app.watch_load();
            app.load_tick();
        });
        on!(on_mute_toggle, |app| app.toggle_mute());
        on!(on_stage_resized, |app, place, width, height| {
            let mut sizes = app.stage_sizes.get();
            sizes[usize::from(place == 1)] = (width, height);
            app.stage_sizes.set(sizes);
            app.stage_tick();
        });
        on!(on_lyrics_lead_changed, |app, value, save| {
            let lead = ((value / 10.0).round() * 10.0).clamp(0.0, 1000.0) as u32;
            app.lyrics_lead.set(lead);
            if save {
                app.window.set_lyrics_lead(lead as f32);
                app.save_setting(SETTING_LYRICS_LEAD, &lead.to_string());
            }
            if app.lyrics_visible() {
                app.show_lyrics();
                app.schedule_line();
            }
        });
        on!(on_lyrics_edit, |app| {
            let current = app.current.borrow().clone();
            if let Some(track) = current {
                app.window.set_lyrics_open(false);
                app.open_editor(track);
            }
        });
        on!(on_info_lyrics, |app| {
            let track = app.info.borrow().as_ref().map(|info| info.track.clone());
            if let Some(track) = track {
                app.open_editor(track);
            }
        });
        on!(on_editor_go, |app, step| app.editor_go(step));
        on!(on_editor_plain_edited, |app, text| app.change_draft(false, |editor| {
            editor.draft.set_plain(&text);
            editor.raw = None;
        }));
        on!(on_editor_raw_edited, |app, text| app.change_draft(false, |editor| {
            editor.raw = Some(text.into());
        }));
        on!(on_editor_stamp, |app| app.editor_stamp());
        on!(on_editor_unstamp, |app| app.change_draft(true, |editor| {
            if let Some(index) = editor.draft.unstamp() {
                editor.selected = index;
            }
        }));
        on!(on_editor_play_pause, |app| app.editor_play_pause());
        on!(on_editor_seek_by, |app, delta| app.editor_seek_by(i64::from(delta)));
        on!(on_editor_move, |app, delta| {
            let selected = app.editor.borrow().as_ref().map_or(0, |editor| editor.selected as i64);
            app.editor_select((selected + i64::from(delta)).max(0) as usize);
        });
        on!(on_editor_select, |app, index| app.editor_select(index.max(0) as usize));
        on!(on_editor_nudge, |app, index, delta| app.change_draft(true, |editor| {
            let index = if index < 0 { editor.selected } else { index as usize };
            editor.draft.nudge(index, i64::from(delta));
        }));
        on!(on_editor_set_time, |app, index, text| {
            let ok = app
                .editor
                .borrow_mut()
                .as_mut()
                .is_some_and(|editor| editor.draft.set_time(index.max(0) as usize, &text));
            if ok {
                app.change_draft(true, |_| {});
            } else {
                app.window.set_editor_message(app.lex().lrc_bad_time.into());
                app.render_editor();
            }
        });
        on!(on_editor_play_line, |app, index| app.editor_play_line(index));
        on!(on_editor_clear_line, |app, index| app.change_draft(true, |editor| {
            editor.draft.set_time(index.max(0) as usize, "");
            // Разметка вернулась к этой строке — она и выбрана.
            if editor.draft.next == index.max(0) as usize {
                editor.selected = editor.draft.next;
            }
        }));
        on!(on_editor_shift_all, |app, delta| app.change_draft(true, |editor| {
            editor.draft.shift_all(i64::from(delta));
        }));
        on!(on_editor_tap_offset_changed, |app, offset| {
            let changed = app.editor.borrow_mut().as_mut().is_some_and(|editor| {
                let changed = editor.draft.tap_offset != i64::from(offset);
                editor.draft.tap_offset = i64::from(offset);
                changed
            });
            if changed {
                app.save_setting(SETTING_TAP_OFFSET, &offset.to_string());
            }
        });
        on!(on_classical_changed, |app, on| {
            app.classical.set(on);
            app.save_flag(SETTING_CLASSICAL, on);
            // Без модуля классики — никаких «по композиторам».
            if !on
                && let Some(library) = app.library.as_ref()
            {
                if let Err(error) = library.drop_composer_roots() {
                    app.report(error);
                }
                app.refresh_settings_page();
                app.refresh_artists();
            }
        });
        on!(on_tags_cp1251_changed, |app, on| app.request_fix_encoding(on));
        on!(on_long_records_changed, |app, on| {
            app.long_on.set(on);
            app.save_flag(SETTING_LONG, on);
            if on {
                app.load_chapters();
            } else {
                app.window.set_chapters_open(false);
            }
        });
        on!(on_chapters_toggle, |app| {
            let open = !app.window.get_chapters_open();
            app.window.set_chapters_open(open);
            if open {
                app.load_chapters();
            }
        });
        on!(on_chapters_close, |app| app.window.set_chapters_open(false));
        on!(on_smart_enabled_changed, |app, on| {
            app.smart_on.set(on);
            app.save_flag(SETTING_SMART, on);
        });
        on!(on_shuffle_cycle, |app| app.cycle_shuffle());
        on!(on_health_enabled_changed, |app, on| {
            app.health_on.set(on);
            app.save_flag(SETTING_HEALTH, on);
            if on {
                app.start_health_checks();
            } else {
                app.stop_health_checks();
                if app.window.get_page() == PAGE_HEALTH {
                    app.go(PAGE_SETTINGS);
                }
            }
        });
        on!(on_health_now, |app| app.health_now());
        on!(on_eq_enabled_changed, |app, on| {
            app.eq_on.set(on);
            app.save_flag(equalizer::SETTING_EQ, on);
            app.apply_equalizer();
            if !on {
                app.window.set_eq_open(false);
            }
        });
        on!(on_device_profiles_changed, |app, on| {
            app.profiles_on.set(on);
            app.save_flag(equalizer::SETTING_DEVICE_PROFILES, on);
            if on {
                app.remember_device();
            }
            app.send_device_volumes();
        });
        on!(on_eq_toggle, |app| {
            if app.window.get_eq_open() {
                app.window.set_eq_open(false);
            } else {
                app.open_eq();
            }
        });
        on!(on_eq_close, |app| app.window.set_eq_open(false));
        on!(on_eq_active_changed, |app, on| app.eq_set_active(on));
        on!(on_eq_preamp_changed, |app, db| app.eq_set_preamp(db));
        on!(on_eq_kind_changed, |app, index, kind| app.eq_set_kind(index.max(0) as usize, kind));
        on!(on_eq_freq_edited, |app, index, text| {
            if let Ok(freq) = text.trim().replace(',', ".").parse::<f32>()
                && (10.0..=24_000.0).contains(&freq)
            {
                app.eq_edit(index.max(0) as usize, |band| band.freq = freq);
            }
        });
        on!(on_eq_gain_changed, |app, index, gain| {
            let gain = (gain * 10.0).round() / 10.0;
            app.eq_edit(index.max(0) as usize, |band| band.gain_db = gain);
            let index = index.max(0) as usize;
            if let Some(mut row) = app.eq_rows.row_data(index) {
                row.gain = gain;
                app.eq_rows.set_row_data(index, row);
            }
        });
        on!(on_eq_q_edited, |app, index, text| {
            if let Ok(q) = text.trim().replace(',', ".").parse::<f32>()
                && (0.05..=50.0).contains(&q)
            {
                app.eq_edit(index.max(0) as usize, |band| band.q = q);
            }
        });
        on!(on_eq_add, |app| app.eq_add());
        // Свой заголовок окна.
        on!(on_window_minimize, |app| app.window.window().set_minimized(true));
        on!(on_window_maximize, |app| {
            let window = app.window.window();
            window.set_maximized(!window.is_maximized());
        });
        on!(on_window_close, |app| {
            app.window.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
        });
        #[cfg(not(target_os = "android"))]
        on!(on_window_drag, |app| {
            use slint::winit_030::WinitWindowAccessor;
            app.window.window().with_winit_window(|winit| {
                let _ = winit.drag_window();
            });
        });
        #[cfg(not(target_os = "android"))]
        on!(on_window_menu, |app, x, y| {
            use slint::winit_030::WinitWindowAccessor;
            let scale = app.window.window().scale_factor();
            app.window.window().with_winit_window(|winit| {
                winit.show_window_menu(slint::winit_030::winit::dpi::PhysicalPosition::new(f64::from(x * scale), f64::from(y * scale)));
            });
        });
        on!(on_gpu_render_changed, |app, on| {
            app.save_setting(SETTING_RENDERER, if on { "gpu" } else { "software" });
            app.window.set_status(app.lex().gpu_render_restart.into());
        });
        on!(on_device_pause_changed, |app, on| {
            app.save_flag(SETTING_DEVICE_PAUSE, on);
            app.engine.set_pause_on_device_change(on);
        });
        on!(on_daily_force, |app| app.daily_force());
        on!(on_exclusive_enabled_changed, |app, on| {
            app.exclusive_on.set(on);
            app.save_flag(SETTING_EXCLUSIVE, on);
            app.engine.set_exclusive(on);
            app.show_signal_path();
        });
        on!(on_sync_enabled_changed, |app, on| {
            app.sync_on.set(on);
            app.save_flag(link::SETTING_SYNC, on);
            if on {
                app.sync_now();
            }
        });
        on!(on_sync_choose, |app| app.choose_sync_folder());
        on!(on_convert_enabled_changed, |app, on| {
            app.convert_on.set(on);
            app.save_flag(conversion::SETTING_CONVERT, on);
        });
        on!(on_convert_album, |app| {
            let tracks = app.lists[LIST_ALBUM].borrow().clone();
            app.open_conversion(tracks);
        });
        on!(on_convert_playlist, |app| {
            let tracks = app.lists[LIST_PLAYLIST].borrow().clone();
            app.open_conversion(tracks);
        });
        on!(on_convert_choose, |app| app.choose_convert_folder());
        on!(on_convert_start, |app| app.start_conversion());
        on!(on_convert_stop, |app| app.stop_conversion());
        on!(on_convert_close, |app| app.window.set_convert_open(false));
        on!(on_sync_now, |app| app.sync_now());
        on!(on_crossfade_changed, |app, on| {
            app.save_flag(SETTING_CROSSFADE, on);
            app.apply_crossfade();
        });
        on!(on_crossfade_index_changed, |app, index| {
            let seconds = CROSSFADE_SECONDS.get(index.max(0) as usize).copied().unwrap_or(5);
            app.save_setting(SETTING_CROSSFADE_SECS, &seconds.to_string());
            app.apply_crossfade();
        });
        on!(on_eq_remove, |app, index| app.eq_remove(index.max(0) as usize));
        on!(on_eq_reset, |app| app.eq_reset());
        on!(on_eq_profile_chosen, |app, index| app.eq_choose_profile(index.max(0) as usize));
        on!(on_eq_profile_save, |app, name| app.eq_save_profile(&name));
        on!(on_eq_profile_delete, |app| app.eq_delete_profile());
        on!(on_eq_import, |app| app.eq_import());
        on!(on_wavebar_enabled_changed, |app, on| {
            app.wavebar_on.set(on);
            app.save_flag(SETTING_WAVEBAR, on);
            app.load_wavebar();
        });
        on!(on_health_open, |app, index| app.open_health_row(index.max(0) as usize));
        on!(on_shuffle_all, |app| app.shuffle_everything());
        on!(on_smart_new, |app| app.open_smart_editor(None));
        on!(on_smart_edit, |app| {
            let id = app.open_smart_id();
            if id.is_some() {
                app.open_smart_editor(id);
            }
        });
        on!(on_smart_preset, |app, index| app.smart_preset(index));
        on!(on_smart_add_condition, |app| app.smart_add_condition());
        on!(on_smart_remove_condition, |app, index| app.smart_remove_condition(index.max(0) as usize));
        on!(on_smart_kind_changed, |app, index, kind| app.smart_kind_changed(index.max(0) as usize, kind));
        on!(on_smart_value_edited, |app, index, text| app.smart_value_edited(index.max(0) as usize, &text));
        on!(on_smart_changed, |app| app.count_smart());
        on!(on_smart_save, |app| app.save_smart());
        on!(on_smart_close, |app| app.window.set_smart_open(false));
        on!(on_chapter_jump, |app, index| {
            let at = app.chapters.borrow().get(index.max(0) as usize).map(|item| item.at_ms);
            if let Some(at) = at {
                app.engine.seek(Duration::from_millis(at.max(0) as u64));
                Timer::single_shot(Duration::from_millis(80), || with_app(|app| app.update_position()));
            }
        });
        on!(on_bookmark_add, |app, name| app.add_bookmark(&name));
        on!(on_bookmark_delete, |app, index| {
            let id = app.chapters.borrow().get(index.max(0) as usize).and_then(|item| item.bookmark);
            if let (Some(id), Some(library)) = (id, app.library.as_ref()) {
                if let Err(error) = library.delete_bookmark(id) {
                    app.report(error);
                }
                app.load_chapters();
            }
        });
        on!(on_confirm_accept, |app| app.confirm_accepted());
        on!(on_confirm_cancel, |app| {
            app.confirm.set(None);
            app.window.set_confirm_open(false);
            // Переключатель возвращается, пока действие не подтверждено.
            app.window.set_tags_cp1251(
                app.library
                    .as_ref()
                    .and_then(|library| library.setting(nyanoto_library::SETTING_FIX_ENCODING).ok().flatten())
                    .as_deref()
                    == Some("1"),
            );
        });
        on!(on_speed_module_changed, |app, on| {
            app.speed_module.set(on);
            app.save_flag(SETTING_SPEED, on);
            // Выключение возвращает обычную скорость.
            app.set_normal_speed(if on { app.normal_speed.get() } else { 1.0 });
        });
        on!(on_keep_pitch_changed, |app, on| {
            app.keep_pitch.set(on);
            app.save_flag(SETTING_KEEP_PITCH, on);
            app.apply_speed();
        });
        // Пока тянут ползунок — только подпись: смена скорости перезапускает звук.
        on!(on_speed_changed, |app, value| {
            let text = app.speed_text(round_speed(f64::from(value), 0.25, 3.0));
            app.window.set_speed_text(text.into());
            app.speed_timer.start(TimerMode::SingleShot, SPEED_SETTLE, move || {
                with_app(|app| app.set_normal_speed(f64::from(value)));
            });
        });
        on!(on_speed_released, |app, value| {
            app.speed_timer.stop();
            app.set_normal_speed(f64::from(value));
        });
        on!(on_speed_reset, |app| app.set_normal_speed(1.0));
        on!(on_editor_speed_changed, |app, value| {
            let text = app.speed_text(round_speed(f64::from(value), 0.05, 1.0));
            app.window.set_editor_speed_text(text.into());
            app.speed_timer.start(TimerMode::SingleShot, SPEED_SETTLE, move || {
                with_app(|app| app.set_editor_speed(f64::from(value)));
            });
        });
        on!(on_editor_speed_released, |app, value| {
            app.speed_timer.stop();
            app.set_editor_speed(f64::from(value));
        });
        on!(on_editor_overview_seek, |app, fraction| {
            let duration = app.wave_duration();
            app.editor_seek_to((f64::from(fraction) * duration as f64) as i64);
        });
        on!(on_editor_wave_seek, |app, fraction| {
            let from = app.wave_center() - WAVE_HALF_MS;
            app.editor_seek_to(from + (f64::from(fraction) * (2 * WAVE_HALF_MS) as f64) as i64);
        });
        on!(on_editor_drag, |app, on| {
            if on {
                app.drag_window.set(app.wave_strip.get());
            } else {
                app.drag_window.set(None);
                app.refresh_wave();
            }
        });
        on!(on_editor_mark_dropped, |app, index, fraction| {
            let Some(from) = app.drag_window.get() else {
                return;
            };
            let at = (from + (f64::from(fraction) * WAVE_STRIP_MS as f64).round() as i64).max(0);
            app.change_draft(true, |editor| {
                let index = index.max(0) as usize;
                if let Some(line) = editor.draft.lines.get_mut(index) {
                    line.at = Some(at);
                    editor.selected = index;
                }
            });
        });
        on!(on_editor_save, |app| app.editor_save());
        on!(on_editor_write, |app| app.editor_write());
        on!(on_editor_undo, |app| app.editor_undo());
        on!(on_editor_close, |app, discard| app.close_editor(discard));
        on!(on_auto_next_changed, |app, on| {
            app.auto_next.set(on);
            app.save_flag(SETTING_AUTO_NEXT, on);
        });
        on!(on_auto_next_order_changed, |app, index| {
            app.auto_next_random.set(index == 1);
            app.save_setting(SETTING_AUTO_NEXT_ORDER, if index == 1 { "random" } else { "ui" });
        });
        on!(on_track_badge, |app, list, index| {
            if let Some(track) = app.track_at(list as usize, index as usize) {
                app.open_quality(track);
            }
        });
        on!(on_badge_current, |app| {
            let current = app.current.borrow().clone();
            if let Some(track) = current {
                app.open_quality(track);
            }
        });
        on!(on_quality_replace, |app, index| {
            let label = QUALITY_LABELS.get(index as usize).copied().unwrap_or("CD");
            app.set_override(Some(QualityOverride::Label(label.to_string())));
        });
        on!(on_quality_use_detected, |app| app.set_override(Some(QualityOverride::Detected)));
        on!(on_quality_accept, |app| app.set_override(Some(QualityOverride::Accept)));
        on!(on_quality_reset, |app| app.set_override(None));
        on!(on_quality_close, |app| app.window.set_quality_open(false));
        on!(on_info_close, |app| app.close_info());
        on!(on_tag_key_edited, |app, index, text| app.edit_tag(index as usize, Some(text.into()), None));
        on!(on_tag_value_edited, |app, index, text| app.edit_tag(index as usize, None, Some(text.into())));
        on!(on_tag_add, |app| app.change_tags(|tags| tags.push(TagField { key: String::new(), value: String::new() })));
        on!(on_tag_remove, |app, index| app.change_tags(|tags| {
            if (index as usize) < tags.len() {
                tags.remove(index as usize);
            }
        }));
        on!(on_tags_reset, |app| app.change_tags_from_original());
        on!(on_tags_save, |app| app.save_tags());
        on!(on_tags_undo, |app| app.undo_tags());
        on!(on_picker_choose, |app, index| app.pick(index as usize));
        on!(on_picker_create, |app, name| {
            if let Some(id) = app.create_playlist(&name) {
                app.add_picked(OpenList::Playlist(id));
            }
        });
        on!(on_picker_close, |app| app.window.set_picker_open(false));
    }

    // ─────────────────────────── настройки ───────────────────────────

    fn load_settings(&self) {
        let library = self.library.as_ref();
        let setting = |key| library.and_then(|library| library.setting(key).ok().flatten());
        let lang = setting(SETTING_LANGUAGE).map(|code| Lang::from_code(&code)).unwrap_or_default();
        let classical = setting(SETTING_CLASSICAL).as_deref() == Some("1");
        // Раскладка — у каждой папки своя.
        let grouping = Grouping::PerRoot;
        self.classical.set(classical);
        self.long_on.set(!UNFINISHED && setting(SETTING_LONG).as_deref() == Some("1"));
        self.smart_on.set(setting(SETTING_SMART).as_deref() == Some("1"));
        self.health_on.set(setting(SETTING_HEALTH).as_deref() == Some("1"));
        self.wavebar_on.set(setting(SETTING_WAVEBAR).as_deref() == Some("1"));
        self.shuffle.set(smart::Shuffle::from_code(setting(SETTING_SHUFFLE).as_deref()));
        self.lang.set(lang);
        self.grouping.set(grouping);
        self.advanced.set(setting(SETTING_ADVANCED).as_deref() == Some("1"));
        self.statistics.set(setting(SETTING_STATISTICS).as_deref() == Some("1"));
        // Подлинность выключена, пока не включат; следующий альбом — включён, пока не выключат.
        self.authenticity.set(setting(SETTING_AUTHENTICITY).as_deref() == Some("1"));
        self.level.set(match setting(SETTING_AUTHENTICITY_LEVEL).as_deref() {
            Some("medium") => CheckLevel::Medium,
            Some("high") => CheckLevel::High,
            _ => CheckLevel::Low,
        });
        self.auto_next.set(setting(SETTING_AUTO_NEXT).as_deref() != Some("0"));
        self.cover_cache_on.set(setting(SETTING_COVER_CACHE).as_deref() != Some("0"));
        self.cover_disk_on.set(setting(SETTING_COVER_DISK).as_deref() != Some("0"));
        self.cover_quality.set(covers::Quality::from_code(setting(SETTING_COVER_QUALITY).as_deref()));
        self.window.set_system_media(setting(SETTING_SYSTEM_MEDIA).as_deref() != Some("0"));
        self.gestures_on.set(setting(SETTING_GESTURES).as_deref() != Some("0"));
        self.window.set_artists_grid(setting(SETTING_VIEW_ARTISTS).as_deref() != Some("list"));
        self.window.set_albums_grid(setting(SETTING_VIEW_ALBUMS).as_deref() != Some("list"));
        self.window.set_gestures_enabled(self.gestures_on.get());
        let ffmpeg = setting(SETTING_FFMPEG).as_deref() != Some("0");
        nyanoto_core::set_ffmpeg_enabled(ffmpeg);
        self.window.set_ffmpeg(ffmpeg);
        self.show_ffmpeg_status();
        let old_sleep_ab = setting(SETTING_OLD_SLEEP_AB);
        let sleep_timer = setting(SETTING_SLEEP_TIMER).or_else(|| old_sleep_ab.clone());
        let ab_repeat = setting(SETTING_AB_REPEAT).or(old_sleep_ab);
        self.sleep_on.set(sleep_timer.as_deref() == Some("1"));
        self.ab_on.set(ab_repeat.as_deref() == Some("1"));

        self.replaygain.set(!UNFINISHED && setting(SETTING_REPLAYGAIN).as_deref() == Some("1"));
        self.replaygain_album
            .set(setting(SETTING_REPLAYGAIN_MODE).as_deref() == Some("album"));
        self.lyrics_on.set(setting(SETTING_LYRICS).as_deref() == Some("1"));
        let lead = setting(SETTING_LYRICS_LEAD).and_then(|value| value.parse::<u32>().ok()).unwrap_or(LYRICS_LEAD_MS).min(1000);
        self.lyrics_lead.set(lead);
        self.window.set_lyrics_lead(lead as f32);
        let style = setting(SETTING_LYRICS_STYLE).and_then(|code| LYRICS_STYLES.iter().position(|known| *known == code)).unwrap_or(0);
        self.lyrics_style.set(style);
        self.window.set_lyrics_style_index(style as i32);
        let scene = setting(SETTING_STAGE_SCENE).map_or(0, |code| stage_scene_index(&code));
        self.stage_scene.set(scene);
        self.window.set_stage_scene_index(scene as i32);
        let fallback_load = setting(SETTING_STAGE_FALLBACK_LOAD).as_deref() != Some("0");
        let fallback_power = setting(SETTING_STAGE_FALLBACK_POWER).as_deref() != Some("0");
        self.stage_fallback_load.set(fallback_load);
        self.stage_fallback_power.set(fallback_power);
        self.window.set_stage_fallback_load(fallback_load);
        self.window.set_stage_fallback_power(fallback_power);
        let fps = if setting(SETTING_STAGE_FPS).as_deref() == Some("30") { 30 } else { 60 };
        self.stage_fps.set(fps);
        self.window.set_stage_fps_index(i32::from(fps == 30));
        self.speed_module.set(setting(SETTING_SPEED).as_deref() == Some("1"));
        // Высота звука сохраняется, пока её не отключили.
        self.keep_pitch.set(setting(SETTING_KEEP_PITCH).as_deref() != Some("0"));
        self.auto_next_random
            .set(setting(SETTING_AUTO_NEXT_ORDER).as_deref() == Some("random"));
        if let Some(library) = library {
            self.liked.replace(library.liked_ids().unwrap_or_default());
            self.suspicious.replace(library.suspicious(self.level.get()).unwrap_or_default());
            self.overrides.replace(library.quality_overrides().unwrap_or_default());
        }

        let window = &self.window;
        window.set_language_index(lang.index());
        window.set_classical(classical);
        window.set_long_records(self.long_on.get());
        window.set_smart_enabled(self.smart_on.get());
        window.set_health_enabled(self.health_on.get());
        window.set_wavebar_enabled(self.wavebar_on.get());
        window.set_crossfade(setting(SETTING_CROSSFADE).as_deref() == Some("1"));
        self.exclusive_on.set(!UNFINISHED && setting(SETTING_EXCLUSIVE).as_deref() == Some("1"));
        // По умолчанию включено.
        window.set_gpu_render(setting(SETTING_RENDERER).as_deref() == Some("gpu"));
        let device_pause = setting(SETTING_DEVICE_PAUSE).as_deref() != Some("0");
        self.engine.set_pause_on_device_change(device_pause);
        self.window.set_device_pause(device_pause);
        self.sync_on.set(!UNFINISHED && setting(link::SETTING_SYNC).as_deref() == Some("1"));
        self.convert_on.set(!UNFINISHED && setting(conversion::SETTING_CONVERT).as_deref() == Some("1"));
        window.set_convert_enabled(self.convert_on.get());
        window.set_sync_enabled(self.sync_on.get());
        self.show_sync_folder();
        window.set_exclusive_enabled(self.exclusive_on.get());
        self.engine.set_exclusive(self.exclusive_on.get());
        let seconds: u64 = setting(SETTING_CROSSFADE_SECS).and_then(|value| value.parse().ok()).unwrap_or(5);
        window.set_crossfade_index(CROSSFADE_SECONDS.iter().position(|value| *value == seconds).unwrap_or(2) as i32);
        self.apply_crossfade();
        if let Some(volume) = setting(SETTING_VOLUME)
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|volume| volume.is_finite())
        {
            window.set_volume(volume.clamp(0.0, 1.0));
        }
        // Громкость устройства из профиля, если она известна, — поверх общей.
        self.load_equalizer();
        // Кодировка в разработке: сохранённое включение снимаем, чтобы скан его не применял.
        if UNFINISHED
            && setting(nyanoto_library::SETTING_FIX_ENCODING).as_deref() == Some("1")
            && let Some(library) = library
        {
            let _ = library.set_setting(nyanoto_library::SETTING_FIX_ENCODING, "0");
        }
        window.set_tags_cp1251(!UNFINISHED && setting(nyanoto_library::SETTING_FIX_ENCODING).as_deref() == Some("1"));
        window.set_advanced(self.advanced.get());
        window.set_statistics(self.statistics.get());
        window.set_statistics_enabled(self.statistics.get());
        window.set_authenticity(self.authenticity.get());
        window.set_authenticity_level(match self.level.get() {
            CheckLevel::Low => 0,
            CheckLevel::Medium => 1,
            CheckLevel::High => 2,
        });
        window.set_auto_next(self.auto_next.get());
        window.set_cover_cache(self.cover_cache_on.get());
        window.set_cover_disk(self.cover_disk_on.get());
        window.set_cover_quality(
            covers::Quality::ALL.iter().position(|quality| *quality == self.cover_quality.get()).unwrap_or(1) as i32,
        );
        self.apply_covers();
        window.set_sleep_enabled(self.sleep_on.get());
        window.set_ab_enabled(self.ab_on.get());

        window.set_replaygain(self.replaygain.get());
        window.set_replaygain_mode(i32::from(self.replaygain_album.get()));
        window.set_lyrics_enabled(self.lyrics_on.get());
        window.set_speed_enabled(self.speed_module.get());
        window.set_keep_pitch(self.keep_pitch.get());
        self.apply_loudness();
        window.set_auto_next_order(i32::from(self.auto_next_random.get()));
        self.load_daily_settings(&|key: &str| library.and_then(|library| library.setting(key).ok().flatten()));
        self.load_look_settings(&|key: &str| library.and_then(|library| library.setting(key).ok().flatten()));
        self.apply_language();
    }

    // ───────────────────────── длинные записи ─────────────────────────

    /// Играющий трек, если модуль включён и трек длинный.
    fn long_record(&self) -> Option<Track> {
        if !self.long_on.get() {
            return None;
        }
        self.current
            .borrow()
            .as_ref()
            .filter(|track| track.duration_ms.is_some_and(|ms| ms >= LONG_TRACK_MS))
            .cloned()
    }

    /// Запоминает место в длинной записи; дослушанную забывает.
    fn remember_position(&self) {
        self.position_saved.set(Some(Instant::now()));
        let (Some(track), Some(library)) = (self.long_record(), self.library.as_ref()) else {
            return;
        };
        let Some(position) = self.engine.position_in(&track.path).map(|position| position.as_millis() as i64) else {
            return;
        };
        let duration = track.duration_ms.unwrap_or(0);
        let saved = if position >= duration - 30_000 {
            library.forget_position(track.id)
        } else {
            library.save_position(track.id, position)
        };
        if let Err(error) = saved {
            self.report(error);
        }
    }

    /// Длинная запись началась с начала: продолжаем там, где остановились.
    fn resume_long_record(&self) {
        let (Some(track), Some(library)) = (self.long_record(), self.library.as_ref()) else {
            return;
        };
        let Some(position) = library.position(track.id).ok().flatten() else {
            return;
        };
        if position > 5_000 && position < track.duration_ms.unwrap_or(0) - 30_000 {
            self.engine.seek(Duration::from_millis(position as u64));
            let lex = self.lex();
            self.window
                .set_status(format!("{} {}", lex.resumed_at, rows::clock(position as f32 / 1000.0)).into());
        }
    }

    /// Главы из файла и закладки из базы для играющего трека.
    fn load_chapters(&self) {
        let track = self.current.borrow().clone();
        let mut items = Vec::new();
        if self.long_on.get()
            && let Some(track) = track
        {
            items.extend(chapters::chapters(&track.path).into_iter().map(|chapter| ChapterItem {
                at_ms: chapter.at_ms,
                name: chapter.name,
                bookmark: None,
            }));
            if let Some(library) = self.library.as_ref() {
                items.extend(library.bookmarks(track.id).unwrap_or_default().into_iter().map(|mark| {
                    ChapterItem { at_ms: mark.position_ms, name: mark.name, bookmark: Some(mark.id) }
                }));
            }
            items.sort_by_key(|item| item.at_ms);
            self.window.set_chapters_track(file_name(&track.path).into());
        }
        *self.chapters.borrow_mut() = items;
        self.show_chapters();
    }

    /// Список глав с текущей главой и время для новой закладки.
    fn show_chapters(&self) {
        let position = self.engine.position().map_or(0, |position| position.as_millis() as i64);
        let items = self.chapters.borrow();
        let current = items.iter().rposition(|item| item.bookmark.is_none() && item.at_ms <= position);
        let rows: Vec<ChapterRow> = items
            .iter()
            .enumerate()
            .map(|(index, item)| ChapterRow {
                time: rows::clock(item.at_ms as f32 / 1000.0).into(),
                name: item.name.as_str().into(),
                bookmark: item.bookmark.is_some(),
                current: Some(index) == current,
            })
            .collect();
        self.window.set_chapter_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        self.window.set_bookmark_time(rows::clock(position as f32 / 1000.0).into());
    }

    fn add_bookmark(&self, name: &str) {
        let track = self.current.borrow().as_ref().map(|track| track.id);
        let (Some(track), Some(library)) = (track, self.library.as_ref()) else {
            return;
        };
        let position = self.engine.position().map_or(0, |position| position.as_millis() as i64);
        let name = if name.trim().is_empty() { self.lex().bookmark_default } else { name };
        if let Err(error) = library.add_bookmark(track, position, name) {
            self.report(error);
        }
        self.window.set_bookmark_name(SharedString::new());
        self.load_chapters();
    }

    // ───────────────────────── теги в CP1251 ─────────────────────────

    /// Включение — через предпросмотр, выключение — сразу.
    fn request_fix_encoding(&self, on: bool) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        if !on {
            if let Err(error) = library.set_fix_encoding(false) {
                self.report(error);
            }
            self.start_scan();
            return;
        }
        let lex = self.lex();
        let (count, examples) = library.encoding_preview(200).unwrap_or_default();
        let body = if count == 0 { lex.cp1251_none.to_string() } else { format!("{}: {count}", lex.cp1251_found) };
        let rows: Vec<InfoRow> = examples
            .into_iter()
            .map(|(was, will)| InfoRow { name: was.into(), value: will.into() })
            .collect();
        self.open_confirm(Confirm::FixEncoding, lex.cp1251_title, &body, rows, lex.cp1251_apply);
    }

    fn open_confirm(&self, action: Confirm, title: &str, body: &str, rows: Vec<InfoRow>, yes: &str) {
        self.confirm.set(Some(action));
        let window = &self.window;
        window.set_confirm_title(title.into());
        window.set_confirm_body(body.into());
        window.set_confirm_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        window.set_confirm_yes(yes.into());
        window.set_confirm_open(true);
    }

    fn confirm_accepted(&self) {
        let action = self.confirm.take();
        self.window.set_confirm_open(false);
        match action {
            Some(Confirm::FixEncoding) => {
                if let Some(library) = self.library.as_ref()
                    && let Err(error) = library.set_fix_encoding(true)
                {
                    self.report(error);
                    return;
                }
                self.window.set_tags_cp1251(true);
                self.start_scan();
            }
            Some(Confirm::RemoveFolder(id)) => self.remove_folder(id),
            None => {}
        }
    }

    fn save_setting(&self, key: &str, value: &str) {
        if let Some(library) = self.library.as_ref()
            && let Err(error) = library.set_setting(key, value)
        {
            self.report(error);
        }
    }

    /// Видеокарта не дорисовала окно: дальше — процессором, и скажем почему.
    fn renderer_failed(&self) {
        self.save_setting(SETTING_RENDERER, "software");
        self.window.set_gpu_render(false);
        self.window.set_status(self.lex().gpu_render_failed.into());
    }

    fn save_flag(&self, key: &str, on: bool) {
        self.save_setting(key, if on { "1" } else { "0" });
    }

    fn set_language(&self, lang: Lang) {
        if lang == self.lang.get() {
            return;
        }
        self.lang.set(lang);
        self.save_setting(SETTING_LANGUAGE, lang.code());
        self.apply_language();
        self.show_ffmpeg_status();
        // Всё, что подписано кодом, собирается заново.
        self.refresh_artists();
        let artist = self.artist.borrow().clone();
        if let Some(artist) = artist {
            self.fill_albums(&artist);
        }
        self.render_all();
        self.show_repeat();
        self.refresh_now_labels();
        self.close_info();
        self.go(self.window.get_page());
    }

    /// Подписи играющего трека заново (язык, раскладка).
    fn refresh_now_labels(&self) {
        let Some(track) = self.current.borrow().clone() else {
            return;
        };
        let window = &self.window;
        let (title, place) = self.now_labels(Some(&track), &track.path);
        let (owner, album) = self.now_parts(&track);
        window.set_now_title(title.into());
        window.set_now_artist(place.into());
        window.set_now_owner(owner.into());
        window.set_now_album(album.into());
        window.set_now_quality(rows::quality_label(&quality(&track), self.lex()).into());
        if window.get_full_open() {
            self.show_full_next();
        }
        self.update_system_media();
    }

    /// Отдаёт окну слова выбранного языка.
    fn apply_language(&self) {
        let lex = self.lex();
        self.window.global::<Tr>().set_w(lex.words());
        let words = self.window.global::<WidgetWords>();
        let [undo, redo, cut, copy, paste, select_all] = lex.widget_words;
        words.set_undo(undo.into());
        words.set_redo(redo.into());
        words.set_cut(cut.into());
        words.set_copy(copy.into());
        words.set_paste(paste.into());
        words.set_select_all(select_all.into());
        self.show_theme_cards();
        self.apply_look(false);
        let names = |names: &[&str]| ModelRc::from(Rc::new(VecModel::from(names.iter().map(|name| SharedString::from(*name)).collect::<Vec<_>>())));
        self.window.set_stage_scene_names(names(&lex.stage_scenes));
        self.window.set_stage_fps_names(names(&lex.stage_fps_names));
        self.show_shuffle();
        self.window
            .set_future_options(ModelRc::from(Rc::new(VecModel::from(rows::options(lex)))));
        self.show_repeat();
        self.show_sleep();
        self.show_ab();
        self.window.set_speed_text(self.speed_text(self.normal_speed.get()).into());
        self.window.set_editor_speed_text(self.speed_text(self.editor_speed.get()).into());
        if self.window.get_editor_open() {
            self.render_editor();
        }
    }

    fn refresh_settings_page(&self) {
        let offline = self.lex().offline;
        let found = self.library.as_ref().and_then(|library| library.roots().ok()).unwrap_or_default();
        *self.root_ids.borrow_mut() = found.iter().map(|root| root.id).collect();
        let roots: Vec<RootRow> = found
            .iter()
            .map(|root| {
                let state = if root.online { String::new() } else { format!(" ({offline})") };
                RootRow {
                    path: format!("{}{state}", root.path.display()).into(),
                    grouping: match root.grouping {
                        Grouping::Tags => 1,
                        Grouping::Composers => 2,
                        Grouping::Folders | Grouping::PerRoot => 0,
                    },
                }
            })
            .collect();
        self.window.set_roots(ModelRc::from(Rc::new(VecModel::from(roots))));
    }

    // ─────────────────────────── навигация ───────────────────────────

    fn go(&self, page: i32) {
        self.navigated();
        let window = &self.window;
        let lex = self.lex();
        match page {
            PAGE_ARTISTS | PAGE_ALBUMS | PAGE_TRACKS => {
                // Повторное нажатие на «Музыка» ведёт к списку исполнителей.
                let target = if window.get_page() <= PAGE_SEARCH && page == PAGE_ARTISTS {
                    PAGE_ARTISTS
                } else {
                    self.browse_page.get()
                };
                window.set_query(SharedString::new());
                self.show_browse(target);
            }
            PAGE_SEARCH => window.set_crumb(lex.search.into()),
            PAGE_PLAYLISTS => {
                self.refresh_playlists();
                window.set_crumb(lex.playlists.into());
                window.set_page(PAGE_PLAYLISTS);
                self.daily_page_shown();
            }
            PAGE_PLAYLIST => self.load_open_playlist(),
            PAGE_STATS => {
                self.refresh_stats();
                window.set_crumb(lex.statistics.into());
                window.set_page(PAGE_STATS);
            }
            PAGE_HEALTH => {
                self.show_health();
                window.set_crumb(lex.health_tab.into());
                window.set_page(PAGE_HEALTH);
            }
            PAGE_SETTINGS => {
                self.refresh_settings_page();
                window.set_crumb(lex.settings.into());
                window.set_page(PAGE_SETTINGS);
                // Хватает ли уже статистики для плейлиста дня.
                self.daily_check();
            }
            PAGE_FOLDERS => {
                self.refresh_settings_page();
                window.set_crumb(lex.folders_title.into());
                window.set_page(PAGE_FOLDERS);
            }
            _ => {}
        }
    }

    /// Показывает страницу обзора с правильной подписью.
    fn show_browse(&self, page: i32) {
        let lex = self.lex();
        let crumb = match page {
            PAGE_ALBUMS => self
                .artist
                .borrow()
                .as_ref()
                .map(|artist| artist_name(artist, lex).to_string())
                .unwrap_or_default(),
            PAGE_TRACKS => self
                .album
                .borrow()
                .as_ref()
                .map(|album| rows::album_title(album, lex))
                .unwrap_or_default(),
            _ => lex.artists.to_string(),
        };
        self.window.set_crumb(crumb.into());
        self.window.set_page(page);
        self.browse_page.set(page);
    }

    fn back(&self) {
        if let Some(place) = self.place_here() {
            let mut places = self.forward_places.borrow_mut();
            places.push(place);
            if places.len() > FORWARD_LIMIT {
                places.remove(0);
            }
        }
        self.history_moving.set(true);
        self.back_inner();
        self.history_moving.set(false);
    }

    /// Где мы сейчас — чтобы «Вперёд» вернул сюда.
    fn place_here(&self) -> Option<Place> {
        Some(match self.window.get_page() {
            PAGE_ARTISTS => return None,
            PAGE_ALBUMS => Place::Albums(self.artist.borrow().clone()?),
            PAGE_TRACKS => Place::Tracks(self.album.borrow().clone()?),
            PAGE_PLAYLIST => Place::Playlist(self.open_list.get()),
            PAGE_SEARCH => Place::Search(self.window.get_query().to_string()),
            PAGE_QUEUE => Place::Queue,
            page => Place::Page(page),
        })
    }

    /// «Вперёд» — туда, откуда ушли «Назад».
    fn forward(&self) {
        let Some(place) = self.forward_places.borrow_mut().pop() else {
            return;
        };
        self.history_moving.set(true);
        match place {
            Place::Albums(artist) => self.open_artist(artist),
            Place::Tracks(album) => self.show_album(album),
            Place::Playlist(OpenList::Daily) => self.open_daily(),
            Place::Playlist(list) => {
                let name = match list {
                    OpenList::Liked => Some(self.lex().favorites.to_string()),
                    OpenList::Playlist(id) => {
                        self.playlists.borrow().iter().find(|playlist| playlist.id == id).map(|playlist| playlist.name.clone())
                    }
                    OpenList::Daily => None,
                };
                if let Some(name) = name {
                    self.open_list.set(list);
                    self.load_open_playlist();
                    self.window.set_playlist_deletable(list != OpenList::Liked);
                    self.window.set_crumb(name.into());
                    self.window.set_page(PAGE_PLAYLIST);
                }
            }
            Place::Search(query) => {
                self.window.set_query(query.clone().into());
                self.search(&query);
            }
            Place::Queue => self.toggle_queue_page(),
            Place::Page(page) => self.go(page),
        }
        self.history_moving.set(false);
    }

    /// Обычный переход (не «Назад» и не «Вперёд»): «Вперёд» больше некуда.
    fn navigated(&self) {
        if !self.history_moving.get() && !self.forward_places.borrow().is_empty() {
            self.forward_places.borrow_mut().clear();
        }
    }

    fn back_inner(&self) {
        match self.window.get_page() {
            PAGE_SEARCH => {
                self.window.set_query(SharedString::new());
                self.show_browse(self.browse_page.get());
            }
            PAGE_TRACKS => self.show_browse(PAGE_ALBUMS),
            PAGE_PLAYLIST => self.go(PAGE_PLAYLISTS),
            PAGE_QUEUE => self.leave_queue(),
            _ => self.show_browse(PAGE_ARTISTS),
        }
    }

    fn refresh_artists(&self) {
        let Some(library) = self.library.as_ref() else {
            self.window.set_empty(true);
            return;
        };
        match library.artists(self.grouping.get()) {
            Ok(artists) => {
                trace("artists read");
                let lex = self.lex();
                self.artist_rows
                    .set_vec(artists.iter().map(|artist| artist_row(artist, lex)).collect::<Vec<_>>());
                trace("artist rows");
                self.window.set_empty(artists.is_empty());
                *self.artists.borrow_mut() = artists;
                self.grid_pictures.borrow_mut().clear();
                self.load_avatars(ArtistList::Library);
                trace("avatars");
                if self.window.get_page() <= PAGE_TRACKS && self.artist.borrow().is_none() {
                    self.show_browse(PAGE_ARTISTS);
                }
            }
            Err(error) => self.report(error),
        }
    }

    fn open_artist(&self, artist: Artist) {
        self.navigated();
        if self.fill_albums(&artist) {
            *self.artist.borrow_mut() = Some(artist);
            self.show_artist_header();
            self.show_browse(PAGE_ALBUMS);
        }
    }

    // ─────────────────────── аватары исполнителей ───────────────────────

    /// Шапка страницы исполнителя: аватар, сводка и кнопки аватара.
    fn show_artist_header(&self) {
        let Some(artist) = self.artist.borrow().clone() else {
            return;
        };
        let window = &self.window;
        let row = artist_row(&artist, self.lex());
        window.set_artist_letter(row.letter);
        window.set_artist_detail(row.detail);
        window.set_artist_avatar(Image::default());
        let own = self.avatars.borrow().get(&artist.name).map(Path::to_path_buf);
        window.set_artist_avatar_editable(self.avatars.borrow().target(&artist.name).is_some());
        window.set_artist_own_avatar(own.is_some());
        let folder = self
            .albums
            .borrow()
            .iter()
            .find_map(|album| nyanoto_library::artist_folder(&artist.name, &album.folder));
        let Some(path) = own.clone().or_else(|| folder.clone()) else {
            return;
        };
        let size = picture_side(96.0, self.window.window().scale_factor());
        let name = artist.name;
        self.show_picture((path, size, covers::Shape::Round), covers::Source::Artist { own, folder }, move |app, image| {
            if app.artist.borrow().as_ref().is_some_and(|artist| artist.name == name) {
                app.window.set_artist_avatar(image);
            }
        });
    }

    /// Аватары в списке — только свои: искать фото у всех слишком дорого.
    fn load_avatars(&self, list: ArtistList) {
        let names: Vec<String> = list.artists(self).borrow().iter().map(|artist| artist.name.clone()).collect();
        for (index, name) in names.iter().enumerate() {
            self.load_avatar(list, index, name);
        }
    }

    /// Вид сменили: запомнить; альбомы сеткой — обложки крупнее.
    fn set_view(&self, kind: i32, grid: bool) {
        let key = if kind == 0 { SETTING_VIEW_ARTISTS } else { SETTING_VIEW_ALBUMS };
        self.save_setting(key, if grid { "grid" } else { "list" });
        if kind == 1
            && let Some(artist) = self.artist.borrow().clone()
        {
            self.fill_albums(&artist);
        }
    }

    /// Картинки исполнителей заново, в новом размере (сменился масштаб).
    #[cfg_attr(target_os = "android", allow(dead_code))]
    pub(crate) fn reload_artist_pictures(&self) {
        let shown = std::mem::take(&mut *self.grid_pictures.borrow_mut());
        if shown.is_empty() {
            return;
        }
        let indices: Vec<usize> = self
            .artists
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, artist)| shown.contains(&artist.name))
            .map(|(index, _)| index)
            .collect();
        for index in indices {
            self.artist_picture(index);
        }
    }

    /// Картинка плитки исполнителя: свой аватар, фото из папки или обложка (кругом).
    fn artist_picture(&self, index: usize) {
        let Some(artist) = self.artists.borrow().get(index).cloned() else {
            return;
        };
        if !self.grid_pictures.borrow_mut().insert(artist.name.clone()) {
            return;
        }
        let own = self.avatars.borrow().get(&artist.name).map(Path::to_path_buf);
        let (path, source) = match own {
            Some(own) => (own.clone(), covers::Source::Artist { own: Some(own), folder: None }),
            None => {
                let Some(albums) = self.library.as_ref().and_then(|library| library.albums(&artist).ok()) else {
                    return;
                };
                let photo = albums
                    .iter()
                    .find_map(|album| nyanoto_library::artist_folder(&artist.name, &album.folder))
                    .filter(|folder| covers::has_artist_photo(folder));
                match photo {
                    Some(folder) => (folder.clone(), covers::Source::Artist { own: None, folder: Some(folder) }),
                    None => {
                        let Some(first) = albums.first().map(|album| album.folder.clone()) else {
                            return;
                        };
                        (first.clone(), covers::Source::Album(first))
                    }
                }
            }
        };
        let size = picture_side(GRID_PICTURE, self.window.window().scale_factor());
        let name = artist.name;
        self.show_picture((path, size, covers::Shape::Round), source, move |app, image| {
            let same = app.artists.borrow().get(index).is_some_and(|artist| artist.name == name);
            if same && let Some(mut row) = app.artist_rows.row_data(index) {
                row.avatar = image;
                app.artist_rows.set_row_data(index, row);
            }
        });
    }

    fn load_avatar(&self, list: ArtistList, index: usize, name: &str) {
        let Some(own) = self.avatars.borrow().get(name).map(Path::to_path_buf) else {
            if let Some(mut row) = list.rows(self).row_data(index)
                && row.avatar.size().width > 0
            {
                row.avatar = Image::default();
                list.rows(self).set_row_data(index, row);
            }
            return;
        };
        // В сетке исполнителей — крупнее (иначе мелкий аватар растягивался бы).
        let points = if matches!(list, ArtistList::Library) && self.window.get_artists_grid() { GRID_PICTURE } else { 36.0 };
        let size = picture_side(points, self.window.window().scale_factor());
        let name = name.to_string();
        let source = covers::Source::Artist { own: Some(own.clone()), folder: None };
        self.show_picture((own, size, covers::Shape::Round), source, move |app, image| {
            let same = list.artists(app).borrow().get(index).is_some_and(|artist| artist.name == name);
            if same && let Some(mut row) = list.rows(app).row_data(index) {
                row.avatar = image;
                list.rows(app).set_row_data(index, row);
            }
        });
    }

    fn choose_avatar(&self) {
        let Some(name) = self.artist.borrow().as_ref().map(|artist| artist.name.clone()) else {
            return;
        };
        let lex = self.lex();
        self.choose(choose::Want::File(lex.avatar_pictures, &avatars::PICTURES), lex.avatar_dialog, move |app, picture| {
            app.make_avatar(name, picture)
        });
    }

    /// Аватар делается в фоне: большое фото декодируется долго.
    fn make_avatar(&self, name: String, picture: PathBuf) {
        let Some(base) = self.avatars.borrow().target(&name) else {
            return;
        };
        let _ = std::thread::Builder::new().name("nyanoto-avatar".into()).spawn(move || {
            let made = avatars::make(&picture, &base);
            in_ui(move |app| match made {
                Ok(file) => {
                    app.avatars.borrow_mut().set(&name, file);
                    app.avatar_changed(&name);
                }
                Err(error) => app.report(error),
            });
        });
    }

    fn remove_avatar(&self) {
        let Some(name) = self.artist.borrow().as_ref().map(|artist| artist.name.clone()) else {
            return;
        };
        let removed = self.avatars.borrow_mut().remove(&name);
        if let Err(error) = removed {
            self.report(error);
        }
        self.avatar_changed(&name);
    }

    /// Аватар сменился — обновить везде.
    fn avatar_changed(&self, name: &str) {
        self.forget_pictures(covers::Shape::Round);
        self.grid_pictures.borrow_mut().clear();
        let same = |other: &str| other.trim().to_lowercase() == name.trim().to_lowercase();
        for list in [ArtistList::Library, ArtistList::Found] {
            let found: Vec<(usize, String)> = list
                .artists(self)
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, artist)| same(&artist.name))
                .map(|(index, artist)| (index, artist.name.clone()))
                .collect();
            for (index, name) in found {
                self.load_avatar(list, index, &name);
            }
        }
        let nodes: Vec<usize> = {
            let (graph, artists) = (self.graph.borrow(), self.graph_artists.borrow());
            graph
                .as_ref()
                .map(|layout| {
                    layout
                        .nodes
                        .iter()
                        .enumerate()
                        .filter(|(_, node)| node.album.is_none() && artists.get(node.artist).is_some_and(|artist| same(&artist.name)))
                        .map(|(index, _)| index)
                        .collect()
                })
                .unwrap_or_default()
        };
        for index in nodes {
            self.load_graph_picture(index);
        }
        if self.artist.borrow().as_ref().is_some_and(|artist| same(&artist.name)) {
            self.show_artist_header();
        }
    }

    /// Заполняет список альбомов исполнителя. `false`, если он не прочитался.
    fn fill_albums(&self, artist: &Artist) -> bool {
        let Some(library) = self.library.as_ref() else {
            return false;
        };
        let albums = match library.albums(artist) {
            Ok(albums) => albums,
            Err(error) => {
                self.report(error);
                return false;
            }
        };
        let lex = self.lex();
        let view = self.view.get() + 1;
        self.view.set(view);
        // Сеткой — обложки крупнее.
        let size = if self.window.get_albums_grid() { GRID_COVER } else { LIST_COVER };
        let rows: Vec<AlbumRow> = albums
            .iter()
            .map(|album| AlbumRow {
                title: rows::album_title(album, lex).into(),
                detail: rows::album_detail(album, lex).into(),
                cover: self.cached_cover(&album.folder, size).unwrap_or_default(),
            })
            .collect();
        self.album_rows.set_vec(rows);
        for (index, album) in albums.iter().enumerate() {
            if self.cached_cover(&album.folder, size).is_some() {
                continue;
            }
            let folder = album.folder.clone();
            self.covers.request(&album.folder, size, move |pixels| {
                in_ui(move |app| {
                    let Some(image) = app.remember_cover(folder, size, pixels) else {
                        return;
                    };
                    if app.view.get() == view
                        && let Some(mut row) = app.album_rows.row_data(index)
                    {
                        row.cover = image;
                        app.album_rows.set_row_data(index, row);
                    }
                });
            });
        }
        *self.albums.borrow_mut() = albums;
        true
    }

    fn open_album(&self, index: usize) {
        let Some(album) = self.albums.borrow().get(index).cloned() else {
            return;
        };
        self.show_album(album);
    }

    fn show_album(&self, album: Album) {
        self.navigated();
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let tracks = match library.album_tracks(&album) {
            Ok(tracks) => tracks,
            Err(error) => {
                self.report(error);
                return;
            }
        };
        let lex = self.lex();
        *self.lists[LIST_ALBUM].borrow_mut() = tracks;
        let window = &self.window;
        let owner = self
            .artist
            .borrow()
            .as_ref()
            .map(|artist| artist_name(artist, lex).to_string())
            .unwrap_or_default();
        window.set_album_artist(owner.into());
        window.set_album_detail(rows::album_detail(&album, lex).into());
        window.set_album_cover(Image::default());
        self.show_cover(&album.folder, HEADER_COVER, |app, image| app.window.set_album_cover(image));
        *self.album.borrow_mut() = Some(album);
        self.render(LIST_ALBUM);
        self.show_browse(PAGE_TRACKS);
    }

    fn search(&self, text: &str) {
        self.navigated();
        let window = &self.window;
        if text.trim().is_empty() {
            self.show_browse(self.browse_page.get());
            return;
        }
        if self.library.is_none() {
            return;
        }
        // Исполнители ищутся здесь же: список уже в памяти.
        let matcher = nyanoto_library::TextMatcher::new(text);
        let artists: Vec<Artist> =
            self.artists.borrow().iter().filter(|artist| matcher.matches(&artist.name)).take(30).cloned().collect();
        let lex = self.lex();
        self.found_artist_rows
            .set_vec(artists.iter().map(|artist| artist_row(artist, lex)).collect::<Vec<_>>());
        *self.found_artists.borrow_mut() = artists;
        self.load_avatars(ArtistList::Found);
        // Треки — в потоке поиска, через 100 мс после последней буквы.
        let id = self.search_id.get() + 1;
        self.search_id.set(id);
        let text = text.to_string();
        self.search_timer.start(TimerMode::SingleShot, SEARCH_DELAY, move || {
            with_app(|app| {
                if app.search_id.get() == id {
                    app.searcher.find(id, &text);
                }
            });
        });
        if window.get_page() <= PAGE_TRACKS {
            self.browse_page.set(window.get_page());
        }
        window.set_crumb(lex.search.into());
        window.set_page(PAGE_SEARCH);
    }

    /// Треки, найденные потоком поиска. Ответ на устаревший запрос не показывается.
    fn search_found(&self, found: searcher::Found) {
        if found.id != self.search_id.get() || self.window.get_page() != PAGE_SEARCH {
            return;
        }
        *self.lists[LIST_SEARCH].borrow_mut() = found.tracks;
        self.render(LIST_SEARCH);
    }

    // ───────────────────────── списки треков ─────────────────────────

    fn track_at(&self, list: usize, index: usize) -> Option<Track> {
        self.lists.get(list)?.borrow().get(index).cloned()
    }

    /// Перерисовывает строки списка: после лайка, смены трека и т. п.
    fn render(&self, list: usize) {
        let lex = self.lex();
        let liked = self.liked.borrow();
        let is_liked = |id: i64| liked.contains(&id);
        let playing = self.current.borrow().as_ref().map(|track| track.path.clone());
        let album = self.album.borrow();
        let tracks = self.lists[list].borrow();
        let context = RowContext {
            lex,
            // Альбом — так, как разложена его папка.
            grouping: match album.as_ref() {
                Some(album) if list == LIST_ALBUM => album.grouping,
                _ => self.grouping.get(),
            },
            album_folder: (list == LIST_ALBUM).then(|| album.as_ref().map(|album| album.folder.as_path())).flatten(),
            multi_disc: list == LIST_ALBUM && tracks.iter().any(|track| track.disc_no.unwrap_or(1) > 1),
            playing: playing.as_deref(),
            liked: &is_liked,
            badge: &|track| self.badge(track),
        };
        let counts = self.stat_counts.borrow();
        let rows: Vec<TrackRow> = tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                let extra = (list == LIST_STATS)
                    .then(|| counts.get(index).map(|times| lex.count(*times, Noun::Time)))
                    .flatten();
                track_row(track, &context, extra)
            })
            .collect();
        self.track_rows[list].set_vec(rows);
    }

    fn render_all(&self) {
        for list in [LIST_ALBUM, LIST_SEARCH, LIST_PLAYLIST, LIST_STATS] {
            self.render(list);
        }
        let current = self.current.borrow().as_ref().map(|track| track.id);
        self.window
            .set_now_liked(current.is_some_and(|id| self.liked.borrow().contains(&id)));
        self.refresh_now_badge();
    }

    /// Появился `.lrc` — значок текста сразу, без пересканирования.
    fn mark_lyrics(&self, id: i64) {
        if let Some(library) = self.library.as_ref()
            && let Err(error) = library.set_lyrics_file(id, true)
        {
            self.report(error);
            return;
        }
        for list in &self.lists {
            for track in list.borrow_mut().iter_mut().filter(|track| track.id == id) {
                track.lyrics = true;
            }
        }
        for track in self.queue.borrow_mut().values_mut().filter(|track| track.id == id) {
            track.lyrics = true;
        }
        if let Some(track) = self.current.borrow_mut().as_mut().filter(|track| track.id == id) {
            track.lyrics = true;
        }
        self.render_all();
    }

    fn track_clicked(&self, list: usize, index: usize) {
        match list {
            LIST_ALBUM => self.play_list(LIST_ALBUM, index, QueueKind::Album),
            LIST_SEARCH => self.play_found(index),
            LIST_PLAYLIST => self.play_list(LIST_PLAYLIST, index, QueueKind::Playlist),
            _ => self.play_list(LIST_STATS, index, QueueKind::List),
        }
    }

    fn play_list(&self, list: usize, start: usize, kind: QueueKind) {
        let tracks = self.lists[list].borrow().clone();
        self.play(tracks, start, kind);
        if list == LIST_PLAYLIST && self.open_list.get() == OpenList::Daily {
            self.daily.queue.set(true);
            self.daily_shuffle_hint();
        }
    }

    fn play(&self, tracks: Vec<Track>, start: usize, kind: QueueKind) {
        if tracks.is_empty() {
            return;
        }
        let mode = self.shuffle.get();
        let shuffle = mode != smart::Shuffle::Off && tracks.len() > 1;
        // Исходный порядок — чтобы вернуть его, если перемешивание выключат.
        *self.shuffle_original.borrow_mut() = shuffle.then(|| tracks.iter().map(|track| track.path.clone()).collect());
        let (tracks, start) = if shuffle {
            (smart::shuffled(tracks, start, mode, self.next_random()), 0)
        } else {
            (tracks, start)
        };
        let paths: Vec<PathBuf> = tracks.iter().map(|track| track.path.clone()).collect();
        *self.chosen.borrow_mut() = paths.get(start).cloned();
        self.leaving.set(self.listening.borrow().is_some());
        self.daily.queue.set(false);
        *self.queue.borrow_mut() = tracks
            .into_iter()
            .map(|track| (track.path.clone(), track))
            .collect();
        self.queue_kind.set(kind);
        self.show_repeat();
        // Пользователь сам выбрал, что играть: цепочка альбомов начинается заново.
        self.chain.borrow_mut().clear();
        self.album_trail.borrow_mut().clear();
        self.remember_position();
        self.engine.play_list(paths, start);
    }

    /// Найденный трек играет вместе со своим альбомом.
    fn play_found(&self, index: usize) {
        let Some(track) = self.track_at(LIST_SEARCH, index) else {
            return;
        };
        let album = self.library.as_ref().and_then(|library| {
            let album = library.album_of(&track, self.grouping.get()).ok().flatten()?;
            let tracks = library.album_tracks(&album).ok()?;
            let start = tracks.iter().position(|item| item.id == track.id)?;
            Some((tracks, start))
        });
        match album {
            Some((tracks, start)) => self.play(tracks, start, QueueKind::Album),
            None => self.play(vec![track], 0, QueueKind::List),
        }
    }

    fn toggle_like(&self, track: i64) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let liked = !self.liked.borrow().contains(&track);
        if let Err(error) = library.set_liked(track, liked) {
            self.report(error);
            return;
        }
        if liked {
            self.liked.borrow_mut().insert(track);
        } else {
            self.liked.borrow_mut().remove(&track);
        }
        // В открытом «Любимом» трек сразу исчезает или появляется.
        if self.window.get_page() == PAGE_PLAYLIST && self.open_list.get() == OpenList::Liked {
            self.load_open_playlist();
        }
        self.render_all();
    }

    // ─────────────────────────── плейлисты ───────────────────────────

    /// Строки списка плейлистов: первым идёт «Любимые».
    fn refresh_playlists(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let lex = self.lex();
        let playlists = library.playlists().unwrap_or_default();
        let mut rows = vec![rows::playlist_row(lex.favorites, self.liked.borrow().len() as u32, lex)];
        rows.extend(playlists.iter().map(|playlist| {
            let mut row = rows::playlist_row(&playlist.name, playlist.tracks, lex);
            if playlist.smart {
                row.detail = lex.smart_label.into();
            }
            row
        }));
        self.playlist_rows.set_vec(rows);
        *self.playlists.borrow_mut() = playlists;
    }

    fn list_at(&self, index: usize) -> Option<(OpenList, String)> {
        if index == 0 {
            return Some((OpenList::Liked, self.lex().favorites.to_string()));
        }
        let playlists = self.playlists.borrow();
        let playlist = playlists.get(index - 1)?;
        Some((OpenList::Playlist(playlist.id), playlist.name.clone()))
    }

    fn open_playlist(&self, index: usize) {
        let Some((list, name)) = self.list_at(index) else {
            return;
        };
        self.navigated();
        self.open_list.set(list);
        self.load_open_playlist();
        let window = &self.window;
        window.set_playlist_deletable(list != OpenList::Liked);
        window.set_crumb(name.into());
        window.set_page(PAGE_PLAYLIST);
    }

    fn load_open_playlist(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let rules = match self.open_list.get() {
            OpenList::Playlist(id) => library.smart_rules(id).ok().flatten(),
            OpenList::Liked | OpenList::Daily => None,
        };
        self.window.set_playlist_smart(rules.is_some());
        self.window.set_playlist_daily(self.open_list.get() == OpenList::Daily);
        let (positions, tracks): (Vec<i64>, Vec<Track>) = match (self.open_list.get(), rules) {
            (_, Some(rules)) => (Vec::new(), library.smart_tracks(&rules, self.next_random()).unwrap_or_default()),
            (OpenList::Liked, None) => (Vec::new(), library.liked_tracks().unwrap_or_default()),
            (OpenList::Playlist(id), None) => library.playlist_tracks(id).unwrap_or_default().into_iter().unzip(),
            (OpenList::Daily, None) => (Vec::new(), self.daily.tracks.borrow().clone()),
        };
        *self.positions.borrow_mut() = positions;
        *self.lists[LIST_PLAYLIST].borrow_mut() = tracks;
        self.render(LIST_PLAYLIST);
    }

    fn create_playlist(&self, name: &str) -> Option<i64> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let library = self.library.as_ref()?;
        match library.create_playlist(name) {
            Ok(id) => {
                self.window.set_new_playlist_name(SharedString::new());
                self.refresh_playlists();
                Some(id)
            }
            Err(error) => {
                self.report(error);
                None
            }
        }
    }

    fn delete_playlist(&self) {
        if let (OpenList::Playlist(id), Some(library)) = (self.open_list.get(), self.library.as_ref()) {
            if let Err(error) = library.delete_playlist(id) {
                self.report(error);
            }
            self.go(PAGE_PLAYLISTS);
        }
    }

    fn remove_from_list(&self, list: usize, index: usize) {
        if list != LIST_PLAYLIST {
            return;
        }
        match self.open_list.get() {
            OpenList::Liked => {
                if let Some(track) = self.track_at(list, index) {
                    self.toggle_like(track.id);
                }
            }
            OpenList::Daily => {}
            OpenList::Playlist(id) => {
                let position = self.positions.borrow().get(index).copied();
                if let (Some(position), Some(library)) = (position, self.library.as_ref()) {
                    if let Err(error) = library.remove_from_playlist(id, position) {
                        self.report(error);
                    }
                    self.load_open_playlist();
                }
            }
        }
    }

    /// Окно «+»: в очередь (если `queue_options`) или в плейлист.
    fn open_picker(&self, tracks: Vec<Track>, queue_options: bool) {
        *self.picking.borrow_mut() = tracks;
        self.refresh_playlists();
        self.window.set_picker_queue_options(queue_options);
        self.window.set_picker_open(true);
    }

    fn pick(&self, index: usize) {
        if let Some((list, _)) = self.list_at(index) {
            self.add_picked(list);
        }
    }

    fn add_picked(&self, list: OpenList) {
        let tracks: Vec<i64> = std::mem::take(&mut *self.picking.borrow_mut()).iter().map(|track| track.id).collect();
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let result = match list {
            OpenList::Liked => tracks.iter().try_for_each(|track| {
                self.liked.borrow_mut().insert(*track);
                library.set_liked(*track, true)
            }),
            OpenList::Playlist(id) => library.add_to_playlist(id, &tracks),
            OpenList::Daily => Ok(()),
        };
        self.window.set_picker_open(false);
        match result {
            Ok(()) => self.window.set_status(self.lex().added.into()),
            Err(error) => self.report(error),
        }
        self.refresh_playlists();
        if self.window.get_page() == PAGE_PLAYLIST && self.open_list.get() == list {
            self.load_open_playlist();
        }
        self.render_all();
    }


    /// Как разложен трек: так, как выбрано для его папки музыки.
    fn track_grouping(&self, track: &Track) -> Grouping {
        match self.grouping.get() {
            Grouping::PerRoot => self
                .library
                .as_ref()
                .and_then(|library| library.track_grouping(track.id).ok())
                .unwrap_or(Grouping::Folders),
            other => other,
        }
    }

    /// Название трека и где он лежит — для панели плеера и системного виджета.
    fn now_labels(&self, track: Option<&Track>, path: &Path) -> (String, String) {
        let lex = self.lex();
        let grouping = track.map_or(Grouping::Folders, |track| self.track_grouping(track));
        match (track, grouping) {
            (Some(track), Grouping::Folders | Grouping::PerRoot) => (file_name(&track.path), rows::folders_of(track, lex)),
            (Some(track), Grouping::Tags) => (
                track.title.clone().unwrap_or_else(|| file_name(&track.path)),
                rows::tags_of(track, lex),
            ),
            (Some(track), Grouping::Composers) => {
                let title = rows::movement_title(track);
                let title = match track.movement_no {
                    Some(number) => format!("{}. {title}", rows::roman(number)),
                    None => title,
                };
                (title, rows::classical_of(track, lex))
            }
            (None, _) => (file_name(path), String::new()),
        }
    }


    /// Исполнитель и альбом играющего трека — для ссылок.
    fn now_parts(&self, track: &Track) -> (String, String) {
        let lex = self.lex();
        match self.track_grouping(track) {
            Grouping::Folders | Grouping::PerRoot => (
                track.folder_artist.clone().unwrap_or_else(|| lex.unknown_artist_folder.into()),
                track.folder_album.clone().unwrap_or_default(),
            ),
            Grouping::Tags => (
                track.artist.clone().unwrap_or_else(|| lex.unknown_artist_tag.into()),
                track.album.clone().unwrap_or_else(|| lex.unknown_album_tag.into()),
            ),
            Grouping::Composers => (
                track.composer.clone().unwrap_or_else(|| lex.unknown_composer.into()),
                track.work.clone().or_else(|| track.album.clone()).unwrap_or_else(|| lex.unknown_work.into()),
            ),
        }
    }

    /// Открыть исполнителя (или альбом) играющего трека.
    fn open_now(&self, album_too: bool) {
        let (Some(track), Some(library)) = (self.current.borrow().clone(), self.library.as_ref()) else {
            return;
        };
        let album = library.album_of(&track, self.grouping.get()).ok().flatten();
        let owner = match (&album, self.track_grouping(&track)) {
            (Some(album), _) => album.owners.first().cloned().unwrap_or_default(),
            (None, Grouping::Folders | Grouping::PerRoot) => track.folder_artist.clone().unwrap_or_default(),
            (None, Grouping::Tags) => track.owner.clone().unwrap_or_default(),
            (None, Grouping::Composers) => track.composer.clone().unwrap_or_default(),
        }
        .to_lowercase();
        let artist = self
            .artists
            .borrow()
            .iter()
            .find(|artist| artist.name.to_lowercase() == owner || artist.names.iter().any(|name| name.to_lowercase() == owner))
            .cloned();
        let Some(artist) = artist else {
            self.window.set_status(self.lex().artist_missing.into());
            return;
        };
        self.close_full();
        self.window.set_query(SharedString::new());
        self.open_artist(artist);
        if album_too
            && let Some(album) = album
        {
            let position = self.albums.borrow().iter().position(|item| item.title == album.title);
            if let Some(position) = position {
                self.open_album(position);
            }
        }
    }

    // ─────────────────────── полноэкранный плеер ───────────────────────

    fn toggle_full(&self) {
        if self.window.get_full_open() {
            self.close_full();
            return;
        }
        if self.current.borrow().is_none() {
            return;
        }
        self.window.set_full_open(true);
        // Телефон: над тёмным полным плеером значки строки состояния светлые.
        #[cfg(target_os = "android")]
        crate::android::set_dark_bars(true);
        self.show_full();
    }

    /// Закрыть полный плеер и отдать память его картинок.
    fn close_full(&self) {
        if !self.window.get_full_open() {
            return;
        }
        self.window.set_full_open(false);
        #[cfg(target_os = "android")]
        crate::android::set_dark_bars(self.window.global::<Theme>().get_dark());
        self.window.set_full_drag(0.0);
        self.window.set_full_dragging(false);
        // Крупная обложка и размытый фон нужны, пока плеер сворачивается.
        Timer::single_shot(Duration::from_millis(450), || {
            with_app(|app| {
                if !app.window.get_full_open() {
                    app.window.set_full_cover(Image::default());
                    app.window.set_full_background(Image::default());
                }
            });
        });
        if self.window.window().is_fullscreen() {
            self.window.window().set_fullscreen(false);
        }
        self.window.set_os_fullscreen(false);
        self.window.invoke_focus_keys();
    }

    fn toggle_os_fullscreen(&self) {
        let on = !self.window.window().is_fullscreen();
        self.window.window().set_fullscreen(on);
        self.window.set_os_fullscreen(on);
    }

    /// Обложка, фон, «дальше» и текст для полноэкранного плеера.
    fn show_full(&self) {
        let Some(track) = self.current.borrow().clone() else {
            return;
        };
        let Some(folder) = track.path.parent().map(Path::to_path_buf) else {
            return;
        };
        let side = self.full_cover_side();
        let path = track.path.clone();
        let current = move |app: &App| app.current.borrow().as_ref().is_some_and(|track| track.path == path);
        let ready = self.full_ready.borrow().as_ref().filter(|(known, known_side, _)| *known == folder && *known_side == side).map(|(.., image)| image.clone());
        match ready {
            Some(image) => self.window.set_full_cover(image),
            None => {
                let key = folder.clone();
                self.show_picture((folder.clone(), side, covers::Shape::Fit), covers::Source::Album(folder.clone()), move |app, image| {
                    if current(app) {
                        *app.full_ready.borrow_mut() = Some((key, side, image.clone()));
                        if app.window.get_full_open() {
                            app.window.set_full_cover(image);
                        }
                    }
                });
            }
        }
        self.full_used.set(true);
        self.prepare_backdrop();
        self.show_full_next();
        self.window.set_full_lyrics(self.lyrics_on.get() && self.lyrics.borrow().is_some());
        if self.lyrics_visible() {
            self.show_lyrics();
            self.schedule_line();
        }
    }

    /// Размытый фон полного плеера — заранее и в размер окна, чтобы не вставал посреди полёта.
    fn prepare_backdrop(&self) {
        let Some(folder) = self.current.borrow().as_ref().and_then(|track| track.path.parent().map(Path::to_path_buf)) else {
            return;
        };
        let screen = self.window.window().size();
        let (width, height) = (screen.width.clamp(64, 8192), screen.height.clamp(64, 8192));
        let ready = self.backdrop.borrow().as_ref().filter(|(known, w, h, _)| *known == folder && *w == width && *h == height).map(|(.., image)| image.clone());
        if let Some(image) = ready {
            if self.window.get_full_open() {
                self.window.set_full_background(image);
            }
            return;
        }
        let key = folder.clone();
        self.show_picture((folder.clone(), width, covers::Shape::Screen(height)), covers::Source::Album(folder), move |app, image| {
            let current = app.current.borrow().as_ref().and_then(|track| track.path.parent().map(Path::to_path_buf));
            if current.as_deref() != Some(key.as_path()) {
                return;
            }
            *app.backdrop.borrow_mut() = Some((key, width, height, image.clone()));
            if app.window.get_full_open() {
                app.window.set_full_background(image);
            }
        });
    }

    /// Крупная обложка — тоже заранее.
    fn prepare_full_cover(&self) {
        let Some(folder) = self.current.borrow().as_ref().and_then(|track| track.path.parent().map(Path::to_path_buf)) else {
            return;
        };
        let side = self.full_cover_side();
        if self.full_ready.borrow().as_ref().is_some_and(|(known, known_side, _)| *known == folder && *known_side == side) {
            return;
        }
        let key = folder.clone();
        self.show_picture((folder.clone(), side, covers::Shape::Fit), covers::Source::Album(folder), move |app, image| {
            let current = app.current.borrow().as_ref().and_then(|track| track.path.parent().map(Path::to_path_buf));
            if current.as_deref() == Some(key.as_path()) {
                *app.full_ready.borrow_mut() = Some((key, side, image));
            }
        });
    }

    /// Сторона крупной обложки: высота окна без шапки и нижних строк (~330 точек).
    fn full_cover_side(&self) -> u32 {
        let window = self.window.window();
        let (width, height) = (window.size().width, window.size().height);
        let free = height.saturating_sub((330.0 * window.scale_factor()) as u32).min(width);
        free.clamp(256, FULL_COVER).next_multiple_of(8)
    }

    /// «Дальше: …» — следующий элемент очереди (с повтором — первый).
    fn show_full_next(&self) {
        let items = self.engine.queue();
        let index = self.engine.current_id().and_then(|id| items.iter().position(|item| item.id == id));
        let next = match index {
            Some(index) if index + 1 < items.len() => items.get(index + 1),
            Some(_) if self.repeat.get() == Repeat::All => items.first(),
            _ => None,
        };
        let text = next
            .map(|item| {
                let track = self.queue.borrow().get(&item.path).cloned();
                let (title, _) = self.now_labels(track.as_ref(), &item.path);
                match track.map(|track| self.now_parts(&track).0) {
                    Some(owner) if !owner.is_empty() => format!("{title} — {owner}"),
                    _ => title,
                }
            })
            .unwrap_or_default();
        self.window.set_full_next(text.into());
    }

    // ─────────────────────── журнал «что играло» ───────────────────────

    /// Страница очереди: вкладка «Дальше» или «История».
    fn render_queue_page(&self) {
        if self.window.get_queue_tab() == 1 {
            self.render_history();
        } else {
            self.render_queue();
        }
    }

    fn render_history(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let history = library.history(HISTORY_SHOWN).unwrap_or_default();
        let lex = self.lex();
        let today = clock::local_day(clock::now());
        let liked = self.liked.borrow();
        let is_liked = |id: i64| liked.contains(&id);
        let context = RowContext {
            lex,
            grouping: self.grouping.get(),
            album_folder: None,
            multi_disc: false,
            playing: None,
            liked: &is_liked,
            badge: &|_| (String::new(), 0, false),
        };
        let mut last_day = None;
        let rows: Vec<HistoryRow> = history
            .iter()
            .map(|(track, at)| {
                let day = clock::local_day(*at);
                let heading = if last_day == Some(day) {
                    String::new()
                } else if day == today {
                    lex.today.into()
                } else if day == today - 1 {
                    lex.yesterday.into()
                } else {
                    rows::date(*at)
                };
                last_day = Some(day);
                let row = track_row(track, &context, None);
                HistoryRow {
                    day: heading.into(),
                    time: clock::time_of_day(*at).into(),
                    title: row.title,
                    detail: row.detail,
                    duration: row.duration,
                }
            })
            .collect();
        self.history_rows.set_vec(rows);
        *self.history_tracks.borrow_mut() = history.into_iter().map(|(track, _)| track).collect();
    }

    /// Трек из истории играет сейчас же; очередь не теряется — он встаёт за текущим.
    fn history_play(&self, index: usize) {
        let Some(track) = self.history_tracks.borrow().get(index).cloned() else {
            return;
        };
        self.queue.borrow_mut().insert(track.path.clone(), track.clone());
        self.queue_kind.set(QueueKind::List);
        self.chain.borrow_mut().clear();
        self.show_repeat();
        *self.chosen.borrow_mut() = Some(track.path.clone());
        self.leaving.set(self.listening.borrow().is_some());
        self.engine.play_now(vec![track.path]);
    }

    // ───────────────────────────── очередь ─────────────────────────────

    /// В очередь — следующими или в конец. Ничего не играет — очередь с них, без звука.
    fn enqueue(&self, tracks: Vec<Track>, next: bool) {
        let Some(first) = tracks.first().cloned() else {
            return;
        };
        let count = tracks.len();
        let paths: Vec<PathBuf> = tracks.iter().map(|track| track.path.clone()).collect();
        let idle = self.engine.state() == State::Stopped && self.engine.current_id().is_none();
        {
            let mut queue = self.queue.borrow_mut();
            if idle {
                queue.clear();
            }
            queue.extend(tracks.into_iter().map(|track| (track.path.clone(), track)));
        }
        if idle {
            self.engine.cue(paths, 0, Duration::ZERO);
            self.show_cued(&first, 0.0);
        } else if next {
            self.engine.insert_next(paths);
        } else {
            self.engine.append(paths);
        }
        // Очередь собрана руками: это уже не альбом, и цепочка альбомов не продолжается.
        self.queue_kind.set(QueueKind::List);
        self.chain.borrow_mut().clear();
        self.show_repeat();
        let lex = self.lex();
        let what = if count == 1 { self.now_labels(Some(&first), &first.path).0 } else { lex.count(count as u32, Noun::Track) };
        let label = if next { lex.queued_next } else { lex.queued };
        self.window.set_status(format!("{label}: {what}").into());
    }

    /// Показывает в панели подготовленный, но ещё не играющий трек.
    fn show_cued(&self, track: &Track, seconds: f32) {
        let window = &self.window;
        let lex = self.lex();
        let (title, place) = self.now_labels(Some(track), &track.path);
        let (owner, album) = self.now_parts(track);
        let duration = track.duration_ms.map_or(0.0, |ms| ms.max(0) as f32 / 1000.0);
        window.set_now_title(title.into());
        window.set_now_artist(place.into());
        window.set_now_owner(owner.into());
        window.set_now_album(album.into());
        window.set_now_quality(rows::quality_label(&quality(track), lex).into());
        window.set_now_liked(self.liked.borrow().contains(&track.id));
        window.set_has_track(true);
        window.set_duration(duration);
        window.set_duration_text(rows::clock(duration).into());
        window.set_position(seconds);
        window.set_position_text(rows::clock(seconds).into());
        window.set_now_cover(Image::default());
        if let Some(folder) = track.path.parent() {
            self.show_cover(folder, BAR_COVER, |app, image| {
                app.look_cover_image(&image);
                app.window.set_now_cover(image);
            });
        }
        *self.current.borrow_mut() = Some(track.clone());
        *self.shown_cue.borrow_mut() = Some(track.path.clone());
        // Текст подготовленного трека — сразу, а не только когда он заиграет.
        self.load_lyrics();
        self.render_all();
        self.update_system_media();
        if window.get_full_open() {
            self.show_full();
        }
    }

    /// Очередь поменялась: счётчик, страница, панель, сохранение.
    fn queue_changed(&self) {
        self.queue_dirty.set(true);
        let items = self.engine.queue();
        // Считаем только поставленное руками и ещё не сыгранное.
        let current = self.engine.current_id().and_then(|id| items.iter().position(|item| item.id == id));
        let ahead = items.iter().skip(current.map_or(0, |index| index + 1)).filter(|item| item.manual).count();
        self.window.set_queue_count(ahead as i32);
        if self.window.get_page() == PAGE_QUEUE {
            self.render_queue();
        }
        if self.window.get_full_open() {
            self.show_full_next();
        }
        // «Следующий» без звука сдвинул подготовленный трек.
        if self.engine.state() == State::Stopped
            && let Some(id) = self.engine.current_id()
            && let Some(item) = items.iter().find(|item| item.id == id)
            && self.shown_cue.borrow().as_deref() != Some(item.path.as_path())
        {
            let track = self.queue.borrow().get(&item.path).cloned();
            if let Some(track) = track {
                self.show_cued(&track, 0.0);
            }
        }
        self.save_queue_soon();
    }

    fn render_queue(&self) {
        let items = self.engine.queue();
        let current = self.engine.current_id();
        let current_index = current.and_then(|id| items.iter().position(|item| item.id == id));
        let lex = self.lex();
        let liked = self.liked.borrow();
        let is_liked = |id: i64| liked.contains(&id);
        let context = RowContext {
            lex,
            grouping: self.grouping.get(),
            album_folder: None,
            multi_disc: false,
            playing: None,
            liked: &is_liked,
            badge: &|_| (String::new(), 0, false),
        };
        let queue = self.queue.borrow();
        let (mut total_ms, mut left_ms) = (0u64, 0u64);
        let rows: Vec<QueueRow> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let track = queue.get(&item.path);
                let ms = track.and_then(|track| track.duration_ms).unwrap_or(0).max(0) as u64;
                total_ms += ms;
                if current_index.is_none_or(|current| index >= current) {
                    left_ms += ms;
                }
                let (title, detail, duration) = match track {
                    Some(track) => {
                        let row = track_row(track, &context, None);
                        (row.title, row.detail, row.duration)
                    }
                    None => (file_name(&item.path).into(), SharedString::new(), SharedString::new()),
                };
                QueueRow {
                    number: (index + 1).to_string().into(),
                    title,
                    detail,
                    duration,
                    current: Some(index) == current_index,
                    past: current_index.is_some_and(|current| index < current),
                }
            })
            .collect();
        self.queue_rows.set_vec(rows);
        *self.queue_ids.borrow_mut() = items.iter().map(|item| item.id).collect();
        let summary = if items.is_empty() {
            String::new()
        } else {
            format!(
                "{} · {} · {} {}",
                lex.count(items.len() as u32, Noun::Track),
                rows::duration_words(total_ms, lex),
                lex.queue_left,
                rows::duration_words(left_ms, lex)
            )
        };
        self.window.set_queue_summary(summary.into());
    }

    fn toggle_queue_page(&self) {
        if self.window.get_page() == PAGE_QUEUE {
            self.leave_queue();
            return;
        }
        self.navigated();
        // Очередь открывается вместо полноэкранного плеера.
        self.close_full();
        self.queue_before.set(self.window.get_page());
        self.render_queue_page();
        self.window.set_crumb(self.lex().queue.into());
        self.window.set_page(PAGE_QUEUE);
    }

    /// Назад со страницы очереди — туда, откуда её открыли.
    fn leave_queue(&self) {
        match self.queue_before.get() {
            PAGE_SEARCH => {
                self.window.set_crumb(self.lex().search.into());
                self.window.set_page(PAGE_SEARCH);
            }
            PAGE_QUEUE => self.go(PAGE_ARTISTS),
            page => self.go(page),
        }
    }

    fn queue_id(&self, index: usize) -> Option<u64> {
        self.queue_ids.borrow().get(index).copied()
    }

    fn queue_jump(&self, index: usize) {
        if let Some(id) = self.queue_id(index) {
            if let Some(item) = self.engine.queue().into_iter().find(|item| item.id == id) {
                *self.chosen.borrow_mut() = Some(item.path);
            }
            self.leaving.set(self.listening.borrow().is_some());
            self.engine.jump_to(id);
        }
    }

    fn queue_remove(&self, index: usize) {
        if let Some(id) = self.queue_id(index) {
            self.engine.remove(id);
        }
    }

    fn queue_move(&self, index: usize, step: i32) {
        let len = self.queue_ids.borrow().len() as i64;
        let target = index as i64 + i64::from(step);
        if let Some(id) = self.queue_id(index)
            && (0..len).contains(&target)
        {
            self.engine.move_item(id, target as usize);
        }
    }

    /// Убирает из очереди всё, кроме того, что играет (или ждёт «играть»).
    fn queue_clear(&self) {
        match self.engine.current_id() {
            Some(current) => {
                for id in self.queue_ids.borrow().iter().filter(|id| **id != current) {
                    self.engine.remove(*id);
                }
            }
            None => self.engine.clear_queue(),
        }
    }

    /// Очередь — в плейлист: то же окно «+», только без пунктов очереди.
    fn queue_save(&self) {
        let tracks: Vec<Track> = {
            let queue = self.queue.borrow();
            self.engine.queue().iter().filter_map(|item| queue.get(&item.path).cloned()).collect()
        };
        if !tracks.is_empty() {
            self.open_picker(tracks, false);
        }
    }

    /// Сохранить очередь: целиком, если она менялась, иначе только место в ней.
    fn save_queue_soon(&self) {
        self.queue_timer.start(TimerMode::SingleShot, QUEUE_SAVE_DELAY, || with_app(|app| app.save_queue_now()));
    }

    /// Очередь и место в треке — до следующего запуска. Саму очередь пишем, только если менялась.
    fn save_queue_now(&self) {
        self.queue_timer.stop();
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let current = self.engine.current_id();
        if !self.queue_dirty.replace(false) {
            let position = if current.is_some() { (self.window.get_position().max(0.0) * 1000.0) as u64 } else { 0 };
            let index = self.engine.queue_position().0.unwrap_or(0);
            if let Err(error) = library.save_queue_place(index, position) {
                self.report(error);
            }
            return;
        }
        let (mut ids, mut current_index) = (Vec::new(), 0);
        {
            let queue = self.queue.borrow();
            for item in self.engine.queue() {
                if let Some(track) = queue.get(&item.path) {
                    if Some(item.id) == current {
                        current_index = ids.len();
                    }
                    ids.push(track.id);
                }
            }
        }
        let position = if current.is_some() { (self.window.get_position().max(0.0) * 1000.0) as u64 } else { 0 };
        let kind = match self.queue_kind.get() {
            _ if self.daily.queue.get() => "daily",
            QueueKind::Album => "album",
            QueueKind::Playlist => "playlist",
            QueueKind::List => "list",
        };
        if let Err(error) = library.save_queue(&ids, current_index, position, kind) {
            self.report(error);
        }
    }

    /// Очередь прошлого запуска: подготовлена с того же места, звук — по «играть».
    fn restore_queue(&self) {
        let Some(saved) = self.library.as_ref().and_then(|library| library.saved_queue().ok()) else {
            return;
        };
        let Some(current) = saved.tracks.get(saved.current).cloned() else {
            return;
        };
        self.queue_kind.set(match saved.kind.as_deref() {
            Some("album") => QueueKind::Album,
            Some("playlist" | "daily") => QueueKind::Playlist,
            _ => QueueKind::List,
        });
        self.daily.queue.set(saved.kind.as_deref() == Some("daily"));
        self.show_repeat();
        let paths: Vec<PathBuf> = saved.tracks.iter().map(|track| track.path.clone()).collect();
        *self.queue.borrow_mut() = saved.tracks.into_iter().map(|track| (track.path.clone(), track)).collect();
        self.engine.cue(paths, saved.current, Duration::from_millis(saved.position_ms));
        self.show_cued(&current, saved.position_ms as f32 / 1000.0);
    }


    // ─────────────────── медиаклавиши и виджет системы ───────────────────

    /// Включает или выключает связь с системой по настройке.
    fn apply_system_media(&self) {
        let wanted = self.window.get_system_media();
        if !wanted {
            self.media.borrow_mut().take();
            return;
        }
        if self.media.borrow().is_some() {
            return;
        }
        let act: system_media::Act = Arc::new(|action| in_ui(move |app| app.media_action(action)));
        match system_media::SystemMedia::start(self.window.window(), act) {
            Ok(media) => {
                *self.media.borrow_mut() = Some(media);
                self.update_system_media();
            }
            Err(error) => self.window.set_status(format!("{}: {error}", self.lex().system_media_failed).into()),
        }
    }

    fn system_media_active(&self) -> bool {
        self.media.borrow().as_ref().is_some_and(system_media::SystemMedia::active)
    }

    /// Рассказывает системе, что играет: трек, состояние, позиция, громкость.
    fn update_system_media(&self) {
        let media = self.media.borrow();
        let Some(media) = media.as_ref() else {
            return;
        };
        let track = self.current.borrow().clone();
        // Маленькая обложка для системы; новая — в фоне, придёт следующим обновлением.
        if let (Some(folder), Some(dir)) = (track.as_ref().and_then(|track| track.path.parent()), self.system_art_dir.as_ref())
            && self.system_art.borrow().as_ref().is_none_or(|(known, _)| known.as_path() != folder)
        {
            *self.system_art.borrow_mut() = Some((folder.to_path_buf(), None));
            let (folder, dir) = (folder.to_path_buf(), dir.clone());
            let _ = std::thread::Builder::new().name("nyanoto-media-art".into()).spawn(move || {
                crate::quiet::enter_background();
                let art = covers::system_thumbnail(&folder, &dir);
                in_ui(move |app| {
                    let current = app.system_art.borrow().as_ref().is_some_and(|(known, _)| *known == folder);
                    if current && art.is_some() {
                        *app.system_art.borrow_mut() = Some((folder, art));
                        app.update_system_media();
                    }
                });
            });
        }
        let playback = match self.engine.state() {
            State::Playing => system_media::Playback::Playing,
            State::Paused => system_media::Playback::Paused,
            State::Stopped => system_media::Playback::Stopped,
        };
        let current = self.engine.current_id();
        let (index, length) = self.engine.queue_position();
        let mut now = system_media::Now {
            playback,
            track: current.filter(|_| track.is_some()),
            length: f64::from(self.window.get_duration()),
            position: self.engine.position().map_or(f64::from(self.window.get_position()), |position| position.as_secs_f64()),
            rate: self.engine.speed(),
            volume: f64::from(self.window.get_volume()),
            repeat: self.repeat.get(),
            can_next: index.is_some_and(|index| index + 1 < length || self.repeat.get() == Repeat::All || self.queue_kind.get() == QueueKind::Album),
            can_previous: index.is_some(),
            ..Default::default()
        };
        if let Some(track) = &track {
            let (title, _) = self.now_labels(Some(track), &track.path);
            now.title = title;
            now.artist = track
                .artist
                .clone()
                .or_else(|| track.album_artist.clone())
                .or_else(|| track.folder_artist.clone())
                .unwrap_or_default();
            now.album = track.album.clone().or_else(|| track.folder_album.clone()).unwrap_or_default();
            // Маленькая копия обложки, когда она готова (см. covers::system_thumbnail).
            now.art = self.system_art.borrow().as_ref().filter(|(folder, _)| Some(folder.as_path()) == track.path.parent()).and_then(|(_, art)| art.clone());
        }
        media.update(now);
    }

    fn media_seeked(&self, seconds: f32) {
        if let Some(media) = self.media.borrow().as_ref() {
            media.seeked(f64::from(seconds.max(0.0)));
        }
    }

    fn media_action(&self, action: system_media::Action) {
        use system_media::Action;
        match action {
            Action::PlayPause => self.engine.toggle(),
            Action::Play => self.engine.play(),
            Action::Pause => self.engine.pause(),
            Action::Stop => self.engine.stop(),
            Action::Next => self.go_next(),
            Action::Previous => self.go_previous(),
            Action::SeekBy(seconds) => self.seek_by(seconds as f32),
            Action::SeekTo(seconds) => {
                let seconds = seconds.max(0.0) as f32;
                self.window.set_position(seconds);
                self.window.set_position_text(rows::clock(seconds).into());
                self.engine.seek(Duration::from_secs_f32(seconds));
                self.media_seeked(seconds);
            }
            Action::Volume(volume) => {
                self.muted.set(None);
                self.apply_volume(volume as f32);
            }
            Action::Repeat(repeat) => self.set_repeat(repeat),
            Action::Raise => {
                let _ = self.window.show();
                #[cfg(not(target_os = "android"))]
                {
                    use slint::winit_030::WinitWindowAccessor;
                    self.window.window().with_winit_window(|window| window.focus_window());
                }
            }
            Action::Quit => {
                let _ = slint::quit_event_loop();
            }
        }
    }

    // ─────────────────────────── горячие клавиши ───────────────────────────

    /// Нажатие, которое не забрало поле ввода. Буквы — и в русской раскладке.
    fn hotkey(&self, text: &str, control: bool, shift: bool) -> bool {
        use slint::platform::Key;
        let is = |key: Key| text.chars().eq(std::iter::once(char::from(key)));
        if is(Key::LeftArrow) || is(Key::RightArrow) {
            let forward = is(Key::RightArrow);
            match (control, forward) {
                (true, true) => self.go_next(),
                (true, false) => self.go_previous(),
                (false, _) => {
                    let step = if shift { SEEK_LONG_STEP } else { SEEK_STEP };
                    self.seek_by(if forward { step } else { -step });
                }
            }
            return true;
        }
        if control {
            return false;
        }
        if is(Key::UpArrow) || is(Key::DownArrow) {
            self.change_volume(if is(Key::UpArrow) { VOLUME_STEP } else { -VOLUME_STEP });
            return true;
        }
        match text {
            " " => self.engine.toggle(),
            "m" | "M" | "ь" | "Ь" => self.toggle_mute(),
            "l" | "L" | "д" | "Д" => {
                let current = self.current.borrow().as_ref().map(|track| track.id);
                if let Some(id) = current {
                    self.toggle_like(id);
                }
            }
            "q" | "Q" | "й" | "Й" => self.toggle_queue_page(),
            _ => return false,
        }
        true
    }

    /// Медиаклавиша в окне; если системные включены, она придёт и оттуда — второй раз не надо.
    fn media_key(&self, key: keys::MediaKey) {
        if self.system_media_active() {
            return;
        }
        match key {
            keys::MediaKey::PlayPause => self.engine.toggle(),
            keys::MediaKey::Play => self.engine.play(),
            keys::MediaKey::Pause => self.engine.pause(),
            keys::MediaKey::Stop => self.engine.stop(),
            keys::MediaKey::Next => self.go_next(),
            keys::MediaKey::Previous => self.go_previous(),
        }
    }

    fn seek_by(&self, seconds: f32) {
        if !self.window.get_has_track() {
            return;
        }
        let now = self.engine.position().map_or(self.window.get_position(), |position| position.as_secs_f32());
        let target = (now + seconds).max(0.0);
        self.window.set_position(target.min(self.window.get_duration()));
        self.window.set_position_text(rows::clock(target.min(self.window.get_duration())).into());
        self.engine.seek(Duration::from_secs_f32(target));
        self.media_seeked(target);
    }

    /// Перемотка к строке, чуть раньше начала.
    fn seek_lyric_line(&self, index: i32) {
        let at = self.lyrics.borrow().as_ref().and_then(|lyrics| lyrics.lines.get(index.max(0) as usize).and_then(|line| line.at));
        let (Some(at), true) = (at, self.window.get_has_track()) else {
            return;
        };
        let target = at.saturating_sub(LYRIC_SEEK_LEAD).as_secs_f32();
        self.window.set_position(target.min(self.window.get_duration()));
        self.window.set_position_text(rows::clock(target.min(self.window.get_duration())).into());
        self.engine.seek(Duration::from_secs_f32(target));
        self.media_seeked(target);
        // Отметка строки — сразу, а не к следующей строке (на паузе таймер стоит).
        Timer::single_shot(Duration::from_millis(80), || {
            with_app(|app| {
                app.show_lyrics();
                app.schedule_line();
            })
        });
    }

    fn change_volume(&self, step: f32) {
        self.muted.set(None);
        self.apply_volume((self.window.get_volume() + step).clamp(0.0, 1.0));
    }

    fn toggle_mute(&self) {
        match self.muted.take() {
            Some(volume) => self.apply_volume(volume),
            None => {
                self.muted.set(Some(self.window.get_volume()));
                self.apply_volume(0.0);
            }
        }
    }

    /// Громкость не с ползунка: ползунок двигается вслед, громкость запоминается.
    fn apply_volume(&self, volume: f32) {
        self.window.set_volume(volume);
        self.set_volume(volume);
        self.update_system_media();
        self.volume_timer.start(TimerMode::SingleShot, VOLUME_SETTLE, || with_app(|app| app.save_volume()));
    }

    // ───────────────────────── воспроизведение ─────────────────────────

    fn save_volume(&self) {
        self.volume_timer.stop();
        self.save_setting(SETTING_VOLUME, &self.window.get_volume().to_string());
    }

    fn set_volume(&self, volume: f32) {
        // Квадрат — чтобы ползунок был равномерным на слух.
        self.engine.set_volume(volume * volume);
        self.show_signal_path();
    }

    fn cycle_repeat(&self) {
        let next = match self.repeat.get() {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
        self.set_repeat(next);
    }

    fn set_repeat(&self, repeat: Repeat) {
        self.repeat.set(repeat);
        self.engine.set_repeat(repeat);
        self.show_repeat();
        self.update_system_media();
    }

    fn show_repeat(&self) {
        let lex = self.lex();
        let mode = match self.repeat.get() {
            Repeat::Off => lex.repeat_off,
            Repeat::One => lex.repeat_track,
            Repeat::All => match self.queue_kind.get() {
                QueueKind::Album => lex.queue_album,
                QueueKind::Playlist => lex.queue_playlist,
                QueueKind::List => lex.queue_list,
            },
        };
        // Выключенный переключатель — просто название, включённый — с состоянием.
        let text = if self.repeat.get() == Repeat::Off { lex.repeat.to_string() } else { format!("{}: {mode}", lex.repeat) };
        self.window.set_repeat_text(text.into());
        self.window.set_repeat_on(self.repeat.get() != Repeat::Off);
        self.window.set_repeat_mode(match self.repeat.get() {
            Repeat::Off => 0,
            Repeat::One => 1,
            Repeat::All => 2,
        });
    }

    fn drain_events(&self) {
        let lex = self.lex();
        // Очередь перерисовывается один раз за пачку событий, а не на каждое.
        let mut queue_changed = false;
        while let Ok(event) = self.engine.events().try_recv() {
            match event {
                Event::TrackStarted(now) => {
                    queue_changed = true;
                    self.track_started(now);
                    // Трек мог открыться через FFmpeg — тогда он только что загрузился.
                    self.show_ffmpeg_status();
                }
                Event::StateChanged(state) => self.state_changed(state),
                Event::TrackFailed { path, error, .. } => {
                    self.window
                        .set_status(format!("{} {}: {}", lex.cannot_play, file_name(&path), rows::track_error(&error, lex)).into());
                }
                Event::TrackDamaged { path, .. } => {
                    self.window
                        .set_status(format!("{}: {}", file_name(&path), lex.damaged).into());
                }
                Event::OutputFailed(message) => {
                    self.window
                        .set_status(format!("{}: {message}", lex.output_failed).into());
                }
                Event::OutputLost => self.window.set_status(lex.output_lost.into()),
                Event::PausedForDevice => self.window.set_status(lex.device_paused.into()),
                Event::ExclusiveUnavailable(message) => {
                    self.window
                        .set_status(format!("{}: {message}", lex.exclusive_unavailable).into());
                }
                Event::Finished => {
                    if self.sleep.get() == Sleep::AfterTrack {
                        self.set_sleep(Sleep::Off);
                    }
                    self.queue_finished();
                }
                Event::OutputOpened { device, rate, exclusive_bits, .. } => {
                    *self.output_info.borrow_mut() = Some(signal::OutputInfo { device: device.clone(), rate, exclusive_bits });
                    self.device_opened(device);
                    self.show_signal_path();
                }
                Event::QueueChanged => queue_changed = true,
            }
        }
        if queue_changed {
            self.queue_changed();
            self.update_system_media();
        }
    }

    fn track_started(&self, now: NowPlaying) {
        self.finish_listening();
        self.leaving.set(false);
        self.daily_tick();
        let lex = self.lex();
        let track = self.queue.borrow().get(&now.path).cloned();
        let window = &self.window;
        let seconds = now.info.duration.map_or(0.0, |duration| duration.as_secs_f32());
        *self.shown_cue.borrow_mut() = None;

        let (title, place) = self.now_labels(track.as_ref(), &now.path);
        let (owner, album) = track.as_ref().map(|track| self.now_parts(track)).unwrap_or_default();
        window.set_now_owner(owner.into());
        window.set_now_album(album.into());
        if let (Some(track), Some(library)) = (track.as_ref(), self.library.as_ref()) {
            if let Err(error) = library.add_history(track.id) {
                self.report(error);
            }
            if window.get_page() == PAGE_QUEUE && window.get_queue_tab() == 1 {
                self.render_history();
            }
        }
        window.set_now_title(title.into());
        window.set_now_artist(place.into());
        match &track {
            Some(track) => {
                let quality = quality(track);
                window.set_now_quality(rows::quality_label(&quality, lex).into());
                window.set_now_liked(self.liked.borrow().contains(&track.id));
                *self.listening.borrow_mut() =
                    Some(Listening { track: track.id, seconds: 0.0, duration: seconds, flags: self.listening_flags(&now.path) });
            }
            None => {
                window.set_now_quality(now.info.codec.clone().into());
                window.set_now_liked(false);
            }
        }
        window.set_has_track(track.is_some());
        window.set_duration(seconds);
        window.set_duration_text(rows::clock(seconds).into());
        window.set_position(0.0);
        window.set_position_text(rows::clock(0.0).into());
        window.set_status(SharedString::new());
        self.daily_hint_on_start();

        window.set_now_cover(Image::default());
        if let Some(folder) = now.path.parent() {
            self.show_cover(folder, BAR_COVER, |app, image| {
                app.look_cover_image(&image);
                app.window.set_now_cover(image);
            });
        }
        *self.current.borrow_mut() = track;
        if self.full_used.get() {
            self.prepare_backdrop();
            self.prepare_full_cover();
        }
        self.resume_long_record();
        self.show_signal_path();
        self.load_wavebar();
        self.load_chapters();
        // Отрезок A–B относится к прошлому треку.
        self.set_ab((None, None));
        self.load_lyrics();
        self.render_all();
        if window.get_editor_open() {
            self.render_editor();
        }
        if window.get_full_open() {
            self.show_full();
        }
    }

    fn state_changed(&self, state: State) {
        self.window.set_playing(state == State::Playing);
        self.save_queue_soon();
        self.update_system_media();
        if state == State::Playing {
            self.start_ticker();
        } else {
            self.ticker.stop();
            self.update_position();
        }
        if state != State::Playing {
            self.remember_position();
        }
        if state == State::Stopped {
            self.finish_listening();
        }
        if self.window.get_editor_open() {
            self.render_editor();
        }
    }

    fn update_position(&self) {
        let seconds = self.engine.position().map_or(0.0, |position| position.as_secs_f32());
        if let Some(listening) = self.listening.borrow_mut().as_mut() {
            listening.seconds = listening.seconds.max(seconds);
        }
        // Отрезок A–B: дошли до конца — назад к началу.
        if let (Some(start), Some(end)) = self.ab.get()
            && seconds >= end
        {
            self.engine.seek(Duration::from_secs_f32(start));
        }
        if self.lyrics_visible() {
            self.show_lyrics();
        }
        if self.window.get_editor_open() {
            self.tick_editor();
        }
        if self.window.get_chapters_open() {
            self.show_chapters();
        }
        if self.position_saved.get().is_none_or(|saved| saved.elapsed() >= POSITION_SAVE_EVERY) {
            self.remember_position();
        }
        self.schedule_line();
        // Пока ползунок тянут, не дёргаем его из-под руки.
        if self.window.get_seeking() {
            return;
        }
        self.window.set_position(seconds);
        self.window.set_position_text(rows::clock(seconds).into());
    }

    /// Таймер полосы перемотки; чаще — при A–B и в редакторе текста.
    fn start_ticker(&self) {
        let fast = matches!(self.ab.get(), (Some(_), Some(_))) || self.window.get_editor_open();
        let interval = Duration::from_millis(if fast { 50 } else { 250 });
        self.ticker.start(TimerMode::Repeated, interval, || {
            with_app(|app| app.update_position());
        });
    }

    // ───────────────────────── таймер сна и A–B ─────────────────────────

    /// Меню сна: открыть или закрыть.
    fn toggle_sleep_menu(&self) {
        if self.window.get_sleep_menu_open() {
            self.window.set_sleep_menu_open(false);
            return;
        }
        let now = clock::now();
        let target = match self.sleep.get() {
            Sleep::Until(deadline, _) => now + deadline.saturating_duration_since(Instant::now()).as_secs() as i64,
            _ => {
                let now_minute = (now + clock::offset_at(now)).rem_euclid(86_400) / 60;
                now + sleep::snap(now_minute, SLEEP_DEFAULT_MINUTES) * 60
            }
        };
        // Цель — ровно на минуте.
        let target = target - (target + clock::offset_at(target)).rem_euclid(60);
        self.sleep_target.set(target.max(now + 60));
        self.sleep_raw.set(((self.sleep_target.get() - now) / 60) as f32);
        self.window.set_sleep_menu_open(true);
        self.show_sleep_menu();
        self.sleep_timer_kick();
    }

    /// Минута суток сейчас (местная) и сколько минут до цели.
    fn sleep_plan(&self) -> (i64, i64) {
        let now = clock::now();
        let now_minute = (now + clock::offset_at(now)).rem_euclid(86_400) / 60;
        let left = (self.sleep_target.get() - now + 59).div_euclid(60);
        (now_minute, sleep::clamp_minutes(left))
    }

    /// Начало текущей минуты (секунды Unix).
    fn minute_start() -> i64 {
        let now = clock::now();
        now - (now + clock::offset_at(now)).rem_euclid(60)
    }

    /// Новая цель «во сколько»: не раньше чем через минуту, не позже чем через сутки.
    fn set_sleep_target(&self, target: i64) {
        let start = Self::minute_start();
        let target = target.clamp(start + sleep::MIN_MINUTES * 60, start + sleep::MAX_MINUTES * 60);
        self.sleep_target.set(target);
        self.sleep_raw.set(((target - start) / 60) as f32);
        self.show_sleep_menu();
    }

    /// Новое «через сколько» (минуты от начала текущей минуты).
    fn set_sleep_minutes(&self, minutes: i64) {
        self.sleep_target.set(Self::minute_start() + sleep::clamp_minutes(minutes) * 60);
        self.show_sleep_menu();
    }

    /// Палец на круге: нажатие ставит ручку под него, движение ведёт её дальше.
    fn sleep_dial(&self, x: f32, y: f32, start: bool) {
        let angle = sleep::angle_of(x, y);
        let (now_minute, minutes) = self.sleep_plan();
        let minutes = if start {
            let minutes = sleep::press(now_minute, minutes, angle);
            self.sleep_raw.set(minutes as f32);
            minutes
        } else {
            let raw = sleep::drag(self.sleep_raw.get(), self.sleep_angle.get(), angle);
            self.sleep_raw.set(raw);
            sleep::snap(now_minute, raw.round() as i64)
        };
        self.sleep_angle.set(angle);
        self.set_sleep_minutes(minutes);
    }

    /// Меню сна: часы цели, «сегодня/завтра», круг и сколько это.
    fn show_sleep_menu(&self) {
        if !self.window.get_sleep_menu_open() {
            return;
        }
        let lex = self.lex();
        let now = clock::now();
        let (now_minute, minutes) = self.sleep_plan();
        let target = self.sleep_target.get();
        let local = (target + clock::offset_at(target)).rem_euclid(86_400);
        let window = &self.window;
        window.set_sleep_hours(format!("{:02}", local / 3600).into());
        window.set_sleep_minutes(format!("{:02}", local % 3600 / 60).into());
        let tomorrow = clock::local_day(target) > clock::local_day(now);
        window.set_sleep_day((if tomorrow { lex.sleep_tomorrow } else { lex.sleep_today }).into());
        window.set_sleep_plan(rows::duration_words(minutes as u64 * 60_000, lex).into());
        window.set_sleep_arc(sleep::arc(now_minute, minutes).into());
        let ring = if minutes > 12 * 60 { sleep::INNER_RING } else { sleep::RING };
        let (x, y) = sleep::point(sleep::clock_angle(now_minute + minutes), ring);
        window.set_sleep_handle_x(x);
        window.set_sleep_handle_y(y);
        let (x, y) = sleep::point(sleep::clock_angle(now_minute), sleep::RING);
        window.set_sleep_now_x(x);
        window.set_sleep_now_y(y);
    }

    fn set_sleep(&self, sleep: Sleep) {
        self.sleep.set(sleep);
        self.engine.set_stop_after_current(sleep == Sleep::AfterTrack);
        if !matches!(sleep, Sleep::Until(..)) {
            self.restore_sleep_volume();
        }
        self.show_sleep();
        self.sleep_timer_kick();
    }

    /// Таймер сна: раз в секунду, в последние 30 с чаще (затухание). Не нужен — стоит.
    fn sleep_timer_kick(&self) {
        let interval = match self.sleep.get() {
            Sleep::Until(deadline, _) if deadline.saturating_duration_since(Instant::now()) <= SLEEP_FADE + Duration::from_secs(1) => {
                Duration::from_millis(100)
            }
            Sleep::Until(..) => Duration::from_secs(1),
            _ if self.window.get_sleep_menu_open() => Duration::from_secs(1),
            _ => {
                self.sleep_timer.stop();
                return;
            }
        };
        if self.sleep_timer.running() && self.sleep_timer.interval() == interval {
            return;
        }
        self.sleep_timer.start(TimerMode::Repeated, interval, || with_app(|app| app.sleep_tick()));
    }

    fn sleep_tick(&self) {
        if let Sleep::Until(deadline, _) = self.sleep.get() {
            let left = deadline.saturating_duration_since(Instant::now());
            let playing = self.engine.state() == State::Playing;
            if left.is_zero() {
                if playing {
                    self.engine.pause();
                }
                self.set_sleep(Sleep::Off);
                return;
            }
            if left <= SLEEP_FADE && playing {
                // Затихает по громкости на слух: доля ползунка, потом квадрат.
                let share = left.as_secs_f32() / SLEEP_FADE.as_secs_f32();
                let volume = self.window.get_volume() * share;
                self.engine.set_volume(volume * volume);
                self.sleep_fading.set(true);
            }
            self.show_sleep();
        }
        self.show_sleep_menu();
        self.sleep_timer_kick();
    }

    /// Музыка затихала — громкость обратно, как на ползунке.
    fn restore_sleep_volume(&self) {
        if self.sleep_fading.replace(false) {
            self.set_volume(self.window.get_volume());
        }
    }

    fn show_sleep(&self) {
        let lex = self.lex();
        let (state, left) = match self.sleep.get() {
            Sleep::Off => (lex.repeat_off.to_string(), String::new()),
            Sleep::AfterTrack => (lex.sleep_after.to_string(), String::new()),
            Sleep::Until(deadline, _) => {
                let left = deadline.saturating_duration_since(Instant::now()).as_secs();
                let short = if left >= 3600 {
                    format!("{}:{:02}:{:02}", left / 3600, left % 3600 / 60, left % 60)
                } else {
                    format!("{}:{:02}", left / 60, left % 60)
                };
                (format!("{} {}", left.div_ceil(60), lex.minutes), short)
            }
        };
        let text = if self.sleep.get() == Sleep::Off { lex.sleep.to_string() } else { format!("{}: {state} ({left})", lex.sleep) };
        self.window.set_sleep_text(text.into());
        // Сколько осталось — кольцом вокруг луны: размер кнопки не меняется.
        let ring = match self.sleep.get() {
            Sleep::Until(deadline, minutes) => {
                let total = (f64::from(minutes.max(1)) * 60.0) as f32;
                sleep::ring(deadline.saturating_duration_since(Instant::now()).as_secs_f32() / total)
            }
            _ => String::new(),
        };
        self.window.set_sleep_ring(ring.into());
        self.window.set_sleep_on(self.sleep.get() != Sleep::Off);
        self.window.set_sleep_running(matches!(self.sleep.get(), Sleep::Until(..)));
        self.window.set_sleep_after_track(self.sleep.get() == Sleep::AfterTrack);
    }

    fn cycle_ab(&self) {
        let now = self.engine.position().map_or(0.0, |position| position.as_secs_f32());
        let next = match self.ab.get() {
            (None, _) => (Some(now), None),
            (Some(start), None) if now > start + 0.2 => (Some(start), Some(now)),
            (Some(_), None) => (None, None),
            (Some(_), Some(_)) => (None, None),
        };
        self.set_ab(next);
    }

    fn set_ab(&self, ab: (Option<f32>, Option<f32>)) {
        let was_looping = matches!(self.ab.get(), (Some(_), Some(_)));
        self.ab.set(ab);
        let looping = matches!(ab, (Some(_), Some(_)));
        if was_looping != looping {
            if self.window.get_playing() {
                self.start_ticker();
            }
            self.apply_crossfade();
        }
        self.show_ab();
    }

    fn show_ab(&self) {
        let lex = self.lex();
        let text = match self.ab.get() {
            (None, _) => "A–B".to_string(),
            (Some(_), None) => lex.ab_wait.to_string(),
            (Some(_), Some(_)) => lex.ab_on.to_string(),
        };
        self.window.set_ab_text(text.into());
        self.window.set_ab_on(self.ab.get().0.is_some());
        self.window.set_ab_stage(match self.ab.get() {
            (None, _) => 0,
            (Some(_), None) => 1,
            (Some(_), Some(_)) => 2,
        });
    }

    // ───────────────────────── ReplayGain и тексты ─────────────────────────

    fn apply_loudness(&self) {
        let loudness = match (self.replaygain.get(), self.replaygain_album.get()) {
            (false, _) => Loudness::Off,
            (true, false) => Loudness::Track,
            (true, true) => Loudness::Album,
        };
        self.engine.set_loudness(loudness);
        self.show_signal_path();
    }

    /// Ищет текст играющего трека, если модуль текстов включён.
    fn load_lyrics(&self) {
        let path = self.current.borrow().as_ref().map(|track| track.path.clone());
        let lyrics = path.filter(|_| self.lyrics_on.get()).and_then(|path| lyrics::load(&path));
        // `.lrc`, положенный рядом уже после сканирования, — сразу в значок.
        let unmarked = self
            .current
            .borrow()
            .as_ref()
            .filter(|track| !track.lyrics && ["lrc", "LRC"].iter().any(|extension| track.path.with_extension(extension).is_file()))
            .map(|track| track.id);
        if let Some(id) = unmarked {
            self.mark_lyrics(id);
        }
        *self.lyrics.borrow_mut() = lyrics;
        // Строки нового текста строятся заново при показе.
        self.lyrics_rows.borrow_mut().take();
        self.stage.borrow_mut().clear();
        self.stage_paused.set(None);
        let salt = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64);
        self.stage_salt.set(salt);
        self.window.set_full_lyrics(self.lyrics_on.get() && self.lyrics.borrow().is_some());
        if self.lyrics_visible() {
            self.show_lyrics();
            self.schedule_line();
        }
    }

    /// Текст песни где-то виден: в окне текста или колонкой полноэкранного плеера.
    fn lyrics_visible(&self) -> bool {
        self.window.get_lyrics_open() || (self.window.get_full_open() && self.window.get_full_lyrics())
    }

    /// Таймер ровно на начало следующей строки (тик полосы давал опоздание до 250 мс).
    fn schedule_line(&self) {
        // Сцена встала (пауза, перемотка) — будим.
        if !self.stage_timer.running() {
            self.stage_tick();
        }
        let lyrics_open = self.lyrics_visible();
        let editor_open = self.window.get_editor_open();
        let position = self.engine.position().map(|position| position.as_millis() as i64);
        let (Some(position), true, true) = (position, self.window.get_playing(), lyrics_open || editor_open) else {
            self.line_timer.stop();
            return;
        };
        let mut next: Option<i64> = None;
        if lyrics_open && let Some(lyrics) = self.lyrics.borrow().as_ref() {
            // Подсветка раньше на сдвиг × скорость.
            let lead = self.lyrics_lead_track_ms();
            next = lyrics
                .lines
                .iter()
                .filter_map(|line| line.at)
                .map(|at| at.as_millis() as i64 - lead)
                .find(|at| *at > position);
        }
        if editor_open && self.editor_track_is_current() && let Some(editor) = self.editor.borrow().as_ref() {
            let soonest = editor.draft.lines.iter().filter_map(|line| line.at).filter(|at| *at > position).min();
            next = match (next, soonest) {
                (Some(one), Some(other)) => Some(one.min(other)),
                (one, other) => one.or(other),
            };
        }
        let Some(at) = next else {
            self.line_timer.stop();
            return;
        };
        let speed = self.engine.speed().max(0.01);
        let wait = Duration::from_millis((((at - position) as f64) / speed).max(1.0) as u64);
        self.line_timer.start(TimerMode::SingleShot, wait, || {
            with_app(|app| {
                if app.lyrics_visible() {
                    app.show_lyrics();
                }
                if app.window.get_editor_open() {
                    app.tick_editor();
                }
                app.schedule_line();
            });
        });
    }

    /// Какой вид текста на экране: колонка (0) или сцены ([`LOOK_STAGE`]).
    fn lyrics_look(&self) -> i32 {
        if self.lyrics_style.get() == 1 && self.stage_fallback.get().is_none() { LOOK_STAGE } else { 0 }
    }

    /// Какие сцены показывать.
    fn stage_choice(&self) -> stage::Choice {
        match self.stage_scene.get() {
            0 => stage::Choice::Song,
            1 => stage::Choice::Moment,
            scene => stage::Choice::Only(stage::Kind::ALL[(scene - 2).min(stage::Kind::ALL.len() - 1)]),
        }
    }

    // ─────────────── возврат «Современных» на «Обычные» ───────────────

    /// Нужен ли замер: видны «Современные» и включён хоть один возврат.
    fn load_wanted(&self) -> bool {
        self.lyrics_visible()
            && self.lyrics_style.get() == 1
            && (self.stage_fallback_load.get() || self.stage_fallback_power.get())
    }

    /// Замер нагрузки раз в 2 с, пока видны сцены.
    fn watch_load(&self) {
        if !self.load_wanted() || self.load_timer.running() {
            return;
        }
        self.load_watch.borrow_mut().reset();
        self.load_timer.start(TimerMode::Repeated, LOAD_EVERY, || with_app(|app| app.load_tick()));
        self.watch_power();
    }

    /// Энергосбережение — раз в 10 с в своём потоке (D-Bus бывает медленным).
    fn watch_power(&self) {
        let wanted = self.load_wanted() && self.stage_fallback_power.get();
        // `swap`: поток уже есть — ему достаточно узнать, что он нужен.
        if !wanted || self.power_wanted.swap(true, Ordering::AcqRel) {
            return;
        }
        let (saving, wanted) = (self.power_saving.clone(), self.power_wanted.clone());
        let spawned = std::thread::Builder::new().name("nyanoto-power".into()).spawn(move || {
            while wanted.load(Ordering::Acquire) {
                saving.store(load::power_saving().unwrap_or(false), Ordering::Release);
                std::thread::sleep(POWER_EVERY);
            }
        });
        if spawned.is_err() {
            self.power_wanted.store(false, Ordering::Release);
        }
    }

    /// Обычные строки при энергосбережении или долгой нагрузке, сцены — когда спало.
    fn load_tick(&self) {
        if !self.load_wanted() {
            self.load_timer.stop();
            self.power_wanted.store(false, Ordering::Release);
            self.load_watch.borrow_mut().reset();
            self.set_stage_fallback(None);
            return;
        }
        self.watch_power();
        if !self.stage_fallback_power.get() {
            self.power_wanted.store(false, Ordering::Release);
        }
        let busy = self.stage_fallback_load.get() && self.load_watch.borrow_mut().sample(Instant::now());
        let power = self.stage_fallback_power.get() && self.power_saving.load(Ordering::Acquire);
        let reason = if power {
            Some(load::Fallback::Power)
        } else if busy {
            Some(load::Fallback::Load)
        } else {
            None
        };
        self.set_stage_fallback(reason);
    }

    fn set_stage_fallback(&self, reason: Option<load::Fallback>) {
        if self.stage_fallback.replace(reason) == reason {
            return;
        }
        let lex = self.lex();
        let text = match reason {
            Some(load::Fallback::Load) => lex.stage_fallback_now_load,
            Some(load::Fallback::Power) => lex.stage_fallback_now_power,
            None => "",
        };
        self.window.set_stage_fallback(text.into());
        self.stage.borrow_mut().clear();
        self.stage_paused.set(None);
        if self.lyrics_visible() {
            self.show_lyrics();
            self.schedule_line();
        }
    }

    /// Настроение песни: из кэша или разбором в фоне.
    fn load_stage_mood(&self) {
        let Some(path) = self.current.borrow().as_ref().map(|track| track.path.clone()) else {
            return;
        };
        if self.stage_mood.borrow().as_ref().is_some_and(|(known, _)| *known == path) {
            return;
        }
        self.stage_mood_stop.borrow().store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        *self.stage_mood_stop.borrow_mut() = stop.clone();
        // Пока разбора нет — ничего не известно (сцены идут со средним настроением).
        *self.stage_mood.borrow_mut() = Some((path.clone(), stage::feel::Mood::default()));
        let cache = self.database.as_deref().and_then(Path::parent).map(|dir| dir.join("feel"));
        let _ = std::thread::Builder::new().name("nyanoto-feel".into()).spawn(move || {
            quiet::enter_background();
            let cached = cache.as_ref().and_then(|cache| {
                let file = cache.join(wavebar::cache_key(&path, "nkf")?);
                stage::feel::from_bytes(&std::fs::read(file).ok()?)
            });
            let feel = cached.or_else(|| {
                let feel = nyanoto_core::feel(&path, &stop).ok()??;
                if let Some(cache) = &cache
                    && let Some(name) = wavebar::cache_key(&path, "nkf")
                    && std::fs::create_dir_all(cache).is_ok()
                {
                    let _ = std::fs::write(cache.join(name), stage::feel::to_bytes(&feel));
                }
                Some(feel)
            });
            let Some(feel) = feel else {
                return;
            };
            let mood = stage::feel::Mood::from_feel(&feel);
            in_ui(move |app| {
                let current = app.current.borrow().as_ref().map(|track| track.path.clone());
                if current.as_deref() != Some(path.as_path()) {
                    return;
                }
                *app.stage_mood.borrow_mut() = Some((path, mood.clone()));
                let mut stage = app.stage.borrow_mut();
                if stage.has_lyrics() {
                    stage.set_mood(mood);
                }
                drop(stage);
                app.stage_paused.set(None);
                if !app.stage_timer.running() {
                    app.stage_tick();
                }
            });
        });
    }

    /// Кадр сцены; следующий — чаще, когда движется быстро. Не видно — стоит.
    fn stage_tick(&self) {
        let window = &self.window;
        let active = self.lyrics_visible() && window.get_lyrics_synced() && self.lyrics_look() == LOOK_STAGE;
        if !active {
            self.stage_timer.stop();
            self.stage.borrow_mut().release();
            self.stage_paused.set(None);
            return;
        }
        // Свёрнутое окно рисовать незачем: проверить снова через секунду.
        if window.window().is_minimized() {
            self.stage_timer.start(TimerMode::SingleShot, Duration::from_secs(1), || with_app(|app| app.stage_tick()));
            return;
        }
        if !self.stage.borrow().has_lyrics() {
            let lyrics = self.lyrics.borrow();
            let Some(lyrics) = lyrics.as_ref() else {
                return;
            };
            self.load_stage_mood();
            let (path, genre) = self.current.borrow().as_ref().map(|track| (Some(track.path.clone()), track.genre.clone().unwrap_or_default())).unwrap_or_default();
            let mood = self.stage_mood.borrow().as_ref().filter(|(known, mood)| Some(known) == path.as_ref() && mood.measured()).map(|(_, mood)| mood.clone());
            self.stage.borrow_mut().set_lyrics(lyrics, self.stage_choice(), self.stage_salt.get(), mood, &genre);
        }
        let full = window.get_full_open() && window.get_full_lyrics();
        let (width, height) = self.stage_sizes.get()[usize::from(full)];
        let color = |color: slint::Color| tiny_skia::Color::from_rgba8(color.red(), color.green(), color.blue(), color.alpha());
        let accent = color(window.get_stage_accent());
        // В полноэкранном плеере — белым по тёмной подложке обложки.
        let palette = if full {
            stage::Palette { ink: tiny_skia::Color::from_rgba8(255, 255, 255, 235), ground: tiny_skia::Color::from_rgba8(22, 22, 22, 255), accent }
        } else {
            stage::Palette { ink: color(window.get_stage_ink()), ground: color(window.get_stage_ground()), accent }
        };
        let playing = window.get_playing();
        let speed = self.engine.speed().max(0.01);
        let position = self.engine.position().unwrap_or_default().as_secs_f64() + self.lyrics_lead_track_ms() as f64 / 1000.0;
        let time = self.stage_clock.borrow_mut().time(position, playing, speed);
        let paused = (!playing).then_some((time.to_bits(), (width, height), full));
        if paused.is_some() && self.stage_paused.get() == paused {
            return;
        }
        self.stage_paused.set(paused);
        let lead = self.lyrics_lead_track_ms() as f32 / 1000.0;
        let view = stage::View { width, height, scale: window.window().scale_factor(), palette, lead, max_fps: self.stage_fps.get() as f32 };
        let started = Instant::now();
        let rendered = self.stage.borrow_mut().render(time as f32, &view);
        let (frames, spent) = self.stage_cost.get();
        let (frames, spent) = (frames + 1, spent + started.elapsed());
        if frames == 120 {
            trace(&format!(
                "сцена: 120 кадров, рисование {:.2} мс в среднем, {width:.0}×{height:.0} × {:.2}, {}",
                spent.as_secs_f64() * 1000.0 / 120.0,
                view.scale,
                self.stage.borrow().shown(time as f32)
            ));
            self.stage_cost.set((0, Duration::ZERO));
        } else {
            self.stage_cost.set((frames, spent));
        }
        let Some((image, next)) = rendered else {
            self.stage_timer.stop();
            return;
        };
        window.set_stage_frame(image);
        if playing {
            let wait = Duration::from_secs_f64((f64::from(next) / speed).max(0.004));
            self.stage_timer.start(TimerMode::SingleShot, wait, || with_app(|app| app.stage_tick()));
        } else {
            self.stage_timer.stop();
        }
    }

    /// Сдвиг раннего появления строки на шкале времени трека.
    fn lyrics_lead_track_ms(&self) -> i64 {
        (f64::from(self.lyrics_lead.get()) * self.engine.speed()).round() as i64
    }

    /// Текст: синхронный — колонкой, обычный — сплошняком.
    fn show_lyrics(&self) {
        self.watch_load();
        let window = &self.window;
        window.set_lyrics_title(window.get_now_title());
        let lyrics = self.lyrics.borrow();
        let Some(lyrics) = lyrics.as_ref() else {
            window.set_lyrics_synced(false);
            window.set_lyrics_plain(self.lex().no_lyrics.into());
            return;
        };
        if !lyrics.synced {
            window.set_lyrics_synced(false);
            let text: Vec<&str> = lyrics.lines.iter().map(|line| line.text.as_str()).collect();
            window.set_lyrics_plain(text.join("\n").into());
            return;
        }
        // Строки строятся раз на текст, дальше меняется только текущая.
        let rows = self.lyrics_rows.borrow().clone();
        let rows = match rows {
            Some(rows) => rows,
            None => {
                let rows = Rc::new(VecModel::from(
                    lyrics.lines.iter().map(|line| LyricRow { text: line.text.as_str().into(), current: false }).collect::<Vec<_>>(),
                ));
                window.set_lyrics_lines(ModelRc::from(rows.clone()));
                *self.lyrics_rows.borrow_mut() = Some(rows.clone());
                self.lyrics_current.set(None);
                rows
            }
        };
        let position = self.engine.position().unwrap_or_default().as_millis() as i64 + self.lyrics_lead_track_ms();
        let position = Duration::from_millis(position.max(0) as u64);
        let current = lyrics.line_at(position);
        let look = self.lyrics_look();
        window.set_lyrics_look(look);
        let previous = self.lyrics_current.replace(current);
        if previous != current {
            for (index, mark) in [(previous, false), (current, true)] {
                if let Some(index) = index
                    && let Some(mut row) = rows.row_data(index)
                {
                    row.current = mark;
                    rows.set_row_data(index, row);
                }
            }
        }
        window.set_lyrics_synced(true);
        if look == LOOK_STAGE && !self.stage_timer.running() {
            self.stage_tick();
        }
    }

    // ───────────────────────── редактор текста ─────────────────────────

    /// Редактор: текст из `.lrc`, иначе из тегов, иначе пустой.
    fn open_editor(&self, track: Track) {
        let lex = self.lex();
        let window = &self.window;
        let path = lrc::lrc_path(&track.path);
        let original = std::fs::read(&path).ok();
        let mut message = "";
        let mut draft = match &original {
            Some(bytes) => {
                let (text, legacy) = nyanoto_library::decode_text(bytes);
                if legacy {
                    message = lex.lrc_cp1251;
                }
                lrc::Draft::from_text(&text)
            }
            None => lyrics::tag_text(&track.path)
                .map(|text| lrc::Draft::from_text(&text))
                .unwrap_or_default(),
        };
        draft.tap_offset = self
            .library
            .as_ref()
            .and_then(|library| library.setting(SETTING_TAP_OFFSET).ok().flatten())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        // Текст из тегов ещё не лежит в файле: его можно сразу сохранить.
        let dirty = original.is_none() && !draft.lines.is_empty();
        let step = if draft.lines.is_empty() { 0 } else { 1 };
        window.set_editor_title(file_name(&track.path).into());
        window.set_editor_tap_offset(draft.tap_offset as i32);
        window.set_editor_message(message.into());
        window.set_editor_confirm_close(false);
        window.set_editor_confirm_save(false);
        window.set_editor_can_undo(false);
        window.set_editor_dirty(dirty);
        *self.editor.borrow_mut() = Some(Editor {
            selected: draft.next.min(draft.lines.len().saturating_sub(1)),
            track,
            path,
            draft,
            raw: None,
            saved: original.clone(),
            original,
            dirty,
            current: None,
        });
        self.show_editor_step(step);
        window.set_editor_open(true);
        self.apply_speed();
        let path = self.editor.borrow().as_ref().map(|editor| editor.track.path.clone());
        if let Some(path) = path {
            self.load_wave(&path);
        }
        if window.get_playing() {
            self.start_ticker();
        }
    }

    fn close_editor(&self, discard: bool) {
        if !discard && self.editor.borrow().as_ref().is_some_and(|editor| editor.dirty) {
            self.window.set_editor_confirm_close(true);
            return;
        }
        self.editor.borrow_mut().take();
        self.editor_rows.set_vec(Vec::new());
        self.wave_marks.set_vec(Vec::new());
        self.overview_marks.set_vec(Vec::new());
        self.wave_stop.borrow().store(true, Ordering::Relaxed);
        self.window.set_editor_confirm_close(false);
        self.window.set_editor_open(false);
        self.apply_speed();
        if self.window.get_playing() {
            self.start_ticker();
        }
    }

    /// Шаги редактора. Текст с шага «Файл LRC» становится черновиком.
    fn editor_go(&self, step: i32) {
        if step == self.window.get_editor_step() {
            return;
        }
        if let Some(editor) = self.editor.borrow_mut().as_mut()
            && let Some(raw) = editor.raw.take()
        {
            let tap_offset = editor.draft.tap_offset;
            editor.draft = lrc::Draft::from_text(&raw);
            editor.draft.tap_offset = tap_offset;
            editor.selected = editor.selected.min(editor.draft.lines.len().saturating_sub(1));
        }
        self.show_editor_step(step);
    }

    fn show_editor_step(&self, step: i32) {
        if let Some(editor) = self.editor.borrow().as_ref() {
            match step {
                0 => self.window.set_editor_plain(editor.draft.plain().into()),
                2 => self.window.set_editor_raw(
                    editor.raw.clone().unwrap_or_else(|| editor.draft.to_lrc()).into(),
                ),
                _ => {}
            }
        }
        self.window.set_editor_step(step);
        self.render_editor();
    }

    /// Меняет черновик; `render` — перерисовать строки синхронизации.
    fn change_draft(&self, render: bool, change: impl FnOnce(&mut Editor)) {
        if let Some(editor) = self.editor.borrow_mut().as_mut() {
            change(editor);
            editor.dirty = true;
        }
        self.window.set_editor_dirty(true);
        if render {
            self.render_editor();
        }
    }

    /// Играет ли сейчас трек из редактора.
    fn editor_track_is_current(&self) -> bool {
        let editor = self.editor.borrow();
        let current = self.current.borrow();
        matches!((editor.as_ref(), current.as_ref()), (Some(editor), Some(current)) if editor.track.path == current.path)
    }

    /// Позиция трека из редактора, мс.
    fn editor_position(&self) -> Option<i64> {
        if !self.editor_track_is_current() {
            return None;
        }
        self.engine.position().map(|position| position.as_millis() as i64)
    }

    /// Строки синхронизации, сводка, часы и волна.
    fn render_editor(&self) {
        self.render_editor_rows();
        self.show_overview_marks();
        self.refresh_wave();
    }

    fn render_editor_rows(&self) {
        let lex = self.lex();
        let position = self.editor_position();
        self.window
            .set_editor_playing(self.window.get_playing() && self.editor_track_is_current());
        let mut editor = self.editor.borrow_mut();
        let Some(editor) = editor.as_mut() else {
            return;
        };
        let warn: HashSet<usize> = editor.draft.out_of_order().into_iter().collect();
        editor.current = position.and_then(|ms| editor.draft.current(ms));
        let rows: Vec<EditorLine> = editor
            .draft
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| EditorLine {
                time: line.at.map_or_else(|| "—".to_string(), lrc::format_time).into(),
                text: line.text.as_str().into(),
                next: index == editor.draft.next,
                selected: index == editor.selected,
                current: Some(index) == editor.current,
                warn: warn.contains(&index),
            })
            .collect();
        self.editor_rows.set_vec(rows);

        let total = editor.draft.lines.len();
        let mut summary = format!("{} {} {} {total}", lex.lrc_stamped, total - editor.draft.unstamped(), lex.of);
        if !warn.is_empty() {
            summary.push_str(&format!(" · {}: {}", lex.lrc_out_of_order, warn.len()));
        }
        let window = &self.window;
        window.set_editor_summary(summary.into());
        window.set_editor_position(position.map_or_else(|| "--:--.--".to_string(), lrc::clock).into());
        window.set_editor_focus_line(editor.draft.next.min(total.saturating_sub(1)) as i32);
        window.set_editor_dirty(editor.dirty);
        window.set_editor_can_undo(editor.original.is_some() && editor.saved != editor.original);
    }

    /// Часы, подсветка звучащей строки и бегущая волна.
    fn tick_editor(&self) {
        self.tick_editor_rows();
        self.show_wave();
    }

    /// Меняются только две строки: погасшая и загоревшаяся.
    fn tick_editor_rows(&self) {
        let position = self.editor_position();
        self.window
            .set_editor_position(position.map_or_else(|| "--:--.--".to_string(), lrc::clock).into());
        let mut editor = self.editor.borrow_mut();
        let Some(editor) = editor.as_mut() else {
            return;
        };
        let current = position.and_then(|ms| editor.draft.current(ms));
        if current == editor.current {
            return;
        }
        for (index, on) in [(editor.current, false), (current, true)] {
            if let Some(index) = index
                && let Some(mut row) = self.editor_rows.row_data(index)
            {
                row.current = on;
                self.editor_rows.set_row_data(index, row);
            }
        }
        editor.current = current;
    }

    /// При A–B без кроссфейда, иначе следующий вступит до точки B.
    fn apply_crossfade(&self) {
        let window = &self.window;
        let seconds = CROSSFADE_SECONDS
            .get(window.get_crossfade_index().max(0) as usize)
            .copied()
            .unwrap_or(5);
        let looping = matches!(self.ab.get(), (Some(_), Some(_)));
        self.engine
            .set_crossfade((window.get_crossfade() && !looping).then(|| Duration::from_secs(seconds)));
    }

    // ───────────────────────── скорость ─────────────────────────

    fn speed_text(&self, speed: f64) -> String {
        format!("{speed:.2}×").replace('.', &self.lex().decimal.to_string())
    }

    fn set_normal_speed(&self, value: f64) {
        let speed = round_speed(value, 0.25, 3.0);
        self.normal_speed.set(speed);
        self.window.set_speed(speed as f32);
        self.window.set_speed_text(self.speed_text(speed).into());
        self.apply_speed();
    }

    fn set_editor_speed(&self, value: f64) {
        let speed = round_speed(value, 0.05, 1.0);
        self.editor_speed.set(speed);
        self.window.set_editor_speed(speed as f32);
        self.window.set_editor_speed_text(self.speed_text(speed).into());
        self.apply_speed();
    }

    /// Скорость: в редакторе — своя, иначе с ползунка.
    fn apply_speed(&self) {
        let factor = if self.window.get_editor_open() {
            self.editor_speed.get()
        } else if self.speed_module.get() {
            self.normal_speed.get()
        } else {
            1.0
        };
        self.engine.set_speed(Speed { factor, keep_pitch: self.keep_pitch.get() });
        // Таймер строк считал время по старой скорости.
        self.schedule_line();
    }

    // ───────────────────────── волна редактора ─────────────────────────

    /// Огибающая трека — в своём потоке.
    fn load_wave(&self, path: &Path) {
        if self.wave.borrow().as_ref().is_some_and(|(wave, _)| wave == path) {
            self.window.set_editor_wave_ready(true);
            self.show_overview();
            self.wave_strip.set(None);
            self.refresh_wave();
            return;
        }
        self.wave.borrow_mut().take();
        self.wave_stop.borrow().store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        *self.wave_stop.borrow_mut() = stop.clone();
        self.window.set_editor_wave_ready(false);
        self.window.set_editor_wave(Image::default());
        self.window.set_editor_overview(Image::default());
        let path = path.to_path_buf();
        let spawned = std::thread::Builder::new().name("nyanoto-wave".into()).spawn(move || {
            let result = nyanoto_core::peaks(&path, &stop);
            in_ui(move |app| app.wave_ready(path, result));
        });
        if spawned.is_err() {
            self.window.set_editor_wave_ready(true);
        }
    }

    fn wave_ready(&self, path: PathBuf, result: Result<Option<Peaks>, TrackError>) {
        let editing = self.editor.borrow().as_ref().is_some_and(|editor| editor.track.path == path);
        if !editing {
            return;
        }
        match result {
            Ok(Some(peaks)) => {
                *self.wave.borrow_mut() = Some((path, Arc::new(peaks)));
                self.show_overview();
            }
            Ok(None) => return,
            Err(error) => {
                self.window
                    .set_editor_message(format!("{}: {error}", self.lex().wave_failed).into());
            }
        }
        self.window.set_editor_wave_ready(true);
        self.show_overview_marks();
        self.wave_strip.set(None);
        self.refresh_wave();
    }

    /// Длина трека для обзора: по огибающей, пока её нет — по библиотеке.
    fn wave_duration(&self) -> i64 {
        if let Some((_, peaks)) = self.wave.borrow().as_ref() {
            return peaks.duration_ms().max(1);
        }
        self.editor
            .borrow()
            .as_ref()
            .and_then(|editor| editor.track.duration_ms)
            .map_or(1, |ms| ms.max(1))
    }

    /// Середина крупной волны: место игры или метка выбранной строки.
    fn wave_center(&self) -> i64 {
        if let Some(position) = self.editor_position() {
            return position;
        }
        self.editor
            .borrow()
            .as_ref()
            .and_then(|editor| {
                let end = (editor.selected + 1).min(editor.draft.lines.len());
                editor.draft.lines[..end].iter().rev().find_map(|line| line.at)
            })
            .unwrap_or(0)
    }

    fn show_overview(&self) {
        let wave = self.wave.borrow();
        let Some((_, peaks)) = wave.as_ref() else {
            return;
        };
        let image = wave::render(peaks, 0, peaks.duration_ms().max(1), wave::WIDTH, wave::OVERVIEW_HEIGHT);
        self.window.set_editor_overview(Image::from_rgba8(image));
    }

    /// Метки всех строк на обзоре трека.
    fn show_overview_marks(&self) {
        let duration = self.wave_duration() as f32;
        let marks: Vec<WaveMark> = self.editor.borrow().as_ref().map_or_else(Vec::new, |editor| {
            editor
                .draft
                .lines
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    Some(WaveMark {
                        x: (line.at? as f32 / duration).min(1.0),
                        index: index as i32,
                        label: SharedString::new(),
                        selected: index == editor.selected,
                    })
                })
                .collect()
        });
        self.overview_marks.set_vec(marks);
    }

    /// Крупная волна на тике воспроизведения: обычно только сдвиг полосы.
    fn show_wave(&self) {
        self.update_wave(false);
    }

    /// Крупная волна после правки меток: метки пересчитываются.
    fn refresh_wave(&self) {
        self.update_wave(true);
    }

    /// Крупная волна вокруг текущего места с метками строк и место на обзоре.
    fn update_wave(&self, mut marks_changed: bool) {
        if self.drag_window.get().is_some() || !self.window.get_editor_open() {
            return;
        }
        let center = self.wave_center();
        let visible = center - WAVE_HALF_MS;
        self.window
            .set_editor_overview_position((center as f32 / self.wave_duration() as f32).clamp(0.0, 1.0));
        let strip = match self.wave_strip.get() {
            Some(strip) if visible >= strip && visible + 2 * WAVE_HALF_MS <= strip + WAVE_STRIP_MS => strip,
            _ => {
                let strip = center - WAVE_STRIP_MS / 2;
                if let Some((_, peaks)) = self.wave.borrow().as_ref() {
                    let image = wave::render(peaks, strip, strip + WAVE_STRIP_MS, wave::WIDTH * 3, wave::ZOOM_HEIGHT);
                    self.window.set_editor_wave(Image::from_rgba8(image));
                }
                self.wave_strip.set(Some(strip));
                marks_changed = true;
                strip
            }
        };
        // Сдвиг полосы в ширинах видимого окна.
        self.window
            .set_editor_wave_shift((visible - strip) as f32 / (2 * WAVE_HALF_MS) as f32);
        if !marks_changed {
            return;
        }
        let (from, to) = (strip, strip + WAVE_STRIP_MS);
        let marks: Vec<WaveMark> = self.editor.borrow().as_ref().map_or_else(Vec::new, |editor| {
            editor
                .draft
                .lines
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    let at = line.at.filter(|at| (from..=to).contains(at))?;
                    Some(WaveMark {
                        x: (at - from) as f32 / (to - from) as f32,
                        index: index as i32,
                        label: (index + 1).to_string().into(),
                        selected: index == editor.selected,
                    })
                })
                .collect()
        });
        self.wave_marks.set_vec(marks);
    }

    /// Перемотка трека из редактора; если играет другой трек — сначала включить этот.
    fn editor_seek_to(&self, ms: i64) {
        self.start_editor_track();
        self.seek_editor(ms.max(0));
    }

    /// Пробел: метка очередной строке в текущей позиции.
    fn editor_stamp(&self) {
        let Some(position) = self.editor_position() else {
            self.window.set_editor_message(self.lex().lrc_other_track.into());
            return;
        };
        let stamped = self.editor.borrow_mut().as_mut().and_then(|editor| {
            let index = editor.draft.stamp(position)?;
            editor.selected = index;
            editor.dirty = true;
            Some(index)
        });
        if stamped.is_some() {
            self.window.set_editor_message(SharedString::new());
            self.render_editor();
        }
    }

    /// Выбирает строку; с неё же продолжится разметка пробелом.
    fn editor_select(&self, index: usize) {
        if let Some(editor) = self.editor.borrow_mut().as_mut() {
            let index = index.min(editor.draft.lines.len().saturating_sub(1));
            editor.selected = index;
            editor.draft.next = index;
        }
        self.render_editor();
    }

    /// Включает трек из редактора, если играет другой.
    fn start_editor_track(&self) -> bool {
        if self.editor_track_is_current() {
            return false;
        }
        let track = self.editor.borrow().as_ref().map(|editor| editor.track.clone());
        if let Some(track) = track {
            self.play(vec![track], 0, QueueKind::List);
        }
        true
    }

    fn editor_play_pause(&self) {
        if !self.start_editor_track() {
            self.engine.toggle();
        }
    }

    fn editor_seek_by(&self, delta: i64) {
        let Some(position) = self.editor_position() else {
            return;
        };
        self.seek_editor((position + delta).max(0));
    }

    /// Перемотка; на паузе таймер стоит, поэтому часы обновляются отдельно.
    fn seek_editor(&self, ms: i64) {
        self.engine.seek(Duration::from_millis(ms.max(0) as u64));
        Timer::single_shot(Duration::from_millis(80), || with_app(|app| app.update_position()));
    }

    /// Прослушать строку с запасом.
    fn editor_play_line(&self, index: i32) {
        let target = {
            let mut editor = self.editor.borrow_mut();
            let Some(editor) = editor.as_mut() else {
                return;
            };
            let index = if index < 0 { editor.selected } else { index as usize };
            if index >= editor.draft.lines.len() {
                return;
            }
            editor.selected = index;
            editor.draft.next = index;
            let at = editor.draft.lines[..=index].iter().rev().find_map(|line| line.at).unwrap_or(0);
            (at - PREROLL_MS).max(0)
        };
        self.render_editor();
        self.start_editor_track();
        self.seek_editor(target);
        if !self.window.get_playing() {
            self.engine.play();
        }
    }

    /// «Сохранить .lrc»: сначала спросить.
    fn editor_save(&self) {
        let lex = self.lex();
        let Some(path) = self.editor.borrow().as_ref().map(|editor| editor.path.clone()) else {
            return;
        };
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let question = if path.exists() { lex.lrc_confirm_replace } else { lex.lrc_confirm_new };
        self.window.set_editor_confirm_text(format!("{question} {name}").into());
        self.window.set_editor_confirm_save(true);
    }

    /// Пишет файл. С шага «Файл LRC» — ровно как набран, иначе из черновика.
    fn editor_write(&self) {
        self.window.set_editor_confirm_save(false);
        let lex = self.lex();
        let step = self.window.get_editor_step();
        let result = {
            let mut editor = self.editor.borrow_mut();
            let Some(editor) = editor.as_mut() else {
                return;
            };
            let text = match (&editor.raw, step) {
                (Some(raw), 2) => raw.clone(),
                _ => editor.draft.to_lrc(),
            };
            lrc::save(&editor.path, text.as_bytes()).map(|()| {
                editor.saved = Some(text.into_bytes());
                editor.dirty = false;
                editor.path.clone()
            })
        };
        match result {
            Ok(path) => {
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                self.window.set_editor_message(format!("{}: {name}", lex.lrc_saved).into());
                let id = self.editor.borrow().as_ref().map(|editor| editor.track.id);
                if let Some(id) = id {
                    self.mark_lyrics(id);
                }
                self.lyrics_changed();
            }
            Err(error) => self
                .window
                .set_editor_message(format!("{}: {error}", lex.lrc_not_saved).into()),
        }
        self.render_editor();
    }

    /// «Вернуть как было»: файл, каким он был при открытии редактора.
    fn editor_undo(&self) {
        let lex = self.lex();
        let result = {
            let mut editor = self.editor.borrow_mut();
            let Some(editor) = editor.as_mut() else {
                return;
            };
            let Some(original) = editor.original.clone() else {
                return;
            };
            lrc::save(&editor.path, &original).map(|()| {
                let tap_offset = editor.draft.tap_offset;
                editor.draft = lrc::Draft::from_text(&nyanoto_library::decode_text(&original).0);
                editor.draft.tap_offset = tap_offset;
                editor.selected = editor.selected.min(editor.draft.lines.len().saturating_sub(1));
                editor.raw = None;
                editor.saved = Some(original);
                editor.dirty = false;
            })
        };
        match result {
            Ok(()) => {
                self.window.set_editor_message(lex.lrc_restored.into());
                self.lyrics_changed();
                self.show_editor_step(self.window.get_editor_step());
            }
            Err(error) => self
                .window
                .set_editor_message(format!("{}: {error}", lex.lrc_not_saved).into()),
        }
    }

    /// Файл текста изменился: играющий трек перечитывает его.
    fn lyrics_changed(&self) {
        if self.editor_track_is_current() {
            self.load_lyrics();
        }
    }

    /// Засчитать прослушивание, если слушали достаточно.
    fn finish_listening(&self) {
        let Some(listening) = self.listening.borrow_mut().take() else {
            return;
        };
        let needed = COUNTED_AFTER.min(listening.duration / 2.0).max(1.0);
        let Some(library) = self.library.as_ref().filter(|_| self.statistics.get()) else {
            return;
        };
        let mut flags = listening.flags;
        if self.sleep.get() != Sleep::Off || self.sleep_fading.get() {
            flags |= PLAY_SLEEPY;
        }
        let listened = (listening.seconds * 1000.0) as u64;
        let result = if listening.seconds >= needed {
            library.record_play_as(listening.track, listened, flags)
        } else if self.leaving.get() && listening.seconds >= SKIP_FROM {
            // Не дослушали — для плейлиста дня это «не то».
            library.record_skip(listening.track, listened, flags)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            self.report(error);
        }
    }

    /// Отметки трека: выбран руками, из плейлиста дня, под таймером сна.
    fn listening_flags(&self, path: &Path) -> u8 {
        let manual = self.engine.current_id().is_some_and(|id| self.engine.queue().iter().any(|item| item.id == id && item.manual));
        let chosen = self.chosen.borrow_mut().take().is_some_and(|chosen| chosen == path);
        let mut flags = 0;
        if chosen || manual {
            flags |= PLAY_CHOSEN;
        }
        if self.daily.queue.get() && !manual {
            flags |= PLAY_DAILY;
        }
        if self.sleep.get() != Sleep::Off {
            flags |= PLAY_SLEEPY;
        }
        flags
    }

    fn refresh_stats(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let stats = match library.listen_stats(self.grouping.get(), 20) {
            Ok(stats) => stats,
            Err(error) => {
                self.report(error);
                return;
            }
        };
        let lex = self.lex();
        let summary = vec![
            InfoRow { name: lex.plays.into(), value: stats.plays.to_string().into() },
            InfoRow { name: lex.listening_time.into(), value: rows::duration_words(stats.listened_ms, lex).into() },
            InfoRow {
                name: lex.history_since.into(),
                value: stats.since.map(rows::date).unwrap_or_else(|| lex.history_empty.into()).into(),
            },
        ];
        self.window.set_stats_summary(ModelRc::from(Rc::new(VecModel::from(summary))));
        let artists: Vec<InfoRow> = stats
            .top_artists
            .iter()
            .map(|(name, tracks, listened_ms)| InfoRow {
                name: if name.is_empty() { "—".into() } else { name.as_str().into() },
                value: format!("{} · {}", lex.count(*tracks, Noun::Track), rows::duration_words(*listened_ms, lex)).into(),
            })
            .collect();
        self.window.set_top_artists(ModelRc::from(Rc::new(VecModel::from(artists))));
        self.refresh_graph();
        let (tracks, counts): (Vec<Track>, Vec<u32>) = stats.top_tracks.into_iter().unzip();
        *self.stat_counts.borrow_mut() = counts;
        *self.lists[LIST_STATS].borrow_mut() = tracks;
        self.render(LIST_STATS);
    }

    // ───────────────────────── карта прослушиваний ─────────────────────────

    /// Карта: 30 самых слушаемых исполнителей, до 10 альбомов у каждого.
    fn refresh_graph(&self) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let artists = library.listen_graph(self.grouping.get(), 30, 10).unwrap_or_default();
        let (width, height) = self.graph_size();
        let layout = graph::Layout::new(&artists, width, height);
        let lex = self.lex();
        let rows: Vec<GraphNode> = layout
            .nodes
            .iter()
            .map(|node| {
                let artist = &artists[node.artist];
                let hue = graph::hue(node.artist);
                let (label, (r, g, b)) = match node.album {
                    None => (artist.name.clone(), graph::hsl(hue, 0.55, 0.58)),
                    Some(album) => {
                        let album = &artist.albums[album];
                        let title = album.title.clone().unwrap_or_else(|| "—".into());
                        let label = format!("{title} · {} · {}", lex.times(album.plays), rows::duration_words(album.listened_ms, lex));
                        (label, graph::hsl(hue, 0.45, 0.38))
                    }
                };
                GraphNode {
                    x: node.x,
                    y: node.y,
                    r: node.r,
                    color: slint::Color::from_rgb_u8(r, g, b),
                    letter: rows::first_letter(&artist.name),
                    label: if label.is_empty() { "—".into() } else { label.into() },
                    artist: node.album.is_none(),
                    image: Image::default(),
                }
            })
            .collect();
        let count = rows.len();
        self.graph_rows.set_vec(rows);
        self.window.set_graph_nodes(self.graph_rows.clone().into());
        self.window.set_graph_links(layout.links_path().into());
        *self.graph.borrow_mut() = Some(layout);
        *self.graph_artists.borrow_mut() = artists;
        self.graph_view.set(self.graph_view.get() + 1);
        for index in 0..count {
            self.load_graph_picture(index);
        }
        self.wake_graph();
    }

    /// Картинка узла карты ровно в размер узла.
    fn load_graph_picture(&self, index: usize) {
        let scale = self.window.window().scale_factor();
        let picture = {
            let (graph, artists) = (self.graph.borrow(), self.graph_artists.borrow());
            let Some(node) = graph.as_ref().and_then(|layout| layout.nodes.get(index)) else {
                return;
            };
            let Some(artist) = artists.get(node.artist) else {
                return;
            };
            let size = picture_side(node.r * 2.0, scale);
            match node.album.and_then(|album| artist.albums.get(album)) {
                Some(album) => Some((
                    (album.folder.clone(), size, covers::Shape::Square),
                    covers::Source::Album(album.folder.clone()),
                )),
                None => {
                    let own = self.avatars.borrow().get(&artist.name).map(Path::to_path_buf);
                    let folder = artist.folder.clone();
                    own.clone()
                        .or_else(|| folder.clone())
                        .map(|path| ((path, size, covers::Shape::Round), covers::Source::Artist { own, folder }))
                }
            }
        };
        let view = self.graph_view.get();
        let show = move |app: &App, image: Image| {
            if app.graph_view.get() == view
                && let Some(mut row) = app.graph_rows.row_data(index)
            {
                row.image = image;
                app.graph_rows.set_row_data(index, row);
            }
        };
        match picture {
            Some((key, source)) => self.show_picture(key, source, show),
            None => show(self, Image::default()),
        }
    }

    /// Размер холста; до первой раскладки окна — разумный по умолчанию.
    fn graph_size(&self) -> (f32, f32) {
        let (width, height) = (self.window.get_graph_width(), self.window.get_graph_height());
        if width > 10.0 && height > 10.0 { (width, height) } else { (800.0, 420.0) }
    }

    /// Анимация карты: 30 кадров/с; встаёт сама, когда граф успокоился.
    fn wake_graph(&self) {
        if self.graph_timer.running() {
            return;
        }
        self.graph_timer.start(TimerMode::Repeated, Duration::from_millis(33), || {
            with_app(|app| app.graph_step());
        });
    }

    fn graph_step(&self) {
        let visible = self.window.get_page() == PAGE_STATS && self.window.get_stats_view() == 0;
        let mut guard = self.graph.borrow_mut();
        let Some(layout) = guard.as_mut().filter(|_| visible) else {
            self.graph_timer.stop();
            return;
        };
        let (width, height) = self.graph_size();
        layout.resize(width, height);
        let moving = layout.tick() && layout.tick();
        for (index, node) in layout.nodes.iter().enumerate() {
            if let Some(mut row) = self.graph_rows.row_data(index) {
                row.x = node.x;
                row.y = node.y;
                self.graph_rows.set_row_data(index, row);
            }
        }
        self.window.set_graph_links(layout.links_path().into());
        if !moving {
            self.graph_timer.stop();
        }
    }

    /// Нажатие на узел карты: исполнитель открывается в «Музыке», альбом — сразу списком треков.
    fn open_graph_node(&self, index: usize) {
        let target = self.graph.borrow().as_ref().and_then(|layout| layout.nodes.get(index).map(|node| (node.artist, node.album)));
        let Some((artist_index, album)) = target else {
            return;
        };
        let (name, album_title) = {
            let artists = self.graph_artists.borrow();
            let Some(artist) = artists.get(artist_index) else {
                return;
            };
            (artist.name.clone(), album.and_then(|album| artist.albums.get(album).and_then(|album| album.title.clone())))
        };
        let artist = self.artists.borrow().iter().find(|artist| artist.name == name || artist.names.contains(&name)).cloned();
        let Some(artist) = artist else {
            return;
        };
        self.open_artist(artist);
        if album.is_some() {
            let position = self.albums.borrow().iter().position(|album| album.title == album_title);
            if let Some(position) = position {
                self.open_album(position);
            }
        }
    }

    // ───────────────────────── сведения о треке ─────────────────────────

    fn open_info(&self, track: Track) {
        let lex = self.lex();
        let window = &self.window;
        let quality = quality(&track);
        window.set_info_title(file_name(&track.path).into());
        window.set_info_quality(rows::quality_label(&quality, lex).into());
        let (badge, kind, _) = self.badge(&track);
        window.set_info_badge(badge.into());
        window.set_info_badge_kind(kind);

        let mut rows = vec![(lex.path, track.path.display().to_string())];
        if let Ok(metadata) = std::fs::metadata(&track.path) {
            let megabytes = format!("{:.1}", metadata.len() as f64 / 1_048_576.0).replace('.', &lex.decimal.to_string());
            rows.push((lex.size, format!("{megabytes} {}", lex.megabytes)));
        }
        rows.push((lex.compression, if quality.lossless { lex.lossless } else { lex.lossy }.to_string()));
        if let Some(ms) = track.duration_ms {
            rows.push((lex.duration, rows::clock(ms as f32 / 1000.0)));
        }
        if let Some(rate) = track.sample_rate {
            rows.push((lex.sample_rate, format!("{rate} {}", lex.hz)));
        }
        if let Some(bits) = track.bit_depth {
            rows.push((lex.bit_depth, format!("{bits} {}", lex.bit)));
        }
        if let Some(channels) = track.channels {
            rows.push((lex.channels, channels.to_string()));
        }
        if let Some(bitrate) = track.bitrate {
            rows.push((lex.bitrate, format!("{bitrate} {}", lex.kbps)));
        }
        if let Some(problem) = &track.problem {
            rows.push((lex.problem, problem.clone()));
        }
        let playing = self.current.borrow().as_ref().is_some_and(|current| current.path == track.path);
        if playing && let Some(path) = self.signal_path() {
            rows.push((lex.path_label, path));
        }
        if self.classical.get() {
            let movement = match (track.movement_no, &track.movement) {
                (Some(number), Some(name)) => Some(format!("{}. {name}", rows::roman(number))),
                (Some(number), None) => Some(rows::roman(number)),
                (None, name) => name.clone(),
            };
            for (name, value) in [
                (lex.composer, track.composer.clone()),
                (lex.conductor, track.conductor.clone()),
                (lex.work, track.work.clone()),
                (lex.movement, movement),
            ] {
                if let Some(value) = value {
                    rows.push((name, value));
                }
            }
        }
        if self.authenticity.get()
            && let Some(check) = self.suspicious.borrow().get(&track.id)
        {
            let verdict = match check.verdict {
                Authenticity::LossyOrigin => lex.verdict_lossy,
                Authenticity::Upsampled => lex.verdict_upsampled,
                _ => lex.verdict_padded,
            };
            rows.push((lex.authenticity_row, verdict.to_string()));
        }
        self.info_rows.set_vec(
            rows.into_iter()
                .map(|(name, value)| InfoRow { name: name.into(), value: value.into() })
                .collect::<Vec<_>>(),
        );

        let (fields, editable, kind, message) = match read_fields(&track.path) {
            Ok(tags) => {
                let message = match (tags.editable, self.advanced.get()) {
                    (true, true) => "",
                    (true, false) => lex.edit_needs_advanced,
                    (false, _) => lex.edit_flac_only,
                };
                let kind = if tags.kind.is_empty() { lex.no_tags.to_string() } else { tags.kind };
                (tags.fields, tags.editable && self.advanced.get(), kind, message.to_string())
            }
            Err(error) => (Vec::new(), false, "?".into(), format!("{}: {error}", lex.tags_unreadable)),
        };
        window.set_info_tag_kind(kind.into());
        window.set_info_editable(editable);
        window.set_info_message(message.into());
        window.set_info_confirm(false);
        window.set_info_can_undo(false);
        *self.info.borrow_mut() = Some(Info { track, original: fields.clone(), edited: fields, undo: None });
        self.show_tags();
        window.set_info_open(true);
    }

    fn close_info(&self) {
        self.info.borrow_mut().take();
        self.window.set_info_open(false);
    }

    /// Показывает теги из правок и отмечает, есть ли несохранённое.
    fn show_tags(&self) {
        let info = self.info.borrow();
        let Some(info) = info.as_ref() else {
            return;
        };
        self.tag_rows.set_vec(
            info.edited
                .iter()
                .map(|field| TagRow { key: field.key.as_str().into(), value: field.value.as_str().into() })
                .collect::<Vec<_>>(),
        );
        self.window.set_info_dirty(info.edited != info.original);
        self.window.set_info_can_undo(info.undo.is_some());
    }

    /// Правка в поле ввода. Строки не перерисовываются, чтобы не сбить курсор.
    fn edit_tag(&self, index: usize, key: Option<String>, value: Option<String>) {
        let mut info = self.info.borrow_mut();
        let Some(info) = info.as_mut() else {
            return;
        };
        if let Some(field) = info.edited.get_mut(index) {
            if let Some(key) = key {
                field.key = key;
            }
            if let Some(value) = value {
                field.value = value;
            }
        }
        self.window.set_info_dirty(info.edited != info.original);
    }

    fn change_tags(&self, change: impl FnOnce(&mut Vec<TagField>)) {
        if let Some(info) = self.info.borrow_mut().as_mut() {
            change(&mut info.edited);
        }
        self.show_tags();
    }

    fn change_tags_from_original(&self) {
        if let Some(info) = self.info.borrow_mut().as_mut() {
            info.edited = info.original.clone();
        }
        self.show_tags();
    }

    fn save_tags(&self) {
        let (path, fields, original) = {
            let info = self.info.borrow();
            let Some(info) = info.as_ref() else {
                return;
            };
            let fields: Vec<TagField> = info
                .edited
                .iter()
                .filter(|field| !field.key.trim().is_empty())
                .cloned()
                .collect();
            (info.track.path.clone(), fields, info.original.clone())
        };
        self.write_tags(&path, &fields, Some(original), self.lex().written);
    }

    fn undo_tags(&self) {
        let (path, undo) = {
            let info = self.info.borrow();
            let Some(info) = info.as_ref() else {
                return;
            };
            (info.track.path.clone(), info.undo.clone())
        };
        if let Some(previous) = undo {
            self.write_tags(&path, &previous, None, self.lex().restored);
        }
    }

    fn write_tags(&self, path: &Path, fields: &[TagField], undo: Option<Vec<TagField>>, done: &str) {
        match write_fields(path, fields) {
            Ok(()) => {
                let now = read_fields(path).map(|tags| tags.fields).unwrap_or_else(|_| fields.to_vec());
                if let Some(info) = self.info.borrow_mut().as_mut() {
                    info.original = now.clone();
                    info.edited = now;
                    info.undo = undo;
                }
                self.window.set_info_message(done.into());
                self.show_tags();
                // Библиотека увидит новые теги при пересканировании.
                self.start_scan();
            }
            Err(error) => self
                .window
                .set_info_message(format!("{}: {error}", self.lex().not_written).into()),
        }
    }

    // ───────────────────────── библиотека ─────────────────────────

    /// Выбрали папку — спросить, как её раскладывать.
    fn add_folder(&self) {
        self.choose(choose::Want::Folder, self.lex().add_folder, |app, folder| {
            app.window.set_folder_choice_path(folder.display().to_string().into());
            app.choosing_root.replace(Some(folder));
            app.window.set_folder_choice_open(true);
        });
    }

    /// 0 — по папкам, 1 — по тегам, 2 — по композиторам, меньше нуля — передумали.
    fn folder_chosen(&self, choice: i32) {
        self.window.set_folder_choice_open(false);
        let Some(folder) = self.choosing_root.take() else {
            return;
        };
        let grouping = match choice {
            0 => Grouping::Folders,
            1 => Grouping::Tags,
            2 if self.classical.get() => Grouping::Composers,
            _ => return,
        };
        // Сканер пишет в базу — папку добавим, когда он остановится.
        if self.scanning.get() {
            self.pending_roots.borrow_mut().push((folder, grouping));
            if let Some(progress) = self.scan_progress.borrow().as_ref() {
                progress.cancel();
            }
            return;
        }
        if self.add_root(&folder, grouping) {
            self.start_scan();
        }
    }

    /// Раскладку папки поменяли на странице «Папки».
    fn root_grouping_changed(&self, index: usize, choice: i32) {
        let (Some(library), Some(root)) = (self.library.as_ref(), self.root_ids.borrow().get(index).copied()) else {
            return;
        };
        let grouping = match choice {
            1 => Grouping::Tags,
            2 if self.classical.get() => Grouping::Composers,
            _ => Grouping::Folders,
        };
        if let Err(error) = library.set_root_grouping(root, grouping) {
            self.report(error);
            return;
        }
        self.artist.borrow_mut().take();
        self.browse_page.set(PAGE_ARTISTS);
        self.refresh_artists();
        self.refresh_now_labels();
        self.refresh_settings_page();
    }

    /// Добавляет папку музыки; `true`, если она в библиотеке.
    fn add_root(&self, folder: &Path, grouping: Grouping) -> bool {
        let Some(library) = self.library.as_ref() else {
            return false;
        };
        let before = library.roots().map(|roots| roots.len()).unwrap_or(0);
        let lex = self.lex();
        match library.add_root(folder).and_then(|root| library.set_root_grouping(root, grouping)) {
            Ok(()) => {
                // Папка забрала вложенные: записей папок стало меньше.
                if library.roots().is_ok_and(|roots| roots.len() <= before) && before > 0 {
                    self.window.set_status(lex.folder_merged.into());
                    self.refresh_artists();
                }
                self.refresh_settings_page();
                true
            }
            Err(nyanoto_library::LibraryError::InsideRoot(outer)) => {
                self.window.set_status(format!("{} {}", lex.folder_inside, outer.display()).into());
                false
            }
            Err(nyanoto_library::LibraryError::FolderName(_)) => {
                self.window.set_status(lex.folder_name.into());
                false
            }
            Err(error) => {
                self.report(error);
                false
            }
        }
    }

    /// Спрашивает, убирать ли папку: с ней уходят лайки и статистика её треков.
    fn ask_remove_folder(&self, index: usize) {
        let lex = self.lex();
        if self.scanning.get() {
            self.window.set_status(lex.folders_busy.into());
            return;
        }
        let (Some(library), Some(id)) = (self.library.as_ref(), self.root_ids.borrow().get(index).copied()) else {
            return;
        };
        let path = library
            .roots()
            .ok()
            .and_then(|roots| roots.into_iter().find(|root| root.id == id))
            .map(|root| root.path.display().to_string())
            .unwrap_or_default();
        let tracks = library.root_tracks(id).unwrap_or(0);
        let body = format!("{path}\n{}\n\n{}", lex.count(tracks as u32, Noun::Track), lex.remove_folder_body);
        self.open_confirm(Confirm::RemoveFolder(id), lex.remove_folder_title, &body, Vec::new(), lex.remove_folder);
    }

    fn remove_folder(&self, id: i64) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        if let Err(error) = library.remove_root(id) {
            self.report(error);
            return;
        }
        *self.liked.borrow_mut() = library.liked_ids().unwrap_or_default();
        self.artist.borrow_mut().take();
        self.refresh_settings_page();
        self.refresh_artists();
        self.refresh_playlists();
        self.render_all();
    }

    /// Сканирование в фоне; неизменившиеся файлы не читаются.
    fn start_scan(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        if self.scanning.replace(true) {
            // Скан уже идёт — после него ещё один.
            self.rescan_pending.set(true);
            return;
        }
        self.window.set_status(self.lex().scanning.into());
        let progress = Arc::new(ScanProgress::default());
        *self.scan_progress.borrow_mut() = Some(progress.clone());
        // Счётчики сканера раз в 250 мс.
        self.scan_timer.start(TimerMode::Repeated, Duration::from_millis(250), || {
            with_app(|app| app.show_scan_progress());
        });
        let _ = std::thread::Builder::new()
            .name("nyanoto-scan".into())
            .spawn(move || {
                let result = Library::open(&database)
                    .and_then(|mut library| library.scan(&progress))
                    .map_err(|error| error.to_string());
                in_ui(move |app| app.scan_finished(result));
            });
    }

    /// Сколько найдено и прочитано — прямо из счётчиков сканера.
    fn show_scan_progress(&self) {
        let Some(progress) = self.scan_progress.borrow().clone() else {
            return;
        };
        let lex = self.lex();
        let found = progress.found.load(Ordering::Relaxed);
        let unchanged = progress.unchanged.load(Ordering::Relaxed);
        let read = progress.read.load(Ordering::Relaxed);
        let to_read = found.saturating_sub(unchanged);
        // Обход ещё идёт — всего файлов никто не знает, показываем оба счётчика.
        let (text, share) = if !progress.walked.load(Ordering::Relaxed) {
            let mut text = format!("{} {}", lex.scan_found, rows::count(found));
            if read > 0 {
                text.push_str(&format!(", {} {}", lex.scan_read, rows::count(read)));
            }
            (text, -1.0)
        } else if to_read > 0 {
            (
                format!("{} {} {} {}", lex.scan_reading, rows::count(read), lex.of, rows::count(to_read)),
                read as f32 / to_read as f32,
            )
        } else {
            (lex.scanning.to_string(), -1.0)
        };
        self.window.set_status(text.into());
        self.window.set_scan_progress(share);
        // Папку добавили в пустую библиотеку: список наполняется по ходу.
        if read > 0 && self.artists.borrow().is_empty() {
            self.refresh_artists();
        }
    }

    fn scan_finished(&self, result: Result<ScanReport, String>) {
        trace("scan finished");
        self.scanning.set(false);
        // Отложенные папки — теперь, и новый скан.
        let pending: Vec<(PathBuf, Grouping)> = std::mem::take(&mut *self.pending_roots.borrow_mut());
        if !pending.is_empty() || self.rescan_pending.take() {
            self.scan_timer.stop();
            for (folder, grouping) in pending {
                self.add_root(&folder, grouping);
            }
            self.start_scan();
            return;
        }
        self.scan_timer.stop();
        self.scan_progress.borrow_mut().take();
        self.window.set_scan_progress(-1.0);
        let lex = self.lex();
        match result {
            Ok(report) => {
                let changed = report.added + report.updated + report.missing + report.revived;
                let mut notes = Vec::new();
                if changed > 0 {
                    notes.push(format!(
                        "{}: {} {}, {} {}, {} {}",
                        lex.library_updated,
                        lex.added_files,
                        rows::count(report.added),
                        lex.changed_files,
                        rows::count(report.updated),
                        lex.gone_files,
                        rows::count(report.missing)
                    ));
                }
                if report.problems > 0 {
                    notes.push(format!("{}: {}", lex.unparsed_files, report.problems));
                }
                for root in &report.offline_roots {
                    notes.push(format!("{} {}", lex.folder_offline, root.display()));
                }
                self.window.set_status(notes.join(" · ").into());
                self.start_checks();
                self.start_health_checks();
                if !self.synced_once.replace(true) {
                    self.sync_now();
                }
                if changed > 0 {
                    if let Some(library) = self.library.as_ref() {
                        library.forget_search_cache();
                    }
                    self.searcher.forget();
                    if self.window.get_page() == PAGE_ARTISTS {
                        self.refresh_artists();
                    }
                }
            }
            Err(error) => self.report(error),
        }
    }

    // ──────────────────────── подлинность качества ────────────────────────

    /// Значок качества трека: текст, вид и можно ли на него нажать.
    fn badge(&self, track: &Track) -> (String, i32, bool) {
        let (claim, kind) = quality_tier(&quality(track));
        if self.authenticity.get()
            && let Some(check) = self.suspicious.borrow().get(&track.id)
        {
            return match self.overrides.borrow().get(&track.id) {
                None => (claim.to_string(), BADGE_RED, true),
                Some(QualityOverride::Accept) => (claim.to_string(), kind, true),
                Some(QualityOverride::Detected) => {
                    let detected = match check.verdict {
                        Authenticity::LossyOrigin => "Lossy",
                        _ => "CD",
                    };
                    (detected.to_string(), BADGE_GREY, true)
                }
                Some(QualityOverride::Label(label)) => (label.clone(), BADGE_GREY, true),
            };
        }
        (claim.to_string(), kind, false)
    }

    fn refresh_now_badge(&self) {
        let lyrics = self.current.borrow().as_ref().is_some_and(|track| track.lyrics);
        self.window.set_now_lyrics(lyrics);
        let badge = self.current.borrow().as_ref().map(|track| self.badge(track));
        let (text, kind, clickable) = badge.unwrap_or_default();
        self.window.set_now_badge(text.into());
        self.window.set_now_badge_kind(kind);
        self.window.set_now_badge_clickable(clickable);
    }

    /// Окно с объяснением, почему значок красный, и выбором, что с ним делать.
    fn open_quality(&self, track: Track) {
        let check = self.suspicious.borrow().get(&track.id).copied();
        let Some(check) = check else {
            return;
        };
        let lang = self.lang.get();
        let lex = self.lex();
        let quality = quality(&track);
        let window = &self.window;
        window.set_quality_title(file_name(&track.path).into());
        window.set_quality_claim(
            format!("{}: {}", lex.claimed, rows::quality_label(&quality, lex)).into(),
        );
        window.set_quality_reason(lang.reason(&check, &quality).into());
        window.set_quality_detected(
            format!("{}: {}", lex.detected, lang.detected_label(&check, &quality)).into(),
        );
        window.set_quality_overridden(self.overrides.borrow().contains_key(&track.id));
        let suggested = if check.verdict == Authenticity::LossyOrigin { "MP3" } else { "CD" };
        let index = QUALITY_LABELS.iter().position(|label| *label == suggested).unwrap_or(0);
        window.set_quality_label_index(index as i32);
        *self.quality_track.borrow_mut() = Some(track);
        window.set_quality_open(true);
    }

    /// Сохраняет решение по треку из окна подлинности; `None` возвращает проверку.
    fn set_override(&self, choice: Option<QualityOverride>) {
        let track = self.quality_track.borrow().clone();
        let (Some(track), Some(library)) = (track, self.library.as_ref()) else {
            return;
        };
        if let Err(error) = library.set_quality_override(track.id, choice.as_ref()) {
            self.report(error);
            return;
        }
        match choice {
            Some(choice) => self.overrides.borrow_mut().insert(track.id, choice),
            None => self.overrides.borrow_mut().remove(&track.id),
        };
        self.window.set_quality_open(false);
        self.render_all();
    }

    /// Выключается сразу, включается — через предупреждение.
    fn request_authenticity(&self, on: bool) {
        if !on {
            self.set_authenticity(false);
            return;
        }
        if self.authenticity.get() {
            return;
        }
        // Пока окно не подтвердили, переключатель стоит в «выкл».
        self.window.set_authenticity(false);
        self.open_warning(Warning::Authenticity);
    }

    /// Включается сразу, выключается — через предупреждение.
    fn request_cover_cache(&self, on: bool) {
        if on {
            self.set_cover_cache(true);
            return;
        }
        if !self.cover_cache_on.get() {
            return;
        }
        // Пока окно не подтвердили, переключатель стоит во «вкл».
        self.window.set_cover_cache(true);
        self.open_warning(Warning::Covers);
    }

    fn open_warning(&self, warning: Warning) {
        self.warning.set(warning);
        self.window.set_warning_kind(warning as i32);
        self.window.set_warning_open(true);
    }

    fn close_warning(&self) {
        self.window.set_warning_open(false);
    }

    fn set_level(&self, index: i32) {
        let (level, code) = match index {
            1 => (CheckLevel::Medium, "medium"),
            2 => (CheckLevel::High, "high"),
            _ => (CheckLevel::Low, "low"),
        };
        self.level.set(level);
        self.save_setting(SETTING_AUTHENTICITY_LEVEL, code);
        self.reload_checks(false);
    }

    fn set_cover_cache(&self, on: bool) {
        self.cover_cache_on.set(on);
        self.window.set_cover_cache(on);
        self.save_flag(SETTING_COVER_CACHE, on);
        self.apply_covers();
    }

    /// Строка под переключателем FFmpeg.
    fn show_ffmpeg_status(&self) {
        let words = self.lex();
        let text = match nyanoto_core::ffmpeg_status() {
            nyanoto_core::FfmpegStatus::Disabled => words.ffmpeg_off.to_string(),
            nyanoto_core::FfmpegStatus::NotLoaded => words.ffmpeg_idle.to_string(),
            nyanoto_core::FfmpegStatus::Loaded { version, place } => format!("{} {version} ({place}).", words.ffmpeg_loaded),
            nyanoto_core::FfmpegStatus::Missing(why) => format!("{} ({why})", words.ffmpeg_missing),
        };
        self.window.set_ffmpeg_status(text.into());
        self.show_about();
    }

    /// «О приложении»: версия, сборка, папка данных и компоненты с версиями.
    fn show_about(&self) {
        let lex = self.lex();
        self.window.set_about_version(format!("Nyanoto {}", env!("CARGO_PKG_VERSION")).into());
        self.window.set_about_build(
            format!("{}: {} · {} · {}", lex.about_build, env!("NYANOTO_PROFILE"), env!("NYANOTO_TARGET"), env!("NYANOTO_RUSTC"))
                .into(),
        );
        let data = self.database.as_deref().and_then(Path::parent).map(|dir| dir.display().to_string()).unwrap_or_default();
        self.window.set_about_data(format!("{}: {data}", lex.about_data).into());
        let mut rows: Vec<AboutRow> = nyanoto_core::components()
            .into_iter()
            .map(|component| AboutRow {
                name: component.name.into(),
                version: component.version.into(),
                license: component.license.into(),
            })
            .collect();
        let licenses = |name: &str| match name {
            "symphonia" => "MPL-2.0",
            "lofty" | "image" | "realfft" => "MIT OR Apache-2.0",
            "cpal" => "Apache-2.0",
            "rubato" | "rusqlite" | "rfd" => "MIT",
            "slint" => "Slint Royalty-free 2.0",
            _ => "",
        };
        for pair in env!("NYANOTO_DEPENDENCIES").split(';').filter(|pair| !pair.is_empty()) {
            if let Some((name, version)) = pair.split_once('=') {
                rows.push(AboutRow { name: name.into(), version: version.into(), license: licenses(name).into() });
            }
        }
        // Шрифты сцен и темы «Терминал» вшиты в плеер.
        for (name, version) in [("Oswald", "4.103"), ("Unbounded", "1.701"), ("Playfair Display", "1.203"), ("Great Vibes", "1.103"), ("Press Start 2P", "3.000"), ("JetBrains Mono", "2.304")] {
            rows.push(AboutRow { name: name.into(), version: version.into(), license: "OFL-1.1".into() });
        }
        rows.push(AboutRow {
            name: "SQLite".into(),
            version: nyanoto_library::sqlite_version().into(),
            license: "Public domain".into(),
        });
        self.window.set_about_rows(Rc::new(VecModel::from(rows)).into());
    }

    /// Загрузчику: делать ли копии на диске и какого качества.
    fn apply_covers(&self) {
        let cache = self.cover_cache_on.get();
        self.covers.set_disk((cache && self.cover_disk_on.get()).then(|| self.cover_quality.get()));
        if !cache {
            self.forget_covers();
        }
    }

    fn set_authenticity(&self, on: bool) {
        self.authenticity.set(on);
        self.window.set_authenticity(on);
        self.save_flag(SETTING_AUTHENTICITY, on);
        if on {
            self.start_checks();
        } else {
            self.stop_checking.store(true, Ordering::Relaxed);
            self.stop_urgent.store(true, Ordering::Relaxed);
        }
        self.render_all();
    }

    /// Проверка подлинности в фоне; результат сохраняется сразу, прерванная продолжится.
    fn start_checks(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        if !self.authenticity.get() || self.urgent.get() || self.checking.replace(true) {
            return;
        }
        let stop = self.stop_checking.clone();
        stop.store(false, Ordering::Relaxed);
        let _ = std::thread::Builder::new()
            .name("nyanoto-authenticity".into())
            .spawn(move || {
                // Тихий режим: всем остальным программам процессор и диск нужнее.
                quiet::enter_background();
                let Ok(library) = Library::open(&database) else {
                    in_ui(|app| app.checking.set(false));
                    return;
                };
                let (mut checked, mut after) = (0usize, 0i64);
                while !stop.load(Ordering::Relaxed) {
                    let batch = library.tracks_to_check_after(after, CHECK_BATCH).unwrap_or_default();
                    let Some(last) = batch.last().map(|(id, _)| *id) else {
                        break;
                    };
                    after = last;
                    if checked == 0 {
                        in_ui(|app| app.window.set_status(app.lex().checking.into()));
                    }
                    for (id, path) in batch {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let started = std::time::Instant::now();
                        // Вердикт сразу для всех чувствительностей; неразобранный файл тоже отмечаем.
                        let record = inspect_record(&path);
                        let _ = library.store_check(id, &record);
                        checked += 1;
                        quiet::rest(started.elapsed(), &stop);
                    }
                    in_ui(|app| app.reload_checks(false));
                }
                in_ui(move |app| {
                    app.checking.set(false);
                    if checked > 0 {
                        app.reload_checks(true);
                    }
                });
            });
    }

    /// Проверка сейчас: на всех ядрах, кроме одного.
    fn check_now(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        if !self.authenticity.get() || self.urgent.replace(true) {
            return;
        }
        self.stop_checking.store(true, Ordering::Relaxed);
        let stop = self.stop_urgent.clone();
        stop.store(false, Ordering::Relaxed);
        self.window.set_checking_now(true);
        let _ = std::thread::Builder::new()
            .name("nyanoto-check-now".into())
            .spawn(move || {
                let pending = Library::open(&database)
                    .and_then(|library| library.tracks_to_check(usize::MAX))
                    .unwrap_or_default();
                let total = pending.len();
                let workers = std::thread::available_parallelism()
                    .map_or(2, |count| count.get())
                    .saturating_sub(1)
                    .max(1);
                let next = std::sync::atomic::AtomicUsize::new(0);
                let (results, received) = std::sync::mpsc::channel::<(i64, CheckRecord)>();
                let mut done = 0usize;
                std::thread::scope(|scope| {
                    for _ in 0..workers {
                        let results = results.clone();
                        let (next, pending, stop) = (&next, &pending, &stop);
                        scope.spawn(move || {
                            while !stop.load(Ordering::Relaxed) {
                                let index = next.fetch_add(1, Ordering::Relaxed);
                                let Some((id, path)) = pending.get(index) else {
                                    return;
                                };
                                if results.send((*id, inspect_record(path))).is_err() {
                                    return;
                                }
                            }
                        });
                    }
                    drop(results);
                    let Ok(library) = Library::open(&database) else {
                        return;
                    };
                    for (id, record) in received {
                        let _ = library.store_check(id, &record);
                        done += 1;
                        if done.is_multiple_of(50) {
                            in_ui(move |app| {
                                let lex = app.lex();
                                app.window.set_status(
                                    format!("{} {done} {} {total}", lex.checked_progress, lex.of).into(),
                                );
                            });
                        }
                    }
                });
                in_ui(move |app| {
                    app.urgent.set(false);
                    app.window.set_checking_now(false);
                    if total == 0 {
                        app.window.set_status(app.lex().all_checked.into());
                    } else {
                        app.reload_checks(true);
                    }
                });
            });
    }

    fn reload_checks(&self, finished: bool) {
        if let Some(library) = self.library.as_ref() {
            self.suspicious.replace(library.suspicious(self.level.get()).unwrap_or_default());
        }
        self.render_all();
        if finished && self.authenticity.get() {
            let lex = self.lex();
            self.window
                .set_status(format!("{}: {}", lex.checked, self.suspicious.borrow().len()).into());
        }
    }

    // ─────────────────── соседние альбомы исполнителя ───────────────────

    /// «Следующий»: на последнем треке альбома — следующий альбом исполнителя.
    fn go_next(&self) {
        self.leaving.set(self.listening.borrow().is_some());
        if self.at_album_edge(true) && self.move_to_album(true, false) {
            return;
        }
        self.engine.next();
    }

    /// «Предыдущий»: в начале альбома — альбом, откуда пришли, или предыдущий по списку.
    fn go_previous(&self) {
        if self.at_album_edge(false) && self.move_to_album(false, false) {
            return;
        }
        self.engine.previous();
    }

    /// На краю альбома?
    fn at_album_edge(&self, forward: bool) -> bool {
        if self.queue_kind.get() != QueueKind::Album || self.repeat.get() != Repeat::Off {
            return false;
        }
        let (Some(index), length) = self.engine.queue_position() else {
            return false;
        };
        if forward {
            index + 1 == length
        } else {
            index == 0 && self.engine.position().is_none_or(|position| position.as_secs_f64() <= PREVIOUS_RESTARTS_AFTER)
        }
    }

    /// Очередь доиграна: следующий альбом исполнителя или тишина.
    fn queue_finished(&self) {
        let album_ended = self.queue_kind.get() == QueueKind::Album;
        if !self.auto_next.get() || self.repeat.get() != Repeat::Off || !album_ended {
            self.chain.borrow_mut().clear();
            return;
        }
        self.move_to_album(true, true);
    }

    /// Альбомы исполнителя играющего трека и место его альбома.
    fn artist_albums(&self) -> Option<(Vec<Album>, usize)> {
        let last = self.current.borrow().clone()?;
        let library = self.library.as_ref()?;
        let grouping = self.grouping.get();
        let album = library.album_of(&last, grouping).ok().flatten()?;
        // Исполнитель целиком, со всеми написаниями имени.
        let owner = album.owners.first().cloned().unwrap_or_default().to_lowercase();
        let artist = self
            .artists
            .borrow()
            .iter()
            .find(|artist| artist.names.iter().any(|name| name.to_lowercase() == owner))
            .cloned()
            .unwrap_or_else(|| Artist {
                grouping,
                name: owner.clone(),
                names: album.owners.clone(),
                albums: 0,
                tracks: 0,
            });
        let albums = library.albums(&artist).ok()?;
        // По папке: у синглов без тега альбома названия совпадают (пустые).
        let here = albums
            .iter()
            .position(|item| item.folder == album.folder && item.title == album.title)
            .or_else(|| albums.iter().position(|item| item.title == album.title))?;
        Some((albums, here))
    }

    /// Соседний альбом исполнителя. Сам (`auto`) — без повторов, кончились — тишина;
    /// кнопкой — по кругу. Вперёд — с первого трека, назад — с последнего.
    fn move_to_album(&self, forward: bool, auto: bool) -> bool {
        let (Some((albums, here)), Some(library)) = (self.artist_albums(), self.library.as_ref()) else {
            return false;
        };
        self.chain.borrow_mut().insert(albums[here].title.clone());
        let titles: Vec<Option<String>> = albums.iter().map(|item| item.title.clone()).collect();
        let target = if forward {
            let shuffled = self.auto_next_random.get();
            let chained = if auto || shuffled {
                let random = shuffled.then(|| self.next_random());
                chain::next_album(&titles, here, &self.chain.borrow(), random)
            } else {
                None
            };
            match chained {
                Some(next) => Some(next),
                None if auto => {
                    self.chain.borrow_mut().clear();
                    None
                }
                None => chain::beside(albums.len(), here, true, None),
            }
        } else {
            let back = self.album_trail.borrow_mut().pop();
            let back = back.and_then(|folder| albums.iter().position(|item| item.folder == folder));
            chain::beside(albums.len(), here, false, back)
        };
        let Some(target) = target else {
            return false;
        };
        let Ok(tracks) = library.album_tracks(&albums[target]) else {
            return false;
        };
        if tracks.is_empty() {
            return false;
        }
        if forward {
            self.album_trail.borrow_mut().push(albums[here].folder.clone());
        }
        self.chain.borrow_mut().insert(albums[target].title.clone());
        let start = if forward { 0 } else { tracks.len() - 1 };
        let paths: Vec<PathBuf> = tracks.iter().map(|track| track.path.clone()).collect();
        *self.queue.borrow_mut() = tracks
            .into_iter()
            .map(|track| (track.path.clone(), track))
            .collect();
        if !auto {
            self.remember_position();
        }
        if !auto && self.engine.state() != State::Playing {
            self.engine.cue(paths, start, Duration::ZERO);
        } else {
            self.engine.play_list(paths, start);
        }
        true
    }

    fn next_random(&self) -> u64 {
        let mut state = self.random.get();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.random.set(state);
        state
    }

    // ─────────────────────────── обложки ───────────────────────────

    fn cached_cover(&self, folder: &Path, size: u32) -> Option<Image> {
        self.cover_cache
            .borrow()
            .get(&(folder.to_path_buf(), size, covers::Shape::Fit))
            .cloned()
    }

    /// Запомнить обложку (и её отсутствие), если кэш включён.
    fn remember_cover(&self, folder: PathBuf, size: u32, pixels: Option<covers::Pixels>) -> Option<Image> {
        self.remember_picture((folder, size, covers::Shape::Fit), pixels)
    }

    fn remember_picture(&self, key: PictureKey, pixels: Option<covers::Pixels>) -> Option<Image> {
        let image = pixels.map(Image::from_rgba8).unwrap_or_default();
        if !self.cover_cache_on.get() {
            return (image.size().width > 0).then_some(image);
        }
        if key.1 > REMEMBERED_SIDE {
            return (image.size().width > 0).then_some(image);
        }
        let bytes = |image: &Image| image.size().width as usize * image.size().height as usize * 4;
        let (mut cache, mut order) = (self.cover_cache.borrow_mut(), self.cover_order.borrow_mut());
        let mut total = self.cover_bytes.get() + bytes(&image);
        match cache.insert(key.clone(), image.clone()) {
            Some(old) => total -= bytes(&old),
            None => order.push_back(key),
        }
        while order.len() > COVERS_IN_MEMORY || (total > COVER_MEMORY_BYTES && order.len() > 1) {
            if let Some(oldest) = order.pop_front()
                && let Some(gone) = cache.remove(&oldest)
            {
                total -= bytes(&gone);
            }
        }
        self.cover_bytes.set(total);
        (image.size().width > 0).then_some(image)
    }

    fn forget_covers(&self) {
        self.cover_cache.borrow_mut().clear();
        self.cover_order.borrow_mut().clear();
        self.cover_bytes.set(0);
    }

    /// Забывает картинки одной формы: например, все круглые, когда сменился аватар.
    fn forget_pictures(&self, shape: covers::Shape) {
        let mut cache = self.cover_cache.borrow_mut();
        cache.retain(|key, _| key.2 != shape);
        self.cover_order.borrow_mut().retain(|key| key.2 != shape);
        self.cover_bytes
            .set(cache.values().map(|image| image.size().width as usize * image.size().height as usize * 4).sum());
    }

    /// Картинка — сразу из памяти или после загрузки.
    fn show_picture(&self, key: PictureKey, source: covers::Source, show: impl FnOnce(&App, Image) + Send + 'static) {
        // Отпускаем кэш до `show`, иначе двойное заимствование.
        let cached = self.cover_cache.borrow().get(&key).cloned();
        if let Some(image) = cached {
            show(self, image);
            return;
        }
        let (size, shape) = (key.1, key.2);
        self.covers.picture(source, size, shape, move |pixels| {
            in_ui(move |app| {
                if let Some(image) = app.remember_picture(key, pixels) {
                    show(app, image);
                }
            });
        });
    }

    /// Обложка — сразу из памяти или после загрузки.
    fn show_cover(&self, folder: &Path, size: u32, show: fn(&App, Image)) {
        if let Some(image) = self.cached_cover(folder, size) {
            show(self, image);
            return;
        }
        let key = folder.to_path_buf();
        self.covers.request(folder, size, move |pixels| {
            in_ui(move |app| {
                if let Some(image) = app.remember_cover(key, size, pixels) {
                    show(app, image);
                }
            });
        });
    }
}

/// Подлинность одного файла: вердикты всех чувствительностей.
fn inspect_record(path: &Path) -> CheckRecord {
    match nyanoto_core::inspect(path) {
        Ok(inspection) => CheckRecord {
            verdicts: nyanoto_core::Sensitivity::ALL.map(|level| match inspection.verdict(level) {
                nyanoto_core::Verdict::Genuine => Authenticity::Genuine,
                nyanoto_core::Verdict::LossyOrigin => Authenticity::LossyOrigin,
                nyanoto_core::Verdict::Upsampled => Authenticity::Upsampled,
                nyanoto_core::Verdict::PaddedBits => Authenticity::PaddedBits,
                nyanoto_core::Verdict::Unknown => Authenticity::Unknown,
            }),
            cutoff_hz: inspection.cutoff_hz,
            real_bits: inspection.real_bits(),
            real_rate: inspection.real_rate(),
        },
        Err(_) => CheckRecord {
            verdicts: [Authenticity::Unknown; 3],
            cutoff_hz: None,
            real_bits: None,
            real_rate: None,
        },
    }
}
