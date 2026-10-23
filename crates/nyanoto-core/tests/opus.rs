//! Opus на настоящих файлах от libopus: длина, форма, усиление, перемотка, 5.1.

use std::f32::consts::TAU;
use std::ffi::{c_int, c_void};
use std::path::Path;
use std::time::Duration;

use nyanoto_core::{OpenOptions, TrackDecoder, guard};
use nyanoto_testkit::Scratch;

const RATE: usize = 48_000;
/// Пакет 20 мс.
const FRAME: usize = 960;

unsafe extern "C" {
    fn opus_multistream_surround_encoder_create(
        rate: i32,
        channels: c_int,
        family: c_int,
        streams: *mut c_int,
        coupled_streams: *mut c_int,
        mapping: *mut u8,
        application: c_int,
        error: *mut c_int,
    ) -> *mut c_void;
    fn opus_multistream_encode_float(state: *mut c_void, pcm: *const f32, frames: c_int, data: *mut u8, max: i32) -> i32;
    fn opus_multistream_encoder_ctl(state: *mut c_void, request: c_int, ...) -> c_int;
    fn opus_multistream_encoder_destroy(state: *mut c_void);
}

const OPUS_APPLICATION_AUDIO: c_int = 2049;
const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
const OPUS_GET_LOOKAHEAD_REQUEST: c_int = 4027;

/// `samples` вперемежку по `channels`, `gain` — в 1/256 дБ.
fn encode(samples: &[f32], channels: usize, gain: i16) -> Vec<u8> {
    let frames = samples.len() / channels;
    let family = if channels > 2 { 1 } else { 0 };
    let (mut streams, mut coupled, mut mapping, mut error) = (0, 0, [0u8; 8], 0);
    let encoder = unsafe {
        opus_multistream_surround_encoder_create(
            RATE as i32,
            channels as c_int,
            family,
            &mut streams,
            &mut coupled,
            mapping.as_mut_ptr(),
            OPUS_APPLICATION_AUDIO,
            &mut error,
        )
    };
    assert!(error == 0 && !encoder.is_null(), "кодировщик не создан: {error}");
    let mut lookahead: c_int = 0;
    unsafe {
        opus_multistream_encoder_ctl(encoder, OPUS_SET_BITRATE_REQUEST, 96_000 * channels as c_int);
        opus_multistream_encoder_ctl(encoder, OPUS_GET_LOOKAHEAD_REQUEST, &mut lookahead as *mut c_int);
    }
    let pre_skip = lookahead as usize;

    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, channels as u8]);
    head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
    head.extend_from_slice(&(RATE as u32).to_le_bytes());
    head.extend_from_slice(&gain.to_le_bytes());
    head.push(family as u8);
    if family == 1 {
        head.extend_from_slice(&[streams as u8, coupled as u8]);
        head.extend_from_slice(&mapping[..channels]);
    }
    let mut tags = b"OpusTags".to_vec();
    let vendor = b"nyanoto";
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());

    let mut ogg = Ogg::default();
    ogg.page(0x02, 0, &head);
    ogg.page(0, 0, &tags);
    // Кодировщик отстаёт на pre-skip — дописываем тишину и закрываем точной длиной.
    let mut padded = samples.to_vec();
    padded.resize((frames + pre_skip).div_ceil(FRAME) * FRAME * channels, 0.0);
    let packets = padded.len() / channels / FRAME;
    let mut packet = vec![0u8; 4000];
    for index in 0..packets {
        let pcm = &padded[index * FRAME * channels..];
        let len = unsafe {
            opus_multistream_encode_float(encoder, pcm.as_ptr(), FRAME as c_int, packet.as_mut_ptr(), packet.len() as i32)
        };
        assert!(len > 0, "пакет не закодирован: {len}");
        let last = index + 1 == packets;
        // Позиция страницы — сколько отсчётов декодировано к её концу, вместе с pre-skip.
        let granule = if last { pre_skip + frames } else { (index + 1) * FRAME };
        ogg.page(if last { 0x04 } else { 0 }, granule as u64, &packet[..len as usize]);
    }
    unsafe { opus_multistream_encoder_destroy(encoder) };
    ogg.bytes
}

/// Страницы Ogg: по одному пакету на страницу.
#[derive(Default)]
struct Ogg {
    bytes: Vec<u8>,
    sequence: u32,
}

impl Ogg {
    fn page(&mut self, flags: u8, granule: u64, packet: &[u8]) {
        let mut lacing = vec![255u8; packet.len() / 255];
        lacing.push((packet.len() % 255) as u8);
        let start = self.bytes.len();
        self.bytes.extend_from_slice(b"OggS");
        self.bytes.extend_from_slice(&[0, flags]);
        self.bytes.extend_from_slice(&granule.to_le_bytes());
        self.bytes.extend_from_slice(&0x6E656B6Fu32.to_le_bytes());
        self.bytes.extend_from_slice(&self.sequence.to_le_bytes());
        self.bytes.extend_from_slice(&[0; 4]);
        self.bytes.push(lacing.len() as u8);
        self.bytes.extend_from_slice(&lacing);
        self.bytes.extend_from_slice(packet);
        let crc = crc32(&self.bytes[start..]);
        self.bytes[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

fn tone(frames: usize, frequencies: &[f32], level: f32) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * frequencies.len());
    for frame in 0..frames {
        for &frequency in frequencies {
            samples.push(level * (TAU * frequency * frame as f32 / RATE as f32).sin());
        }
    }
    samples
}

struct Decoded {
    samples: Vec<f32>,
    channels: usize,
    duration: Option<Duration>,
}

fn decode(path: &Path, seek: Option<Duration>) -> (Decoded, Option<Duration>) {
    guard(|| {
        let mut decoder = TrackDecoder::open(path, OpenOptions::default())?;
        let landed = seek.map(|position| decoder.seek(position)).transpose()?;
        let (mut samples, mut channels) = (Vec::new(), 0);
        while let Some(chunk) = decoder.next_chunk()? {
            assert_eq!(chunk.rate, 48_000);
            channels = chunk.channels.count();
            samples.extend_from_slice(chunk.samples);
        }
        assert_eq!(decoder.decode_errors(), 0);
        Ok((Decoded { samples, channels, duration: decoder.info().duration }, landed))
    })
    .unwrap()
}

/// Отношение сигнал/ошибка в децибелах между двумя одноканальными отрезками.
fn snr(reference: &[f32], decoded: &[f32]) -> f32 {
    let signal: f32 = reference.iter().map(|x| x * x).sum();
    let noise: f32 = reference.iter().zip(decoded).map(|(x, y)| (x - y) * (x - y)).sum();
    10.0 * (signal / noise.max(1e-12)).log10()
}

fn channel(samples: &[f32], channels: usize, index: usize) -> Vec<f32> {
    samples.iter().skip(index).step_by(channels).copied().collect()
}

#[test]
fn length_is_exact_and_waveform_lines_up() {
    let scratch = Scratch::new("opus-exact");
    // Не кратно пакету: проверяется и обрезка конца.
    let frames = RATE * 2 + 1_234;
    let source = tone(frames, &[440.0, 1_000.0], 0.5);
    let path = scratch.write("a.opus", &encode(&source, 2, 0));
    let (decoded, _) = decode(&path, None);

    assert_eq!(decoded.channels, 2);
    assert_eq!(decoded.samples.len(), frames * 2, "лишние или потерянные отсчёты");
    let duration = decoded.duration.expect("длительность из заголовка");
    assert!(duration.abs_diff(Duration::from_secs_f64(frames as f64 / RATE as f64)) < Duration::from_millis(1), "{duration:?}");
    // Форма сигнала совпадает без сдвига: pre-skip выброшен ровно.
    let middle = RATE / 10..frames - RATE / 10;
    for index in 0..2 {
        let (left, right) = (channel(&source, 2, index), channel(&decoded.samples, 2, index));
        let quality = snr(&left[middle.clone()], &right[middle.clone()]);
        assert!(quality > 20.0, "канал {index}: {quality:.1} дБ");
        let shifted = snr(&left[middle.clone()], &right[middle.start + 1..middle.end + 1]);
        assert!(shifted < quality, "сигнал сдвинут: {shifted:.1} ≥ {quality:.1}");
    }
}

#[test]
fn header_gain_is_applied() {
    let scratch = Scratch::new("opus-gain");
    let source = tone(RATE, &[1_000.0], 0.5);
    let plain = scratch.write("plain.opus", &encode(&source, 1, 0));
    // −6,02 дБ — вдвое тише.
    let quiet = scratch.write("quiet.opus", &encode(&source, 1, -1_541));
    let rms = |path: &Path| {
        let samples = decode(path, None).0.samples;
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    };
    let ratio = rms(&quiet) / rms(&plain);
    assert!((ratio - 0.5).abs() < 0.01, "{ratio}");
}

#[test]
fn seek_lands_on_the_sample_and_sounds_right_at_once() {
    let scratch = Scratch::new("opus-seek");
    let frames = RATE * 3;
    let source = tone(frames, &[700.0, 1_300.0], 0.5);
    let path = scratch.write("a.opus", &encode(&source, 2, 0));
    let at = Duration::from_millis(1_500);
    let (decoded, landed) = decode(&path, Some(at));

    assert_eq!(landed, Some(at));
    let from = RATE * 3 / 2;
    assert_eq!(decoded.samples.len(), (frames - from) * 2);
    // Уже первые 20 мс после перемотки как исходник (без разгона было 1 дБ, с 80 мс — 15).
    let first = channel(&source, 2, 0)[from..from + FRAME].to_vec();
    let quality = snr(&first, &channel(&decoded.samples, 2, 0)[..FRAME]);
    assert!(quality > 35.0, "{quality:.1} дБ");
}

#[test]
fn surround_channels_keep_their_places() {
    let scratch = Scratch::new("opus-51");
    // Порядок Opus: L C R RL RR LFE; у каждого канала свой тон.
    let tones = [400.0, 600.0, 800.0, 1_000.0, 1_200.0, 60.0];
    let path = scratch.write("a.opus", &encode(&tone(RATE, &tones, 0.4), 6, 0));
    let (decoded, _) = decode(&path, None);

    assert_eq!(decoded.channels, 6);
    // На выходе порядок Symphonia: L R C LFE RL RR.
    let expected = [400.0, 800.0, 600.0, 60.0, 1_000.0, 1_200.0];
    for (index, frequency) in expected.into_iter().enumerate() {
        let samples = channel(&decoded.samples, 6, index);
        let loudest = tones
            .iter()
            .copied()
            .max_by(|a, b| power(&samples, *a).total_cmp(&power(&samples, *b)))
            .unwrap();
        assert_eq!(loudest, frequency, "канал {index}");
    }
}

/// Мощность частоты в сигнале (алгоритм Гёрцеля).
fn power(samples: &[f32], frequency: f32) -> f32 {
    let coefficient = 2.0 * (TAU * frequency / RATE as f32).cos();
    let (mut previous, mut before) = (0.0f32, 0.0f32);
    for &sample in samples {
        let value = sample + coefficient * previous - before;
        before = previous;
        previous = value;
    }
    previous * previous + before * before - coefficient * previous * before
}


/// WebM от FFmpeg: `ffmpeg -i ff_source.wav -c:a libopus -b:a 96k opus.webm`.
/// Хвост по DiscardPadding — ровно 48 000 кадров.
#[test]
fn webm_ends_exactly_where_the_source_ended() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opus.webm");
    let (decoded, _) = decode(&path, None);
    assert_eq!(decoded.samples.len() / decoded.channels, 48_000);
    // Метки времени в Matroska — целые миллисекунды, отсюда и точность длины.
    let duration = decoded.duration.unwrap().as_secs_f64();
    assert!((duration - 1.0).abs() <= 0.0015, "{duration}");
}
