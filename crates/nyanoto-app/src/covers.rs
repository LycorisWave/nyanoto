//! Обложки и фото исполнителей: поиск и уменьшение в фоне.
//!
//! Оригинал в мегабайты окну не нужен — уменьшаем до показа. Копии на диске (JPEG)
//! переживают перезапуск; папка не растёт выше потолка.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use image::DynamicImage;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Насколько подробны копии обложек на диске.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    High,
    Medium,
    Minimal,
}

impl Quality {
    /// В том порядке, в каком уровни стоят в настройках.
    pub const ALL: [Quality; 3] = [Quality::High, Quality::Medium, Quality::Minimal];

    pub fn code(self) -> &'static str {
        match self {
            Quality::High => "high",
            Quality::Medium => "medium",
            Quality::Minimal => "minimal",
        }
    }

    /// Уровень из настройки; без настройки — средний.
    pub fn from_code(code: Option<&str>) -> Self {
        Self::ALL.into_iter().find(|quality| Some(quality.code()) == code).unwrap_or(Quality::Medium)
    }

    /// Сторона копии: минимальная — списки, средняя — шапка на масштабе 2, высокая — 3–4.
    fn side(self) -> u32 {
        match self {
            Quality::High => 480,
            Quality::Medium => 240,
            Quality::Minimal => 128,
        }
    }

    fn jpeg(self) -> u8 {
        match self {
            Quality::High => 92,
            Quality::Medium => 90,
            Quality::Minimal => 80,
        }
    }

    /// Потолок папки кэша: на любом уровне это порядка трёх тысяч альбомов.
    fn limit(self) -> u64 {
        match self {
            Quality::High => 256 * 1024 * 1024,
            Quality::Medium => 64 * 1024 * 1024,
            Quality::Minimal => 16 * 1024 * 1024,
        }
    }

    /// Начало имени копии: по нему видно, к какому уровню она относится.
    fn prefix(self) -> &'static str {
        match self {
            Quality::High => "h-",
            Quality::Medium => "m-",
            Quality::Minimal => "s-",
        }
    }

    fn pack(disk: Option<Quality>) -> u8 {
        disk.map_or(0, |quality| quality as u8 + 1)
    }

    fn unpack(value: u8) -> Option<Quality> {
        Self::ALL.into_iter().find(|quality| *quality as u8 + 1 == value)
    }
}

/// Имена файлов, которые считаются обложкой альбома, в порядке предпочтения.
const COVER_NAMES: &[&str] = &[
    "cover.jpg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.png",
    "folder.webp",
    "front.jpg",
    "front.png",
    "front.webp",
    "album.jpg",
    "albumart.jpg",
];

/// Фото исполнителя в его папке: так их кладут Kodi, foobar2000 и MusicBee.
const ARTIST_NAMES: &[&str] = &[
    "artist.jpg",
    "artist.png",
    "artist.webp",
    "folder.jpg",
    "folder.png",
    "folder.webp",
];

/// Как подать картинку.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    /// Целиком, большей стороной в размер.
    Fit,
    /// Квадрат из середины.
    Square,
    /// Круг: квадрат с прозрачными углами.
    Round,
    /// Фон полного плеера ровно в размер окна: растянутая мелочь стоила десятки мс на кадр.
    Screen(u32),
}

/// Откуда взять картинку.
pub enum Source {
    /// Обложка альбома из его папки.
    Album(PathBuf),
    /// Свой аватар, а без него — картинка из папки исполнителя.
    Artist { own: Option<PathBuf>, folder: Option<PathBuf> },
}

pub type Pixels = SharedPixelBuffer<Rgba8Pixel>;
type Reply = Box<dyn FnOnce(Option<Pixels>) + Send>;

struct Job {
    source: Source,
    size: u32,
    shape: Shape,
    reply: Reply,
}

pub struct Loader {
    jobs: Sender<Job>,
    /// Папка для уменьшенных копий; без неё копий на диске не бывает.
    folder: Option<PathBuf>,
    /// Копии на диске: 0 — не делать, иначе уровень качества (см. [`Quality::pack`]).
    disk: Arc<AtomicU8>,
}

impl Loader {
    /// Пока не вызван [`Loader::set_disk`], копии на диске не делаются.
    pub fn new(folder: Option<PathBuf>) -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        let queue = Arc::new(Mutex::new(queue));
        let disk = Arc::new(AtomicU8::new(0));
        for number in 0..2 {
            let (queue, folder, disk) = (queue.clone(), folder.clone(), disk.clone());
            let _ = std::thread::Builder::new()
                .name(format!("nyanoto-covers-{number}"))
                .spawn(move || work(&queue, folder.as_deref(), &disk));
        }
        Self { jobs, folder, disk }
    }

    /// `None` — копии выключены. Лишние удаляются в фоне.
    pub fn set_disk(&self, disk: Option<Quality>) {
        self.disk.store(Quality::pack(disk), Ordering::Relaxed);
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let _ = std::thread::Builder::new().name("nyanoto-covers-sweep".into()).spawn(move || {
            crate::quiet::enter_background();
            sweep(&folder, disk);
        });
    }

    /// Обложка альбома из `folder` стороной `size`, ответ — из фонового потока.
    pub fn request(&self, folder: &Path, size: u32, reply: impl FnOnce(Option<Pixels>) + Send + 'static) {
        self.picture(Source::Album(folder.to_path_buf()), size, Shape::Fit, reply);
    }

    /// Картинка из `source` стороной `size` в форме `shape`.
    pub fn picture(&self, source: Source, size: u32, shape: Shape, reply: impl FnOnce(Option<Pixels>) + Send + 'static) {
        let _ = self.jobs.send(Job { source, size, shape, reply: Box::new(reply) });
    }
}

fn work(queue: &Mutex<Receiver<Job>>, folder: Option<&Path>, disk: &AtomicU8) {
    loop {
        let job = queue.lock().unwrap_or_else(|error| error.into_inner()).recv();
        let Ok(job) = job else {
            return;
        };
        let started = Instant::now();
        let copies = folder.zip(Quality::unpack(disk.load(Ordering::Relaxed)));
        // Битая картинка не должна ронять поток обложек.
        let pixels = std::panic::catch_unwind(|| load(&job.source, job.size, job.shape, copies))
            .ok()
            .flatten();
        crate::trace(&format!("cover {} µs", started.elapsed().as_micros()));
        (job.reply)(pixels);
    }
}

/// Обложка крупнее копии берётся из оригинала. Аватару копия не нужна.
fn load(source: &Source, size: u32, shape: Shape, copies: Option<(&Path, Quality)>) -> Option<Pixels> {
    let (path, copy) = match source {
        Source::Album(folder) => (find(folder)?, true),
        Source::Artist { own: Some(own), .. } => (own.clone(), false),
        Source::Artist { own: None, folder } => (find_in(folder.as_deref()?, ARTIST_NAMES)?, true),
    };
    let image = match copies.filter(|(_, quality)| copy && size <= quality.side()) {
        Some((cache, quality)) => cached(&path, cache, quality)?,
        None => image::open(path).ok()?,
    };
    let image = match shape {
        Shape::Fit => image.thumbnail(size, size).to_rgba8(),
        Shape::Screen(height) => backdrop_screen(&image, size, height),
        Shape::Square | Shape::Round => {
            let side = image.width().min(image.height());
            let square = image.crop_imm((image.width() - side) / 2, (image.height() - side) / 2, side, side);
            let mut square = square.resize_exact(size, size, image::imageops::FilterType::Triangle).to_rgba8();
            if shape == Shape::Round {
                round(&mut square);
            }
            square
        }
    };
    Some(SharedPixelBuffer::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

/// Размытый фон `width` × `height`: размываем в четверть размера, иначе секунды.
fn backdrop_screen(image: &DynamicImage, width: u32, height: u32) -> image::RgbaImage {
    let (width, height) = (width.max(1), height.max(1));
    let aspect = width as f32 / height as f32;
    let (iw, ih) = (image.width() as f32, image.height() as f32);
    let (crop_w, crop_h) = if iw / ih > aspect { (ih * aspect, ih) } else { (iw, iw / aspect) };
    let crop = image.crop_imm(((iw - crop_w) / 2.0) as u32, ((ih - crop_h) / 2.0) as u32, crop_w.max(1.0) as u32, crop_h.max(1.0) as u32);
    let tiny_h = ((24.0 / aspect).round() as u32).clamp(4, 96);
    let tiny = crop.resize_exact(24, tiny_h, image::imageops::FilterType::Triangle);
    let (quarter_w, quarter_h) = ((width / 4).max(8), (height / 4).max(8));
    let smooth = tiny.resize_exact(quarter_w, quarter_h, image::imageops::FilterType::CatmullRom).to_rgba8();
    let mut soft = image::imageops::blur(&smooth, quarter_w.max(quarter_h) as f32 / 64.0);
    for pixel in soft.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] = (f32::from(pixel[channel]) * 0.42) as u8;
        }
        pixel[3] = 255;
    }
    image::imageops::resize(&soft, width, height, image::imageops::FilterType::Triangle)
}

/// Вырезает круг здесь, а не в окне — так он одинаков при любой отрисовке.
fn round(image: &mut image::RgbaImage) {
    let radius = image.width().min(image.height()) as f32 / 2.0;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
        let inside = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
        pixel[3] = (f32::from(pixel[3]) * inside).round() as u8;
    }
}

/// Копия из кэша, а нет — делаем и кладём.
fn cached(path: &Path, cache: &Path, quality: Quality) -> Option<DynamicImage> {
    let file = cache.join(cache_key(path, quality)?);
    if let Ok(image) = image::open(&file) {
        return Some(image);
    }
    let image = image::open(path).ok()?.thumbnail(quality.side(), quality.side());
    // Не записалось (диск полон, папка недоступна) — не беда, покажем и так.
    let _ = store(&image, &file, quality.jpeg());
    Some(image)
}

/// Через временный файл: оборванная запись не оставит битой картинки.
fn store(image: &DynamicImage, file: &Path, jpeg: u8) -> std::io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temporary = file.with_extension("tmp");
    let mut bytes = Vec::new();
    let rgb = image.to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, jpeg)
        .encode_image(&rgb)
        .map_err(std::io::Error::other)?;
    std::fs::File::create(&temporary)?.write_all(&bytes)?;
    std::fs::rename(&temporary, file)
}

/// Имя копии: качество + FNV-1a от пути, размера и времени. Заменили обложку — новое имя.
fn cache_key(path: &Path, quality: Quality) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let source = format!("{}|{}|{modified}|{}|{}", path.display(), metadata.len(), quality.side(), quality.jpeg());
    for byte in source.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Some(format!("{}{hash:016x}.jpg", quality.prefix()))
}

/// Убирает недописанное и чужой уровень качества, остальное — до потолка.
fn sweep(cache: &Path, disk: Option<Quality>) {
    tidy(cache, disk.map(|quality| (quality.prefix(), quality.limit())));
}

/// Самые старые — вон, пока не останется три четверти. Трогает только .jpg и .tmp.
fn tidy(cache: &Path, keep: Option<(&str, u64)>) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let extension = path.extension().and_then(|extension| extension.to_str());
        if !matches!(extension, Some("jpg" | "tmp")) {
            continue;
        }
        let name = entry.file_name();
        let wanted = keep.is_some_and(|(prefix, _)| {
            extension == Some("jpg") && name.to_str().is_some_and(|name| name.starts_with(prefix))
        });
        if !wanted {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if let Ok(metadata) = entry.metadata()
            && metadata.is_file()
        {
            files.push((metadata.modified().unwrap_or(std::time::UNIX_EPOCH), metadata.len(), path));
        }
    }
    let Some((_, limit)) = keep else {
        return;
    };
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    if total <= limit {
        return;
    }
    files.sort();
    for (_, size, path) in files {
        if total <= limit / 4 * 3 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

/// Сторона обложки для виджета плеера системы.
const SYSTEM_SIDE: u32 = 256;

/// Копия 256 точек для виджета системы.
/// GNOME перечитывает обложку на каждый чих в главном потоке — оригинал вешал стол на 1–2 с.
/// Медленно, не из потока окна.
pub fn system_thumbnail(folder: &Path, dir: &Path) -> Option<PathBuf> {
    let original = find(folder)?;
    let name = cache_key(&original, Quality::Minimal)?.replacen("s-", "mpris-", 1);
    let file = dir.join(name);
    if !file.is_file() {
        let image = image::open(&original).ok()?.thumbnail(SYSTEM_SIDE, SYSTEM_SIDE);
        store(&image, &file, 88).ok()?;
    }
    // Прошлые копии больше не нужны.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path() != file {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Some(file)
}

/// Обложка лежит в папке альбома или, у многодисковых, на уровень выше.
fn find(folder: &Path) -> Option<PathBuf> {
    [Some(folder), folder.parent()]
        .into_iter()
        .flatten()
        .find_map(|dir| find_in(dir, COVER_NAMES))
}

/// Есть ли в папке исполнителя его фото (artist.jpg и т. п.).
pub fn has_artist_photo(folder: &Path) -> bool {
    find_in(folder, ARTIST_NAMES).is_some()
}

fn find_in(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    let files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().to_lowercase(), entry.path()))
        .collect();
    names.iter().find_map(|wanted| {
        files
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, path)| path.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(dir: &Path) -> PathBuf {
        let album = dir.join("album");
        std::fs::create_dir_all(&album).unwrap();
        let original = image::RgbImage::from_fn(1500, 1500, |x, y| image::Rgb([(x / 6) as u8, (y / 6) as u8, 128]));
        original.save(album.join("Cover.jpg")).unwrap();
        album
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn copy_is_made_once_and_reused() {
        let dir = std::env::temp_dir().join(format!("nyanoto-covers-{}", std::process::id()));
        let (album, cache) = (album(&dir), dir.join("cache"));

        let first = load(&Source::Album(album.clone()), 120, Shape::Fit, Some((&cache, Quality::Medium))).unwrap();
        let copies: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().collect();
        assert_eq!(copies.len(), 1);
        assert!(copies[0].file_name().to_string_lossy().starts_with("m-"));
        let copy = image::open(copies[0].path()).unwrap();
        assert_eq!((copy.width(), copy.height()), (240, 240));
        // Второй раз читается копия: её портим, и это видно по результату.
        let second = load(&Source::Album(album.clone()), 120, Shape::Fit, Some((&cache, Quality::Medium))).unwrap();
        assert_eq!((first.width(), first.height()), (second.width(), second.height()));
        std::fs::write(copies[0].path(), b"not a jpeg").unwrap();
        assert!(cached(&album.join("Cover.jpg"), &cache, Quality::Medium).is_some(), "битая копия пересоздаётся");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_level_has_its_own_copy_and_big_covers_come_from_the_original() {
        let dir = std::env::temp_dir().join(format!("nyanoto-levels-{}", std::process::id()));
        let (album, cache) = (album(&dir), dir.join("cache"));
        // Шапка альбома крупнее минимальной копии: берётся оригинал, копия не делается.
        let header = load(&Source::Album(album.clone()), 240, Shape::Fit, Some((&cache, Quality::Minimal))).unwrap();
        assert_eq!(header.width(), 240);
        assert!(!cache.exists());
        load(&Source::Album(album.clone()), 120, Shape::Fit, Some((&cache, Quality::Minimal))).unwrap();
        load(&Source::Album(album.clone()), 120, Shape::Fit, Some((&cache, Quality::High))).unwrap();
        let sides: Vec<(String, u32)> = names(&cache)
            .into_iter()
            .map(|name| {
                let side = image::open(cache.join(&name)).unwrap().width();
                (name[..2].to_string(), side)
            })
            .collect();
        assert_eq!(sides, [("h-".to_string(), 480), ("s-".to_string(), 128)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn square_covers_and_round_artist_photos() {
        let dir = std::env::temp_dir().join(format!("nyanoto-shapes-{}", std::process::id()));
        let artist = dir.join("Исполнитель");
        std::fs::create_dir_all(&artist).unwrap();
        // Широкая картинка: квадрат берётся из середины.
        let wide = image::RgbImage::from_fn(300, 100, |x, _| if (100..200).contains(&x) { image::Rgb([0, 200, 0]) } else { image::Rgb([200, 0, 0]) });
        wide.save(artist.join("Artist.JPG")).unwrap();
        let source = || Source::Artist { own: None, folder: Some(artist.clone()) };
        let square = load(&source(), 64, Shape::Square, None).unwrap();
        assert_eq!((square.width(), square.height()), (64, 64));
        let pixel = |pixels: &Pixels, x: usize, y: usize| pixels.as_slice()[y * pixels.width() as usize + x];
        assert!(pixel(&square, 2, 32).g > 150 && pixel(&square, 61, 32).g > 150, "середина картинки");
        let circle = load(&source(), 64, Shape::Round, None).unwrap();
        assert_eq!(pixel(&circle, 0, 0).a, 0, "угол прозрачный");
        assert_eq!(pixel(&circle, 32, 32).a, 255, "середина видна");
        assert!(pixel(&circle, 32, 0).a > 200, "край круга на месте");
        let backdrop = load(&source(), 32, Shape::Screen(20), None).unwrap();
        assert_eq!((backdrop.width(), backdrop.height()), (32, 20));
        assert!(pixel(&backdrop, 16, 16).g < 120, "затемнён");
        assert_eq!(pixel(&backdrop, 0, 0).a, 255, "непрозрачен");
        // Копия для виджета плеера системы: маленькая, одна в своей папке.
        let thumbs = dir.join("mpris");
        std::fs::create_dir_all(&thumbs).unwrap();
        std::fs::write(thumbs.join("mpris-old.jpg"), b"x").unwrap();
        let album = dir.join("album");
        std::fs::create_dir_all(&album).unwrap();
        image::RgbImage::from_pixel(1200, 1200, image::Rgb([10, 200, 30])).save(album.join("cover.jpg")).unwrap();
        let thumb = system_thumbnail(&album, &thumbs).unwrap();
        let small = image::open(&thumb).unwrap();
        assert_eq!((small.width(), small.height()), (256, 256));
        assert_eq!(names(&thumbs), [thumb.file_name().unwrap().to_string_lossy().into_owned()]);
        // Свой аватар важнее картинки из папки; без того и другого картинки нет.
        let own = dir.join("own.png");
        image::RgbImage::from_pixel(10, 10, image::Rgb([0, 0, 250])).save(&own).unwrap();
        let mine = load(&Source::Artist { own: Some(own), folder: Some(artist.clone()) }, 16, Shape::Round, None).unwrap();
        assert!(pixel(&mine, 8, 8).b > 200);
        assert!(load(&Source::Artist { own: None, folder: Some(dir.clone()) }, 16, Shape::Round, None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_keeps_the_newest_copies_of_the_current_level() {
        let dir = std::env::temp_dir().join(format!("nyanoto-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..8u8 {
            let file = dir.join(format!("m-{index}.jpg"));
            std::fs::write(&file, vec![index; 1000]).unwrap();
            // Разное время изменения, чтобы было ясно, какие старше.
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + u64::from(index));
            std::fs::File::options().write(true).open(&file).unwrap().set_modified(time).unwrap();
        }
        // Чужой уровень, копия без уровня, недописанный и чужой файл (его не трогать).
        for name in ["h-0.jpg", "0123456789abcdef.jpg", "half.tmp", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        tidy(&dir, Some(("m-", 4000)));
        assert_eq!(names(&dir), ["m-5.jpg", "m-6.jpg", "m-7.jpg", "notes.txt"]);
        // Копии выключены — удаляются все.
        tidy(&dir, None);
        assert_eq!(names(&dir), ["notes.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quality_codes_round_trip() {
        for quality in Quality::ALL {
            assert_eq!(Quality::from_code(Some(quality.code())), quality);
            assert_eq!(Quality::unpack(Quality::pack(Some(quality))), Some(quality));
        }
        assert_eq!(Quality::from_code(None), Quality::Medium);
        assert_eq!(Quality::unpack(Quality::pack(None)), None);
    }
}
