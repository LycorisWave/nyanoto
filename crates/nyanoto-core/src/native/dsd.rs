//! DSD: контейнеры DSF и DFF, перевод в PCM.
//!
//! Две ступени ФНЧ с прореживанием: FIR по байтам с таблицей на 256 значений (÷8),
//! потом Кайзер до 88,2 или 176,4 кГц. Полоса ровная до 35/50 кГц, подавление ~130 дБ.
//! Задержка целая, без остатка — перемотка попадает в кадр. Тишина DSD — байт 0x69.
//! DST не поддерживается.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};

use super::{Opened, Stream, layout, tags};
use crate::source::{TrackError, TrackInfo};

/// Тишина DSD.
const SILENCE: u8 = 0x69;
/// Отводов первой ступени (кратно восьми: таблицы по байтам).
const STAGE1_TAPS: usize = 96;
/// Подавление в полосе задерживания, на которое считаются окна Кайзера, дБ.
const ATTENUATION_DB: f64 = 130.0;
/// Сколько байтов на канал читается из файла за раз.
const READ_BYTES: usize = 4096;

/// DSD64 → 88,2 кГц, DSD128+ → 176,4 (у семейства 48 кГц — 96 и 192).
pub fn pcm_rate(dsd_rate: u32) -> Option<u32> {
    let base = [44_100u32, 48_000].into_iter().find(|base| dsd_rate.is_multiple_of(base * 64))?;
    match dsd_rate / (base * 64) {
        1 => Some(base * 2),
        2 | 4 | 8 => Some(base * 4),
        _ => None,
    }
}

/// Граница ровной полосы, Гц.
fn passband(dsd_rate: u32) -> f64 {
    if [44_100u32, 48_000].contains(&(dsd_rate / 64)) { 35_000.0 } else { 50_000.0 }
}

/// Однобитный поток → PCM.
pub(super) struct Converter {
    channels: usize,
    /// Прореживание второй ступени.
    factor: usize,
    pcm_rate: u32,
    /// Таблицы первой ступени: `tables[k][байт]` — вклад k-й восьмёрки отводов.
    tables: Vec<[f32; 256]>,
    /// Отводы второй ступени.
    taps: Vec<f64>,
    /// Байты первой ступени и история второй (дважды подряд — окно сплошное).
    bytes: Vec<u128>,
    history: Vec<Vec<f64>>,
    write: usize,
    /// Сколько входов второй ступени до следующего выхода.
    phase: usize,
}

impl Converter {
    pub(super) fn new(dsd_rate: u32, channels: usize) -> Result<Self, TrackError> {
        // DSD64 — 64 × 44,1 кГц; реже встречается семейство 48 кГц (3,072 МГц).
        let Some(pcm_rate) = pcm_rate(dsd_rate) else {
            return Err(TrackError::Unsupported(format!("DSD {dsd_rate} Гц")));
        };
        let dsd = f64::from(dsd_rate);
        let first = dsd / 8.0;
        let passband = passband(dsd_rate);
        let factor = (first / f64::from(pcm_rate)) as usize;
        // Вторая ступень: без наложения ровно до границы полосы пропускания.
        let stop2 = f64::from(pcm_rate) - passband;
        // Первая ступень режет всё, что после ÷8 легло бы ниже `stop2`.
        let stop1 = first - stop2;
        let mut stage1 = lowpass(STAGE1_TAPS - 1, (passband + stop1) / 2.0 / dsd);
        stage1.push(0.0);
        let width2 = (stop2 - passband) / first;
        let needed = ((ATTENUATION_DB - 7.95) / (14.36 * width2)).ceil() as usize + 1;
        let half = half_length(needed / 2, factor);
        let taps = lowpass(2 * half + 1, (passband + stop2) / 2.0 / first);

        let tables = (0..STAGE1_TAPS / 8)
            .map(|group| {
                let mut table = [0f32; 256];
                for (byte, entry) in table.iter_mut().enumerate() {
                    // Старший бит — самый ранний отсчёт: у младшего задержка меньше.
                    *entry = (0..8)
                        .map(|bit| if byte >> bit & 1 == 1 { stage1[group * 8 + bit] } else { -stage1[group * 8 + bit] })
                        .sum::<f64>() as f32;
                }
                table
            })
            .collect();
        let mut converter = Self {
            channels,
            factor,
            pcm_rate,
            tables,
            history: vec![vec![0.0; 2 * taps.len()]; channels],
            taps,
            bytes: vec![0; channels],
            write: 0,
            phase: 0,
        };
        converter.reset();
        Ok(converter)
    }

    pub(super) fn pcm_rate(&self) -> u32 {
        self.pcm_rate
    }

    /// Байтов DSD (на канал) в одном кадре PCM.
    pub(super) fn bytes_per_frame(&self) -> usize {
        self.factor
    }

    /// Задержка в кадрах PCM, целая.
    pub(super) fn delay(&self) -> usize {
        (STAGE1_DELAY + (self.taps.len() - 1) / 2 + 1 - self.factor) / self.factor
    }

    /// Сколько байтов прочитать до точки перемотки, чтобы фильтры прогрелись.
    pub(super) fn warmup_bytes(&self) -> usize {
        STAGE1_TAPS / 8 + self.taps.len() + self.factor
    }

    /// Забывает историю, как будто перед потоком была тишина.
    pub(super) fn reset(&mut self) {
        let silence: f64 = self.tables.iter().map(|table| f64::from(table[usize::from(SILENCE)])).sum();
        for (bytes, history) in self.bytes.iter_mut().zip(&mut self.history) {
            *bytes = u128::from_ne_bytes([SILENCE; 16]);
            history.fill(silence);
        }
        self.write = 0;
        self.phase = 0;
    }

    /// Байты DSD (каналы подряд, старший бит первый) → кадры PCM в `out`.
    pub(super) fn push(&mut self, input: &[u8], out: &mut Vec<f32>) {
        let length = self.taps.len();
        for frame in input.chunks_exact(self.channels) {
            for (channel, byte) in frame.iter().enumerate() {
                let bytes = (self.bytes[channel] << 8) | u128::from(*byte);
                self.bytes[channel] = bytes;
                // Четыре независимые суммы вместо одной цепочки из двенадцати сложений.
                let history = bytes.to_le_bytes();
                let mut sums = [0.0f64; 4];
                for (group, table) in self.tables.iter().enumerate() {
                    sums[group % 4] += f64::from(table[usize::from(history[group])]);
                }
                let sample = (sums[0] + sums[1]) + (sums[2] + sums[3]);
                let history = &mut self.history[channel];
                history[self.write] = sample;
                history[self.write + length] = sample;
            }
            self.write += 1;
            if self.write == length {
                self.write = 0;
            }
            self.phase += 1;
            if self.phase == self.factor {
                self.phase = 0;
                // Отводы симметричны, порядок окна не важен.
                for history in &self.history {
                    out.push(dot(&history[self.write..self.write + length], &self.taps) as f32);
                }
            }
        }
    }
}

/// Четыре независимые суммы — так компилятор векторизует.
fn dot(samples: &[f64], taps: &[f64]) -> f64 {
    let mut sums = [0.0f64; 4];
    let (samples4, samples_tail) = samples.as_chunks::<4>();
    let (taps4, taps_tail) = taps[..samples.len()].as_chunks::<4>();
    for (four, taps) in samples4.iter().zip(taps4) {
        for lane in 0..4 {
            sums[lane] += four[lane] * taps[lane];
        }
    }
    let tail: f64 = samples_tail.iter().zip(taps_tail).map(|(sample, tap)| sample * tap).sum();
    (sums[0] + sums[1]) + (sums[2] + sums[3]) + tail
}

/// Центр фильтра — 47-й отвод, бит последний в байте: 47 − 7 = 40 отсчётов, пять байтов.
const STAGE1_DELAY: usize = 5;

/// Полудлина второй ступени: не меньше `needed` и с задержкой, кратной прореживанию.
fn half_length(needed: usize, factor: usize) -> usize {
    (needed..).find(|half| (STAGE1_DELAY + half + 1).is_multiple_of(factor)).unwrap_or(needed)
}

/// ФНЧ Кайзера, усиление на нуле — ровно 1.
fn lowpass(count: usize, cutoff: f64) -> Vec<f64> {
    let beta = 0.1102 * (ATTENUATION_DB - 8.7);
    let middle = (count - 1) as f64 / 2.0;
    let mut taps: Vec<f64> = (0..count)
        .map(|index| {
            let x = index as f64 - middle;
            let sinc = if x == 0.0 { 2.0 * cutoff } else { (std::f64::consts::TAU * cutoff * x).sin() / (std::f64::consts::PI * x) };
            let ratio = if middle > 0.0 { x / middle } else { 0.0 };
            sinc * bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / bessel_i0(beta)
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for tap in &mut taps {
        *tap /= sum;
    }
    taps
}

/// Модифицированная функция Бесселя нулевого порядка (ряд).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, half) = (1.0, 1.0, x / 2.0);
    for k in 1..200 {
        term *= half / k as f64;
        sum += term * term;
        if term * term < sum * 1e-18 {
            break;
        }
    }
    sum
}

/// Откуда берутся байты DSD.
pub(super) trait DsdSource: Send {
    /// Дописывает байты в `out`. Ничего не дописал — поток кончился.
    fn read(&mut self, out: &mut Vec<u8>) -> Result<(), TrackError>;
    /// Встаёт на байт `byte` каждого канала.
    fn seek(&mut self, byte: u64) -> Result<(), TrackError>;
    fn errors(&self) -> u64 {
        0
    }
}

/// Поток DSD, переведённый в PCM, с компенсацией задержки фильтров.
pub(super) struct DsdStream<S: DsdSource> {
    source: S,
    converter: Converter,
    channels: usize,
    /// Кадров PCM во всём потоке, если длина известна.
    total: Option<u64>,
    /// Номер следующего отдаваемого кадра PCM.
    position: u64,
    /// Сколько кадров выхода ещё выбросить: задержка фильтров и разгон после перемотки.
    skip: usize,
    /// Сколько кадров тишины осталось дописать, чтобы вытолкнуть хвост из фильтров.
    tail: usize,
    ended: bool,
    bytes: Vec<u8>,
    pcm: Vec<f32>,
}

impl<S: DsdSource> DsdStream<S> {
    /// `total_bytes` — байтов DSD на канал во всём потоке.
    pub(super) fn new(source: S, dsd_rate: u32, channels: usize, total_bytes: Option<u64>) -> Result<Self, TrackError> {
        let converter = Converter::new(dsd_rate, channels)?;
        let total = total_bytes.map(|bytes| bytes / converter.bytes_per_frame() as u64);
        Ok(Self {
            skip: converter.delay(),
            tail: converter.delay() + 1,
            source,
            converter,
            channels,
            total,
            position: 0,
            ended: false,
            bytes: Vec::new(),
            pcm: Vec::new(),
        })
    }

    pub(super) fn pcm_rate(&self) -> u32 {
        self.converter.pcm_rate()
    }

    pub(super) fn frames(&self) -> Option<u64> {
        self.total
    }
}

impl<S: DsdSource> Stream for DsdStream<S> {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        loop {
            if self.total.is_some_and(|total| self.position >= total) {
                return Ok(());
            }
            self.bytes.clear();
            if !self.ended {
                self.source.read(&mut self.bytes)?;
                self.ended = self.bytes.is_empty();
            }
            if self.ended {
                if self.tail == 0 {
                    return Ok(());
                }
                // Хвост: тишина выталкивает из фильтров последние кадры.
                let frames = self.tail.min(READ_BYTES / self.converter.bytes_per_frame());
                self.tail -= frames;
                self.bytes.resize(frames * self.converter.bytes_per_frame() * self.channels, SILENCE);
            }
            self.pcm.clear();
            self.converter.push(&self.bytes, &mut self.pcm);
            let skip = self.skip.min(self.pcm.len() / self.channels);
            self.skip -= skip;
            let mut frames = self.pcm.len() / self.channels - skip;
            if let Some(total) = self.total {
                frames = frames.min((total - self.position) as usize);
            }
            let start = skip * self.channels;
            out.extend_from_slice(&self.pcm[start..start + frames * self.channels]);
            self.position += frames as u64;
            if frames > 0 {
                return Ok(());
            }
        }
    }

    fn seek(&mut self, frame: u64) -> Result<u64, TrackError> {
        let frame = self.total.map_or(frame, |total| frame.min(total));
        let per_frame = self.converter.bytes_per_frame() as u64;
        let byte = frame * per_frame;
        // Разгон — целое число кадров, чтобы точка попала ровно в кадр.
        let warmup = (self.converter.warmup_bytes() as u64).div_ceil(per_frame) * per_frame;
        let from = byte.saturating_sub(warmup);
        self.source.seek(from)?;
        self.converter.reset();
        self.skip = ((byte - from) / per_frame) as usize + self.converter.delay();
        self.tail = self.converter.delay() + 1;
        self.ended = false;
        self.position = frame;
        Ok(frame)
    }

    fn decode_errors(&self) -> u64 {
        self.source.errors()
    }
}

/// Длина из заголовка, но не больше, чем есть в файле.
fn audio_bytes(declared: u64, available: u64) -> u64 {
    declared.min(available)
}

/// DSF (Sony): блоки по 4096 байт на канал по очереди, младший бит — первый.
pub(super) fn open_dsf(file: File) -> Result<Opened, TrackError> {
    let mut file = BufReader::new(file);
    let mut head = [0u8; 28 + 52 + 12];
    file.read_exact(&mut head).map_err(TrackError::Io)?;
    let u32_at = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().unwrap_or_default());
    let u64_at = |at: usize| u64::from_le_bytes(head[at..at + 8].try_into().unwrap_or_default());
    if &head[..4] != b"DSD " || &head[28..32] != b"fmt " || &head[80..84] != b"data" {
        return Err(TrackError::Malformed("DSF: нет обязательных блоков".into()));
    }
    let metadata = u64_at(20);
    if u32_at(44) != 0 {
        return Err(TrackError::Unsupported("DSF: не сырой DSD".into()));
    }
    let kind = u32_at(48);
    let channels = u32_at(52) as usize;
    let rate = u32_at(56);
    let lsb_first = u32_at(60) == 1;
    let samples = u64_at(64);
    let block = u32_at(72) as usize;
    let data_size = u64_at(84).saturating_sub(12);
    if !(1..=6).contains(&channels) || block == 0 || block > 1 << 20 {
        return Err(TrackError::Malformed(format!("DSF: {channels} кан., блок {block}")));
    }
    let data_start = 28 + 52 + 12;
    let available = data_size / channels as u64;
    let total_bytes = audio_bytes(samples / 8, available);
    let replay_gain = tags::replay_gain(file.get_mut(), (metadata > 0).then_some(metadata));
    let source = Dsf {
        file,
        data_start,
        channels,
        block,
        lsb_first,
        total_bytes,
        next: 0,
        raw: Vec::new(),
    };
    // Раскладка по типу каналов DSF; порядок в файле совпадает с порядком битов маски.
    let mask = match kind {
        3 => 0x7,
        4 => 0x33,
        5 => 0xf,
        6 => 0x37,
        7 => 0x3f,
        _ => 0,
    };
    let id3v2 = (metadata > 0).then_some(metadata);
    finish(source, "dsf", rate, channels, layout(channels, mask), total_bytes, replay_gain, id3v2)
}

#[allow(clippy::too_many_arguments)]
fn finish<S: DsdSource + 'static>(
    source: S,
    codec: &str,
    rate: u32,
    channels: usize,
    layout: symphonia::core::audio::Channels,
    total_bytes: u64,
    replay_gain: crate::source::ReplayGain,
    id3v2: Option<u64>,
) -> Result<Opened, TrackError> {
    let stream = DsdStream::new(source, rate, channels, Some(total_bytes))?;
    let info = TrackInfo {
        codec: codec.into(),
        sample_rate: Some(stream.pcm_rate()),
        channels: Some(channels),
        bits_per_sample: Some(24),
        duration: Some(std::time::Duration::from_secs_f64(total_bytes as f64 * 8.0 / f64::from(rate))),
        replay_gain,
    };
    let frames = stream.frames();
    Ok(Opened { stream: Box::new(stream), info, layout, frames, dsd_rate: Some(rate), id3v2 })
}

struct Dsf {
    file: BufReader<File>,
    data_start: u64,
    channels: usize,
    block: usize,
    lsb_first: bool,
    total_bytes: u64,
    /// Следующий байт каждого канала.
    next: u64,
    raw: Vec<u8>,
}

impl DsdSource for Dsf {
    fn read(&mut self, out: &mut Vec<u8>) -> Result<(), TrackError> {
        if self.next >= self.total_bytes {
            return Ok(());
        }
        // Дочитываем текущий блок: группа блоков — по блоку на канал.
        let group = self.next / self.block as u64;
        let within = (self.next % self.block as u64) as usize;
        let count = (self.block - within).min((self.total_bytes - self.next) as usize);
        let group_start = self.data_start + group * (self.block * self.channels) as u64;
        self.raw.resize(self.block * self.channels, 0);
        self.file.seek(SeekFrom::Start(group_start)).map_err(TrackError::Io)?;
        let mut filled = 0;
        while filled < self.raw.len() {
            match self.file.read(&mut self.raw[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) => return Err(TrackError::Io(error)),
            }
        }
        // Обрезанный файл: чего нет, того нет.
        let have = (0..self.channels)
            .map(|channel| filled.saturating_sub(channel * self.block).min(self.block).saturating_sub(within))
            .min()
            .unwrap_or(0)
            .min(count);
        out.reserve(have * self.channels);
        for index in 0..have {
            for channel in 0..self.channels {
                let byte = self.raw[channel * self.block + within + index];
                out.push(if self.lsb_first { byte.reverse_bits() } else { byte });
            }
        }
        self.next += have as u64;
        if have < count {
            self.next = self.total_bytes;
        }
        Ok(())
    }

    fn seek(&mut self, byte: u64) -> Result<(), TrackError> {
        self.next = byte.min(self.total_bytes);
        Ok(())
    }
}

/// DSDIFF: big-endian, каналы вперемежку, старший бит первый.
pub(super) fn open_dff(file: File) -> Result<Opened, TrackError> {
    let mut file = BufReader::new(file);
    let length = file.get_ref().metadata().map_err(TrackError::Io)?.len();
    let mut head = [0u8; 16];
    file.read_exact(&mut head).map_err(TrackError::Io)?;
    if &head[..4] != b"FRM8" || &head[12..16] != b"DSD " {
        return Err(TrackError::Malformed("DFF: нет заголовка FRM8/DSD".into()));
    }
    let (mut rate, mut order, mut data, mut id3) = (None, Vec::new(), None, None);
    let mut at = 16u64;
    while at + 12 <= length {
        let (id, size) = chunk_header(&mut file, at)?;
        let body = at + 12;
        match &id {
            b"PROP" => {
                let mut kind = [0u8; 4];
                file.read_exact(&mut kind).map_err(TrackError::Io)?;
                let mut inner = body + 4;
                while inner.saturating_add(12) <= body.saturating_add(size) {
                    let (id, inner_size) = chunk_header(&mut file, inner)?;
                    match &id {
                        b"FS  " => rate = Some(read_u32_be(&mut file)?),
                        b"CHNL" => {
                            let count = read_u16_be(&mut file)?;
                            order = (0..count)
                                .map(|_| {
                                    let mut id = [0u8; 4];
                                    file.read_exact(&mut id).map(|_| id)
                                })
                                .collect::<Result<_, _>>()
                                .map_err(TrackError::Io)?;
                        }
                        b"CMPR" => {
                            let mut kind = [0u8; 4];
                            file.read_exact(&mut kind).map_err(TrackError::Io)?;
                            if &kind != b"DSD " {
                                return Err(TrackError::Unsupported("DFF со сжатием DST".into()));
                            }
                        }
                        _ => {}
                    }
                    inner = inner.saturating_add(12).saturating_add(inner_size).saturating_add(inner_size & 1);
                }
            }
            b"DSD " => data = Some((body, size)),
            b"DST " => return Err(TrackError::Unsupported("DFF со сжатием DST".into())),
            b"ID3 " => id3 = Some(body),
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    let rate = rate.ok_or_else(|| TrackError::Malformed("DFF: нет частоты".into()))?;
    let (data_start, data_size) = data.ok_or_else(|| TrackError::Malformed("DFF: нет звука".into()))?;
    let channels = order.len();
    if !(1..=8).contains(&channels) {
        return Err(TrackError::Malformed(format!("DFF: {channels} кан.")));
    }
    let (layout, permutation) = dff_layout(&order);
    let available = length.saturating_sub(data_start) / channels as u64;
    let total_bytes = audio_bytes(data_size / channels as u64, available);
    let replay_gain = tags::replay_gain(file.get_mut(), id3);
    let source = Dff { file, data_start, channels, permutation, total_bytes, next: 0, raw: Vec::new() };
    finish(source, "dff", rate, channels, layout, total_bytes, replay_gain, id3)
}

fn chunk_header(file: &mut BufReader<File>, at: u64) -> Result<([u8; 4], u64), TrackError> {
    let mut header = [0u8; 12];
    file.seek(SeekFrom::Start(at)).map_err(TrackError::Io)?;
    file.read_exact(&mut header).map_err(TrackError::Io)?;
    let id = [header[0], header[1], header[2], header[3]];
    Ok((id, u64::from_be_bytes(header[4..12].try_into().unwrap_or_default())))
}

fn read_u32_be(file: &mut BufReader<File>) -> Result<u32, TrackError> {
    let mut bytes = [0u8; 4];
    file.read_exact(&mut bytes).map_err(TrackError::Io)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u16_be(file: &mut BufReader<File>) -> Result<u16, TrackError> {
    let mut bytes = [0u8; 2];
    file.read_exact(&mut bytes).map_err(TrackError::Io)?;
    Ok(u16::from_be_bytes(bytes))
}

/// Раскладка по именам каналов DFF и перестановка под маску. Незнакомое — как есть.
fn dff_layout(order: &[[u8; 4]]) -> (symphonia::core::audio::Channels, Vec<usize>) {
    use symphonia::core::audio::Channels as C;
    let flags: Option<Vec<C>> = order
        .iter()
        .map(|id| match id {
            b"SLFT" | b"MLFT" => Some(C::FRONT_LEFT),
            b"SRGT" | b"MRGT" => Some(C::FRONT_RIGHT),
            b"C   " => Some(C::FRONT_CENTRE),
            b"LFE " => Some(C::LFE1),
            b"LS  " => Some(C::REAR_LEFT),
            b"RS  " => Some(C::REAR_RIGHT),
            _ => None,
        })
        .collect();
    let identity: Vec<usize> = (0..order.len()).collect();
    let Some(flags) = flags else {
        return (layout(order.len(), 0), identity);
    };
    let layout_flags = flags.iter().fold(C::empty(), |all, flag| all | *flag);
    if layout_flags.count() != order.len() {
        return (layout(order.len(), 0), identity);
    }
    let mut permutation = identity;
    permutation.sort_by_key(|index| flags[*index].bits());
    (layout_flags, permutation)
}

struct Dff {
    file: BufReader<File>,
    data_start: u64,
    channels: usize,
    permutation: Vec<usize>,
    total_bytes: u64,
    next: u64,
    raw: Vec<u8>,
}

impl DsdSource for Dff {
    fn read(&mut self, out: &mut Vec<u8>) -> Result<(), TrackError> {
        let count = (self.total_bytes - self.next.min(self.total_bytes)).min(READ_BYTES as u64) as usize;
        if count == 0 {
            return Ok(());
        }
        self.raw.resize(count * self.channels, 0);
        self.file
            .seek(SeekFrom::Start(self.data_start + self.next * self.channels as u64))
            .map_err(TrackError::Io)?;
        let mut filled = 0;
        while filled < self.raw.len() {
            match self.file.read(&mut self.raw[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) => return Err(TrackError::Io(error)),
            }
        }
        let frames = filled / self.channels;
        for frame in self.raw[..frames * self.channels].chunks_exact(self.channels) {
            out.extend(self.permutation.iter().map(|from| frame[*from]));
        }
        self.next += frames as u64;
        if frames < count {
            self.next = self.total_bytes;
        }
        Ok(())
    }

    fn seek(&mut self, byte: u64) -> Result<(), TrackError> {
        self.next = byte.min(self.total_bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_passbands_follow_the_dsd_multiple() {
        for (dsd, pcm, band) in [
            (2_822_400, 88_200, 35_000.0),
            (5_644_800, 176_400, 50_000.0),
            (11_289_600, 176_400, 50_000.0),
            (22_579_200, 176_400, 50_000.0),
            (3_072_000, 96_000, 35_000.0),
            (6_144_000, 192_000, 50_000.0),
        ] {
            assert_eq!(pcm_rate(dsd), Some(pcm), "{dsd}");
            assert_eq!(passband(dsd), band, "{dsd}");
        }
        assert_eq!(pcm_rate(44_100), None);
        assert_eq!(pcm_rate(2_822_400 * 3), None);
    }
}

/// Замер: `cargo test --release -p nyanoto-core --lib dsd_bench -- --ignored --nocapture`.
#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore]
    fn dsd_bench() {
        for (rate, name) in [(2_822_400u32, "DSD64"), (5_644_800, "DSD128"), (11_289_600, "DSD256")] {
            let mut converter = Converter::new(rate, 2).unwrap();
            let seconds = 10usize;
            let mut state = 0x1234_5678_9abc_def0u64;
            let input: Vec<u8> = (0..rate as usize / 8 * 2 * seconds)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            let mut out = Vec::with_capacity(input.len());
            let started = std::time::Instant::now();
            for block in input.chunks(8192) {
                out.clear();
                converter.push(block, &mut out);
            }
            let elapsed = started.elapsed().as_secs_f64();
            println!("{name}: {:.1} мс на секунду звука ({:.2} % ядра), отводов второй ступени {}", elapsed * 1000.0 / seconds as f64, elapsed / seconds as f64 * 100.0, converter.taps.len());
        }
    }
}
