//! Замер отрисовки без экрана: то же окно, но в памяти и на своих часах.
//! `cargo run --release -p nyanoto-app --features bench --example render_bench -- [сценарий] [ш в масштаб]`
//! Сценарии: all, drag, glide, lyrics, full, still. `BENCH_LOOP=1` — по кругу.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{Image, LogicalSize, Model, PhysicalSize, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer, VecModel};

slint::include_modules!();

thread_local! {
    static CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

struct Bench {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Bench {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        CLOCK.with(Cell::get)
    }
}

struct Screen {
    window: Rc<MinimalSoftwareWindow>,
    scale: f32,
    buffer: Vec<PremultipliedRgbaColor>,
    width: usize,
}

impl Screen {
    /// Логический размер окна.
    fn resize(&mut self, width: f32, height: f32) {
        let physical = PhysicalSize::new((width * self.scale) as u32, (height * self.scale) as u32);
        self.width = physical.width as usize;
        self.buffer.resize(physical.width as usize * physical.height as usize, PremultipliedRgbaColor::default());
        self.window.set_size(physical);
        self.window.dispatch_event(WindowEvent::Resized { size: LogicalSize::new(width, height) });
    }

    /// Один кадр: часы вперёд на 16 мс, анимации, отрисовка. Время отрисовки (с
    /// раскладкой) или `None`, если рисовать было нечего.
    fn frame(&mut self) -> Option<Duration> {
        CLOCK.with(|clock| clock.set(clock.get() + Duration::from_millis(16)));
        let start = Instant::now();
        slint::platform::update_timers_and_animations();
        let width = self.width;
        let buffer = &mut self.buffer;
        let drawn = self.window.draw_if_needed(|renderer| {
            renderer.render(buffer, width);
        });
        drawn.then(|| start.elapsed())
    }

    /// Кадры, пока что-то меняется (не больше `limit`).
    fn settle(&mut self, limit: usize) -> Vec<Duration> {
        let mut times = Vec::new();
        for _ in 0..limit {
            match self.frame() {
                Some(time) => times.push(time),
                None if times.len() > 2 => break,
                None => {}
            }
        }
        times
    }
}

fn report(name: &str, times: &[Duration]) {
    if std::env::var_os("BENCH_FRAMES").is_some() {
        let list: Vec<String> = times.iter().map(|time| format!("{:.1}", time.as_secs_f64() * 1000.0)).collect();
        println!("{name:>10}: {}", list.join(" "));
    }
    if times.is_empty() {
        println!("{name:>10}: кадров нет");
        return;
    }
    let mut ms: Vec<f64> = times.iter().map(|time| time.as_secs_f64() * 1000.0).collect();
    let first = ms[0];
    let total: f64 = ms.iter().sum();
    ms.sort_by(f64::total_cmp);
    let pick = |share: f64| ms[((ms.len() - 1) as f64 * share) as usize];
    println!(
        "{name:>10}: {:4} кадров  первый {first:6.1}  средний {:6.1}  медиана {:6.1}  90% {:6.1}  худший {:6.1} мс",
        ms.len(),
        total / ms.len() as f64,
        pick(0.5),
        pick(0.9),
        pick(1.0)
    );
}

/// Картинка-заглушка: круг (с прозрачными углами) или непрозрачный квадрат с
/// градиентом — как у плеера (`covers::Pixels`).
fn picture(side: u32, round: bool, seed: u32) -> Image {
    if !round && std::env::var_os("BENCH_RGB").is_some() {
        let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(side, side);
        for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
            let (x, y) = (index as u32 % side, index as u32 / side);
            *pixel = Rgb8Pixel { r: ((x * 255 / side) ^ seed) as u8, g: (y * 255 / side + seed * 40) as u8, b: (seed * 70 % 255) as u8 };
        }
        return Image::from_rgb8(buffer);
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    let r = side as f32 / 2.0;
    for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((index as u32 % side) as f32, (index as u32 / side) as f32);
        let inside = !round || ((x - r).powi(2) + (y - r).powi(2)).sqrt() <= r;
        *pixel = Rgba8Pixel {
            r: ((x * 255.0 / side as f32) as u32 ^ seed) as u8,
            g: ((y * 255.0 / side as f32) as u32 + seed * 40) as u8,
            b: (seed * 70 % 255) as u8,
            a: if inside { 255 } else { 0 },
        };
    }
    Image::from_rgba8(buffer)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scenario = args.first().cloned().unwrap_or_else(|| "all".into());
    let number = |index: usize, default: f32| args.get(index).and_then(|value| value.parse().ok()).unwrap_or(default);
    // По умолчанию — развёрнутое окно ноутбука пользователя: 2256 × 1461 точек
    // экрана, масштаб 1,33 × 1,07.
    let (width, height, scale) = (number(1, 1585.0), number(2, 1027.0), number(3, 1.4231));
    let looping = std::env::var_os("BENCH_LOOP").is_some();

    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Bench { window: window.clone() })).expect("платформа");
    let ui = AppWindow::new().expect("окно");
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    let mut screen = Screen { window: window.clone(), scale, buffer: Vec::new(), width: 0 };
    screen.resize(width, height);

    let side = (150.0 * scale) as u32;
    let artists: Vec<ArtistRow> = (0..40)
        .map(|index| ArtistRow {
            name: format!("Исполнитель номер {index}").into(),
            letter: "И".into(),
            detail: format!("{} альбомов · {} треков", index % 9 + 1, index * 7 + 3).into(),
            avatar: picture(side, true, index),
        })
        .collect();
    ui.set_artists(Rc::new(VecModel::from(artists)).into());
    ui.set_artists_grid(true);
    ui.set_empty(false);
    ui.set_page(0);
    ui.set_has_track(true);
    ui.set_now_title("01. Куда пропала Элис? - Ты довольна?".into());
    ui.set_now_owner("Куда пропала Элис?".into());
    ui.set_now_album("Ты довольна? (2024)".into());
    ui.set_now_quality("FLAC · 16 бит · 44,1 кГц · стерео".into());
    ui.set_now_cover(picture((64.0 * scale) as u32, false, 3));
    ui.set_volume(0.6);
    ui.set_duration(106.0);
    ui.set_position(2.0);
    ui.set_lyrics_enabled(true);
    ui.set_lyrics_synced(true);
    let lines: Vec<LyricRow> = (0..40)
        .map(|index| LyricRow { text: format!("Строка песни номер {index}, чуть длиннее").into(), current: index == 3 })
        .collect();
    ui.set_lyrics_lines(Rc::new(VecModel::from(lines)).into());
    ui.set_lyrics_title("01. Куда пропала Элис? - Ты довольна?".into());
    ui.show().expect("показ");
    let still = screen.settle(200);

    loop {
        if matches!(scenario.as_str(), "all" | "still") {
            report("покой", &still);
            screen.window.request_redraw();
            let mut full = Vec::new();
            for _ in 0..20 {
                // Новый буфер каждый кадр — перерисовка всего окна, как при смене размера.
                screen.resize(width, height - 1.0);
                full.extend(screen.frame());
                screen.resize(width, height);
                full.extend(screen.frame());
            }
            report("всё окно", &full);
        }
        if matches!(scenario.as_str(), "all" | "drag") {
            let mut times = Vec::new();
            let mut w = width;
            while w > 640.0 {
                w -= 8.0;
                screen.resize(w, height);
                times.extend(screen.frame());
            }
            while w < width {
                w += 8.0;
                screen.resize(w.min(width), height);
                times.extend(screen.frame());
            }
            report("край", &times);
            screen.settle(60);
        }
        if matches!(scenario.as_str(), "all" | "glide") {
            screen.resize(width * 0.62, height);
            report("плитки ←", &screen.settle(60));
            screen.resize(width, height);
            report("плитки →", &screen.settle(60));
        }
        if matches!(scenario.as_str(), "all" | "lyrics") {
            ui.set_lyrics_open(true);
            report("текст", &screen.settle(60));
            screen.resize(700.0, height);
            report("текст ←", &screen.settle(60));
            screen.resize(width, height);
            report("текст →", &screen.settle(60));
            ui.set_lyrics_open(false);
            screen.settle(60);
        }
        if matches!(scenario.as_str(), "all" | "full") {
            if ui.get_full_background().size().width == 0 {
                let physical = ((width * scale) as u32, (height * scale) as u32);
                ui.set_full_background(picture(physical.0.max(physical.1), false, 9));
                ui.set_full_cover(picture((560.0 * scale) as u32, false, 5));
            }
            ui.set_full_lyrics(true);
            ui.set_full_open(true);
            report("плеер ↑", &screen.settle(90));
            ui.set_full_open(false);
            report("плеер ↓", &screen.settle(90));
        }
        if !looping {
            break;
        }
    }
    let _ = ui.get_artists().row_count();
}
