//! Monkey's Audio на SDK (`vendor/monkeys-audio`) через `ape.cpp`.
//! Битый кадр SDK меняет на тишину и говорит нам; при проверке ещё и MD5 потока.
//! Колбэки чтения SDK зовёт из своего потока — пока он работает, файл не трогаем.

use std::ffi::{c_int, c_void};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::ptr::NonNull;

use super::{Opened, Stream, layout, tags};
use crate::source::{OpenOptions, TrackError, TrackInfo};

const FORMAT_FLAG_FLOATING_POINT: i32 = 1 << 12;
/// Блоков (кадров звука) за одно обращение к декодеру.
const BLOCKS: usize = 4096;

/// `NyanotoApeIo` из `ape.cpp`.
#[repr(C)]
pub(crate) struct Io {
    pub user: *mut c_void,
    pub read: unsafe extern "C" fn(*mut c_void, *mut c_void, i64) -> i64,
    pub write: Option<unsafe extern "C" fn(*mut c_void, *const c_void, i64) -> i64>,
    pub seek: unsafe extern "C" fn(*mut c_void, i64, c_int) -> c_int,
    pub position: unsafe extern "C" fn(*mut c_void) -> i64,
    pub size: unsafe extern "C" fn(*mut c_void) -> i64,
}

/// `NyanotoApeInfo` из `ape.cpp`.
#[repr(C)]
#[derive(Default)]
struct Info {
    total_blocks: i64,
    sample_rate: i32,
    channels: i32,
    bits: i32,
    bytes_per_sample: i32,
    version: i32,
    format_flags: i32,
    channel_mask: u32,
}

unsafe extern "C" {
    fn nyanoto_ape_open(io: *const Io, info: *mut Info, error: *mut c_int) -> *mut c_void;
    fn nyanoto_ape_read(handle: *mut c_void, buffer: *mut u8, blocks: i64, damaged: *mut c_int) -> i64;
    fn nyanoto_ape_seek(handle: *mut c_void, block: i64) -> c_int;
    fn nyanoto_ape_md5(handle: *mut c_void) -> c_int;
    fn nyanoto_ape_close(handle: *mut c_void);
}

/// # Safety
/// `user` — [`BufReader`] из [`Context`], жив, пока жив декодер.
unsafe fn file<'a>(user: *mut c_void) -> &'a mut BufReader<File> {
    unsafe { &mut *user.cast::<BufReader<File>>() }
}

unsafe extern "C" fn read(user: *mut c_void, buffer: *mut c_void, bytes: i64) -> i64 {
    let file = unsafe { file(user) };
    let Ok(bytes) = usize::try_from(bytes) else {
        return -1;
    };
    if bytes == 0 || buffer.is_null() {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), bytes) };
    let mut done = 0;
    while done < bytes {
        match file.read(&mut out[done..]) {
            Ok(0) => break,
            Ok(count) => done += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return -1,
        }
    }
    done as i64
}

unsafe extern "C" fn seek(user: *mut c_void, position: i64, whence: c_int) -> c_int {
    let file = unsafe { file(user) };
    let target = match whence {
        0 => u64::try_from(position).map(SeekFrom::Start).ok(),
        1 => Some(SeekFrom::Current(position)),
        2 => Some(SeekFrom::End(position)),
        _ => None,
    };
    match target.map(|target| file.seek(target)) {
        Some(Ok(_)) => 0,
        _ => -1,
    }
}

unsafe extern "C" fn position(user: *mut c_void) -> i64 {
    let file = unsafe { file(user) };
    file.stream_position().map_or(-1, |position| position as i64)
}

unsafe extern "C" fn size(user: *mut c_void) -> i64 {
    let file = unsafe { file(user) };
    file.get_ref().metadata().map_or(-1, |metadata| metadata.len() as i64)
}

/// Открытый файл: декодер SDK и файл, на который он держит указатель.
struct Context {
    handle: NonNull<c_void>,
    _file: Box<BufReader<File>>,
}

// Поток SDK трогает файл, только пока мы ждём в `nyanoto_ape_read`.
unsafe impl Send for Context {}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { nyanoto_ape_close(self.handle.as_ptr()) };
    }
}

pub(super) fn open(mut file: File, options: OpenOptions) -> Result<Opened, TrackError> {
    let replay_gain = tags::replay_gain(&mut file, None);
    file.seek(SeekFrom::Start(0)).map_err(TrackError::Io)?;
    let mut source = Box::new(BufReader::new(file));
    let io = Io {
        user: (&mut *source as *mut BufReader<File>).cast(),
        read,
        write: None,
        seek,
        position,
        size,
    };
    let mut info = Info::default();
    let mut error: c_int = 0;
    let handle = unsafe { nyanoto_ape_open(&io, &mut info, &mut error) };
    let handle =
        NonNull::new(handle).ok_or_else(|| TrackError::Malformed(format!("Monkey's Audio: ошибка {error}")))?;
    let context = Context { handle, _file: source };
    let channels = usize::try_from(info.channels).unwrap_or(0);
    let bytes = usize::try_from(info.bytes_per_sample).unwrap_or(0);
    let rate = u32::try_from(info.sample_rate).unwrap_or(0);
    if !(1..=32).contains(&channels) || !(1..=4).contains(&bytes) || rate == 0 {
        return Err(TrackError::Malformed(format!(
            "Monkey's Audio: {channels} кан., {bytes} байт на сэмпл, {rate} Гц"
        )));
    }
    let float = info.format_flags & FORMAT_FLAG_FLOATING_POINT != 0;
    let frames = u64::try_from(info.total_blocks).unwrap_or(0);
    let track = TrackInfo {
        codec: "ape".into(),
        sample_rate: Some(rate),
        channels: Some(channels),
        bits_per_sample: Some(if float { 32 } else { info.bits.max(1) as u32 }),
        duration: Some(std::time::Duration::from_secs_f64(frames as f64 / f64::from(rate))),
        replay_gain,
    };
    let stream = Ape {
        context,
        channels,
        bytes,
        float,
        raw: Vec::new(),
        errors: 0,
        verify: options.verify,
        seeked: false,
    };
    Ok(Opened { stream: Box::new(stream), info: track, layout: layout(channels, info.channel_mask), frames: Some(frames), dsd_rate: None, id3v2: None })
}

struct Ape {
    context: Context,
    channels: usize,
    bytes: usize,
    float: bool,
    raw: Vec<u8>,
    errors: u64,
    verify: bool,
    /// MD5 сверяется, только если файл прочитан подряд, без перемоток.
    seeked: bool,
}

impl Stream for Ape {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        let frame_bytes = self.channels * self.bytes;
        self.raw.resize(BLOCKS * frame_bytes, 0);
        let mut damaged: c_int = 0;
        let blocks = unsafe {
            nyanoto_ape_read(self.context.handle.as_ptr(), self.raw.as_mut_ptr(), BLOCKS as i64, &mut damaged)
        };
        if damaged != 0 {
            self.errors += 1;
        }
        let Ok(blocks) = usize::try_from(blocks) else {
            // Декодер дальше не может: остальное покажет `missing`.
            return Ok(());
        };
        let raw = &self.raw[..blocks.min(BLOCKS) * frame_bytes];
        out.reserve(raw.len() / self.bytes);
        match (self.bytes, self.float) {
            (1, _) => out.extend(raw.iter().map(|byte| (f32::from(*byte) - 128.0) / 128.0)),
            (2, _) => out.extend(raw.as_chunks::<2>().0.iter().map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)),
            (3, _) => out.extend(raw.as_chunks::<3>().0.iter().map(|[low, middle, high]| {
                let value = i32::from_le_bytes([0, *low, *middle, *high]) >> 8;
                value as f32 / 8_388_608.0
            })),
            (_, true) => out.extend(raw.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes))),
            _ => out.extend(raw.as_chunks::<4>().0.iter().map(|bytes| i32::from_le_bytes(*bytes) as f32 / 2_147_483_648.0)),
        }
        Ok(())
    }

    fn seek(&mut self, frame: u64) -> Result<u64, TrackError> {
        self.seeked = true;
        if unsafe { nyanoto_ape_seek(self.context.handle.as_ptr(), frame as i64) } != 0 {
            return Err(TrackError::Malformed("Monkey's Audio: перемотка не удалась".into()));
        }
        Ok(frame)
    }

    fn decode_errors(&self) -> u64 {
        self.errors
    }

    fn finalize(&mut self) -> Option<bool> {
        if !self.verify || self.seeked {
            return None;
        }
        match unsafe { nyanoto_ape_md5(self.context.handle.as_ptr()) } {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }
}
