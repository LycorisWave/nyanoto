//! Буквы сцен: раскладка parley, контуры из шрифта (skrifa) кривыми — повернуть можно как угодно.
//! Размеры в em: базовая линия y = 0, ось y вниз.

use std::sync::Arc;

use parley::fontique::Blob;
use parley::{FontContext, FontStyle, FontWeight, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};
use tiny_skia::{Path, PathBuilder, Rect};

/// Шрифты вшиты (все SIL OFL, с кириллицей) — везде одинаково. Выбор по жанру.
///
/// Узкий гротеск — рок и панк.
static OSWALD: &[u8] = include_bytes!("../../fonts/Oswald.ttf");
/// Широкий гротеск — хип-хоп и электроника. Оставлены латиница и кириллица.
static UNBOUNDED: &[u8] = include_bytes!("../../fonts/Unbounded.ttf");
/// Контрастная антиква — поп, баллады, джаз, классика.
static PLAYFAIR: &[u8] = include_bytes!("../../fonts/PlayfairDisplay.ttf");
static PLAYFAIR_ITALIC: &[u8] = include_bytes!("../../fonts/PlayfairDisplay-Italic.ttf");
/// Каллиграфия пером — «Чернила». Оставлены латиница и кириллица.
static GREAT_VIBES: &[u8] = include_bytes!("../../fonts/GreatVibes.ttf");
/// Пиксельный для «Орбиты», сетка 8 точек. Без изменений — имя зарезервировано.
static PRESS_START: &[u8] = include_bytes!("../../fonts/PressStart2P.ttf");

/// Кегль разметки: parley считает в точках, контуры потом делятся на него.
const LAYOUT_SIZE: f32 = 64.0;

/// Начертание слова.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Узкий гротеск: главные слова громких мест…
    Heavy,
    /// …и служебные рядом с ними.
    Light,
    /// Узкий гротеск спокойных мест.
    Narrow,
    NarrowThin,
    /// Широкий гротеск: громко…
    WideHeavy,
    /// …спокойно…
    Wide,
    /// …и служебные слова.
    WideThin,
    /// Антиква: громко…
    SerifBold,
    /// …спокойно…
    Serif,
    /// …и служебные слова курсивом.
    SerifItalic,
    /// Каллиграфия.
    Script,
    /// Пиксельный шрифт табло.
    Pixel,
    /// Моноширинный — терминал и подписи сети.
    Mono,
    MonoBold,
}

impl Face {
    fn family(self) -> &'static str {
        match self {
            Face::Heavy | Face::Light | Face::Narrow | Face::NarrowThin => "Oswald, sans-serif",
            Face::WideHeavy | Face::Wide | Face::WideThin => "Unbounded, sans-serif",
            Face::SerifBold | Face::Serif | Face::SerifItalic => "Playfair Display, serif",
            Face::Script => "Great Vibes, cursive",
            Face::Pixel => "Press Start 2P, monospace",
            // Adwaita Mono — в GNOME, Cascadia и Consolas — в Windows.
            Face::Mono | Face::MonoBold => "Adwaita Mono, DejaVu Sans Mono, Cascadia Mono, Consolas, Noto Sans Mono, monospace",
        }
    }

    fn weight(self) -> f32 {
        match self {
            Face::Heavy => 700.0,
            Face::Light => 400.0,
            Face::Narrow => 500.0,
            Face::NarrowThin => 300.0,
            Face::WideHeavy => 800.0,
            Face::Wide => 500.0,
            Face::WideThin => 300.0,
            Face::SerifBold => 800.0,
            Face::Serif => 500.0,
            Face::SerifItalic => 400.0,
            Face::Script | Face::Pixel | Face::Mono => 400.0,
            Face::MonoBold => 700.0,
        }
    }
}

/// Буква: контур от её начала и место в слове.
pub struct Glyph {
    pub path: Option<Path>,
    pub x: f32,
    pub advance: f32,
}

/// Слово кеглем 1.
pub struct Word {
    /// Все буквы одним контуром — так слово рисуется за один проход.
    pub path: Option<Path>,
    pub glyphs: Vec<Glyph>,
    pub advance: f32,
    /// Габарит закрашенного (верх отрицательный: выше базовой линии).
    pub ink: Rect,
}

impl Word {
    /// Высота заглавных (или самого слова, если его габарит меньше).
    pub fn ink_height(&self) -> f32 {
        self.ink.height().max(0.01)
    }

    pub fn ink_width(&self) -> f32 {
        self.ink.width().max(0.01)
    }
}

pub struct Fonts {
    fonts: FontContext,
    layouts: LayoutContext<[u8; 4]>,
    layout: Layout<[u8; 4]>,
}

impl Fonts {
    /// Список шрифтов системы читается здесь — вызывать, когда сцены нужны.
    pub fn new() -> Self {
        let mut fonts = FontContext::new();
        for font in [OSWALD, UNBOUNDED, PLAYFAIR, PLAYFAIR_ITALIC, GREAT_VIBES, PRESS_START] {
            fonts.collection.register_fonts(Blob::new(Arc::new(font)), None);
        }
        Self { fonts, layouts: LayoutContext::new(), layout: Layout::new() }
    }

    /// Раскладывает слово и снимает контуры его букв.
    pub fn word(&mut self, text: &str, face: Face) -> Word {
        let mut builder = self.layouts.ranged_builder(&mut self.fonts, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(face.family().into()));
        builder.push_default(StyleProperty::FontSize(LAYOUT_SIZE));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(face.weight())));
        if face == Face::SerifItalic {
            builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
        }
        builder.build_into(&mut self.layout, text);
        self.layout.break_all_lines(None);

        let unit = 1.0 / LAYOUT_SIZE;
        let mut whole = PathBuilder::new();
        let mut glyphs = Vec::new();
        let mut advance = 0.0f32;
        if let Some(line) = self.layout.lines().next() {
            let baseline = line.metrics().baseline;
            advance = line.metrics().advance * unit;
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let font = run.run().font();
                let Ok(font_ref) = skrifa::FontRef::from_index(font.data.data(), font.index) else {
                    continue;
                };
                let coords: Vec<NormalizedCoord> =
                    run.run().normalized_coords().iter().map(|coord| NormalizedCoord::from_bits(*coord)).collect();
                let location = LocationRef::new(&coords);
                let metrics = skrifa::metrics::Metrics::new(&font_ref, Size::unscaled(), location);
                let em = f32::from(metrics.units_per_em.max(1));
                let size = run.run().font_size() * unit;
                // Курсив, которого нет в шрифте, parley просит дорисовать наклоном.
                let skew = run.run().synthesis().skew().map_or(0.0, |degrees| degrees.to_radians().tan());
                let outlines = font_ref.outline_glyphs();
                for glyph in run.positioned_glyphs() {
                    let x = glyph.x * unit;
                    let y = (glyph.y - baseline) * unit;
                    let path = outlines.get(GlyphId::new(glyph.id)).and_then(|outline| {
                        let mut builder = PathBuilder::new();
                        let mut pen = Pen { builder: &mut builder, x: 0.0, y, scale: size / em, skew };
                        outline.draw(DrawSettings::unhinted(Size::unscaled(), location), &mut pen).ok()?;
                        builder.finish()
                    });
                    if let Some(path) = &path
                        && let Some(placed) = path.clone().transform(tiny_skia::Transform::from_translate(x, 0.0))
                    {
                        whole.push_path(&placed);
                    }
                    glyphs.push(Glyph { path, x, advance: glyph.advance * unit });
                }
            }
        }
        let path = whole.finish();
        let ink = path
            .as_ref()
            .and_then(Path::compute_tight_bounds)
            .unwrap_or_else(|| Rect::from_xywh(0.0, -0.7, advance.max(0.01), 0.7).unwrap());
        Word { path, glyphs, advance, ink }
    }
}

/// Перо skrifa → контур tiny-skia: единицы шрифта в доли кегля, ось y вниз.
struct Pen<'a> {
    builder: &'a mut PathBuilder,
    x: f32,
    y: f32,
    scale: f32,
    skew: f32,
}

impl Pen<'_> {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + (x + y * self.skew) * self.scale, self.y - y * self.scale)
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.point(x, y);
        self.builder.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.point(x, y);
        self.builder.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.point(cx0, cy0);
        let (x, y) = self.point(x, y);
        self.builder.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.point(cx0, cy0);
        let (cx1, cy1) = self.point(cx1, cy1);
        let (x, y) = self.point(x, y);
        self.builder.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_have_outlines_in_em() {
        let mut fonts = Fonts::new();
        let word = fonts.word("ГРОМЧЕ", Face::Heavy);
        assert!(word.path.is_some());
        assert_eq!(word.glyphs.len(), 6);
        // Заглавные Oswald — около 0.8 кегля высотой, узкие: шесть букв уже 4 em.
        assert!((0.6..1.0).contains(&word.ink_height()), "{}", word.ink_height());
        assert!(word.advance > 1.5 && word.advance < 4.0, "{}", word.advance);
        assert!(word.ink.top() < 0.0 && word.ink.bottom() < 0.05);
        let serif = fonts.word("tender", Face::SerifItalic);
        assert!(serif.path.is_some() && serif.advance > 1.0);
        // Вшитые находятся по имени и не подменяются системными.
        let narrow = fonts.word("кота", Face::Narrow).advance;
        let wide = fonts.word("кота", Face::Wide).advance;
        assert!(wide > narrow * 1.5, "{narrow} {wide}");
        let script = fonts.word("кота", Face::Script);
        let serif = fonts.word("кота", Face::Serif);
        assert!(script.path.is_some() && (script.advance - serif.advance).abs() > 0.05);
        // Пробелы и пустота — без контуров, но не падают.
        assert!(fonts.word(" ", Face::Light).path.is_none());
        assert!(fonts.word("", Face::Light).glyphs.is_empty());
    }
}
