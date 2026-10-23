//! Форматы, которых нет в Symphonia. Отдают кадры f32, дальше всё как с Symphonia.
//!
//! - WavPack, APE, Musepack — эталонные библиотеки (`vendor/`): только они бит в бит.
//! - TTA — свой код (у libtta LGPL), сверен бит в бит.
//! - DSD — свой код, главное — перевод в PCM.

mod ape;
pub(crate) mod dsd;
pub mod ffmpeg;
mod musepack;
mod tags;
mod tta;
mod wavpack;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use symphonia::core::audio::Channels;
use symphonia::core::meta::Tag;

use crate::source::{OpenOptions, TrackError, TrackInfo};

/// Декодер формата, который сам читает файл.
pub(crate) trait Stream: Send {
    /// Дописывает кадры в `out`. Ничего не дописал — файл кончился.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), TrackError>;

    /// Встаёт точно на `frame` (не дальше конца) и возвращает, откуда пойдёт звук.
    fn seek(&mut self, frame: u64) -> Result<u64, TrackError>;

    /// Сколько мест не декодировалось и заменено тишиной.
    fn decode_errors(&self) -> u64 {
        0
    }

    /// `Some(false)` — сумма не сошлась, `None` — не хранится или не просили.
    fn finalize(&mut self) -> Option<bool> {
        None
    }
}

/// Открытый файл формата из этого модуля.
pub(crate) struct Opened {
    pub stream: Box<dyn Stream>,
    pub info: TrackInfo,
    /// Каналы в том порядке, в каком идут сэмплы в кадре.
    pub layout: Channels,
    /// Сколько кадров в файле по его заголовку.
    pub frames: Option<u64>,
    /// Частота DSD — у DSD, где `info` описывает уже PCM.
    pub dsd_rate: Option<u32>,
    /// Где в файле тег ID3v2, если он не в начале (DSF, DFF) или в начале перед TTA.
    pub id3v2: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Tta,
    WavPack,
    Dsf,
    Dff,
    Musepack,
    Ape,
    /// Только для FFmpeg. Узнаём заранее, иначе Symphonia найдёт в DTS/AC3 «MP3» и сыграет мусор.
    Ffmpeg,
}

/// По первым байтам (ID3v2 пропускаем). `None` — пусть открывает Symphonia.
pub(crate) fn open(path: &Path, options: OpenOptions) -> Option<Result<Opened, TrackError>> {
    let mut file = File::open(path).ok()?;
    let (kind, offset) = sniff(&mut file)?;
    Some(match kind {
        Kind::Tta => tta::open(file, offset),
        Kind::WavPack => wavpack::open(path, file, options),
        Kind::Dsf => dsd::open_dsf(file),
        Kind::Dff => dsd::open_dff(file),
        // ID3v2 перед потоком libmpcdec пропускает сама.
        Kind::Musepack => musepack::open(file),
        // ID3v2 перед потоком SDK тоже пропускает сам.
        Kind::Ape => ape::open(file, options),
        Kind::Ffmpeg => {
            drop(file);
            ffmpeg::open(path)
        }
    })
}

/// Встроенная библиотека или свой декодер: для «О приложении».
#[derive(Clone, Debug)]
pub struct Component {
    pub name: &'static str,
    pub version: String,
    pub license: &'static str,
}

/// Версии вшитых библиотек и FFmpeg, если загружен.
pub fn components() -> Vec<Component> {
    unsafe extern "C" {
        fn opus_get_version_string() -> *const std::ffi::c_char;
        fn WavpackGetLibraryVersionString() -> *const std::ffi::c_char;
        fn nyanoto_ape_version() -> *const std::ffi::c_char;
    }
    let text = |pointer: *const std::ffi::c_char| unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    let opus = text(unsafe { opus_get_version_string() });
    let ffmpeg = match ffmpeg::status() {
        ffmpeg::Status::Loaded { version, .. } => version,
        ffmpeg::Status::Disabled => "—".into(),
        ffmpeg::Status::NotLoaded => "7.x".into(),
        ffmpeg::Status::Missing(_) => "?".into(),
    };
    vec![
        Component { name: "libopus", version: opus.trim_start_matches("libopus ").to_string(), license: "BSD-3-Clause" },
        Component { name: "libwavpack", version: text(unsafe { WavpackGetLibraryVersionString() }), license: "BSD-3-Clause" },
        Component { name: "Monkey's Audio SDK", version: text(unsafe { nyanoto_ape_version() }), license: "BSD-3-Clause" },
        Component { name: "libmpcdec (Musepack)", version: "r495".into(), license: "BSD-3-Clause" },
        Component { name: "Signalsmith Stretch", version: "1.3.1".into(), license: "MIT" },
        Component { name: "FFmpeg", version: ffmpeg, license: "LGPL-2.1-or-later" },
    ]
}

/// Сведения для библиотеки: lofty TTA и DSD не знает. Без декодирования.
#[derive(Clone, Debug)]
pub struct Probe {
    /// `tta`, `dsd`, `wavpack`, `ape`, `mpc` или имя кодека FFmpeg.
    pub format: String,
    /// Без потерь ли формат, если это знает декодер (FFmpeg); иначе решает библиотека по имени.
    pub lossless: Option<bool>,
    /// Частота файла; у DSD — частота DSD (2 822 400 для DSD64).
    pub sample_rate: u32,
    /// Разрядность; у DSD — 1, у Musepack (с потерями) нет.
    pub bits: Option<u32>,
    pub channels: usize,
    pub duration: Duration,
    /// Текстовые теги: ключи как в файле, у знакомых — стандартный смысл.
    pub tags: Vec<Tag>,
}

/// Сведения о файле, если это формат из этого модуля или его знает FFmpeg.
pub fn probe(path: &Path) -> Option<Probe> {
    let Some(Ok(opened)) = open(path, OpenOptions::default()) else {
        return probe_ffmpeg(path);
    };
    let info = &opened.info;
    let format = match info.codec.as_str() {
        "dsf" | "dff" | "wavpack-dsd" => "dsd",
        codec if codec.starts_with("mpc") => "mpc",
        "tta" => "tta",
        "wavpack" => "wavpack",
        "wavpack-lossy" => "wavpack-lossy",
        "ape" => "ape",
        // Открылся через FFmpeg (узнан по сигнатуре).
        _ => return probe_ffmpeg(path),
    }
    .to_string();
    let mut file = File::open(path).ok()?;
    let tags = tags::all(&mut file, opened.id3v2);
    Some(Probe {
        lossless: (format == "wavpack-lossy").then_some(false),
        format,
        sample_rate: opened.dsd_rate.or(info.sample_rate)?,
        bits: if opened.dsd_rate.is_some() { Some(1) } else { info.bits_per_sample },
        channels: info.channels?,
        duration: info.duration?,
        tags,
    })
}

fn probe_ffmpeg(path: &Path) -> Option<Probe> {
    let described = ffmpeg::describe(path)?;
    let info = described.info;
    Some(Probe {
        format: described.format,
        lossless: Some(described.lossless),
        sample_rate: info.sample_rate?,
        bits: info.bits_per_sample,
        channels: info.channels?,
        duration: info.duration.unwrap_or_default(),
        tags: described.tags,
    })
}

/// Файл, который не открыли ни свои декодеры, ни Symphonia, — через FFmpeg.
pub(crate) fn open_ffmpeg(path: &Path) -> Result<Opened, TrackError> {
    ffmpeg::open(path)
}

/// Формат файла и где начинаются его данные.
fn sniff(file: &mut File) -> Option<(Kind, u64)> {
    let mut first = [0u8; 10];
    file.read_exact(&mut first).ok()?;
    let mut offset = 0u64;
    if &first[..3] == b"ID3" {
        offset = tags::id3v2_size(&first)?;
    }
    let mut head = [0u8; 16];
    file.seek(SeekFrom::Start(offset)).ok()?;
    let read = file.read(&mut head).ok()?;
    if read < 8 {
        return None;
    }
    if ffmpeg_only(&head) {
        return Some((Kind::Ffmpeg, offset));
    }
    let kind = match &head[..4] {
        b"TTA1" => Kind::Tta,
        b"wvpk" => Kind::WavPack,
        b"DSD " => Kind::Dsf,
        b"FRM8" => Kind::Dff,
        b"MPCK" => Kind::Musepack,
        // «MACF» — APE с плавающей точкой (Monkey's Audio 10 и новее).
        b"MAC " | b"MACF" => Kind::Ape,
        // SV7: «MP+» и номер версии потока.
        [b'M', b'P', b'+', _] => Kind::Musepack,
        _ => return None,
    };
    file.seek(SeekFrom::Start(offset)).ok()?;
    Some((kind, offset))
}

/// Сигнатуры форматов, которые декодирует только FFmpeg.
fn ffmpeg_only(head: &[u8; 16]) -> bool {
    const ASF: [u8; 16] = [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C];
    let dts = [[0x7F, 0xFE, 0x80, 0x01], [0xFE, 0x7F, 0x01, 0x80], [0x1F, 0xFF, 0xE8, 0x00], [0xFF, 0x1F, 0x00, 0xE8]];
    head == &ASF
        // AC3 и E-AC3: синхрослово 0x0B77 (и с переставленными байтами).
        || head[..2] == [0x0B, 0x77]
        || head[..2] == [0x77, 0x0B]
        || dts.iter().any(|sync| head[..4] == *sync)
        || &head[..8] == b"DTSHDHDR"
        // TrueHD и MLP: большое синхрослово после четырёх байт заголовка блока.
        || head[4..7] == [0xF8, 0x72, 0x6F] && matches!(head[7], 0xBA | 0xBB)
        || &head[..4] == b"tBaK"
        || &head[..4] == b"ajkg"
        // ATRAC в OMA/AA3 и RealAudio.
        || head[..3].eq_ignore_ascii_case(b"ea3")
        || &head[..4] == b".ra\xfd"
        || &head[..4] == b".RMF"
}

/// Раскладка по маске WAVE_FORMAT_EXTENSIBLE, без маски — как у WAV.
fn layout(channels: usize, mask: u32) -> Channels {
    let from_mask = Channels::from_bits_truncate(mask);
    if mask != 0 && from_mask.count() == channels {
        return from_mask;
    }
    use Channels as C;
    match channels {
        1 => C::FRONT_LEFT,
        2 => C::FRONT_LEFT | C::FRONT_RIGHT,
        3 => C::FRONT_LEFT | C::FRONT_RIGHT | C::FRONT_CENTRE,
        4 => C::FRONT_LEFT | C::FRONT_RIGHT | C::REAR_LEFT | C::REAR_RIGHT,
        5 => C::FRONT_LEFT | C::FRONT_RIGHT | C::FRONT_CENTRE | C::REAR_LEFT | C::REAR_RIGHT,
        6 => C::FRONT_LEFT | C::FRONT_RIGHT | C::FRONT_CENTRE | C::LFE1 | C::REAR_LEFT | C::REAR_RIGHT,
        7 => C::FRONT_LEFT | C::FRONT_RIGHT | C::FRONT_CENTRE | C::LFE1 | C::REAR_CENTRE | C::SIDE_LEFT | C::SIDE_RIGHT,
        8 => {
            C::FRONT_LEFT
                | C::FRONT_RIGHT
                | C::FRONT_CENTRE
                | C::LFE1
                | C::REAR_LEFT
                | C::REAR_RIGHT
                | C::SIDE_LEFT
                | C::SIDE_RIGHT
        }
        // Больше восьми — первые биты маски по порядку.
        count => Channels::from_bits_truncate(((1u64 << count.min(32)) - 1) as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_only_formats_are_recognised_by_signature() {
        let head = |bytes: &[u8]| {
            let mut head = [0u8; 16];
            head[..bytes.len()].copy_from_slice(bytes);
            head
        };
        for (what, bytes) in [
            ("ASF/WMA", &[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C][..]),
            ("AC3", &[0x0B, 0x77, 0x12, 0x34][..]),
            ("DTS", &[0x7F, 0xFE, 0x80, 0x01][..]),
            ("DTS 14 бит", &[0x1F, 0xFF, 0xE8, 0x00][..]),
            ("DTS-HD", b"DTSHDHDR"),
            ("TrueHD", &[0x00, 0x00, 0x00, 0x00, 0xF8, 0x72, 0x6F, 0xBA][..]),
            ("TAK", b"tBaK"),
            ("Shorten", b"ajkg"),
        ] {
            assert!(ffmpeg_only(&head(bytes)), "{what}");
        }
        // Обычные форматы к FFmpeg не уходят.
        for bytes in [&b"fLaC"[..], b"RIFF\0\0\0\0WAVE", b"OggS", &[0xFF, 0xFB, 0x90, 0x00][..], b"wvpk"] {
            assert!(!ffmpeg_only(&head(bytes)));
        }
    }

    #[test]
    fn layouts_follow_the_mask_or_the_wav_defaults() {
        assert_eq!(layout(2, 0), Channels::FRONT_LEFT | Channels::FRONT_RIGHT);
        // Маска 5.1 с боковыми каналами вместо тыловых.
        assert_eq!(layout(6, 0x60f).count(), 6);
        assert!(layout(6, 0x60f).contains(Channels::SIDE_LEFT));
        // Маска не на то число каналов — обычная раскладка.
        assert_eq!(layout(6, 0x3), layout(6, 0));
        assert_eq!(layout(10, 0).count(), 10);
    }
}
