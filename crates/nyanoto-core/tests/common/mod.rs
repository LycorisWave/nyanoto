//! Общее для тестов форматов: декодирование целиком, кодировщик WavPack, DSD в DSF и DFF.

// Каждый тестовый файл берёт отсюда своё.
#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void};
use std::path::Path;
use std::time::Duration;

use nyanoto_core::{OpenOptions, TrackDecoder, TrackError, TrackInfo, guard};

pub struct Decoded {
    pub samples: Vec<f32>,
    pub errors: u64,
    pub info: TrackInfo,
    pub missing: Option<Duration>,
    pub checksum: Option<bool>,
    pub rate: u32,
}

pub fn decode(path: &Path) -> Result<Decoded, TrackError> {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions { verify: true })?;
        let mut samples = Vec::new();
        let mut rate = 0;
        while let Some(chunk) = decoder.next_chunk()? {
            samples.extend_from_slice(chunk.samples);
            rate = chunk.rate;
            assert!(samples.len() < 100_000_000, "декодер не останавливается");
        }
        Ok(Decoded {
            samples,
            errors: decoder.decode_errors(),
            info: decoder.info().clone(),
            missing: decoder.missing(),
            checksum: decoder.finalize(),
            rate,
        })
    })
}

/// Первое расхождение, чтобы по ошибке было видно, где именно.
pub fn assert_same(got: &[f32], want: &[f32], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: длина");
    if let Some(index) = got.iter().zip(want).position(|(a, b)| a.to_bits() != b.to_bits()) {
        panic!("{what}: сэмпл {index}: {} вместо {}", got[index], want[index]);
    }
}

// ───────────────────────────── WavPack ─────────────────────────────

/// `WavpackConfig` из `wavpack.h`.
#[repr(C)]
#[derive(Default)]
struct Config {
    bitrate: f32,
    shaping_weight: f32,
    bits_per_sample: c_int,
    bytes_per_sample: c_int,
    qmode: c_int,
    flags: c_int,
    xmode: c_int,
    num_channels: c_int,
    float_norm_exp: c_int,
    block_samples: i32,
    worker_threads: i32,
    sample_rate: i32,
    channel_mask: i32,
    md5_checksum: [u8; 16],
    md5_read: u8,
    num_tag_strings: c_int,
    tag_strings: usize,
}

unsafe extern "C" {
    fn WavpackOpenFileOutput(
        blockout: unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> c_int,
        wv_id: *mut c_void,
        wvc_id: *mut c_void,
    ) -> *mut c_void;
    fn WavpackSetConfiguration64(wpc: *mut c_void, config: *mut Config, total: i64, ids: *const u8) -> c_int;
    fn WavpackPackInit(wpc: *mut c_void) -> c_int;
    fn WavpackPackSamples(wpc: *mut c_void, buffer: *mut i32, count: u32) -> c_int;
    fn WavpackFlushSamples(wpc: *mut c_void) -> c_int;
    fn WavpackStoreMD5Sum(wpc: *mut c_void, data: *mut u8) -> c_int;
    fn WavpackGetErrorMessage(wpc: *mut c_void) -> *const c_char;
    fn WavpackCloseFile(wpc: *mut c_void) -> *mut c_void;
}

unsafe extern "C" fn write_block(id: *mut c_void, data: *mut c_void, count: i32) -> c_int {
    let out = unsafe { &mut *id.cast::<Vec<u8>>() };
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count as usize) });
    1
}

pub const CONFIG_HYBRID_FLAG: c_int = 8;
pub const CONFIG_CREATE_WVC: c_int = 0x80000;
pub const CONFIG_MD5_CHECKSUM: c_int = 0x800_0000;
const QMODE_DSD_MSB_FIRST: c_int = 0x20;

/// Что и как кодировать в WavPack.
pub struct WavpackSource<'a> {
    /// Целые сэмплы (или биты f32 при `float`), каналы подряд.
    pub samples: &'a [i32],
    pub channels: usize,
    pub rate: u32,
    pub bits: u32,
    pub mask: u32,
    pub float: bool,
    /// Гибрид: бит на сэмпл в основном файле и файл коррекции рядом.
    pub hybrid: Option<f32>,
    /// DSD: `samples` — байты (старший бит первый), `rate` — частота DSD.
    pub dsd: bool,
}

pub struct Wavpack {
    pub wv: Vec<u8>,
    pub wvc: Option<Vec<u8>>,
}

pub fn encode_wavpack(source: &WavpackSource) -> Wavpack {
    let mut wv = Vec::new();
    let mut wvc = Vec::new();
    let wvc_id = if source.hybrid.is_some() { (&mut wvc as *mut Vec<u8>).cast() } else { std::ptr::null_mut() };
    let wpc = unsafe { WavpackOpenFileOutput(write_block, (&mut wv as *mut Vec<u8>).cast(), wvc_id) };
    assert!(!wpc.is_null());
    let bytes = if source.dsd { 1 } else { source.bits.div_ceil(8) };
    let mut config = Config {
        bits_per_sample: if source.dsd { 8 } else { source.bits as c_int },
        bytes_per_sample: bytes as c_int,
        num_channels: source.channels as c_int,
        channel_mask: source.mask as i32,
        sample_rate: if source.dsd { (source.rate / 8) as i32 } else { source.rate as i32 },
        float_norm_exp: if source.float { 127 } else { 0 },
        qmode: if source.dsd { QMODE_DSD_MSB_FIRST } else { 0 },
        flags: CONFIG_MD5_CHECKSUM,
        ..Config::default()
    };
    if let Some(bits) = source.hybrid {
        config.flags |= CONFIG_HYBRID_FLAG | CONFIG_CREATE_WVC;
        config.bitrate = bits;
    }
    let frames = source.samples.len() / source.channels;
    unsafe {
        assert_ne!(WavpackSetConfiguration64(wpc, &mut config, frames as i64, std::ptr::null()), 0, "{}", error(wpc));
        assert_ne!(WavpackPackInit(wpc), 0, "{}", error(wpc));
        let mut buffer = source.samples.to_vec();
        assert_ne!(WavpackPackSamples(wpc, buffer.as_mut_ptr(), frames as u32), 0, "{}", error(wpc));
        assert_ne!(WavpackFlushSamples(wpc), 0, "{}", error(wpc));
        let mut md5 = wav_md5(source.samples, bytes, source.dsd);
        WavpackStoreMD5Sum(wpc, md5.as_mut_ptr());
        assert_ne!(WavpackFlushSamples(wpc), 0, "{}", error(wpc));
        WavpackCloseFile(wpc);
    }
    Wavpack { wv, wvc: source.hybrid.map(|_| wvc) }
}

unsafe fn error(wpc: *mut c_void) -> String {
    unsafe { std::ffi::CStr::from_ptr(WavpackGetErrorMessage(wpc)) }.to_string_lossy().into_owned()
}

/// MD5 звука в виде WAV, как его хранит `wavpack`.
fn wav_md5(samples: &[i32], bytes: u32, dsd: bool) -> [u8; 16] {
    use symphonia::core::checksum::Md5;
    use symphonia::core::io::Monitor;
    let mut md5 = Md5::default();
    let mut raw = Vec::new();
    for sample in samples {
        if bytes == 1 && !dsd {
            raw.push((*sample + 0x80) as u8);
        } else {
            raw.extend_from_slice(&sample.to_le_bytes()[..bytes as usize]);
        }
    }
    md5.process_buf_bytes(&raw);
    md5.md5()
}

// ─────────────────────────────── DSD ───────────────────────────────

/// DSD64 и выше — 64 × 44,1 кГц.
pub const DSD64: u32 = 2_822_400;

/// Синусы в однобитный поток модулятором 2-го порядка. Байты каналов подряд, старший бит первый.
pub fn dsd_tones(rate: u32, seconds: f64, tones: &[(f64, f64)]) -> Vec<u8> {
    let channels = tones.len();
    let bytes = (f64::from(rate) * seconds / 8.0) as usize;
    let mut errors = vec![(0.0f64, 0.0f64); channels];
    let mut out = vec![0u8; bytes * channels];
    for byte in 0..bytes {
        for (channel, (frequency, amplitude)) in tones.iter().enumerate() {
            let mut value = 0u8;
            for bit in 0..8 {
                let index = byte * 8 + bit;
                let x = amplitude * (std::f64::consts::TAU * frequency * index as f64 / f64::from(rate)).sin();
                let (e1, e2) = errors[channel];
                let u = x - 2.0 * e1 + e2;
                let y = if u >= 0.0 { 1.0 } else { -1.0 };
                errors[channel] = (y - u, e1);
                value = value << 1 | u8::from(y > 0.0);
            }
            out[byte * channels + channel] = value;
        }
    }
    out
}

/// DSF: блоки по 4096 байт на канал, младший бит — первый.
pub fn write_dsf(bytes: &[u8], channels: usize, rate: u32) -> Vec<u8> {
    const BLOCK: usize = 4096;
    let per_channel = bytes.len() / channels;
    let groups = per_channel.div_ceil(BLOCK);
    let mut data = vec![0u8; groups * BLOCK * channels];
    for index in 0..per_channel {
        for channel in 0..channels {
            let (group, within) = (index / BLOCK, index % BLOCK);
            data[(group * channels + channel) * BLOCK + within] = bytes[index * channels + channel].reverse_bits();
        }
    }
    let mut file = b"DSD ".to_vec();
    file.extend_from_slice(&28u64.to_le_bytes());
    file.extend_from_slice(&((92 + data.len()) as u64).to_le_bytes());
    file.extend_from_slice(&0u64.to_le_bytes());
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&52u64.to_le_bytes());
    file.extend_from_slice(&1u32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&(if channels == 2 { 2u32 } else { 1 }).to_le_bytes());
    file.extend_from_slice(&(channels as u32).to_le_bytes());
    file.extend_from_slice(&rate.to_le_bytes());
    file.extend_from_slice(&1u32.to_le_bytes());
    file.extend_from_slice(&(per_channel as u64 * 8).to_le_bytes());
    file.extend_from_slice(&(BLOCK as u32).to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&((12 + data.len()) as u64).to_le_bytes());
    file.extend_from_slice(&data);
    file
}

/// DSDIFF: каналы вперемежку, `order` — имена в порядке файла.
pub fn write_dff(bytes: &[u8], order: &[&[u8; 4]], rate: u32) -> Vec<u8> {
    let chunk = |id: &[u8; 4], body: &[u8]| {
        let mut out = id.to_vec();
        out.extend_from_slice(&(body.len() as u64).to_be_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    };
    let mut channels = (order.len() as u16).to_be_bytes().to_vec();
    for id in order {
        channels.extend_from_slice(*id);
    }
    let mut prop = b"SND ".to_vec();
    prop.extend(chunk(b"FS  ", &rate.to_be_bytes()));
    prop.extend(chunk(b"CHNL", &channels));
    prop.extend(chunk(b"CMPR", b"DSD \x0enot compressed\x00"));
    let mut form = b"DSD ".to_vec();
    form.extend(chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
    form.extend(chunk(b"PROP", &prop));
    form.extend(chunk(b"DSD ", bytes));
    chunk(b"FRM8", &form)
}
