//! FLAC для конвертации: фиксированные предсказатели 0–4, Райс, mid/side. Около `flac -3`, без LPC.
//! STREAMINFO — заглушка в начале, настоящая с MD5 в конце.

use std::io::{Seek, SeekFrom, Write};

/// Сэмплов на канал в кадре.
const BLOCK: usize = 4096;
const MAX_PARTITION_ORDER: u32 = 6;

/// Потоковая сумма MD5 (RFC 1321).
pub(crate) struct Md5 {
    state: [u32; 4],
    buffer: Vec<u8>,
    length: u64,
    table: [u32; 64],
}

impl Md5 {
    pub fn new() -> Self {
        let mut table = [0u32; 64];
        for (index, value) in table.iter_mut().enumerate() {
            *value = ((index as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
        }
        Self { state: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476], buffer: Vec::with_capacity(64), length: 0, table }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;
        if !self.buffer.is_empty() {
            let take = (64 - self.buffer.len()).min(data.len());
            self.buffer.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buffer.len() == 64 {
                let block: [u8; 64] = self.buffer[..].try_into().expect("ровно 64 байта");
                self.block(&block);
                self.buffer.clear();
            }
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            self.block(block);
        }
        self.buffer.extend_from_slice(rest);
    }

    fn block(&mut self, block: &[u8; 64]) {
        const SHIFTS: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for round in 0..64 {
            let (mixed, index) = match round / 16 {
                0 => ((b & c) | (!b & d), round),
                1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
                2 => (b ^ c ^ d, (3 * round + 5) % 16),
                _ => (c ^ (b | !d), (7 * round) % 16),
            };
            let sum = mixed.wrapping_add(a).wrapping_add(self.table[round]).wrapping_add(words[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(SHIFTS[round / 16 * 4 + round % 4]));
        }
        for (total, part) in self.state.iter_mut().zip([a, b, c, d]) {
            *total = total.wrapping_add(part);
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        let length = self.length * 8;
        self.update(&[0x80]);
        while self.buffer.len() != 56 {
            self.update(&[0]);
        }
        self.update(&length.to_le_bytes());
        let mut digest = [0u8; 16];
        for (bytes, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *bytes = word.to_le_bytes();
        }
        digest
    }
}

struct Bits {
    bytes: Vec<u8>,
    current: u64,
    count: u32,
}

impl Bits {
    fn new() -> Self {
        Self { bytes: Vec::with_capacity(BLOCK * 8), current: 0, count: 0 }
    }

    fn put(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        self.current = (self.current << bits) | (value & ((1u64 << bits) - 1));
        self.count += bits;
        while self.count >= 8 {
            self.count -= 8;
            self.bytes.push((self.current >> self.count) as u8);
        }
        self.current &= (1u64 << self.count) - 1;
    }

    fn signed(&mut self, value: i64, bits: u32) {
        self.put(value as u64, bits);
    }

    fn unary(&mut self, zeros: u64) {
        let mut left = zeros;
        while left >= 32 {
            self.put(0, 32);
            left -= 32;
        }
        self.put(1, left as u32 + 1);
    }

    fn align(&mut self) {
        if self.count > 0 {
            self.put(0, 8 - self.count);
        }
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

/// Остатки фиксированного предсказателя порядка `order`.
fn residuals(samples: &[i64], order: usize) -> Vec<i64> {
    (order..samples.len())
        .map(|n| {
            let x = |k: usize| samples[n - k];
            match order {
                0 => x(0),
                1 => x(0) - x(1),
                2 => x(0) - 2 * x(1) + x(2),
                3 => x(0) - 3 * x(1) + 3 * x(2) - x(3),
                _ => x(0) - 4 * x(1) + 6 * x(2) - 4 * x(3) + x(4),
            }
        })
        .collect()
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

/// Параметр Райса для куска по сумме его значений и примерная длина в битах.
fn estimate_rice(sum: u64, count: usize) -> (u32, u64) {
    if count == 0 {
        return (0, 0);
    }
    let count = count as u64;
    let mean = sum / count;
    let guess = 63 - (mean | 1).leading_zeros();
    let mut best = (0u32, u64::MAX);
    for k in guess.saturating_sub(1)..=(guess + 1).min(30) {
        let bits = count * (u64::from(k) + 1) + (sum >> k);
        if bits < best.1 {
            best = (k, bits);
        }
    }
    best
}

/// Разбиение остатков: суммы на мелком и слияние попарно.
fn plan_residual(values: &[u64], order: usize, block: usize) -> (u32, Vec<u32>, u64) {
    let mut finest = 0u32;
    while finest < MAX_PARTITION_ORDER && block.is_multiple_of(1 << (finest + 1)) && (block >> (finest + 1)) > order {
        finest += 1;
    }
    let size = block >> finest;
    let mut sums = Vec::with_capacity(1 << finest);
    let mut counts = Vec::with_capacity(1 << finest);
    let mut start = 0usize;
    for index in 0..1usize << finest {
        let count = if index == 0 { size - order } else { size };
        sums.push(values[start..start + count].iter().sum::<u64>());
        counts.push(count);
        start += count;
    }
    let mut best = (0u32, Vec::new(), u64::MAX);
    for partition_order in (0..=finest).rev() {
        let mut params = Vec::with_capacity(sums.len());
        let mut total = 6u64;
        for (sum, count) in sums.iter().zip(&counts) {
            let (k, bits) = estimate_rice(*sum, *count);
            params.push(k);
            total += bits;
        }
        let param_bits = if params.iter().any(|k| *k > 14) { 5 } else { 4 };
        total += param_bits * params.len() as u64;
        if total < best.2 {
            best = (partition_order, params, total);
        }
        if partition_order > 0 {
            sums = sums.chunks(2).map(|pair| pair.iter().sum()).collect();
            counts = counts.chunks(2).map(|pair| pair.iter().sum()).collect();
        }
    }
    best
}

/// Подкадр канала: выбранный порядок предсказателя и его стоимость.
struct Subframe {
    order: usize,
    zigzagged: Vec<u64>,
    partition_order: u32,
    params: Vec<u32>,
    bits: u64,
}

fn plan_subframe(samples: &[i64], bps: u32) -> Subframe {
    let block = samples.len();
    let verbatim = u64::from(bps) * block as u64;
    let mut best: Option<Subframe> = None;
    for order in 0..=4usize.min(block.saturating_sub(1)) {
        let zigzagged: Vec<u64> = residuals(samples, order).into_iter().map(zigzag).collect();
        let (partition_order, params, residual_bits) = plan_residual(&zigzagged, order, block);
        if residual_bits == u64::MAX {
            continue;
        }
        let bits = 8 + order as u64 * u64::from(bps) + residual_bits;
        if best.as_ref().is_none_or(|current| bits < current.bits) {
            best = Some(Subframe { order, zigzagged, partition_order, params, bits });
        }
    }
    match best {
        Some(subframe) if subframe.bits < verbatim + 8 => subframe,
        _ => Subframe { order: usize::MAX, zigzagged: Vec::new(), partition_order: 0, params: Vec::new(), bits: verbatim + 8 },
    }
}

fn write_subframe(bits: &mut Bits, samples: &[i64], bps: u32, plan: &Subframe) {
    if samples.iter().all(|sample| *sample == samples[0]) {
        bits.put(0, 8); // постоянный
        bits.signed(samples[0], bps);
        return;
    }
    if plan.order == usize::MAX {
        bits.put(0b0000_0010, 8); // как есть
        for &sample in samples {
            bits.signed(sample, bps);
        }
        return;
    }
    bits.put(0b0001_0000 | (plan.order as u64) << 1, 8);
    for &sample in &samples[..plan.order] {
        bits.signed(sample, bps);
    }
    // Райс с 4-битными параметрами, а если нужен параметр больше 14 — с 5-битными.
    let wide = plan.params.iter().any(|k| *k > 14);
    bits.put(u64::from(wide), 2);
    bits.put(u64::from(plan.partition_order), 4);
    let partitions = 1usize << plan.partition_order;
    let size = samples.len() / partitions;
    let mut start = 0usize;
    for (index, &k) in plan.params.iter().enumerate() {
        let count = if index == 0 { size - plan.order } else { size };
        bits.put(u64::from(k), if wide { 5 } else { 4 });
        for &value in &plan.zigzagged[start..start + count] {
            bits.unary(value >> k);
            bits.put(value & ((1u64 << k) - 1), k);
        }
        start += count;
    }
}

/// Номер кадра в «UTF-8» FLAC: до 36 бит, от одного до семи байт.
fn utf8_number(bits: &mut Bits, value: u64) {
    if value < 0x80 {
        bits.put(value, 8);
        return;
    }
    let count: u32 = match value {
        0x80..0x800 => 2,
        0x800..0x1_0000 => 3,
        0x1_0000..0x20_0000 => 4,
        0x20_0000..0x400_0000 => 5,
        0x400_0000..0x8000_0000 => 6,
        _ => 7,
    };
    let prefix = (0xFF00u64 >> count) & 0xFF;
    bits.put(prefix | (value >> (6 * (count - 1))), 8);
    for index in (0..count - 1).rev() {
        bits.put(0x80 | ((value >> (6 * index)) & 0x3F), 8);
    }
}

pub(crate) struct FlacWriter<W: Write + Seek> {
    out: W,
    rate: u32,
    channels: usize,
    bps: u32,
    /// Накопленные сэмплы по каналам до полного кадра.
    pending: Vec<Vec<i64>>,
    frame: u64,
    total: u64,
    min_frame: u32,
    max_frame: u32,
    md5: Md5,
}

impl<W: Write + Seek> FlacWriter<W> {
    pub fn new(mut out: W, rate: u32, channels: usize, bps: u32) -> std::io::Result<Self> {
        out.write_all(b"fLaC")?;
        // Заглушка STREAMINFO: настоящая запишется в конце.
        out.write_all(&[0x80, 0, 0, 34])?; // последний блок метаданных, 34 байта
        out.write_all(&[0u8; 34])?;
        Ok(Self {
            out,
            rate,
            channels,
            bps,
            pending: vec![Vec::with_capacity(BLOCK); channels],
            frame: 0,
            total: 0,
            min_frame: u32::MAX,
            max_frame: 0,
            md5: Md5::new(),
        })
    }

    /// Дописывает чередующиеся целые сэмплы.
    pub fn write(&mut self, interleaved: &[i32]) -> std::io::Result<()> {
        let bytes = self.bps.div_ceil(8) as usize;
        let mut raw = Vec::with_capacity(interleaved.len() * bytes);
        for &sample in interleaved {
            raw.extend_from_slice(&sample.to_le_bytes()[..bytes]);
        }
        self.md5.update(&raw);
        for frame in interleaved.chunks_exact(self.channels) {
            for (channel, &sample) in frame.iter().enumerate() {
                self.pending[channel].push(i64::from(sample));
            }
            if self.pending[0].len() == BLOCK {
                self.flush_frame()?;
            }
        }
        Ok(())
    }

    fn flush_frame(&mut self) -> std::io::Result<()> {
        let block = self.pending[0].len();
        if block == 0 {
            return Ok(());
        }
        let mut bits = Bits::new();
        bits.put(0b1111_1111_1111_1000, 16); // синхрослово, фиксированный размер кадра
        let (size_code, extra) = match block {
            4096 => (0b1100u64, None),
            1..=256 => (0b0110, Some((block as u64 - 1, 8))),
            _ => (0b0111, Some((block as u64 - 1, 16))),
        };
        bits.put(size_code, 4);
        bits.put(0, 4); // частота — из STREAMINFO

        // Стерео: левый/правый, левый/разность, разность/правый или середина/разность.
        let mut assignment = (self.channels - 1) as u64;
        let mut channels: Vec<(Vec<i64>, u32)> = self.pending.iter().map(|samples| (samples.clone(), self.bps)).collect();
        let mut plans: Vec<Subframe> = channels.iter().map(|(samples, bps)| plan_subframe(samples, *bps)).collect();
        if self.channels == 2 {
            let (left, right) = (&self.pending[0], &self.pending[1]);
            let side: Vec<i64> = left.iter().zip(right).map(|(l, r)| l - r).collect();
            let mid: Vec<i64> = left.iter().zip(right).map(|(l, r)| (l + r) >> 1).collect();
            let side_plan = plan_subframe(&side, self.bps + 1);
            let mid_plan = plan_subframe(&mid, self.bps);
            let options = [
                (plans[0].bits + plans[1].bits, 0b0001u64),
                (plans[0].bits + side_plan.bits, 0b1000),
                (side_plan.bits + plans[1].bits, 0b1001),
                (mid_plan.bits + side_plan.bits, 0b1010),
            ];
            let (_, chosen) = options.into_iter().min_by_key(|(cost, _)| *cost).expect("варианты есть");
            assignment = chosen;
            match chosen {
                0b1000 => {
                    channels = vec![(left.clone(), self.bps), (side.clone(), self.bps + 1)];
                    plans = vec![plans.swap_remove(0), side_plan];
                }
                0b1001 => {
                    channels = vec![(side.clone(), self.bps + 1), (right.clone(), self.bps)];
                    plans = vec![side_plan, plans.swap_remove(1)];
                }
                0b1010 => {
                    channels = vec![(mid, self.bps), (side, self.bps + 1)];
                    plans = vec![mid_plan, side_plan];
                }
                _ => {}
            }
        }
        bits.put(assignment, 4);
        bits.put(0, 3); // разрядность — из STREAMINFO
        bits.put(0, 1);
        utf8_number(&mut bits, self.frame);
        if let Some((value, width)) = extra {
            bits.put(value, width);
        }
        let header_crc = crc8(&bits.bytes);
        bits.put(u64::from(header_crc), 8);
        for ((samples, bps), plan) in channels.iter().zip(&plans) {
            write_subframe(&mut bits, samples, *bps, plan);
        }
        bits.align();
        let frame_crc = crc16(&bits.bytes);
        bits.put(u64::from(frame_crc), 16);

        self.out.write_all(&bits.bytes)?;
        let size = bits.bytes.len() as u32;
        self.min_frame = self.min_frame.min(size);
        self.max_frame = self.max_frame.max(size);
        self.frame += 1;
        self.total += block as u64;
        for channel in &mut self.pending {
            channel.clear();
        }
        Ok(())
    }

    /// Дописывает последний кадр и настоящую STREAMINFO.
    pub fn finish(mut self) -> std::io::Result<W> {
        self.flush_frame()?;
        let mut info = Bits::new();
        info.put(BLOCK as u64, 16);
        info.put(BLOCK as u64, 16);
        info.put(u64::from(if self.min_frame == u32::MAX { 0 } else { self.min_frame }), 24);
        info.put(u64::from(self.max_frame), 24);
        info.put(u64::from(self.rate), 20);
        info.put(self.channels as u64 - 1, 3);
        info.put(u64::from(self.bps - 1), 5);
        info.put(self.total >> 32, 4);
        info.put(self.total & 0xFFFF_FFFF, 32);
        let digest = std::mem::replace(&mut self.md5, Md5::new()).finish();
        for byte in digest {
            info.put(u64::from(byte), 8);
        }
        self.out.seek(SeekFrom::Start(8))?;
        self.out.write_all(&info.bytes)?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_digests() {
        let hex = |digest: [u8; 16]| digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let digest = |data: &[u8]| {
            let mut md5 = Md5::new();
            // Кусками неровной длины, как придут сэмплы.
            for piece in data.chunks(7) {
                md5.update(piece);
            }
            hex(md5.finish())
        };
        assert_eq!(digest(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(digest(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(digest(&[b'a'; 1000]), "cabe45dcc9ae5b66ba86600cca6b8ba8");
    }

    #[test]
    fn frame_numbers_are_utf8_coded() {
        let code = |value| {
            let mut bits = Bits::new();
            utf8_number(&mut bits, value);
            bits.bytes
        };
        assert_eq!(code(0x7F), [0x7F]);
        assert_eq!(code(0x80), [0xC2, 0x80]);
        assert_eq!(code(0x7FF), [0xDF, 0xBF]);
        assert_eq!(code(0x800), [0xE0, 0xA0, 0x80]);
        assert_eq!(code(0x1_0000), [0xF0, 0x90, 0x80, 0x80]);
    }
}
