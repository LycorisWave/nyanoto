//! Monkey's Audio от самого SDK: бит в бит на всех уровнях, 8–32 бит, float, 5.1, MD5, порча, перемотка.

mod common;

use std::ffi::{c_int, c_void};
use std::time::Duration;

use common::{assert_same, decode};
use nyanoto_core::{OpenOptions, TrackDecoder, TrackError};
use nyanoto_testkit::{Rng, Scratch};
use symphonia::core::audio::Channels;

const RATE: u32 = 44_100;

const FAST: c_int = 1000;
const NORMAL: c_int = 2000;
const HIGH: c_int = 3000;
const EXTRA_HIGH: c_int = 4000;
const INSANE: c_int = 5000;

#[repr(C)]
struct Io {
    user: *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, *mut c_void, i64) -> i64,
    write: Option<unsafe extern "C" fn(*mut c_void, *const c_void, i64) -> i64>,
    seek: unsafe extern "C" fn(*mut c_void, i64, c_int) -> c_int,
    position: unsafe extern "C" fn(*mut c_void) -> i64,
    size: unsafe extern "C" fn(*mut c_void) -> i64,
}

unsafe extern "C" {
    fn nyanoto_ape_compress(
        out: *const Io,
        pcm: *const u8,
        bytes: i64,
        rate: c_int,
        channels: c_int,
        bits: c_int,
        is_float: c_int,
        level: c_int,
        header: *const u8,
        header_bytes: i64,
    ) -> c_int;
}

/// Файл в памяти, куда пишет сжатие (оно возвращается и дописывает заголовок).
#[derive(Default)]
struct Memory {
    data: Vec<u8>,
    at: usize,
}

unsafe fn memory<'a>(user: *mut c_void) -> &'a mut Memory {
    unsafe { &mut *user.cast::<Memory>() }
}

unsafe extern "C" fn read(user: *mut c_void, buffer: *mut c_void, bytes: i64) -> i64 {
    let memory = unsafe { memory(user) };
    let count = (bytes as usize).min(memory.data.len().saturating_sub(memory.at));
    unsafe { std::ptr::copy_nonoverlapping(memory.data.as_ptr().add(memory.at), buffer.cast(), count) };
    memory.at += count;
    count as i64
}

unsafe extern "C" fn write(user: *mut c_void, buffer: *const c_void, bytes: i64) -> i64 {
    let memory = unsafe { memory(user) };
    let source = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), bytes as usize) };
    let end = memory.at + source.len();
    if memory.data.len() < end {
        memory.data.resize(end, 0);
    }
    memory.data[memory.at..end].copy_from_slice(source);
    memory.at = end;
    bytes
}

unsafe extern "C" fn seek(user: *mut c_void, position: i64, whence: c_int) -> c_int {
    let memory = unsafe { memory(user) };
    let base = match whence {
        0 => 0,
        1 => memory.at as i64,
        _ => memory.data.len() as i64,
    };
    memory.at = (base + position).max(0) as usize;
    0
}

unsafe extern "C" fn position(user: *mut c_void) -> i64 {
    unsafe { memory(user) }.at as i64
}

unsafe extern "C" fn size(user: *mut c_void) -> i64 {
    unsafe { memory(user) }.data.len() as i64
}

/// Звук в раскладке WAV и как его ждать от плеера.
struct Pcm {
    bytes: Vec<u8>,
    want: Vec<f32>,
    channels: usize,
    bits: u32,
    float: bool,
}

fn pcm(frames: usize, channels: usize, bits: u32, float: bool) -> Pcm {
    let mut rng = Rng(0xA5A5_5A5A_1234_4321);
    let (mut bytes, mut want) = (Vec::new(), Vec::new());
    for frame in 0..frames {
        for channel in 0..channels {
            let phase = frame as f64 * (180.0 + 90.0 * channel as f64) * std::f64::consts::TAU / f64::from(RATE);
            let noise = (rng.below(2001) as f64 - 1000.0) / 1000.0;
            let value = phase.sin() * 0.7 + noise * 0.01;
            if float {
                let sample = (value * 1.5) as f32;
                bytes.extend_from_slice(&sample.to_le_bytes());
                want.push(sample);
                continue;
            }
            let scale = (1u64 << (bits - 1)) as f64;
            let sample = (value * scale) as i64;
            match bits {
                8 => {
                    bytes.push((sample + 128) as u8);
                    want.push(sample as f32 / 128.0);
                }
                16 => {
                    bytes.extend_from_slice(&(sample as i16).to_le_bytes());
                    want.push(sample as f32 / 32_768.0);
                }
                24 => {
                    bytes.extend_from_slice(&(sample as i32).to_le_bytes()[..3]);
                    want.push(sample as f32 / 8_388_608.0);
                }
                _ => {
                    bytes.extend_from_slice(&(sample as i32).to_le_bytes());
                    want.push(sample as i32 as f32 / 2_147_483_648.0);
                }
            }
        }
    }
    Pcm { bytes, want, channels, bits, float }
}

fn compress(pcm: &Pcm, level: c_int, header: Option<&[u8]>) -> Vec<u8> {
    let mut memory = Memory::default();
    let io = Io { user: (&mut memory as *mut Memory).cast(), read, write: Some(write), seek, position, size };
    let (header, header_bytes) = header.map_or((std::ptr::null(), 0), |header| (header.as_ptr(), header.len() as i64));
    let result = unsafe {
        nyanoto_ape_compress(
            &io,
            pcm.bytes.as_ptr(),
            pcm.bytes.len() as i64,
            RATE as c_int,
            pcm.channels as c_int,
            pcm.bits as c_int,
            c_int::from(pcm.float),
            level,
            header,
            header_bytes,
        )
    };
    assert_eq!(result, 0, "сжатие не удалось");
    memory.data
}

#[test]
fn every_level_decodes_bit_exact_and_md5_matches() {
    let source = pcm(60_000, 2, 16, false);
    let scratch = Scratch::new("ape-levels");
    for level in [FAST, NORMAL, HIGH, EXTRA_HIGH, INSANE] {
        let path = scratch.write(&format!("{level}.ape"), &compress(&source, level, None));
        let decoded = decode(&path).unwrap();
        assert_same(&decoded.samples, &source.want, &format!("уровень {level}"));
        assert_eq!(decoded.errors, 0);
        assert_eq!(decoded.checksum, Some(true), "уровень {level}: MD5");
        assert_eq!(decoded.info.codec, "ape");
        assert_eq!(decoded.info.bits_per_sample, Some(16));
        assert_eq!(decoded.missing, None);
    }
}

#[test]
fn bit_depths_and_floats_decode_bit_exact() {
    let scratch = Scratch::new("ape-depths");
    for (bits, channels, float) in [(8, 2, false), (24, 1, false), (32, 2, false), (32, 2, true)] {
        let source = pcm(30_000, channels, bits, float);
        let path = scratch.write(&format!("{bits}-{float}.ape"), &compress(&source, HIGH, None));
        let decoded = decode(&path).unwrap();
        assert_same(&decoded.samples, &source.want, &format!("{bits} бит, float {float}"));
        assert_eq!(decoded.checksum, Some(true));
        assert_eq!(decoded.info.bits_per_sample, Some(bits));
        assert_eq!(decoded.info.channels, Some(channels));
    }
}

/// Заголовок WAV с WAVE_FORMAT_EXTENSIBLE до начала данных.
fn extensible_header(channels: u16, bits: u16, mask: u32, data_bytes: u32) -> Vec<u8> {
    let block = channels * bits / 8;
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&RATE.to_le_bytes());
    fmt.extend_from_slice(&(RATE * u32::from(block)).to_le_bytes());
    fmt.extend_from_slice(&block.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    fmt.extend_from_slice(&22u16.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    fmt.extend_from_slice(&mask.to_le_bytes());
    fmt.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
    let mut header = b"RIFF".to_vec();
    header.extend_from_slice(&(4 + 8 + fmt.len() as u32 + 8 + data_bytes).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    header.extend_from_slice(&fmt);
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_bytes.to_le_bytes());
    header
}

#[test]
fn surround_layout_comes_from_the_stored_wav_header() {
    let mask = 0x60f;
    let source = pcm(10_000, 6, 16, false);
    let header = extensible_header(6, 16, mask, source.bytes.len() as u32);
    let scratch = Scratch::new("ape-51");
    let path = scratch.write("51.ape", &compress(&source, NORMAL, Some(&header)));
    let mut decoder = TrackDecoder::open(&path, OpenOptions::default()).unwrap();
    assert_eq!(decoder.next_chunk().unwrap().unwrap().channels, Channels::from_bits_truncate(mask));
    assert_same(&decode(&path).unwrap().samples, &source.want, "5.1");
}

#[test]
fn damaged_frame_is_found_and_md5_fails() {
    let source = pcm(400_000, 2, 16, false);
    let mut file = compress(&source, NORMAL, None);
    let middle = file.len() / 2;
    file[middle] ^= 0x5A;
    let scratch = Scratch::new("ape-damage");
    let decoded = decode(&scratch.write("damaged.ape", &file)).unwrap();
    assert_eq!(decoded.samples.len(), source.want.len(), "длина та же");
    assert!(decoded.errors > 0, "повреждение замечено");
    assert_eq!(decoded.checksum, Some(false));
}

#[test]
fn seek_lands_on_the_sample() {
    let source = pcm(300_000, 2, 16, false);
    let scratch = Scratch::new("ape-seek");
    let path = scratch.write("seek.ape", &compress(&source, EXTRA_HIGH, None));
    let mut decoder = TrackDecoder::open(&path, OpenOptions::default()).unwrap();
    for frame in [250_001usize, 9, 147_456, 299_999] {
        let landed = decoder.seek(Duration::from_secs_f64(frame as f64 / f64::from(RATE))).unwrap();
        assert_eq!((landed.as_secs_f64() * f64::from(RATE)).round() as usize, frame);
        let chunk = decoder.next_chunk().unwrap().unwrap();
        let count = chunk.samples.len().min(200);
        assert_same(&chunk.samples[..count], &source.want[frame * 2..frame * 2 + count], "после перемотки");
    }
}

#[test]
fn mutated_ape_never_panics() {
    let source = pcm(20_000, 2, 16, false);
    let original = compress(&source, HIGH, None);
    let scratch = Scratch::new("ape-fuzz");
    let mut rng = Rng(13);
    for round in 0..200 {
        let mut bytes = original.clone();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(bytes.len());
            bytes[at] = rng.next() as u8;
        }
        if round % 10 == 0 {
            bytes.truncate(rng.below(bytes.len()));
        }
        if let Err(TrackError::Panicked(message)) = decode(&scratch.write("fuzz.ape", &bytes)) {
            panic!("раунд {round}: паника {message}");
        }
    }
}
