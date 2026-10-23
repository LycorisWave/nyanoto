//! Старые кодировки в тегах и файлах: «Ãîðîä» вместо «Город» (CP1251 как Latin-1) и наоборот.

/// Текст файла. UTF-8 с BOM или без, UTF-16 с BOM, иначе Windows-1251.
/// Второе значение — файл был в Windows-1251.
pub fn decode_text(bytes: &[u8]) -> (String, bool) {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return (String::from_utf8_lossy(rest).into_owned(), false);
    }
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|pair| unit(*pair)).collect();
        (String::from_utf16_lossy(&units), false)
    };
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, u16::from_be_bytes);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), false),
        Err(_) => (bytes.iter().map(|&byte| cp1251(byte)).collect(), true),
    }
}

/// Байт Windows-1251 в символ.
pub fn cp1251(byte: u8) -> char {
    const HIGH: [u16; 64] = [
        0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039,
        0x040A, 0x040C, 0x040B, 0x040F, 0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
        0xFFFD, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F, 0x00A0, 0x040E, 0x045E, 0x0408,
        0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407,
        0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB,
        0x0458, 0x0405, 0x0455, 0x0457,
    ];
    match byte {
        0..=0x7F => char::from(byte),
        0x80..=0xBF => char::from_u32(u32::from(HIGH[usize::from(byte - 0x80)])).unwrap_or('\u{FFFD}'),
        // А…я идут подряд.
        _ => char::from_u32(0x0410 + u32::from(byte - 0xC0)).unwrap_or('\u{FFFD}'),
    }
}

fn has_cyrillic(text: &str) -> bool {
    text.chars().any(|char| ('\u{0400}'..='\u{04FF}').contains(&char))
}

/// Чинит русский, прочитанный не в той кодировке; иначе `None`. «Björk» не трогаем.
pub fn fix_mojibake(text: &str) -> Option<String> {
    if text.chars().any(|char| u32::from(char) > 0xFF) {
        return None;
    }
    let bytes: Vec<u8> = text.chars().map(|char| char as u8).collect();
    if !bytes.iter().any(|byte| *byte >= 0x80) {
        return None;
    }
    // UTF-8, прочитанный как Latin-1.
    if let Ok(decoded) = std::str::from_utf8(&bytes)
        && has_cyrillic(decoded)
    {
        return Some(decoded.to_string());
    }
    // Windows-1251, прочитанная как Latin-1: почти все буквы — из верхней половины.
    let letters = text.chars().filter(|char| char.is_alphabetic()).count();
    let high = bytes
        .iter()
        .filter(|byte| **byte >= 0xC0 || **byte == 0xA8 || **byte == 0xB8)
        .count();
    if high < 3 || high * 10 < letters * 6 {
        return None;
    }
    let decoded: String = bytes.iter().map(|&byte| cp1251(byte)).collect();
    has_cyrillic(&decoded).then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Как строку прочитал бы разборщик тегов, считающий её Latin-1.
    fn as_latin1(bytes: &[u8]) -> String {
        bytes.iter().map(|&byte| char::from(byte)).collect()
    }

    #[test]
    fn russian_in_wrong_encoding_is_fixed() {
        // «Кино — Группа крови» в Windows-1251.
        let cp1251 = b"\xca\xe8\xed\xee \x97 \xc3\xf0\xf3\xef\xef\xe0 \xea\xf0\xee\xe2\xe8";
        assert_eq!(fix_mojibake(&as_latin1(cp1251)).as_deref(), Some("Кино — Группа крови"));
        assert_eq!(fix_mojibake(&as_latin1("Сплин".as_bytes())).as_deref(), Some("Сплин"));
        assert_eq!(fix_mojibake(&as_latin1(b"\xa8\xeb\xea\xe0")).as_deref(), Some("Ёлка"));
    }

    #[test]
    fn real_latin_text_stays() {
        for text in ["Björk", "Mötley Crüe", "Café del Mar", "Sigur Rós — Ágætis byrjun", "Plain ASCII", "Кино"] {
            assert_eq!(fix_mojibake(text), None, "{text}");
        }
    }

    #[test]
    fn text_files_in_old_encodings() {
        let (text, legacy) = decode_text(b"[00:01.00]\xcf\xf0\xe8\xe2\xe5\xf2 \xb8\xa8");
        assert_eq!(text, "[00:01.00]Привет ёЁ");
        assert!(legacy);
        assert_eq!(decode_text("\u{feff}Да".as_bytes()), ("Да".to_string(), false));
        assert_eq!(decode_text(b"\xff\xfe\x14\x04\x30\x04"), ("Да".to_string(), false));
        assert_eq!(decode_text("Да".as_bytes()), ("Да".to_string(), false));
    }
}
