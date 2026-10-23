//! TTA — свой декодер (по libtta 2.3 и FFmpeg `tta.c`).
//! Кадры по 256/245 с независимы — перемотка точная. Битая CRC кадра — тишина.
//! Зашифрованные (формат 2) не умеем.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};

use super::{Opened, Stream, layout, tags};
use crate::source::{TrackError, TrackInfo};

/// Длина заголовка `TTA1` вместе с его CRC32.
const HEADER: u64 = 22;
/// Формат 1 — обычный PCM; 2 — зашифрованный.
const FORMAT_SIMPLE: u16 = 1;
const FORMAT_ENCRYPTED: u16 = 2;
/// Длиннее унарного кода не бывает — значит, данные битые.
const MAX_UNARY: u32 = 1 << 24;

pub(super) fn open(file: File, offset: u64) -> Result<Opened, TrackError> {
    let mut file = BufReader::new(file);
    file.seek(SeekFrom::Start(offset)).map_err(TrackError::Io)?;
    let mut header = [0u8; HEADER as usize];
    file.read_exact(&mut header).map_err(TrackError::Io)?;
    let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    if crc32(&header[..18]) != u32_at(18) {
        return Err(TrackError::Malformed("TTA: заголовок испорчен".into()));
    }
    let format = u16_at(4);
    if format == FORMAT_ENCRYPTED {
        return Err(TrackError::Unsupported("TTA, зашифрованный паролем".into()));
    }
    if format != FORMAT_SIMPLE {
        return Err(TrackError::Unsupported(format!("TTA, формат {format}")));
    }
    let channels = usize::from(u16_at(6));
    let bits = u32::from(u16_at(8));
    let rate = u32_at(10);
    let length = u64::from(u32_at(14));
    if channels == 0 || !matches!(bits, 8 | 16 | 24) || !(1..=1_000_000).contains(&rate) {
        return Err(TrackError::Malformed(format!("TTA: {channels} кан., {bits} бит, {rate} Гц")));
    }
    let frame_length = u64::from(rate) * 256 / 245;
    let count = length.div_ceil(frame_length) as usize;

    // Сумма таблицы не сошлась — не беда, битый кадр поймает своя CRC.
    let mut table = vec![0u8; count * 4 + 4];
    file.read_exact(&mut table).map_err(TrackError::Io)?;
    let table_ok = crc32(&table[..count * 4]) == u32::from_le_bytes(table[count * 4..].try_into().unwrap_or_default());
    let mut starts = Vec::with_capacity(count + 1);
    let mut at = offset + HEADER + table.len() as u64;
    for size in table[..count * 4].as_chunks::<4>().0 {
        starts.push(at);
        at += u64::from(u32::from_le_bytes(*size));
    }
    starts.push(at);

    let id3v2 = (offset > 0).then_some(0);
    let replay_gain = tags::replay_gain(file.get_mut(), id3v2);
    let info = TrackInfo {
        codec: "tta".into(),
        sample_rate: Some(rate),
        channels: Some(channels),
        bits_per_sample: Some(bits),
        duration: Some(std::time::Duration::from_secs_f64(length as f64 / f64::from(rate))),
        replay_gain,
    };
    let stream = Tta {
        file,
        channels,
        bytes: bits / 8,
        frame_length,
        length,
        starts,
        next: 0,
        discard: 0,
        state: vec![Channel::default(); channels],
        raw: Vec::new(),
        values: Vec::new(),
        errors: u64::from(!table_ok),
    };
    Ok(Opened {
        stream: Box::new(stream),
        info,
        layout: layout(channels, 0),
        frames: Some(length),
        dsd_rate: None,
        id3v2,
    })
}

struct Tta {
    file: BufReader<File>,
    channels: usize,
    /// Байт на сэмпл: 1, 2 или 3.
    bytes: u32,
    frame_length: u64,
    /// Сэмплов на канал во всём файле.
    length: u64,
    /// Начало каждого кадра в файле и конец последнего.
    starts: Vec<u64>,
    /// Номер следующего кадра.
    next: usize,
    /// Сколько кадров звука выбросить из начала следующего кадра TTA (после перемотки).
    discard: usize,
    state: Vec<Channel>,
    raw: Vec<u8>,
    values: Vec<i32>,
    errors: u64,
}

impl Stream for Tta {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError> {
        while self.next + 1 < self.starts.len() {
            let index = self.next;
            self.next += 1;
            let frames = self.frame_frames(index);
            let frame = self.decode_frame(index, frames)?;
            let scale = 1.0 / (1u32 << (self.bytes * 8 - 1)) as f32;
            let skip = std::mem::take(&mut self.discard).min(frames) * self.channels;
            let first = out.len();
            match frame {
                Frame::Decoded => out.extend(self.values[skip..].iter().map(|value| *value as f32 * scale)),
                Frame::Damaged => out.resize(first + frames * self.channels - skip, 0.0),
                // Файл обрезан: дальше звука нет, недостающее покажет `missing`.
                Frame::Cut => {
                    self.next = self.starts.len();
                    return Ok(());
                }
            }
            if out.len() > first {
                return Ok(());
            }
        }
        Ok(())
    }

    fn seek(&mut self, frame: u64) -> Result<u64, TrackError> {
        let frame = frame.min(self.length);
        self.next = (frame / self.frame_length) as usize;
        self.discard = (frame % self.frame_length) as usize;
        Ok(frame)
    }

    fn decode_errors(&self) -> u64 {
        self.errors
    }
}

impl Tta {
    /// Сколько сэмплов на канал в кадре `index`: последний короче.
    fn frame_frames(&self, index: usize) -> usize {
        let start = index as u64 * self.frame_length;
        (self.length - start).min(self.frame_length) as usize
    }

    /// Декодирует кадр в `self.values`.
    fn decode_frame(&mut self, index: usize, frames: usize) -> Result<Frame, TrackError> {
        let size = (self.starts[index + 1] - self.starts[index]) as usize;
        self.raw.resize(size, 0);
        self.file.seek(SeekFrom::Start(self.starts[index])).map_err(TrackError::Io)?;
        match self.file.read_exact(&mut self.raw) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(Frame::Cut),
            Err(error) => return Err(TrackError::Io(error)),
        }
        let checked = size
            .checked_sub(4)
            .map(|end| self.raw.split_at(end))
            .filter(|(data, crc)| crc32(data) == u32::from_le_bytes([crc[0], crc[1], crc[2], crc[3]]));
        let decoded = checked.and_then(|(data, _)| {
            decode(data, frames, self.channels, self.bytes, &mut self.state, &mut self.values)
        });
        if decoded.is_none() {
            self.errors += 1;
            return Ok(Frame::Damaged);
        }
        Ok(Frame::Decoded)
    }
}

enum Frame {
    Decoded,
    /// Не сошлась CRC или данные не разбираются: вместо кадра будет тишина.
    Damaged,
    /// Файл кончился раньше кадра.
    Cut,
}

/// Состояние канала: код Райса, адаптивный фильтр и предсказатель.
#[derive(Clone, Default)]
struct Channel {
    k0: u32,
    k1: u32,
    sum0: u32,
    sum1: u32,
    qm: [i32; 8],
    dx: [i32; 8],
    dl: [i32; 8],
    error: i32,
    predictor: i32,
}

impl Channel {
    fn reset(&mut self) {
        *self = Channel {
            k0: 10,
            k1: 10,
            sum0: shift_16(10),
            sum1: shift_16(10),
            ..Channel::default()
        };
    }

    /// Адаптивный фильтр (`ttadsp.c`): веса по знаку прошлой ошибки.
    fn filter(&mut self, value: &mut i32, shift: u32) {
        let (qm, dx, dl) = (&mut self.qm, &mut self.dx, &mut self.dl);
        if self.error < 0 {
            for (weight, step) in qm.iter_mut().zip(dx.iter()) {
                *weight = weight.wrapping_sub(*step);
            }
        } else if self.error > 0 {
            for (weight, step) in qm.iter_mut().zip(dx.iter()) {
                *weight = weight.wrapping_add(*step);
            }
        }
        let mut sum = 1i32 << (shift - 1);
        for (weight, past) in qm.iter().zip(dl.iter()) {
            sum = sum.wrapping_add(past.wrapping_mul(*weight));
        }
        dx.copy_within(1..5, 0);
        dl.copy_within(1..5, 0);
        dx[4] = (dl[4] >> 30) | 1;
        dx[5] = ((dl[5] >> 30) | 2) & !1;
        dx[6] = ((dl[6] >> 30) | 2) & !1;
        dx[7] = ((dl[7] >> 30) | 4) & !3;
        self.error = *value;
        *value = value.wrapping_add(sum >> shift);
        dl[4] = dl[5].wrapping_neg();
        dl[5] = dl[6].wrapping_neg();
        dl[6] = value.wrapping_sub(dl[7]);
        dl[7] = *value;
        dl[5] = dl[5].wrapping_add(dl[6]);
        dl[4] = dl[4].wrapping_add(dl[5]);
    }
}

fn shift_1(power: u32) -> u32 {
    if power < 32 { 1 << power } else { 0x8000_0000 }
}

fn shift_16(power: u32) -> u32 {
    shift_1(power + 4)
}

/// Декодирует один кадр: `frames` сэмплов на каждый из `channels` каналов.
fn decode(
    data: &[u8],
    frames: usize,
    channels: usize,
    bytes: u32,
    state: &mut [Channel],
    out: &mut Vec<i32>,
) -> Option<()> {
    // Сдвиг фильтра и порядок предсказателя зависят от разрядности.
    let shift = match bytes {
        2 => 9,
        _ => 10,
    };
    let order = if bytes == 1 { 4 } else { 5 };
    for channel in state.iter_mut() {
        channel.reset();
    }
    out.clear();
    out.reserve(frames * channels);
    let mut bits = Bits::new(data);
    for _ in 0..frames {
        for channel in state.iter_mut() {
            let mut unary = bits.unary()?;
            let deep = unary > 0;
            let k = if deep {
                unary -= 1;
                channel.k1
            } else {
                channel.k0
            };
            if k > 31 {
                return None;
            }
            let mut value = if k > 0 {
                unary.wrapping_shl(k).wrapping_add(bits.read(k)?)
            } else {
                unary
            };
            if deep {
                channel.sum1 = channel.sum1.wrapping_add(value.wrapping_sub(channel.sum1 >> 4));
                if channel.k1 > 0 && channel.sum1 < shift_16(channel.k1) {
                    channel.k1 -= 1;
                } else if channel.sum1 > shift_16(channel.k1 + 1) {
                    channel.k1 += 1;
                }
                value = value.wrapping_add(shift_1(channel.k0));
            }
            channel.sum0 = channel.sum0.wrapping_add(value.wrapping_sub(channel.sum0 >> 4));
            if channel.k0 > 0 && channel.sum0 < shift_16(channel.k0) {
                channel.k0 -= 1;
            } else if channel.sum0 > shift_16(channel.k0 + 1) {
                channel.k0 += 1;
            }
            // Чётные — отрицательные, нечётные — положительные.
            let value = value as i32;
            let mut sample = 1i32.wrapping_add((value >> 1) ^ ((value & 1) - 1));
            channel.filter(&mut sample, shift);
            let predicted = ((i64::from(channel.predictor) * ((1 << order) - 1)) >> order) as i32;
            sample = sample.wrapping_add(predicted);
            channel.predictor = sample;
            out.push(sample);
        }
        // Последний канал — сумма, остальные — разности с соседом.
        if channels > 1 {
            let frame = out.len() - channels;
            let samples = &mut out[frame..];
            samples[channels - 1] = samples[channels - 1].wrapping_add(samples[channels - 2] / 2);
            for index in (0..channels - 1).rev() {
                samples[index] = samples[index + 1].wrapping_sub(samples[index]);
            }
        }
    }
    Some(())
}

/// Чтение битов младшими вперёд, как пишет TTA.
struct Bits<'a> {
    data: &'a [u8],
    next: usize,
    cache: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, next: 0, cache: 0, count: 0 }
    }

    fn refill(&mut self) {
        while self.count <= 56 {
            let Some(byte) = self.data.get(self.next) else {
                return;
            };
            self.cache |= u64::from(*byte) << self.count;
            self.count += 8;
            self.next += 1;
        }
    }

    fn read(&mut self, count: u32) -> Option<u32> {
        if self.count < count {
            self.refill();
            if self.count < count {
                return None;
            }
        }
        let value = (self.cache & ((1u64 << count) - 1)) as u32;
        self.cache >>= count;
        self.count -= count;
        Some(value)
    }

    /// Число единиц до первого нуля; ноль тоже съедается.
    fn unary(&mut self) -> Option<u32> {
        let mut total = 0u32;
        loop {
            self.refill();
            if self.count == 0 {
                return None;
            }
            let ones = (!self.cache).trailing_zeros().min(self.count);
            // Сдвиг бывает и на все 64 бита: 63 единицы и ноль в полном запасе.
            if ones < self.count {
                total += ones;
                self.cache = self.cache.checked_shr(ones + 1).unwrap_or(0);
                self.count -= ones + 1;
                return Some(total);
            }
            total += ones;
            self.cache = self.cache.checked_shr(ones).unwrap_or(0);
            self.count -= ones;
            if total > MAX_UNARY {
                return None;
            }
        }
    }
}

/// CRC-32 (IEEE, как у zlib).
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (index, entry) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 != 0 { 0xedb8_8320 ^ (value >> 1) } else { value >> 1 };
            }
            *entry = value;
        }
        table
    });
    !data.iter().fold(!0u32, |crc, byte| table[((crc ^ u32::from(*byte)) & 0xff) as usize] ^ (crc >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_zlib() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn bits_come_lowest_first() {
        // 0b1011_0111: три единицы, ноль, затем 0b1011 младшими вперёд.
        let mut bits = Bits::new(&[0b1011_0111, 0xff, 0x00]);
        assert_eq!(bits.unary(), Some(3));
        assert_eq!(bits.read(4), Some(0b1011));
        // Остальные единицы: ещё 8 из второго байта, ноль — в третьем.
        assert_eq!(bits.unary(), Some(8));
        // 63 единицы и ноль: весь запас в 64 бита уходит одним кодом.
        let mut bits = Bits::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0b101]);
        assert_eq!(bits.unary(), Some(63));
        assert_eq!(bits.unary(), Some(1));
    }
}
