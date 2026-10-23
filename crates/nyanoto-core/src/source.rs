//! Открытие и декодирование файла.
//!
//! Главное правило: файл не может уронить плеер. Битые пакеты — тишина,
//! паника декодера ловится в [`guard`].

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use symphonia::core::audio::{Channels, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{CODEC_TYPE_AAC, CODEC_TYPE_FLAC, CODEC_TYPE_NULL, CODEC_TYPE_OPUS, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, Packet, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Tag};
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use crate::opus::{OPUS_RATE, OpusHead, matroska_end, opus_samples};

/// Разгон Opus перед точкой перемотки. RFC просит 80 мс, но чисто становится только к ~200 мс.
const OPUS_PREROLL_FRAMES: u64 = OPUS_RATE as u64 * 200 / 1000;

/// Потолок ошибок подряд, иначе мусорный файл крутит декодер вечно.
const MAX_ERRORS_IN_ROW: u32 = 2048;

/// Потолок тишины на битый пакет: длительности из файла верить нельзя.
const MAX_SILENCE_FRAMES: u64 = 1 << 17;

/// Потолок тишины на дыру: скачок позиции лучше минут тишины.
const MAX_GAP_SECONDS: u64 = 10;

/// Тишина на месте дыры отдаётся кусками такого размера.
const GAP_CHUNK_FRAMES: u64 = 4096;

#[derive(Debug)]
pub enum TrackError {
    Io(std::io::Error),
    /// Формат или кодек не поддерживается.
    Unsupported(String),
    /// Файл повреждён настолько, что играть его нельзя.
    Malformed(String),
    NoAudioTrack,
    /// Декодер запаниковал. Плеер продолжает работу, но это ошибка в коде.
    Panicked(String),
    /// Не удалось записать результат конвертации.
    Write(std::io::Error),
}

impl fmt::Display for TrackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackError::Io(error) => write!(f, "ошибка чтения: {error}"),
            TrackError::Unsupported(what) => write!(f, "не поддерживается: {what}"),
            TrackError::Malformed(what) => write!(f, "файл повреждён: {what}"),
            TrackError::NoAudioTrack => write!(f, "в файле нет звуковой дорожки"),
            TrackError::Panicked(what) => write!(f, "сбой декодера: {what}"),
            TrackError::Write(error) => write!(f, "ошибка записи: {error}"),
        }
    }
}

impl std::error::Error for TrackError {}

impl From<SymError> for TrackError {
    fn from(error: SymError) -> Self {
        match error {
            SymError::IoError(error) => TrackError::Io(error),
            SymError::Unsupported(what) => TrackError::Unsupported(what.to_string()),
            SymError::DecodeError(what) => TrackError::Malformed(what.to_string()),
            SymError::LimitError(what) => TrackError::Malformed(what.to_string()),
            SymError::SeekError(kind) => TrackError::Malformed(format!("перемотка: {kind:?}")),
            SymError::ResetRequired => TrackError::Malformed("поток требует сброса".into()),
        }
    }
}

/// Выполняет работу с файлом и превращает панику в [`TrackError::Panicked`].
pub fn guard<T>(work: impl FnOnce() -> Result<T, TrackError>) -> Result<T, TrackError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| text.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "причина неизвестна".into());
            Err(TrackError::Panicked(message))
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<usize>,
    pub bits_per_sample: Option<u32>,
    pub duration: Option<Duration>,
    pub replay_gain: ReplayGain,
}

/// ReplayGain: поправки в дБ и пики (1.0 — полная шкала).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReplayGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

impl ReplayGain {
    pub(crate) fn read(&mut self, tags: &[Tag]) {
        for tag in tags {
            let key = match tag.std_key {
                Some(key) => key,
                None => match tag.key.to_ascii_uppercase().as_str() {
                    "REPLAYGAIN_TRACK_GAIN" => StandardTagKey::ReplayGainTrackGain,
                    "REPLAYGAIN_TRACK_PEAK" => StandardTagKey::ReplayGainTrackPeak,
                    "REPLAYGAIN_ALBUM_GAIN" => StandardTagKey::ReplayGainAlbumGain,
                    "REPLAYGAIN_ALBUM_PEAK" => StandardTagKey::ReplayGainAlbumPeak,
                    _ => continue,
                },
            };
            // «-6.20 dB», «+1.3 dB», «0.988»: число в начале строки.
            let text = tag.value.to_string();
            let number = text
                .trim()
                .trim_end_matches(|char: char| char.is_ascii_alphabetic() || char.is_whitespace())
                .replace(',', ".")
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite());
            match key {
                StandardTagKey::ReplayGainTrackGain => self.track_gain_db = number.or(self.track_gain_db),
                StandardTagKey::ReplayGainTrackPeak => self.track_peak = number.or(self.track_peak),
                StandardTagKey::ReplayGainAlbumGain => self.album_gain_db = number.or(self.album_gain_db),
                StandardTagKey::ReplayGainAlbumPeak => self.album_peak = number.or(self.album_peak),
                _ => {}
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOptions {
    /// Сверять контрольную сумму потока, если формат её хранит (MD5 во FLAC).
    pub verify: bool,
}

/// Кусок декодированного звука: сэмплы идут кадрами по `channels.count()`.
pub struct Chunk<'a> {
    pub samples: &'a [f32],
    pub rate: u32,
    pub channels: Channels,
}

/// Сплошная нумерация сэмплов (FLAC): битые кадры демультиплексор выкидывает молча,
/// пропажу видно только по скачку номера.
struct Continuity {
    /// Номер сэмпла, с которого должен начаться следующий пакет.
    expected: Option<u64>,
    /// Сколько сэмплов в потоке по его заголовку.
    total: Option<u64>,
    /// Сколько кадров тишины осталось отдать на месте текущей дыры.
    gap_left: u64,
    /// Пакет, прочитанный за дырой: он ждёт, пока уйдёт тишина.
    held: Option<Packet>,
    /// Сколько кадров не хватает в конце файла.
    missing: u64,
}

/// Наибольший кусок от [`crate::native`]: там кадры целиком (у TTA — по секунде).
const NATIVE_CHUNK_FRAMES: usize = 4096;

/// Открытый файл: декодирует, перематывает, считает повреждения.
pub struct TrackDecoder {
    inner: Inner,
}

enum Inner {
    Symphonia(Box<SymphoniaDecoder>),
    Native(NativeDecoder),
}

impl TrackDecoder {
    /// Свои форматы (WavPack, APE, Musepack, TTA, DSD) — по первым байтам, остальное — Symphonia,
    /// не вышло — FFmpeg. Если никто не смог, возвращаем первую ошибку.
    pub fn open(path: &Path, options: OpenOptions) -> Result<Self, TrackError> {
        let first = match crate::native::open(path, options) {
            Some(Ok(opened)) => return Ok(Self { inner: Inner::Native(NativeDecoder::new(opened)) }),
            // Свой формат, но вариант не поддерживается (DST в DFF) — к FFmpeg.
            Some(Err(TrackError::Unsupported(what))) => TrackError::Unsupported(what),
            Some(Err(error)) => return Err(error),
            None => match SymphoniaDecoder::open(path, options) {
                Ok(decoder) => {
                    // HE-AAC Symphonia играет без SBR/PS — глухо, а v2 в моно. FFmpeg умеет целиком.
                    if decoder.needs_sbr
                        && let Ok(opened) = crate::native::open_ffmpeg(path)
                    {
                        return Ok(Self { inner: Inner::Native(NativeDecoder::new(opened)) });
                    }
                    return Ok(Self { inner: Inner::Symphonia(Box::new(decoder)) });
                }
                Err(error) => error,
            },
        };
        // Нечего открывать — нечего и пробовать.
        if let TrackError::Io(error) = &first
            && error.kind() != std::io::ErrorKind::UnexpectedEof
        {
            return Err(first);
        }
        match crate::native::open_ffmpeg(path) {
            Ok(opened) => Ok(Self { inner: Inner::Native(NativeDecoder::new(opened)) }),
            Err(_) => Err(first),
        }
    }

    pub fn info(&self) -> &TrackInfo {
        match &self.inner {
            Inner::Symphonia(decoder) => decoder.info(),
            Inner::Native(decoder) => &decoder.info,
        }
    }

    /// Сколько раз звук заменили тишиной.
    pub fn decode_errors(&self) -> u64 {
        match &self.inner {
            Inner::Symphonia(decoder) => decoder.decode_errors(),
            Inner::Native(decoder) => decoder.stream.decode_errors(),
        }
    }

    /// Сколько кадров уже отдано, включая тишину на месте повреждений.
    pub fn frames(&self) -> u64 {
        match &self.inner {
            Inner::Symphonia(decoder) => decoder.frames(),
            Inner::Native(decoder) => decoder.frames,
        }
    }

    /// Сколько звука не хватает в конце (известно, когда файл дочитан).
    pub fn missing(&self) -> Option<Duration> {
        match &self.inner {
            Inner::Symphonia(decoder) => decoder.missing(),
            Inner::Native(decoder) => decoder.missing(),
        }
    }

    /// Следующий кусок звука или `None`, когда файл закончился.
    pub fn next_chunk(&mut self) -> Result<Option<Chunk<'_>>, TrackError> {
        match &mut self.inner {
            Inner::Symphonia(decoder) => decoder.next_chunk(),
            Inner::Native(decoder) => decoder.next_chunk(),
        }
    }

    /// Перематывает на `position` и возвращает точку, с которой пойдёт звук.
    pub fn seek(&mut self, position: Duration) -> Result<Duration, TrackError> {
        match &mut self.inner {
            Inner::Symphonia(decoder) => decoder.seek(position),
            Inner::Native(decoder) => decoder.seek(position),
        }
    }

    /// `Some(false)` — контрольная сумма не сошлась, `None` — не проверяли.
    pub fn finalize(&mut self) -> Option<bool> {
        match &mut self.inner {
            Inner::Symphonia(decoder) => decoder.finalize(),
            Inner::Native(decoder) => decoder.stream.finalize(),
        }
    }
}

/// Файл, который декодирует свой код или эталонная библиотека формата.
struct NativeDecoder {
    stream: Box<dyn crate::native::Stream>,
    info: TrackInfo,
    layout: Channels,
    rate: u32,
    /// Сколько кадров в файле по заголовку.
    total: Option<u64>,
    /// Декодированный, но ещё не отданный звук и сколько из него уже отдано.
    pending: Vec<f32>,
    served: usize,
    /// Номер кадра, с которого начнётся следующий кусок.
    position: u64,
    frames: u64,
    /// Файл дочитан до конца.
    ended: bool,
}

impl NativeDecoder {
    fn new(opened: crate::native::Opened) -> Self {
        let rate = opened.info.sample_rate.unwrap_or(44_100).max(1);
        Self {
            stream: opened.stream,
            info: opened.info,
            layout: opened.layout,
            rate,
            total: opened.frames,
            pending: Vec::new(),
            served: 0,
            position: 0,
            frames: 0,
            ended: false,
        }
    }

    fn next_chunk(&mut self) -> Result<Option<Chunk<'_>>, TrackError> {
        let channels = self.layout.count().max(1);
        if self.served >= self.pending.len() {
            self.pending.clear();
            self.served = 0;
            self.stream.read(&mut self.pending)?;
            if self.pending.is_empty() {
                self.ended = true;
                return Ok(None);
            }
        }
        let end = self.pending.len().min(self.served + NATIVE_CHUNK_FRAMES * channels);
        let samples = &self.pending[self.served..end];
        self.served = end;
        let frames = (samples.len() / channels) as u64;
        self.position += frames;
        self.frames += frames;
        Ok(Some(Chunk {
            samples,
            rate: self.rate,
            channels: self.layout,
        }))
    }

    fn seek(&mut self, position: Duration) -> Result<Duration, TrackError> {
        let mut frame = (position.as_secs_f64() * f64::from(self.rate)).round() as u64;
        if let Some(total) = self.total {
            frame = frame.min(total);
        }
        let landed = self.stream.seek(frame)?;
        self.pending.clear();
        self.served = 0;
        self.position = landed;
        self.ended = false;
        Ok(Duration::from_secs_f64(landed as f64 / f64::from(self.rate)))
    }

    fn missing(&self) -> Option<Duration> {
        let missing = self.total?.saturating_sub(self.position);
        (self.ended && missing > 0).then(|| Duration::from_secs_f64(missing as f64 / f64::from(self.rate)))
    }
}

/// Декодер на Symphonia: контейнеры и кодеки, которые она знает, плюс Opus.
struct SymphoniaDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    verify: bool,
    info: TrackInfo,
    buffer: Option<SampleBuffer<f32>>,
    buffer_spec: Option<SignalSpec>,
    buffer_frames: usize,
    silence: Vec<f32>,
    /// Формат последнего декодированного пакета.
    spec: Option<SignalSpec>,
    /// Формат из заголовка: на случай тишины до первого пакета.
    header_spec: Option<SignalSpec>,
    continuity: Option<Continuity>,
    /// Сколько кадров выбросить после перемотки: декодер встаёт раньше.
    discard: u64,
    decode_errors: u64,
    frames: u64,
    /// Opus: разгон кодировщика в начале. В метках он есть, в звуке — нет.
    opus_pre_skip: Option<u64>,
    /// У Opus в Matroska/WebM: где кончается звук (см. [`matroska_end`]).
    opus_end: Option<u64>,
    /// AAC с SBR/PS (HE-AAC): Symphonia сыграет его не полностью.
    needs_sbr: bool,
}

/// HE-AAC: тип объекта 5/29 или синхрослово 0x2B7. Спрятанный SBR не узнать,
/// но тогда частота не выше 24 кГц — так музыку не кодируют.
fn needs_sbr(params: &CodecParameters) -> bool {
    if params.codec != CODEC_TYPE_AAC {
        return false;
    }
    if params.sample_rate.is_some_and(|rate| rate <= 24_000) {
        return true;
    }
    let Some(config) = params.extra_data.as_deref() else {
        return false;
    };
    let mut bits = Bits { bytes: config, at: 0 };
    let object = |bits: &mut Bits| match bits.read(5) {
        Some(31) => bits.read(6).map(|extended| 32 + extended),
        other => other,
    };
    let rate = |bits: &mut Bits| match bits.read(4) {
        Some(15) => bits.read(24).map(|_| 15),
        other => other,
    };
    let Some(kind) = object(&mut bits) else {
        return false;
    };
    if kind == 5 || kind == 29 {
        return true;
    }
    // AAC-LC: частота, каналы, GASpecificConfig, затем, может быть, расширение.
    if kind != 2 || rate(&mut bits).is_none() || bits.read(4).is_none() {
        return false;
    }
    let (Some(_frame), Some(core), Some(_extension)) = (bits.read(1), bits.read(1), bits.read(1)) else {
        return false;
    };
    if core == 1 && bits.read(14).is_none() {
        return false;
    }
    bits.read(11) == Some(0x2B7) && object(&mut bits) == Some(5) && bits.read(1) == Some(1)
}

/// Чтение битов старшими вперёд.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            let byte = *self.bytes.get(self.at / 8)?;
            value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Some(value)
    }
}

impl SymphoniaDecoder {
    fn open(path: &Path, options: OpenOptions) -> Result<Self, TrackError> {
        let file = File::open(path).map_err(TrackError::Io)?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());

        // Расширение — только подсказка: формат определяется по содержимому.
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
            hint.with_extension(extension);
        }
        let format_options = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let mut probed = symphonia::default::get_probe().format(
            &hint,
            stream,
            &format_options,
            &MetadataOptions::default(),
        )?;
        // Теги лежат и перед контейнером (ID3), и в нём самом (Vorbis comments).
        let mut replay_gain = ReplayGain::default();
        if let Some(metadata) = probed.metadata.get()
            && let Some(revision) = metadata.current()
        {
            replay_gain.read(revision.tags());
        }
        let mut format = probed.format;
        if let Some(revision) = format.metadata().current() {
            replay_gain.read(revision.tags());
        }

        let track = format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(TrackError::NoAudioTrack)?;
        let params = track.codec_params.clone();
        let track_id = track.id;

        let codecs = crate::opus::codecs();
        let decoder = codecs.make(
            &params,
            &DecoderOptions {
                verify: options.verify,
            },
        )?;

        let opus_pre_skip = (params.codec == CODEC_TYPE_OPUS)
            .then(|| params.extra_data.as_deref().and_then(OpusHead::parse).map_or(0, |head| head.pre_skip));
        // В Matroska/WebM метки — в долях секунды (обычно мс), в Ogg — в отсчётах.
        let opus_end = match (opus_pre_skip, params.time_base) {
            (Some(_), Some(time_base)) if time_base.denom == 1_000_000_000 => {
                matroska_end(path, track_id, time_base)
            }
            _ => None,
        };
        // У Opus длина потока включает pre-skip, который не звучит.
        let unheard = Duration::from_secs_f64(opus_pre_skip.unwrap_or(0) as f64 / f64::from(OPUS_RATE));
        let duration = match (opus_end, params.n_frames, params.time_base) {
            (Some(end), _, _) => Some(Duration::from_secs_f64(end as f64 / f64::from(OPUS_RATE)).saturating_sub(unheard)),
            (None, Some(frames), Some(time_base)) => {
                Some(time_to_duration(time_base.calc_time(frames)).saturating_sub(unheard))
            }
            _ => None,
        };
        let info = TrackInfo {
            codec: codecs
                .get_codec(params.codec)
                .map(|descriptor| descriptor.short_name.to_string())
                .unwrap_or_else(|| "?".into()),
            sample_rate: params.sample_rate,
            channels: params.channels.map(|channels| channels.count()),
            bits_per_sample: params.bits_per_sample,
            duration,
            replay_gain,
        };
        let header_spec = match (params.sample_rate, params.channels) {
            (Some(rate), Some(channels)) => Some(SignalSpec { rate, channels }),
            _ => None,
        };
        // Нумерации сэмплов верим, только если единица времени потока — один сэмпл.
        let counts_samples = params
            .time_base
            .zip(params.sample_rate)
            .is_some_and(|(time_base, rate)| time_base.numer == 1 && time_base.denom == rate);
        let continuity = (params.codec == CODEC_TYPE_FLAC && counts_samples).then_some(Continuity {
            expected: Some(0),
            total: params.n_frames,
            gap_left: 0,
            held: None,
            missing: 0,
        });

        Ok(Self {
            format,
            decoder,
            track_id,
            time_base: params.time_base,
            verify: options.verify,
            info,
            buffer: None,
            buffer_spec: None,
            buffer_frames: 0,
            silence: Vec::new(),
            spec: None,
            header_spec,
            continuity,
            discard: 0,
            decode_errors: 0,
            frames: 0,
            opus_pre_skip,
            opus_end,
            needs_sbr: needs_sbr(&params),
        })
    }

    fn info(&self) -> &TrackInfo {
        &self.info
    }

    /// Сколько раз звук заменили тишиной.
    fn decode_errors(&self) -> u64 {
        self.decode_errors
    }

    /// Сколько кадров уже отдано, включая тишину на месте повреждений.
    fn frames(&self) -> u64 {
        self.frames
    }

    /// Сколько звука не хватает в конце (известно, когда файл дочитан).
    fn missing(&self) -> Option<Duration> {
        let missing = self.continuity.as_ref()?.missing;
        let rate = self.info.sample_rate.filter(|rate| *rate > 0)?;
        (missing > 0).then(|| Duration::from_secs_f64(missing as f64 / f64::from(rate)))
    }

    /// Следующий кусок звука или `None`, когда файл закончился.
    fn next_chunk(&mut self) -> Result<Option<Chunk<'_>>, TrackError> {
        enum Got {
            Decoded { skip: usize, keep: usize },
            Silence { samples: usize },
            End,
        }

        let mut errors_in_row = 0u32;
        let got = loop {
            // Сначала доигрываем тишину на месте найденной дыры.
            if let Some(continuity) = self.continuity.as_mut()
                && continuity.gap_left > 0
            {
                let frames = continuity.gap_left.min(GAP_CHUNK_FRAMES);
                continuity.gap_left -= frames;
                match self.fill_silence(frames) {
                    Some(samples) => break Got::Silence { samples },
                    None => continue,
                }
            }

            let held = self.continuity.as_mut().and_then(|continuity| continuity.held.take());
            let packet = match held {
                Some(packet) => packet,
                None => match self.format.next_packet() {
                    Ok(packet) => packet,
                    Err(SymError::IoError(error))
                        if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        if let Some(continuity) = self.continuity.as_mut()
                            && let (Some(total), Some(reached)) =
                                (continuity.total, continuity.expected)
                        {
                            continuity.missing = total.saturating_sub(reached);
                        }
                        break Got::End;
                    }
                    Err(SymError::ResetRequired) => {
                        self.rebuild_decoder()?;
                        continue;
                    }
                    Err(SymError::DecodeError(_)) => {
                        self.decode_errors += 1;
                        errors_in_row += 1;
                        if errors_in_row > MAX_ERRORS_IN_ROW {
                            break Got::End;
                        }
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                },
            };
            if packet.track_id() != self.track_id {
                continue;
            }

            if let Some(continuity) = self.continuity.as_mut() {
                let starts_at = packet.ts();
                let ends_at = starts_at.saturating_add(packet.dur());
                match continuity.expected {
                    // Номер сэмпла перескочил вперёд: между пакетами пропали кадры.
                    Some(expected) if starts_at > expected => {
                        let rate = self.spec.or(self.header_spec).map_or(0, |spec| spec.rate);
                        continuity.gap_left =
                            (starts_at - expected).min(u64::from(rate) * MAX_GAP_SECONDS);
                        continuity.expected = Some(starts_at);
                        continuity.held = Some(packet);
                        self.decode_errors += 1;
                        continue;
                    }
                    _ => continuity.expected = Some(ends_at),
                }
            }
            let packet_ticks = packet.dur();

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let frames = decoded.frames();
                    if frames == 0 {
                        continue;
                    }
                    let capacity = decoded.capacity();
                    if self.buffer.is_none()
                        || self.buffer_spec != Some(spec)
                        || self.buffer_frames < capacity
                    {
                        self.buffer = Some(SampleBuffer::new(capacity as u64, spec));
                        self.buffer_spec = Some(spec);
                        self.buffer_frames = capacity;
                    }
                    if let Some(buffer) = self.buffer.as_mut() {
                        buffer.copy_interleaved_ref(decoded);
                    }
                    self.spec = Some(spec);

                    let skip = self.discard.min(frames as u64) as usize;
                    self.discard -= skip as u64;
                    let keep = self.opus_keep(packet.ts(), frames);
                    if skip >= keep {
                        continue;
                    }
                    self.frames += (keep - skip) as u64;
                    break Got::Decoded { skip, keep };
                }
                // Битый пакет. Читаем из памяти, так что IoError — тоже порча, не диск.
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.decode_errors += 1;
                    errors_in_row += 1;
                    if errors_in_row > MAX_ERRORS_IN_ROW {
                        break Got::End;
                    }
                    let lost = self.ticks_to_frames(packet_ticks).min(MAX_SILENCE_FRAMES);
                    match self.fill_silence(lost) {
                        Some(samples) => break Got::Silence { samples },
                        None => continue,
                    }
                }
                Err(SymError::ResetRequired) => self.rebuild_decoder()?,
                Err(error) => return Err(error.into()),
            }
        };

        let Some(spec) = self.spec.or(self.header_spec) else {
            return Ok(None);
        };
        let samples = match got {
            Got::Decoded { skip, keep } => match self.buffer.as_ref() {
                Some(buffer) => &buffer.samples()[skip * spec.channels.count()..keep * spec.channels.count()],
                None => return Ok(None),
            },
            Got::Silence { samples } => &self.silence[..samples],
            Got::End => return Ok(None),
        };
        Ok(Some(Chunk {
            samples,
            rate: spec.rate,
            channels: spec.channels,
        }))
    }

    /// Перематывает на `position` и возвращает точку, с которой пойдёт звук.
    fn seek(&mut self, position: Duration) -> Result<Duration, TrackError> {
        if let Some(pre_skip) = self.opus_pre_skip {
            return self.seek_opus(position, pre_skip);
        }
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from(position.as_secs_f64()),
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        self.discard = self.ticks_to_frames(seeked.required_ts.saturating_sub(seeked.actual_ts));
        if let Some(continuity) = self.continuity.as_mut() {
            // Куда встал демультиплексор, покажет первый же пакет.
            continuity.expected = None;
            continuity.gap_left = 0;
            continuity.held = None;
        }
        Ok(match self.time_base {
            Some(time_base) => time_to_duration(time_base.calc_time(seeked.required_ts)),
            None => position,
        })
    }

    /// Встаём на 200 мс раньше и выбрасываем лишнее. Метки — вместе с pre-skip.
    fn seek_opus(&mut self, position: Duration, pre_skip: u64) -> Result<Duration, TrackError> {
        let target = (position.as_secs_f64() * f64::from(OPUS_RATE)).round() as u64;
        let from = (target + pre_skip).saturating_sub(OPUS_PREROLL_FRAMES);
        let seeked = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from(from as f64 / f64::from(OPUS_RATE)),
                track_id: Some(self.track_id),
            },
        )?;
        self.decoder.reset();
        let first = opus_samples(seeked.actual_ts, self.time_base).max(pre_skip) - pre_skip;
        self.discard = target.saturating_sub(first);
        Ok(Duration::from_secs_f64(first.max(target) as f64 / f64::from(OPUS_RATE)))
    }

    /// `Some(false)` — контрольная сумма не сошлась, `None` — не проверяли.
    fn finalize(&mut self) -> Option<bool> {
        self.decoder.finalize().verify_ok
    }

    /// Готовит тишину и отдаёт её длину в сэмплах. `None` — нечего отдавать.
    fn fill_silence(&mut self, frames: u64) -> Option<usize> {
        let spec = self.spec.or(self.header_spec)?;
        let skip = self.discard.min(frames);
        self.discard -= skip;
        let frames = frames - skip;
        if frames == 0 {
            return None;
        }
        let samples = usize::try_from(frames).ok()? * spec.channels.count();
        self.silence.clear();
        self.silence.resize(samples, 0.0);
        self.frames += frames;
        Some(samples)
    }

    /// Сколько кадров пакета звучит: у Opus в Matroska хвост режет DiscardPadding.
    fn opus_keep(&self, ts: u64, frames: usize) -> usize {
        let (Some(end), Some(pre_skip)) = (self.opus_end, self.opus_pre_skip) else {
            return frames;
        };
        let first = opus_samples(ts, self.time_base).max(pre_skip);
        let over = (first + frames as u64).saturating_sub(end).min(frames as u64);
        frames - over as usize
    }

    fn rebuild_decoder(&mut self) -> Result<(), TrackError> {
        let track = self
            .format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(TrackError::NoAudioTrack)?;
        self.track_id = track.id;
        self.time_base = track.codec_params.time_base;
        self.decoder = crate::opus::codecs().make(
            &track.codec_params,
            &DecoderOptions {
                verify: self.verify,
            },
        )?;
        self.continuity = None;
        Ok(())
    }

    fn ticks_to_frames(&self, ticks: u64) -> u64 {
        let rate = self.spec.map(|spec| spec.rate).or(self.info.sample_rate);
        match (self.time_base, rate) {
            (Some(time_base), Some(rate)) if time_base.denom != 0 => {
                let frames = u128::from(ticks) * u128::from(time_base.numer) * u128::from(rate)
                    / u128::from(time_base.denom);
                u64::try_from(frames).unwrap_or(u64::MAX)
            }
            _ => ticks,
        }
    }
}

fn time_to_duration(time: Time) -> Duration {
    Duration::from_secs(time.seconds) + Duration::from_secs_f64(time.frac.clamp(0.0, 1.0))
}

#[cfg(test)]
mod sbr_tests {
    use super::needs_sbr;
    use symphonia::core::codecs::{CODEC_TYPE_AAC, CODEC_TYPE_MP3, CodecParameters};

    fn aac(rate: u32, config: &[u8]) -> CodecParameters {
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_AAC).with_sample_rate(rate).with_extra_data(config.into());
        params
    }

    #[test]
    fn he_aac_is_recognised_by_its_config() {
        // Обычный AAC-LC 48 кГц, стерео.
        assert!(!needs_sbr(&aac(48_000, &[0x11, 0x90])));
        // Явно: тип объекта 5 (SBR) и 29 (PS, HE-AAC v2).
        assert!(needs_sbr(&aac(48_000, &[0x29, 0x91, 0x88, 0x00])));
        assert!(needs_sbr(&aac(48_000, &[0xE9, 0x89, 0x88, 0x00])));
        // Совместимая запись: AAC-LC и расширение 0x2B7 с флагом SBR.
        assert!(needs_sbr(&aac(48_000, &[0x11, 0x90, 0x56, 0xE5, 0x98])));
        assert!(!needs_sbr(&aac(48_000, &[0x11, 0x90, 0x56, 0xE5, 0x00])));
        // Неявно: основной кодек на 24 кГц и ниже.
        assert!(needs_sbr(&aac(22_050, &[0x13, 0x90])));
        // Не AAC — не про него.
        let mut mp3 = CodecParameters::new();
        mp3.for_codec(CODEC_TYPE_MP3).with_sample_rate(16_000);
        assert!(!needs_sbr(&mp3));
    }
}
