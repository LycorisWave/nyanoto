//! WavPack на libwavpack 5.9.0 (`vendor/wavpack`).
//! Читаем через свои колбэки — любые пути в Windows и потом Android. `.wvc` открываем, если есть.
//! Библиотека считает битые блоки не все, поэтому проверяем ещё и сами (как `wvunpack -vv`) плюс MD5.
//! DSD внутри — как есть, дальше общий конвертер.

use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use symphonia::core::checksum::Md5;
use symphonia::core::io::Monitor;

use super::dsd::{DsdSource, DsdStream};
use super::{Opened, Stream, layout, tags};
use crate::source::{OpenOptions, TrackError, TrackInfo};

const OPEN_WVC: c_int = 0x1;
const OPEN_DSD_NATIVE: c_int = 0x100;
const MODE_LOSSLESS: c_int = 0x2;
const MODE_FLOAT: c_int = 0x8;
const MODE_MD5: c_int = 0x800;
/// Сколько сэмплов на канал декодируется за раз.
const BLOCK: usize = 4096;

#[repr(C)]
struct WavpackContext {
    _private: [u8; 0],
}

/// Колбэки чтения, `WavpackStreamReader64` из `wavpack.h`.
#[repr(C)]
struct Reader {
    read_bytes: unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> i32,
    write_bytes: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> i32>,
    get_pos: unsafe extern "C" fn(*mut c_void) -> i64,
    set_pos_abs: unsafe extern "C" fn(*mut c_void, i64) -> c_int,
    set_pos_rel: unsafe extern "C" fn(*mut c_void, i64, c_int) -> c_int,
    push_back_byte: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    get_length: unsafe extern "C" fn(*mut c_void) -> i64,
    can_seek: unsafe extern "C" fn(*mut c_void) -> c_int,
    truncate_here: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    close: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

unsafe extern "C" {
    fn WavpackOpenFileInputEx64(
        reader: *mut Reader,
        wv_id: *mut c_void,
        wvc_id: *mut c_void,
        error: *mut c_char,
        flags: c_int,
        norm_offset: c_int,
    ) -> *mut WavpackContext;
    fn WavpackCloseFile(wpc: *mut WavpackContext) -> *mut WavpackContext;
    fn WavpackGetMode(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetNumChannels(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetChannelMask(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetSampleRate(wpc: *mut WavpackContext) -> u32;
    fn WavpackGetNativeSampleRate(wpc: *mut WavpackContext) -> u32;
    fn WavpackGetBitsPerSample(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetBytesPerSample(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetFloatNormExp(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetNumSamples64(wpc: *mut WavpackContext) -> i64;
    fn WavpackGetNumErrors(wpc: *mut WavpackContext) -> c_int;
    fn WavpackGetMD5Sum(wpc: *mut WavpackContext, data: *mut u8) -> c_int;
    fn WavpackUnpackSamples(wpc: *mut WavpackContext, buffer: *mut i32, samples: u32) -> u32;
    fn WavpackSeekSample64(wpc: *mut WavpackContext, sample: i64) -> c_int;
    fn WavpackVerifySingleBlock(buffer: *mut u8, verify_checksum: c_int) -> c_int;
}

/// Файл для библиотеки: позиция и один «возвращённый» байт.
struct Source {
    file: BufReader<File>,
    pushed: Option<u8>,
}

impl Source {
    fn open(path: &Path) -> Option<Box<Self>> {
        File::open(path).ok().map(|file| Box::new(Self { file: BufReader::new(file), pushed: None }))
    }
}

/// # Safety
/// `id` — живой [`Source`] от [`Context::open`].
unsafe fn source<'a>(id: *mut c_void) -> &'a mut Source {
    unsafe { &mut *id.cast::<Source>() }
}

unsafe extern "C" fn read_bytes(id: *mut c_void, data: *mut c_void, count: i32) -> i32 {
    let source = unsafe { source(id) };
    let Ok(count) = usize::try_from(count) else {
        return 0;
    };
    if count == 0 || data.is_null() {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), count) };
    let mut done = 0;
    if let Some(byte) = source.pushed.take() {
        out[0] = byte;
        done = 1;
    }
    while done < count {
        match source.file.read(&mut out[done..]) {
            Ok(0) => break,
            Ok(read) => done += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    done as i32
}

unsafe extern "C" fn get_pos(id: *mut c_void) -> i64 {
    let source = unsafe { source(id) };
    match source.file.stream_position() {
        Ok(position) => position as i64 - i64::from(source.pushed.is_some()),
        Err(_) => -1,
    }
}

unsafe extern "C" fn set_pos_abs(id: *mut c_void, position: i64) -> c_int {
    let source = unsafe { source(id) };
    source.pushed = None;
    match u64::try_from(position).map(|position| source.file.seek(SeekFrom::Start(position))) {
        Ok(Ok(_)) => 0,
        _ => -1,
    }
}

unsafe extern "C" fn set_pos_rel(id: *mut c_void, delta: i64, mode: c_int) -> c_int {
    let source = unsafe { source(id) };
    // Возвращённый байт уже прочитан из файла, относительный сдвиг считается от него.
    let back = i64::from(source.pushed.take().is_some());
    let target = match mode {
        0 => u64::try_from(delta).map(SeekFrom::Start).ok(),
        1 => Some(SeekFrom::Current(delta - back)),
        2 => Some(SeekFrom::End(delta)),
        _ => None,
    };
    match target.map(|target| source.file.seek(target)) {
        Some(Ok(_)) => 0,
        _ => -1,
    }
}

unsafe extern "C" fn push_back_byte(id: *mut c_void, byte: c_int) -> c_int {
    let source = unsafe { source(id) };
    source.pushed = u8::try_from(byte).ok();
    byte
}

unsafe extern "C" fn get_length(id: *mut c_void) -> i64 {
    let source = unsafe { source(id) };
    source.file.get_ref().metadata().map_or(0, |metadata| metadata.len() as i64)
}

unsafe extern "C" fn can_seek(_id: *mut c_void) -> c_int {
    1
}

static READER: Reader = Reader {
    read_bytes,
    write_bytes: None,
    get_pos,
    set_pos_abs,
    set_pos_rel,
    push_back_byte,
    get_length,
    can_seek,
    truncate_here: None,
    close: None,
};

/// Открытый файл WavPack. Источники живут дольше контекста: он хранит на них указатели.
struct Context {
    wpc: NonNull<WavpackContext>,
    _main: Box<Source>,
    _correction: Option<Box<Source>>,
}

// Контекст и источники принадлежат только этой структуре, а она — одному потоку за раз.
unsafe impl Send for Context {}

impl Context {
    fn open(path: &Path) -> Result<Self, TrackError> {
        let mut main = Source::open(path).ok_or_else(|| TrackError::Io(std::io::Error::other("файл не открылся")))?;
        let mut correction = correction_file(path).and_then(|path| Source::open(&path));
        let mut flags = OPEN_DSD_NATIVE;
        if correction.is_some() {
            flags |= OPEN_WVC;
        }
        let mut error = [0 as c_char; 81];
        let main_id = (&mut *main as *mut Source).cast::<c_void>();
        let correction_id = correction
            .as_deref_mut()
            .map_or(std::ptr::null_mut(), |source| (source as *mut Source).cast::<c_void>());
        // Библиотека не меняет колбэки, указатель на неизменяемую статическую таблицу ей подходит.
        let reader = (&READER as *const Reader).cast_mut();
        let wpc = unsafe { WavpackOpenFileInputEx64(reader, main_id, correction_id, error.as_mut_ptr(), flags, 0) };
        let Some(wpc) = NonNull::new(wpc) else {
            let message = unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) }.to_string_lossy().into_owned();
            return Err(TrackError::Malformed(format!("WavPack: {message}")));
        };
        Ok(Self { wpc, _main: main, _correction: correction })
    }

    fn get(&self) -> *mut WavpackContext {
        self.wpc.as_ptr()
    }

    /// Распаковывает до `frames` кадров в `buffer` (по `channels` значений на кадр).
    fn unpack(&mut self, buffer: &mut Vec<i32>, frames: usize, channels: usize) -> usize {
        buffer.resize(frames * channels, 0);
        let unpacked = unsafe { WavpackUnpackSamples(self.get(), buffer.as_mut_ptr(), frames as u32) } as usize;
        buffer.truncate(unpacked * channels);
        unpacked
    }

    fn errors(&self) -> u64 {
        unsafe { WavpackGetNumErrors(self.get()) }.max(0) as u64
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { WavpackCloseFile(self.get()) };
    }
}

/// Блоки с неверной суммой (и обрезанный последний), как `wvunpack -vv`.
fn bad_blocks(path: &Path) -> u64 {
    [Some(path.to_path_buf()), correction_file(path)]
        .into_iter()
        .flatten()
        .filter_map(|path| File::open(path).ok())
        .map(|file| scan_blocks(BufReader::new(file)))
        .sum()
}

fn scan_blocks(mut file: BufReader<File>) -> u64 {
    let mut bad = 0;
    let mut header = [0u8; 32];
    'blocks: loop {
        // Блок начинается с «wvpk»; между блоками бывает мусор, в конце — теги.
        let mut window = [0u8; 4];
        let mut byte = [0u8; 1];
        while &window != b"wvpk" {
            if file.read_exact(&mut byte).is_err() {
                break 'blocks;
            }
            window.rotate_left(1);
            window[3] = byte[0];
        }
        header[..4].copy_from_slice(b"wvpk");
        if file.read_exact(&mut header[4..]).is_err() {
            break;
        }
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if !(24..=1 << 24).contains(&size) {
            continue;
        }
        let mut block = vec![0u8; size + 8];
        block[..32].copy_from_slice(&header);
        if file.read_exact(&mut block[32..]).is_err() {
            bad += 1;
            break;
        }
        if unsafe { WavpackVerifySingleBlock(block.as_mut_ptr(), 1) } == 0 {
            bad += 1;
        }
    }
    bad
}

/// `песня.wv` → `песня.wvc`, если такой файл есть.
fn correction_file(path: &Path) -> Option<PathBuf> {
    ["wvc", "WVC"].into_iter().map(|extension| path.with_extension(extension)).find(|path| path.is_file())
}

pub(super) fn open(path: &Path, mut file: File, options: OpenOptions) -> Result<Opened, TrackError> {
    let context = Context::open(path)?;
    let wpc = context.get();
    let (mode, channels, mask) =
        unsafe { (WavpackGetMode(wpc), WavpackGetNumChannels(wpc), WavpackGetChannelMask(wpc)) };
    let channels = usize::try_from(channels).ok().filter(|channels| *channels > 0).ok_or_else(|| {
        TrackError::Malformed("WavPack: нет каналов".into())
    })?;
    let total = u64::try_from(unsafe { WavpackGetNumSamples64(wpc) }).ok();
    let layout = layout(channels, mask as u32);
    let replay_gain = tags::replay_gain(&mut file, None);
    let native_rate = unsafe { WavpackGetNativeSampleRate(wpc) };
    let byte_rate = unsafe { WavpackGetSampleRate(wpc) };

    if native_rate != byte_rate {
        // DSD: «сэмпл» — байт из восьми отсчётов DSD.
        let verified_bad = if options.verify { bad_blocks(path) } else { 0 };
        let source = WavpackDsd { context, path: path.to_path_buf(), channels, buffer: Vec::new(), verified_bad };
        let stream = DsdStream::new(source, native_rate, channels, total)?;
        let info = TrackInfo {
            codec: "wavpack-dsd".into(),
            sample_rate: Some(stream.pcm_rate()),
            channels: Some(channels),
            bits_per_sample: Some(24),
            duration: total.map(|bytes| std::time::Duration::from_secs_f64(bytes as f64 * 8.0 / f64::from(native_rate))),
            replay_gain,
        };
        let frames = stream.frames();
        return Ok(Opened { stream: Box::new(stream), info, layout, frames, dsd_rate: Some(native_rate), id3v2: None });
    }

    let rate = byte_rate;
    let float = mode & MODE_FLOAT != 0;
    let bytes = unsafe { WavpackGetBytesPerSample(wpc) }.clamp(1, 4) as u32;
    let scale = if float {
        // Плавающая точка с другим диапазоном, чем ±1, приводится к ±1.
        2f32.powi(127 - unsafe { WavpackGetFloatNormExp(wpc) })
    } else {
        1.0 / (1u64 << (bytes * 8 - 1)) as f32
    };
    let bits = if float { 32 } else { unsafe { WavpackGetBitsPerSample(wpc) }.max(1) as u32 };
    let md5 = (options.verify && mode & MODE_MD5 != 0).then(Md5::default);
    let verified_bad = if options.verify { bad_blocks(path) } else { 0 };
    let info = TrackInfo {
        // Гибридный файл без своего .wvc звучит с потерями — так и называем.
        codec: if mode & MODE_LOSSLESS != 0 { "wavpack" } else { "wavpack-lossy" }.into(),
        sample_rate: Some(rate),
        channels: Some(channels),
        bits_per_sample: Some(bits),
        duration: total.map(|frames| std::time::Duration::from_secs_f64(frames as f64 / f64::from(rate.max(1)))),
        replay_gain,
    };
    let stream = WavPack {
        context: Some(context),
        path: path.to_path_buf(),
        channels,
        float,
        bytes,
        scale,
        md5,
        buffer: Vec::new(),
        errors_before: 0,
        verified_bad,
    };
    Ok(Opened { stream: Box::new(stream), info, layout, frames: total, dsd_rate: None, id3v2: None })
}

struct WavPack {
    /// `None` после неудачной перемотки — открыть заново.
    context: Option<Context>,
    path: PathBuf,
    channels: usize,
    float: bool,
    /// Байт на сэмпл в исходнике: по ним считается MD5.
    bytes: u32,
    scale: f32,
    /// MD5 звука в виде WAV, пока файл читается подряд с начала.
    md5: Option<Md5>,
    buffer: Vec<i32>,
    /// Ошибки прошлых открытий файла (после перемотки он открывается заново).
    errors_before: u64,
    /// Испорченные блоки по отдельной проверке (только при проверке целости).
    verified_bad: u64,
}

impl Stream for WavPack {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        let Some(context) = self.context.as_mut() else {
            return Ok(());
        };
        let unpacked = context.unpack(&mut self.buffer, BLOCK, self.channels);
        if unpacked == 0 {
            return Ok(());
        }
        if let Some(md5) = self.md5.as_mut() {
            md5_update(md5, &self.buffer, self.bytes);
        }
        if self.float {
            out.extend(self.buffer.iter().map(|bits| f32::from_bits(*bits as u32) * self.scale));
        } else {
            out.extend(self.buffer.iter().map(|value| *value as f32 * self.scale));
        }
        Ok(())
    }

    fn seek(&mut self, frame: u64) -> Result<u64, TrackError> {
        self.md5 = None;
        if let Some(context) = self.context.as_mut()
            && unsafe { WavpackSeekSample64(context.get(), frame as i64) } != 0
        {
            return Ok(frame);
        }
        // Неудачная перемотка губит контекст: открываем файл заново и пробуем ещё раз.
        self.errors_before += self.context.take().map_or(0, |context| context.errors());
        let context = Context::open(&self.path)?;
        if unsafe { WavpackSeekSample64(context.get(), frame as i64) } == 0 {
            return Err(TrackError::Malformed("WavPack: перемотка не удалась".into()));
        }
        self.context = Some(context);
        Ok(frame)
    }

    fn decode_errors(&self) -> u64 {
        let counted = self.errors_before + self.context.as_ref().map_or(0, Context::errors);
        counted.max(self.verified_bad)
    }

    fn finalize(&mut self) -> Option<bool> {
        let md5 = self.md5.take()?;
        let context = self.context.as_ref()?;
        let mut stored = [0u8; 16];
        if unsafe { WavpackGetMD5Sum(context.get(), stored.as_mut_ptr()) } == 0 {
            return None;
        }
        Some(md5.md5() == stored)
    }
}

/// Сэмплы как в WAV: little-endian, 8 бит без знака. Так считает и `wvunpack`.
fn md5_update(md5: &mut Md5, samples: &[i32], bytes: u32) {
    let mut raw = Vec::with_capacity(samples.len() * bytes as usize);
    for sample in samples {
        if bytes == 1 {
            raw.push((*sample + 0x80) as u8);
        } else {
            raw.extend_from_slice(&sample.to_le_bytes()[..bytes as usize]);
        }
    }
    md5.process_buf_bytes(&raw);
}

/// DSD из WavPack для общего конвертера: байты по каналам подряд, старший бит первый.
struct WavpackDsd {
    context: Context,
    path: PathBuf,
    channels: usize,
    buffer: Vec<i32>,
    verified_bad: u64,
}

impl DsdSource for WavpackDsd {
    fn read(&mut self, out: &mut Vec<u8>) -> Result<(), TrackError> {
        self.context.unpack(&mut self.buffer, BLOCK, self.channels);
        out.extend(self.buffer.iter().map(|byte| *byte as u8));
        Ok(())
    }

    fn seek(&mut self, byte: u64) -> Result<(), TrackError> {
        if unsafe { WavpackSeekSample64(self.context.get(), byte as i64) } != 0 {
            return Ok(());
        }
        let context = Context::open(&self.path)?;
        if unsafe { WavpackSeekSample64(context.get(), byte as i64) } == 0 {
            return Err(TrackError::Malformed("WavPack: перемотка не удалась".into()));
        }
        self.context = context;
        Ok(())
    }

    fn errors(&self) -> u64 {
        self.context.errors().max(self.verified_bad)
    }
}
