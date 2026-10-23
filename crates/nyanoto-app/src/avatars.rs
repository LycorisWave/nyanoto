//! Аватары исполнителей: квадрат [`SIDE`] точек в `avatars` рядом с базой — оригинал можно удалить.
//! Имя — хэш имени без регистра: один аватар и по папкам, и по тегам. С прозрачностью — PNG, иначе JPEG.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use image::DynamicImage;
use image::imageops::FilterType;

/// С запасом для масштаба 4 и крупного узла карты.
pub const SIDE: u32 = 512;
const JPEG: u8 = 90;
const EXTENSIONS: [&str; 2] = ["jpg", "png"];

/// Какие картинки можно выбрать аватаром.
pub const PICTURES: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

pub struct Avatars {
    folder: Option<PathBuf>,
    /// Ключ имени исполнителя → файл аватара.
    files: HashMap<String, PathBuf>,
}

impl Avatars {
    /// Читает, какие аватары уже есть. Без папки данных аватаров не бывает.
    pub fn open(folder: Option<PathBuf>) -> Self {
        let mut files = HashMap::new();
        if let Some(entries) = folder.as_deref().and_then(|folder| std::fs::read_dir(folder).ok()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let (Some(stem), Some(extension)) = (
                    path.file_stem().and_then(|stem| stem.to_str()),
                    path.extension().and_then(|extension| extension.to_str()),
                ) else {
                    continue;
                };
                if stem.len() == 16 && EXTENSIONS.contains(&extension) {
                    files.insert(stem.to_string(), path.clone());
                }
            }
        }
        Self { folder, files }
    }

    /// Файл аватара исполнителя, если пользователь его выбрал.
    pub fn get(&self, name: &str) -> Option<&Path> {
        self.files.get(&key(name)).map(PathBuf::as_path)
    }

    /// Путь для нового аватара без расширения: его выберет [`make`].
    pub fn target(&self, name: &str) -> Option<PathBuf> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        self.folder.as_ref().map(|folder| folder.join(key(name)))
    }

    pub fn set(&mut self, name: &str, file: PathBuf) {
        self.files.insert(key(name), file);
    }

    /// Удаляет аватар исполнителя.
    pub fn remove(&mut self, name: &str) -> std::io::Result<()> {
        if let Some(file) = self.files.remove(&key(name)) {
            std::fs::remove_file(file)?;
        }
        Ok(())
    }
}

/// Ключ имени: FNV-1a, чтобы не менялся от версии к версии Rust.
fn key(name: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.trim().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Аватар из `picture` в `base` (.jpg или .png). Долго — из фонового потока.
pub fn make(picture: &Path, base: &Path) -> Result<PathBuf, String> {
    // Расширение у скачанных картинок бывает чужим: формат — по содержимому.
    let image = image::ImageReader::open(picture)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| error.to_string())?;
    let avatar = square(&image);
    let side = avatar.width().min(SIDE);
    let avatar = if avatar.width() > side { avatar.resize_exact(side, side, FilterType::Lanczos3) } else { avatar };
    let transparent = avatar.color().has_alpha() && avatar.to_rgba8().pixels().any(|pixel| pixel[3] < 255);
    let mut bytes = Vec::new();
    let extension = if transparent {
        avatar
            .to_rgba8()
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        "png"
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, JPEG)
            .encode_image(&avatar.to_rgb8())
            .map_err(|error| error.to_string())?;
        "jpg"
    };
    let file = base.with_extension(extension);
    write(&file, &bytes).map_err(|error| error.to_string())?;
    for other in EXTENSIONS.iter().filter(|other| **other != extension) {
        let _ = std::fs::remove_file(base.with_extension(other));
    }
    Ok(file)
}

/// Квадрат: у вертикального фото ближе к верху (там лицо), иначе из середины.
fn square(image: &DynamicImage) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    let side = width.min(height);
    let x = (width - side) / 2;
    let y = if height > width { (height - side) / 4 } else { (height - side) / 2 };
    image.crop_imm(x, y, side, side)
}

/// Через временный файл — без битых аватаров.
fn write(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = file.with_extension("tmp");
    std::fs::File::create(&temporary)?.write_all(bytes)?;
    std::fs::rename(&temporary, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nyanoto-avatars-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn portrait_becomes_a_small_square_from_the_top() {
        let dir = dir("portrait");
        // Верхняя половина красная, нижняя синяя: квадрат должен взять в основном красное.
        let photo = image::RgbImage::from_fn(1200, 2400, |_, y| if y < 1200 { image::Rgb([220, 0, 0]) } else { image::Rgb([0, 0, 220]) });
        // Расширение врёт: внутри PNG.
        let picture = dir.join("photo.jpg");
        photo.save_with_format(&picture, image::ImageFormat::Png).unwrap();
        let avatars = Avatars::open(Some(dir.join("avatars")));
        let base = avatars.target("Земфира").unwrap();
        let file = make(&picture, &base).unwrap();
        assert_eq!(file.extension().unwrap(), "jpg");
        let avatar = image::open(&file).unwrap().to_rgb8();
        assert_eq!(avatar.dimensions(), (SIDE, SIDE));
        let red = avatar.pixels().filter(|pixel| pixel[0] > 150).count();
        assert!(red > avatar.pixels().len() / 2, "верх фото: {red}");

        // Другой регистр — тот же аватар, и после перезапуска тоже.
        let mut avatars = Avatars::open(Some(dir.join("avatars")));
        assert_eq!(avatars.get("земфира"), Some(file.as_path()));
        assert_eq!(avatars.get("Земфира "), Some(file.as_path()));
        assert_eq!(avatars.get("Сплин"), None);
        avatars.remove("ЗЕМФИРА").unwrap();
        assert!(!file.exists());
        assert_eq!(Avatars::open(Some(dir.join("avatars"))).get("Земфира"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transparent_picture_stays_png_and_small_one_is_not_enlarged() {
        let dir = dir("png");
        let logo = image::RgbaImage::from_fn(300, 200, |x, _| image::Rgba([0, 120, 0, if x < 150 { 0 } else { 255 }]));
        let picture = dir.join("logo.png");
        logo.save(&picture).unwrap();
        let base = dir.join("avatars").join(key("Лого"));
        // Был JPEG — после новой картинки с прозрачностью остаётся только PNG.
        write(&base.with_extension("jpg"), b"old").unwrap();
        let file = make(&picture, &base).unwrap();
        assert_eq!(file.extension().unwrap(), "png");
        assert!(!base.with_extension("jpg").exists());
        let avatar = image::open(&file).unwrap();
        assert_eq!((avatar.width(), avatar.height()), (200, 200));
        assert!(avatar.to_rgba8().pixels().any(|pixel| pixel[3] == 0));
        assert!(make(&dir.join("нет такой.png"), &base).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
