//! Для тестов: временные папки, повторяемый генератор и FLAC без сжатия с тегами и обложкой.

use std::fs;
use std::ops::Range;
use std::path::PathBuf;

/// Папка для файлов одного теста, удаляется при выходе из него.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nyanoto-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Пишет файл, создавая недостающие папки.
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Простой генератор псевдослучайных чисел: тестам нужна повторяемость.
pub struct Rng(pub u64);

impl Rng {
    // Имя как у итератора, но это не итератор: последовательность бесконечна.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
}

pub const FLAC_BLOCK: usize = 4096;
/// Смещение MD5 в файле: метка `fLaC`, заголовок блока и 18 байт STREAMINFO перед суммой.
pub const FLAC_MD5: Range<usize> = 26..42;

pub struct Flac {
    pub bytes: Vec<u8>,
    /// Где в `bytes` лежит каждый кадр.
    pub frames: Vec<Range<usize>>,
}

/// Стереосигнал: синус с шумом, каналы разные.
pub fn signal(frames: usize, rate: u32) -> Vec<i16> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    (0..frames)
        .flat_map(|frame| {
            let phase = frame as f32 * 330.0 * std::f32::consts::TAU / rate as f32;
            let noise = (rng.next() % 2001) as f32 - 1000.0;
            let left = phase.sin() * 12_000.0 + noise;
            let right = (phase * 1.5).cos() * 9_000.0 - noise;
            [left as i16, right as i16]
        })
        .collect()
}

/// Кодирует чередующиеся 16-битные стереосэмплы в FLAC без тегов.
pub fn encode_flac(samples: &[i16], rate: u32) -> Flac {
    encode_flac_tagged(samples, rate, &[], 0)
}

/// FLAC с тегами и «обложкой» `cover_bytes` байт (важен только размер).
pub fn encode_flac_tagged(
    samples: &[i16],
    rate: u32,
    tags: &[(&str, &str)],
    cover_bytes: usize,
) -> Flac {
    const STREAMINFO: u8 = 0;
    const VORBIS_COMMENT: u8 = 4;
    const PICTURE: u8 = 6;

    let total = (samples.len() / 2) as u64;
    let mut info = Vec::with_capacity(34);
    info.extend_from_slice(&(FLAC_BLOCK as u16).to_be_bytes());
    info.extend_from_slice(&(FLAC_BLOCK as u16).to_be_bytes());
    info.extend_from_slice(&[0; 6]);
    // 20 бит частоты, 3 бита «каналы − 1», 5 бит «разрядность − 1», 36 бит длины.
    let packed = (u64::from(rate) << 44) | (1 << 41) | (15 << 36) | total;
    info.extend_from_slice(&packed.to_be_bytes());
    let pcm: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
    info.extend_from_slice(&md5(&pcm));

    let mut blocks = vec![(STREAMINFO, info)];
    if !tags.is_empty() {
        let vendor = b"nyanoto-testkit";
        let mut comment = Vec::new();
        comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        comment.extend_from_slice(vendor);
        comment.extend_from_slice(&(tags.len() as u32).to_le_bytes());
        for (key, value) in tags {
            let field = format!("{key}={value}");
            comment.extend_from_slice(&(field.len() as u32).to_le_bytes());
            comment.extend_from_slice(field.as_bytes());
        }
        blocks.push((VORBIS_COMMENT, comment));
    }
    if cover_bytes > 0 {
        let mime = b"image/jpeg";
        let mut picture = Vec::with_capacity(cover_bytes + 64);
        picture.extend_from_slice(&3u32.to_be_bytes());
        picture.extend_from_slice(&(mime.len() as u32).to_be_bytes());
        picture.extend_from_slice(mime);
        // Пустое описание, затем ширина, высота, глубина цвета и размер палитры.
        picture.extend_from_slice(&0u32.to_be_bytes());
        for value in [1000u32, 1000, 24, 0] {
            picture.extend_from_slice(&value.to_be_bytes());
        }
        picture.extend_from_slice(&(cover_bytes as u32).to_be_bytes());
        picture.extend(std::iter::repeat_n(0xA5u8, cover_bytes));
        blocks.push((PICTURE, picture));
    }

    let mut bytes = b"fLaC".to_vec();
    let last = blocks.len() - 1;
    for (index, (kind, body)) in blocks.iter().enumerate() {
        assert!(body.len() < 1 << 24, "блок метаданных длиннее 16 МБ");
        bytes.push(if index == last { 0x80 | kind } else { *kind });
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        bytes.extend_from_slice(body);
    }

    let mut frames = Vec::new();
    for (number, block) in samples.chunks(FLAC_BLOCK * 2).enumerate() {
        let start = bytes.len();
        // Синхрокод, блок постоянный (16 бит в конце), частота из STREAMINFO, 2 канала по 16 бит.
        let mut frame = vec![0xFF, 0xF8, 0x70, 0x18];
        if number < 0x80 {
            frame.push(number as u8);
        } else {
            assert!(number < 0x800, "слишком длинный тестовый файл");
            frame.push(0xC0 | (number >> 6) as u8);
            frame.push(0x80 | (number & 0x3F) as u8);
        }
        frame.extend_from_slice(&((block.len() / 2 - 1) as u16).to_be_bytes());
        frame.push(crc8(&frame));
        for channel in 0..2 {
            frame.push(0x02);
            for sample in block.iter().skip(channel).step_by(2) {
                frame.extend_from_slice(&sample.to_be_bytes());
            }
        }
        frame.extend_from_slice(&crc16(&frame).to_be_bytes());
        bytes.extend_from_slice(&frame);
        frames.push(start..bytes.len());
    }
    Flac { bytes, frames }
}

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut crc, byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
        crc
    })
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, byte| {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
        crc
    })
}

fn md5(data: &[u8]) -> [u8; 16] {
    const SHIFTS: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let mut state = [0x6745_2301u32, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];

    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_le_bytes());

    for block in message.as_chunks::<64>().0 {
        let words: Vec<u32> = block
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for round in 0..64 {
            let (mixed, index) = match round / 16 {
                0 => ((b & c) | (!b & d), round),
                1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
                2 => (b ^ c ^ d, (3 * round + 5) % 16),
                _ => (c ^ (b | !d), (7 * round) % 16),
            };
            let constant = ((round as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
            let sum = mixed
                .wrapping_add(a)
                .wrapping_add(constant)
                .wrapping_add(words[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(SHIFTS[round / 16 * 4 + round % 4]));
        }
        for (total, part) in state.iter_mut().zip([a, b, c, d]) {
            *total = total.wrapping_add(part);
        }
    }

    let mut digest = [0u8; 16];
    for (bytes, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *bytes = word.to_le_bytes();
    }
    digest
}
