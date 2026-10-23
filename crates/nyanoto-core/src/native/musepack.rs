//! Musepack SV7/SV8 на libmpcdec (`vendor/musepack`).
//! Тишину и gapless библиотека режет сама, перемотка точная.
//! Сумм у кадров нет — порчу слышно, но не видно; видно только обрыв (`missing`).

use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::ptr::NonNull;
use std::sync::Once;

use super::{Opened, Stream, layout, tags};
use crate::source::{TrackError, TrackInfo};

/// Сэмплов в кадре Musepack и размер буфера декодера (в сэмплах f32).
const FRAME: usize = 36 * 32;
const BUFFER: usize = FRAME * 4;

#[repr(C)]
struct Demux {
    _private: [u8; 0],
}

/// `mpc_reader` из `reader.h`.
#[repr(C)]
struct Reader {
    read: unsafe extern "C" fn(*mut Reader, *mut c_void, i32) -> i32,
    seek: unsafe extern "C" fn(*mut Reader, i32) -> u8,
    tell: unsafe extern "C" fn(*mut Reader) -> i32,
    get_size: unsafe extern "C" fn(*mut Reader) -> i32,
    canseek: unsafe extern "C" fn(*mut Reader) -> u8,
    data: *mut c_void,
}

/// `mpc_streaminfo` из `streaminfo.h`.
#[repr(C)]
struct StreamInfo {
    sample_freq: u32,
    channels: u32,
    stream_version: u32,
    bitrate: u32,
    average_bitrate: f64,
    max_band: u32,
    ms: u32,
    fast_seek: u32,
    block_pwr: u32,
    gain_title: u16,
    gain_album: u16,
    peak_album: u16,
    peak_title: u16,
    is_true_gapless: u32,
    samples: u64,
    beg_silence: u64,
    encoder_version: u32,
    encoder: [c_char; 256],
    pns: u8,
    profile: f32,
    profile_name: *const c_char,
    header_position: i32,
    tag_offset: i32,
    total_file_length: i32,
}

/// `mpc_frame_info` из `mpcdec.h`.
#[repr(C)]
struct FrameInfo {
    samples: u32,
    bits: i32,
    buffer: *mut f32,
    is_key_frame: u8,
}

unsafe extern "C" {
    fn mpc_demux_init(reader: *mut Reader) -> *mut Demux;
    fn mpc_demux_exit(demux: *mut Demux);
    fn mpc_demux_get_info(demux: *mut Demux, info: *mut StreamInfo);
    fn mpc_demux_decode(demux: *mut Demux, frame: *mut FrameInfo) -> c_int;
    fn mpc_demux_seek_sample(demux: *mut Demux, sample: u64) -> c_int;
    fn huff_init_lut(bits: c_int);
}

/// Таблицы Хаффмана общие, заполняем один раз.
fn init_tables() {
    static TABLES: Once = Once::new();
    TABLES.call_once(|| unsafe { huff_init_lut(6) });
}

/// # Safety
/// `reader` — [`Reader`] из [`Context`], `data` — живой [`BufReader`].
unsafe fn file<'a>(reader: *mut Reader) -> &'a mut BufReader<File> {
    unsafe { &mut *(*reader).data.cast::<BufReader<File>>() }
}

unsafe extern "C" fn read(reader: *mut Reader, data: *mut c_void, size: i32) -> i32 {
    let file = unsafe { file(reader) };
    let Ok(size) = usize::try_from(size) else {
        return 0;
    };
    if size == 0 || data.is_null() {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), size) };
    let mut done = 0;
    while done < size {
        match file.read(&mut out[done..]) {
            Ok(0) => break,
            Ok(count) => done += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    done as i32
}

unsafe extern "C" fn seek(reader: *mut Reader, offset: i32) -> u8 {
    let file = unsafe { file(reader) };
    u8::from(u64::try_from(offset).is_ok_and(|offset| file.seek(SeekFrom::Start(offset)).is_ok()))
}

unsafe extern "C" fn tell(reader: *mut Reader) -> i32 {
    let file = unsafe { file(reader) };
    file.stream_position().map_or(-1, |position| i32::try_from(position).unwrap_or(i32::MAX))
}

unsafe extern "C" fn get_size(reader: *mut Reader) -> i32 {
    let file = unsafe { file(reader) };
    file.get_ref().metadata().map_or(0, |metadata| i32::try_from(metadata.len()).unwrap_or(i32::MAX))
}

unsafe extern "C" fn canseek(_reader: *mut Reader) -> u8 {
    1
}

/// Открытый поток. Читатель и файл лежат в куче: библиотека хранит на них указатели.
struct Context {
    demux: NonNull<Demux>,
    _reader: Box<Reader>,
    _file: Box<BufReader<File>>,
}

// Всё принадлежит только этой структуре, а она — одному потоку за раз.
unsafe impl Send for Context {}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { mpc_demux_exit(self.demux.as_ptr()) };
    }
}

pub(super) fn open(mut file: File) -> Result<Opened, TrackError> {
    init_tables();
    let replay_gain = tags::replay_gain(&mut file, None);
    file.seek(SeekFrom::Start(0)).map_err(TrackError::Io)?;
    let mut source = Box::new(BufReader::new(file));
    let mut reader = Box::new(Reader {
        read,
        seek,
        tell,
        get_size,
        canseek,
        data: (&mut *source as *mut BufReader<File>).cast(),
    });
    let demux = unsafe { mpc_demux_init(&mut *reader) };
    let demux = NonNull::new(demux).ok_or_else(|| TrackError::Malformed("Musepack: заголовок не читается".into()))?;
    let context = Context { demux, _reader: reader, _file: source };
    let mut info = std::mem::MaybeUninit::<StreamInfo>::zeroed();
    unsafe { mpc_demux_get_info(context.demux.as_ptr(), info.as_mut_ptr()) };
    let info = unsafe { info.assume_init() };
    let channels = info.channels as usize;
    if !(1..=2).contains(&channels) || info.sample_freq == 0 {
        return Err(TrackError::Malformed(format!("Musepack: {channels} кан., {} Гц", info.sample_freq)));
    }
    let frames = info.samples.saturating_sub(info.beg_silence);
    let rate = info.sample_freq;
    let track = TrackInfo {
        codec: format!("mpc-sv{}", info.stream_version & 0xf),
        sample_rate: Some(rate),
        channels: Some(channels),
        bits_per_sample: None,
        duration: Some(std::time::Duration::from_secs_f64(frames as f64 / f64::from(rate))),
        replay_gain,
    };
    let stream = Musepack { context, channels, buffer: vec![0.0; BUFFER], ended: false };
    Ok(Opened { stream: Box::new(stream), info: track, layout: layout(channels, 0), frames: Some(frames), dsd_rate: None, id3v2: None })
}

struct Musepack {
    context: Context,
    channels: usize,
    buffer: Vec<f32>,
    /// Поток кончился (или сломался — для Musepack это одно и то же).
    ended: bool,
}

impl Stream for Musepack {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        while !self.ended {
            let mut frame = FrameInfo { samples: 0, bits: 0, buffer: self.buffer.as_mut_ptr(), is_key_frame: 0 };
            let status = unsafe { mpc_demux_decode(self.context.demux.as_ptr(), &mut frame) };
            if status != 0 || frame.bits == -1 {
                self.ended = true;
                break;
            }
            let count = (frame.samples as usize).min(FRAME * 2) * self.channels;
            if count > 0 {
                out.extend_from_slice(&self.buffer[..count.min(self.buffer.len())]);
                break;
            }
        }
        Ok(())
    }

    fn seek(&mut self, frame: u64) -> Result<u64, TrackError> {
        if unsafe { mpc_demux_seek_sample(self.context.demux.as_ptr(), frame) } != 0 {
            return Err(TrackError::Malformed("Musepack: перемотка не удалась".into()));
        }
        self.ended = false;
        Ok(frame)
    }
}
