//! Редкие форматы через FFmpeg: WMA, AC3, DTS, TrueHD, TAK, DST, ATRAC и т. п.
//! Других открытых эталонов нет. Не линкуется — грузим при первом таком файле (`ffmpeg.c`).
//! Ищем: `NYANOTO_FFMPEG_DIR`, `ffmpeg/` рядом, папка программы, `../ffmpeg`, система. Нужен 7.x.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use symphonia::core::meta::{StandardTagKey, Tag, Value};

use super::{Opened, Stream, layout};
use crate::source::{ReplayGain, TrackError, TrackInfo};

#[repr(C)]
struct Io {
    user: *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, *mut u8, i64) -> i64,
    seek: unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64,
    size: unsafe extern "C" fn(*mut c_void) -> i64,
}

#[repr(C)]
struct Info {
    frames: i64,
    sample_rate: i32,
    channels: i32,
    bits: i32,
    lossless: i32,
    channel_mask: u64,
    bit_rate: i64,
    codec: [c_char; 32],
    profile: [c_char; 48],
    container: [c_char; 32],
}

unsafe extern "C" {
    fn nyanoto_ff_load(dirs: *const c_char) -> c_int;
    fn nyanoto_ff_error() -> *const c_char;
    fn nyanoto_ff_where() -> *const c_char;
    fn nyanoto_ff_version() -> u32;
    fn nyanoto_ff_open(io: *const Io, info: *mut Info, error: *mut c_char, size: c_int) -> *mut c_void;
    fn nyanoto_ff_tag(handle: *mut c_void, index: c_int, key: *mut *const c_char, value: *mut *const c_char) -> c_int;
    fn nyanoto_ff_read(handle: *mut c_void, out: *mut *const f32, damaged: *mut c_int) -> i64;
    fn nyanoto_ff_seek(handle: *mut c_void, frame: i64) -> c_int;
    fn nyanoto_ff_close(handle: *mut c_void);
}

/// Разрешено ли пользоваться FFmpeg (переключатель в настройках).
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Итог загрузки: `None` — ещё не пробовали.
static LOADED: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// Загруженное остаётся в памяти, но новые файлы через него не открываются.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Что с FFmpeg сейчас: для подписи в настройках.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Выключен в настройках.
    Disabled,
    /// Ещё не понадобился: библиотеки не загружались.
    NotLoaded,
    /// Загружен: версия и откуда.
    Loaded { version: String, place: String },
    /// Не нашёлся или не подошёл.
    Missing(String),
}

pub fn status() -> Status {
    if !ENABLED.load(Ordering::Relaxed) {
        return Status::Disabled;
    }
    match &*LOADED.lock().unwrap_or_else(|error| error.into_inner()) {
        None => Status::NotLoaded,
        Some(Ok(())) => {
            let version = unsafe { nyanoto_ff_version() };
            let place = unsafe { CStr::from_ptr(nyanoto_ff_where()) }.to_string_lossy().into_owned();
            Status::Loaded { version: format!("{}.{}.{}", version >> 16, (version >> 8) & 0xff, version & 0xff), place }
        }
        Some(Err(error)) => Status::Missing(error.clone()),
    }
}

/// Загружает FFmpeg один раз; дальше только возвращает итог.
fn ensure_loaded() -> Result<(), String> {
    if !ENABLED.load(Ordering::Relaxed) {
        return Err("FFmpeg выключен в настройках".into());
    }
    let mut loaded = LOADED.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(result) = &*loaded {
        return result.clone();
    }
    let dirs: Vec<String> = search_dirs().iter().map(|dir| dir.to_string_lossy().into_owned()).collect();
    let list = CString::new(dirs.join("\n")).unwrap_or_default();
    let result = if unsafe { nyanoto_ff_load(list.as_ptr()) } == 1 {
        Ok(())
    } else {
        Err(unsafe { CStr::from_ptr(nyanoto_ff_error()) }.to_string_lossy().into_owned())
    };
    *loaded = Some(result.clone());
    result
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("NYANOTO_FFMPEG_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(exe) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push(exe.join("ffmpeg"));
        dirs.push(exe.clone());
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("ffmpeg"));
        }
    }
    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// # Safety
/// `user` — [`BufReader`] из [`Context`], жив, пока жив декодер.
unsafe fn file<'a>(user: *mut c_void) -> &'a mut BufReader<File> {
    unsafe { &mut *user.cast::<BufReader<File>>() }
}

unsafe extern "C" fn read(user: *mut c_void, buffer: *mut u8, bytes: i64) -> i64 {
    let file = unsafe { file(user) };
    let Ok(bytes) = usize::try_from(bytes) else {
        return -1;
    };
    if bytes == 0 || buffer.is_null() {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(buffer, bytes) };
    loop {
        match file.read(out) {
            Ok(count) => return count as i64,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return -1,
        }
    }
}

unsafe extern "C" fn seek(user: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let file = unsafe { file(user) };
    let target = match whence {
        0 => u64::try_from(offset).map(SeekFrom::Start).ok(),
        1 => Some(SeekFrom::Current(offset)),
        2 => Some(SeekFrom::End(offset)),
        _ => None,
    };
    match target.map(|target| file.seek(target)) {
        Some(Ok(position)) => position as i64,
        _ => -1,
    }
}

unsafe extern "C" fn size(user: *mut c_void) -> i64 {
    let file = unsafe { file(user) };
    file.get_ref().metadata().map_or(-1, |metadata| metadata.len() as i64)
}

/// Открытый файл: декодер FFmpeg и файл, на который он держит указатель.
struct Context {
    handle: NonNull<c_void>,
    _file: Box<BufReader<File>>,
}

// Декодер и файл принадлежат только этой структуре, а она — одному потоку за раз.
unsafe impl Send for Context {}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { nyanoto_ff_close(self.handle.as_ptr()) };
    }
}

/// Сведения о потоке, которые нужны и плееру, и библиотеке.
pub(super) struct Described {
    pub info: TrackInfo,
    pub layout: symphonia::core::audio::Channels,
    pub lossless: bool,
    /// Имя для подписи качества: `wmalossless`, `eac3`, `dts-hd-ma`…
    pub format: String,
    pub tags: Vec<Tag>,
}

fn text(field: &[c_char]) -> String {
    let bytes: Vec<u8> = field.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn open_context(path: &Path) -> Result<(Context, Described), TrackError> {
    ensure_loaded().map_err(TrackError::Unsupported)?;
    let file = File::open(path).map_err(TrackError::Io)?;
    let mut source = Box::new(BufReader::new(file));
    let io = Io { user: (&mut *source as *mut BufReader<File>).cast(), read, seek, size };
    let mut info: Info = unsafe { std::mem::zeroed() };
    let mut error = [0 as c_char; 256];
    let handle = unsafe { nyanoto_ff_open(&io, &mut info, error.as_mut_ptr(), error.len() as c_int) };
    let handle = NonNull::new(handle)
        .ok_or_else(|| TrackError::Unsupported(format!("FFmpeg: {}", text(&error))))?;
    let context = Context { handle, _file: source };

    let channels = usize::try_from(info.channels).unwrap_or(0);
    let rate = u32::try_from(info.sample_rate).unwrap_or(0);
    let codec = text(&info.codec);
    let profile = text(&info.profile);
    let frames = u64::try_from(info.frames).ok().filter(|frames| *frames > 0);
    let tags = tags(&context);
    let mut replay_gain = ReplayGain::default();
    replay_gain.read(&tags);
    let lossless = info.lossless != 0;
    let track = TrackInfo {
        codec: codec.clone(),
        sample_rate: Some(rate),
        channels: Some(channels),
        bits_per_sample: u32::try_from(info.bits).ok().filter(|bits| *bits > 0 && lossless),
        duration: frames.map(|frames| std::time::Duration::from_secs_f64(frames as f64 / f64::from(rate.max(1)))),
        replay_gain,
    };
    let format = match (codec.as_str(), profile.as_str()) {
        ("dts", "DTS-HD MA" | "DTS-HD MA + DTS:X" | "DTS-HD MA + DTS:X IMAX") => "dts-hd-ma".to_string(),
        ("dts", "DTS-HD HRA") => "dts-hd-hra".to_string(),
        _ => codec,
    };
    let described = Described {
        info: track,
        layout: layout(channels, u32::try_from(info.channel_mask).unwrap_or(0)),
        lossless,
        format,
        tags,
    };
    Ok((context, described))
}

/// Теги файла и звуковой дорожки; знакомым ключам — стандартный смысл.
fn tags(context: &Context) -> Vec<Tag> {
    let mut tags = Vec::new();
    for index in 0..1024 {
        let (mut key, mut value) = (std::ptr::null(), std::ptr::null());
        if unsafe { nyanoto_ff_tag(context.handle.as_ptr(), index, &mut key, &mut value) } == 0 {
            break;
        }
        let key = unsafe { CStr::from_ptr(key) }.to_string_lossy().into_owned();
        let value = unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned();
        use StandardTagKey as K;
        let standard = match key.to_ascii_lowercase().as_str() {
            "title" => Some(K::TrackTitle),
            "artist" | "author" => Some(K::Artist),
            "album" => Some(K::Album),
            "album_artist" | "albumartist" | "wm/albumartist" => Some(K::AlbumArtist),
            "date" | "year" | "wm/year" => Some(K::Date),
            "track" | "wm/tracknumber" => Some(K::TrackNumber),
            "disc" | "wm/partofset" => Some(K::DiscNumber),
            "genre" | "wm/genre" => Some(K::Genre),
            "composer" | "wm/composer" => Some(K::Composer),
            "comment" => Some(K::Comment),
            "replaygain_track_gain" => Some(K::ReplayGainTrackGain),
            "replaygain_track_peak" => Some(K::ReplayGainTrackPeak),
            "replaygain_album_gain" => Some(K::ReplayGainAlbumGain),
            "replaygain_album_peak" => Some(K::ReplayGainAlbumPeak),
            _ => None,
        };
        tags.push(Tag::new(standard, &key, Value::String(value)));
    }
    tags
}

pub(super) fn open(path: &Path) -> Result<Opened, TrackError> {
    let (context, described) = open_context(path)?;
    let channels = described.info.channels.unwrap_or(1);
    Ok(Opened {
        stream: Box::new(Ffmpeg { context, channels, errors: 0 }),
        info: described.info,
        layout: described.layout,
        // Длину FFmpeg часто угадывает — «не хватает в конце» по ней не считаем.
        frames: None,
        dsd_rate: None,
        id3v2: None,
    })
}

/// Сведения для библиотеки без декодирования звука.
pub(super) fn describe(path: &Path) -> Option<Described> {
    open_context(path).ok().map(|(_, described)| described)
}

struct Ffmpeg {
    context: Context,
    channels: usize,
    errors: u64,
}

impl Stream for Ffmpeg {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        let mut samples: *const f32 = std::ptr::null();
        let mut damaged: c_int = 0;
        let frames = unsafe { nyanoto_ff_read(self.context.handle.as_ptr(), &mut samples, &mut damaged) };
        self.errors += u64::try_from(damaged).unwrap_or(0);
        if frames <= 0 || samples.is_null() {
            return Ok(());
        }
        let count = frames as usize * self.channels;
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(samples, count) });
        Ok(())
    }

    fn seek(&mut self, frame: u64) -> Result<u64, TrackError> {
        if unsafe { nyanoto_ff_seek(self.context.handle.as_ptr(), frame as i64) } != 0 {
            return Err(TrackError::Malformed("FFmpeg: перемотка не удалась".into()));
        }
        Ok(frame)
    }

    fn decode_errors(&self) -> u64 {
        self.errors
    }
}
