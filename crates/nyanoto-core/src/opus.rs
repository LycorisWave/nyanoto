//! Декодер Opus для Symphonia на libopus (`vendor/opus`).
//! Из OpusHead — каналы, усиление и pre-skip (разгон кодировщика). Звук всегда 48 кГц.

use std::ffi::c_int;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::ptr::NonNull;

use symphonia::core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia::core::codecs::{
    CODEC_TYPE_OPUS, CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult,
};
use symphonia::core::errors::{Result, decode_error, unsupported_error};
use symphonia::core::formats::Packet;
use symphonia::core::support_codec;
use symphonia::core::units::TimeBase;

/// Частота, на которой Opus всегда отдаёт звук.
pub const OPUS_RATE: u32 = 48_000;
/// Самый длинный пакет Opus — 120 мс.
const MAX_PACKET_FRAMES: usize = 5_760;

/// Метка пакета в отсчётах 48 кГц (в Matroska — мс).
pub fn opus_samples(ts: u64, time_base: Option<TimeBase>) -> u64 {
    match time_base {
        Some(base) if base.denom != 0 => {
            let samples = u128::from(ts) * u128::from(base.numer) * u128::from(OPUS_RATE) / u128::from(base.denom);
            u64::try_from(samples).unwrap_or(u64::MAX)
        }
        _ => ts,
    }
}

/// Конец звука Opus в Matroska/WebM по DiscardPadding последнего блока.
/// Symphonia 0.5 его выбрасывает — читаем хвост сами. `None` — не режем.
pub fn matroska_end(path: &Path, track: u32, time_base: TimeBase) -> Option<u64> {
    const CLUSTER: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
    // Кластер в Opus — несколько секунд звука; 4 МБ хватает с большим запасом.
    const TAIL: u64 = 4 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic).ok()?;
    if magic != [0x1A, 0x45, 0xDF, 0xA3] {
        return None;
    }
    let length = file.metadata().ok()?.len();
    let tail = length.min(TAIL);
    file.seek(SeekFrom::Start(length - tail)).ok()?;
    let mut buffer = vec![0; usize::try_from(tail).ok()?];
    file.read_exact(&mut buffer).ok()?;

    let start = buffer.windows(4).rposition(|window| window == CLUSTER)?;
    let (size, size_len, unknown) = ebml_size(&buffer[start + 4..])?;
    let data = start + 4 + size_len;
    let end = if unknown { buffer.len() } else { usize::try_from(size).ok()?.saturating_add(data).min(buffer.len()) };
    let mut cluster_time = None;
    let mut last: Option<(i16, &[u8], i64)> = None;
    for (id, body) in ebml_children(&buffer[data..end]) {
        match id {
            0xE7 => cluster_time = Some(ebml_uint(body)),
            0xA3 => {
                if let Some((time, payload)) = matroska_block(body, track) {
                    last = Some((time, payload, 0));
                }
            }
            0xA0 => {
                let (mut block, mut padding) = (None, 0);
                for (id, body) in ebml_children(body) {
                    match id {
                        0xA1 => block = matroska_block(body, track),
                        0x75A2 => padding = ebml_int(body),
                        _ => {}
                    }
                }
                if let Some((time, payload)) = block {
                    last = Some((time, payload, padding));
                }
            }
            _ => {}
        }
    }
    let (relative, payload, padding_ns) = last?;
    if padding_ns <= 0 || payload.is_empty() {
        return None;
    }
    let time = u64::try_from(i64::try_from(cluster_time?).ok()? + i64::from(relative)).ok()?;
    // Длина пакета — по его заголовку (TOC); декодировать не нужно.
    let frames = unsafe { opus_packet_get_nb_samples(payload.as_ptr(), i32::try_from(payload.len()).ok()?, OPUS_RATE as i32) };
    let frames = u64::try_from(frames).ok().filter(|frames| *frames > 0)?;
    let padding = (padding_ns as u128 * u128::from(OPUS_RATE)).div_ceil(1_000_000_000);
    (opus_samples(time, Some(time_base)) + frames).checked_sub(u64::try_from(padding).ok()?)
}

/// Размер элемента EBML: значение, длина записи и «размер неизвестен».
fn ebml_size(bytes: &[u8]) -> Option<(u64, usize, bool)> {
    let first = *bytes.first()?;
    let length = first.leading_zeros() as usize + 1;
    if length > 8 || bytes.len() < length {
        return None;
    }
    let mut value = u64::from(first) & (0xFF >> length);
    for byte in &bytes[1..length] {
        value = (value << 8) | u64::from(*byte);
    }
    let unknown = value == (1u64 << (7 * length)) - 1;
    Some((value, length, unknown))
}

/// Номер элемента EBML (с битами длины, как в спецификации: 0xA3, 0x75A2).
fn ebml_id(bytes: &[u8]) -> Option<(u32, usize)> {
    let first = *bytes.first()?;
    let length = first.leading_zeros() as usize + 1;
    if length > 4 || bytes.len() < length {
        return None;
    }
    Some((bytes[..length].iter().fold(0u32, |id, byte| (id << 8) | u32::from(*byte)), length))
}

/// Дочерние элементы: номер и содержимое. Оборванный элемент заканчивает перебор.
fn ebml_children(mut bytes: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    std::iter::from_fn(move || {
        let (id, id_length) = ebml_id(bytes)?;
        let (size, size_length, unknown) = ebml_size(&bytes[id_length..])?;
        let body = id_length + size_length;
        let end = if unknown { bytes.len() } else { body.checked_add(usize::try_from(size).ok()?)? };
        if end > bytes.len() {
            return None;
        }
        let item = (id, &bytes[body..end]);
        bytes = &bytes[end..];
        Some(item)
    })
}

fn ebml_uint(bytes: &[u8]) -> u64 {
    bytes.iter().take(8).fold(0, |value, byte| (value << 8) | u64::from(*byte))
}

fn ebml_int(bytes: &[u8]) -> i64 {
    let Some(first) = bytes.first() else {
        return 0;
    };
    let start: i64 = if first & 0x80 != 0 { -1 } else { 0 };
    bytes.iter().take(8).fold(start, |value, byte| (value << 8) | i64::from(*byte))
}

/// Блок дорожки: время от кластера и данные. С lacing не берём — Opus его не использует.
fn matroska_block(body: &[u8], track: u32) -> Option<(i16, &[u8])> {
    let (number, length, _) = ebml_size(body)?;
    if number != u64::from(track) || body.len() < length + 3 {
        return None;
    }
    let time = i16::from_be_bytes([body[length], body[length + 1]]);
    let flags = body[length + 2];
    (flags & 0x06 == 0).then(|| (time, &body[length + 3..]))
}

#[repr(C)]
struct OpusMsDecoder {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn opus_multistream_decoder_create(
        rate: i32,
        channels: c_int,
        streams: c_int,
        coupled_streams: c_int,
        mapping: *const u8,
        error: *mut c_int,
    ) -> *mut OpusMsDecoder;
    fn opus_multistream_decode_float(
        state: *mut OpusMsDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut f32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    fn opus_multistream_decoder_ctl(state: *mut OpusMsDecoder, request: c_int, ...) -> c_int;
    fn opus_multistream_decoder_destroy(state: *mut OpusMsDecoder);
    fn opus_packet_get_nb_samples(packet: *const u8, len: i32, rate: i32) -> c_int;
}

const OPUS_RESET_STATE: c_int = 4028;
const OPUS_SET_GAIN_REQUEST: c_int = 4034;

/// Что сказано в заголовке OpusHead (RFC 7845, раздел 5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusHead {
    pub channels: usize,
    /// Сколько отсчётов 48 кГц в начале потока выбросить.
    pub pre_skip: u64,
    /// Усиление в единицах 1/256 дБ, которое обязан применить декодер.
    pub gain: i16,
    pub streams: usize,
    pub coupled: usize,
    /// Для каждого выходного канала — номер канала в потоках (255 — тишина).
    pub mapping: Vec<u8>,
    pub family: u8,
}

impl OpusHead {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(b"OpusHead")?;
        let (&version, rest) = rest.split_first()?;
        // Старшие четыре бита — несовместимая версия.
        if version >> 4 != 0 || rest.len() < 10 {
            return None;
        }
        let channels = usize::from(rest[0]);
        let pre_skip = u64::from(u16::from_le_bytes([rest[1], rest[2]]));
        let gain = i16::from_le_bytes([rest[7], rest[8]]);
        let family = rest[9];
        if channels == 0 {
            return None;
        }
        let (streams, coupled, mapping) = if family == 0 {
            if channels > 2 {
                return None;
            }
            (1, channels - 1, (0..channels as u8).collect())
        } else {
            let table = rest.get(10..12 + channels)?;
            let (streams, coupled) = (usize::from(table[0]), usize::from(table[1]));
            let mapping = table[2..].to_vec();
            let decoded = streams + coupled;
            if streams == 0 || coupled > streams || decoded > 255 {
                return None;
            }
            if mapping.iter().any(|&index| index != 255 && usize::from(index) >= decoded) {
                return None;
            }
            (streams, coupled, mapping)
        };
        Some(Self { channels, pre_skip, gain, streams, coupled, mapping, family })
    }
}

/// Каналы в порядке Vorbis, для раскладок 0 и 1 — как у Ogg в Symphonia.
fn vorbis_order(channels: usize) -> Option<Vec<Channels>> {
    use Channels as C;
    Some(match channels {
        1 => vec![C::FRONT_LEFT],
        2 => vec![C::FRONT_LEFT, C::FRONT_RIGHT],
        3 => vec![C::FRONT_LEFT, C::FRONT_CENTRE, C::FRONT_RIGHT],
        4 => vec![C::FRONT_LEFT, C::FRONT_RIGHT, C::REAR_LEFT, C::REAR_RIGHT],
        5 => vec![C::FRONT_LEFT, C::FRONT_CENTRE, C::FRONT_RIGHT, C::REAR_LEFT, C::REAR_RIGHT],
        6 => vec![C::FRONT_LEFT, C::FRONT_CENTRE, C::FRONT_RIGHT, C::REAR_LEFT, C::REAR_RIGHT, C::LFE1],
        7 => vec![
            C::FRONT_LEFT,
            C::FRONT_CENTRE,
            C::FRONT_RIGHT,
            C::SIDE_LEFT,
            C::SIDE_RIGHT,
            C::REAR_CENTRE,
            C::LFE1,
        ],
        8 => vec![
            C::FRONT_LEFT,
            C::FRONT_CENTRE,
            C::FRONT_RIGHT,
            C::SIDE_LEFT,
            C::SIDE_RIGHT,
            C::REAR_LEFT,
            C::REAR_RIGHT,
            C::LFE1,
        ],
        _ => return None,
    })
}

/// Меняется только через `&mut` — между потоками можно.
struct State(NonNull<OpusMsDecoder>);

unsafe impl Send for State {}
unsafe impl Sync for State {}

impl Drop for State {
    fn drop(&mut self) {
        // Состояние создано `opus_multistream_decoder_create` и больше нигде не живёт.
        unsafe { opus_multistream_decoder_destroy(self.0.as_ptr()) }
    }
}

pub struct OpusDecoder {
    params: CodecParameters,
    state: State,
    head: OpusHead,
    /// Для каждого канала Opus — номер плоскости в буфере Symphonia.
    planes: Vec<usize>,
    pcm: Vec<f32>,
    buffer: AudioBuffer<f32>,
}

impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self> {
        let Some(head) = params.extra_data.as_deref().and_then(OpusHead::parse) else {
            return decode_error("opus: неверный заголовок OpusHead");
        };
        if head.family > 1 {
            return unsupported_error("opus: раскладка каналов кроме 0 и 1");
        }
        let Some(order) = vorbis_order(head.channels) else {
            return unsupported_error("opus: больше 8 каналов");
        };
        let layout = order.iter().fold(Channels::empty(), |all, &channel| all | channel);
        let planes = order
            .iter()
            .map(|channel| (layout.bits() & (channel.bits() - 1)).count_ones() as usize)
            .collect();

        let mut error: c_int = 0;
        // Таблица раскладки проверена в `OpusHead::parse` и живёт до конца вызова.
        let raw = unsafe {
            opus_multistream_decoder_create(
                OPUS_RATE as i32,
                head.channels as c_int,
                head.streams as c_int,
                head.coupled as c_int,
                head.mapping.as_ptr(),
                &mut error,
            )
        };
        let Some(state) = NonNull::new(raw).filter(|_| error == 0).map(State) else {
            return decode_error("opus: libopus не создала декодер");
        };
        if head.gain != 0 {
            // Усиление из заголовка применяет сама libopus, с её точностью.
            unsafe {
                opus_multistream_decoder_ctl(state.0.as_ptr(), OPUS_SET_GAIN_REQUEST, c_int::from(head.gain));
            }
        }
        let spec = SignalSpec::new(OPUS_RATE, layout);
        Ok(Self {
            params: params.clone(),
            state,
            pcm: vec![0.0; MAX_PACKET_FRAMES * head.channels],
            buffer: AudioBuffer::new(MAX_PACKET_FRAMES as u64, spec),
            planes,
            head,
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_OPUS, "opus", "Opus (libopus)")]
    }

    fn reset(&mut self) {
        // Сброс только обнуляет внутреннее состояние декодера.
        unsafe {
            opus_multistream_decoder_ctl(self.state.0.as_ptr(), OPUS_RESET_STATE);
        }
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        self.buffer.clear();
        let data = packet.buf();
        let Ok(len) = i32::try_from(data.len()) else {
            return decode_error("opus: пакет слишком большой");
        };
        // Буфер `pcm` вмещает самый длинный пакет Opus на все каналы.
        let frames = unsafe {
            opus_multistream_decode_float(
                self.state.0.as_ptr(),
                data.as_ptr(),
                len,
                self.pcm.as_mut_ptr(),
                MAX_PACKET_FRAMES as c_int,
                0,
            )
        };
        let Ok(frames) = usize::try_from(frames) else {
            return decode_error("opus: пакет не декодируется");
        };
        self.buffer.render_reserved(Some(frames));
        let channels = self.head.channels;
        for (channel, &plane) in self.planes.iter().enumerate() {
            let out = self.buffer.chan_mut(plane);
            for (frame, sample) in out.iter_mut().enumerate() {
                *sample = self.pcm[frame * channels + channel];
            }
        }
        // До pre-skip — разгон, конец режет демультиплексор.
        let position = opus_samples(packet.ts(), self.params.time_base);
        let pre_skip = self.head.pre_skip.saturating_sub(position).min(frames as u64) as usize;
        let start = (pre_skip + packet.trim_start() as usize).min(frames);
        let end = (packet.trim_end() as usize).min(frames - start);
        self.buffer.trim(start, end);
        Ok(self.buffer.as_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buffer.as_audio_buffer_ref()
    }
}

/// Все декодеры Symphonia и наш Opus.
pub fn codecs() -> &'static symphonia::core::codecs::CodecRegistry {
    static REGISTRY: std::sync::OnceLock<symphonia::core::codecs::CodecRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = symphonia::core::codecs::CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_all::<OpusDecoder>();
        registry
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(channels: u8, family: u8, table: &[u8]) -> Vec<u8> {
        let mut bytes = b"OpusHead".to_vec();
        bytes.extend_from_slice(&[1, channels]);
        bytes.extend_from_slice(&312u16.to_le_bytes());
        bytes.extend_from_slice(&44_100u32.to_le_bytes());
        bytes.extend_from_slice(&(-256i16).to_le_bytes());
        bytes.push(family);
        bytes.extend_from_slice(table);
        bytes
    }

    #[test]
    fn stereo_header() {
        let head = OpusHead::parse(&header(2, 0, &[])).unwrap();
        assert_eq!((head.channels, head.pre_skip, head.gain), (2, 312, -256));
        assert_eq!((head.streams, head.coupled, head.mapping), (1, 1, vec![0, 1]));
    }

    #[test]
    fn surround_header_and_bad_tables() {
        // 5.1: четыре потока, два из них стерео.
        let head = OpusHead::parse(&header(6, 1, &[4, 2, 0, 4, 1, 2, 3, 5])).unwrap();
        assert_eq!((head.streams, head.coupled), (4, 2));
        // Номер канала за пределами потоков и стерео в раскладке 0 с тремя каналами.
        assert!(OpusHead::parse(&header(6, 1, &[4, 2, 0, 4, 1, 2, 3, 9])).is_none());
        assert!(OpusHead::parse(&header(3, 0, &[])).is_none());
        assert!(OpusHead::parse(b"OpusHea").is_none());
    }

    #[test]
    fn surround_channels_land_on_their_planes() {
        // Порядок Opus: L C R RL RR LFE; плоскости Symphonia: L R C LFE RL RR.
        let order = vorbis_order(6).unwrap();
        let layout = order.iter().fold(Channels::empty(), |all, &channel| all | channel);
        let planes: Vec<usize> =
            order.iter().map(|channel| (layout.bits() & (channel.bits() - 1)).count_ones() as usize).collect();
        assert_eq!(planes, vec![0, 2, 1, 4, 5, 3]);
    }
}
