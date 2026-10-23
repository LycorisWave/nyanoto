//! Карта прослушиваний: силовой граф, как в Obsidian.
//! Площадь узла — прослушивания. Модель «остывает», как d3-force: успокоилась — не тратит процессор.

use nyanoto_library::GraphArtist;

/// Насколько быстро остывает модель за шаг: ~300 шагов от начала до покоя.
const ALPHA_DECAY: f32 = 0.0228;
/// Ниже этого модель считается остывшей.
const ALPHA_MIN: f32 = 0.002;
/// Сколько скорости теряется за шаг (трение).
const VELOCITY_DECAY: f32 = 0.4;
/// До какой «температуры» модель разогревается, пока узел тянут.
const DRAG_ALPHA: f32 = 0.3;
/// Зазор между альбомом и его исполнителем и между любыми узлами.
const GAP: f32 = 10.0;

#[derive(Clone, Debug)]
pub struct Node {
    pub x: f32,
    pub y: f32,
    vx: f32,
    vy: f32,
    pub r: f32,
    /// Номер исполнителя в данных карты.
    pub artist: usize,
    /// Номер альбома у исполнителя; `None` — это сам исполнитель.
    pub album: Option<usize>,
    /// Узел держат мышью.
    pinned: bool,
    /// Где был узел, когда его взяли, и где мышь взяла его относительно центра.
    anchor: (f32, f32),
    grip: (f32, f32),
}

pub struct Layout {
    pub nodes: Vec<Node>,
    /// Пружины: исполнитель — альбом.
    links: Vec<(usize, usize)>,
    alpha: f32,
    alpha_target: f32,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    /// Исполнители по спирали (слушаемые ближе), альбомы вокруг.
    pub fn new(artists: &[GraphArtist], width: f32, height: f32) -> Self {
        let (width, height) = (width.max(200.0), height.max(200.0));
        let base = width.min(height);
        // Размер — по времени прослушивания (у исполнителя и у альбома).
        let most = artists.iter().map(|artist| artist.listened_ms).max().unwrap_or(1).max(1) as f32;
        let most_album = artists
            .iter()
            .flat_map(|artist| artist.albums.iter().map(|album| album.listened_ms))
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let (cx, cy) = (width / 2.0, height / 2.0);
        let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
        let mut nodes = Vec::new();
        let mut links = Vec::new();
        for (index, artist) in artists.iter().enumerate() {
            let r = radius(artist.listened_ms as f32 / most, base * 0.035, base * 0.10);
            let distance = base * 0.09 * (index as f32 + 0.5).sqrt();
            let angle = index as f32 * golden;
            let (x, y) = (cx + distance * angle.cos(), cy + distance * angle.sin());
            let own = nodes.len();
            nodes.push(Node {
                x,
                y,
                vx: 0.0,
                vy: 0.0,
                r,
                artist: index,
                album: None,
                pinned: false,
                anchor: (x, y),
                grip: (0.0, 0.0),
            });
            let count = artist.albums.len().max(1) as f32;
            for (album, entry) in artist.albums.iter().enumerate() {
                let r_album = radius(entry.listened_ms as f32 / most_album, base * 0.012, base * 0.05);
                let around = angle + std::f32::consts::TAU * album as f32 / count;
                let reach = r + r_album + GAP;
                links.push((own, nodes.len()));
                nodes.push(Node {
                    x: x + reach * around.cos(),
                    y: y + reach * around.sin(),
                    vx: 0.0,
                    vy: 0.0,
                    r: r_album,
                    artist: index,
                    album: Some(album),
                    pinned: false,
                    anchor: (0.0, 0.0),
                    grip: (0.0, 0.0),
                });
            }
        }
        let mut layout = Self { nodes, links, alpha: 1.0, alpha_target: 0.0, width, height };
        layout.keep_inside();
        layout
    }

    /// Холст поменял размер: узлы сдвигаются вместе с центром.
    pub fn resize(&mut self, width: f32, height: f32) {
        let (width, height) = (width.max(200.0), height.max(200.0));
        if (width - self.width).abs() < 0.5 && (height - self.height).abs() < 0.5 {
            return;
        }
        let (dx, dy) = ((width - self.width) / 2.0, (height - self.height) / 2.0);
        for node in &mut self.nodes {
            node.x += dx;
            node.y += dy;
        }
        self.width = width;
        self.height = height;
        self.keep_inside();
        self.wake(0.3);
    }

    /// Разогревает модель, чтобы граф снова пришёл в равновесие.
    pub fn wake(&mut self, alpha: f32) {
        self.alpha = self.alpha.max(alpha);
    }

    /// Ещё движется: модель не остыла или узел держат мышью.
    pub fn moving(&self) -> bool {
        self.alpha > ALPHA_MIN || self.nodes.iter().any(|node| node.pinned)
    }

    /// Узел взяли мышью в точке (x, y) холста.
    pub fn grab(&mut self, index: usize, x: f32, y: f32) {
        let Some(node) = self.nodes.get_mut(index) else {
            return;
        };
        node.pinned = true;
        node.anchor = (node.x, node.y);
        node.grip = (x - node.x, y - node.y);
        node.vx = 0.0;
        node.vy = 0.0;
        self.alpha_target = DRAG_ALPHA;
        self.wake(DRAG_ALPHA);
    }

    /// Узел тянется за мышью упруго, не дальше четверти холста.
    pub fn drag_to(&mut self, index: usize, x: f32, y: f32) {
        let reach = self.width.min(self.height) * 0.25;
        let (width, height) = (self.width, self.height);
        let Some(node) = self.nodes.get_mut(index).filter(|node| node.pinned) else {
            return;
        };
        let (px, py) = (x - node.grip.0 - node.anchor.0, y - node.grip.1 - node.anchor.1);
        let length = (px * px + py * py).sqrt();
        let stretched = if length > 0.0 { reach * (length / reach).tanh() / length } else { 0.0 };
        node.x = (node.anchor.0 + px * stretched).clamp(node.r, width - node.r);
        node.y = (node.anchor.1 + py * stretched).clamp(node.r, height - node.r);
        node.vx = 0.0;
        node.vy = 0.0;
        self.alpha_target = DRAG_ALPHA;
        self.wake(DRAG_ALPHA);
    }

    /// Отпустили — модель чуть разогреваем, чтобы узел вернулся.
    pub fn release(&mut self, index: usize) {
        if let Some(node) = self.nodes.get_mut(index) {
            node.pinned = false;
        }
        if !self.nodes.iter().any(|node| node.pinned) {
            self.alpha_target = 0.0;
            self.wake(0.8);
        }
    }

    /// Один шаг модели. `false` — граф успокоился, шаги больше не нужны.
    pub fn tick(&mut self) -> bool {
        if !self.moving() {
            return false;
        }
        self.alpha += (self.alpha_target - self.alpha) * ALPHA_DECAY;
        let alpha = self.alpha;
        let (cx, cy) = (self.width / 2.0, self.height / 2.0);
        let count = self.nodes.len();

        // Пружины: альбом лёгкий и подтягивается к исполнителю, а не наоборот.
        for &(from, to) in &self.links {
            let (a, b) = (&self.nodes[from], &self.nodes[to]);
            let rest = a.r + b.r + GAP * 2.0;
            let dx = b.x + b.vx - a.x - a.vx;
            let dy = b.y + b.vy - a.y - a.vy;
            let length = (dx * dx + dy * dy).sqrt().max(0.01);
            let pull = (length - rest) / length * alpha * 0.7;
            let (mass_a, mass_b) = (a.r * a.r, b.r * b.r);
            let share = mass_a / (mass_a + mass_b);
            self.nodes[to].vx -= dx * pull * share;
            self.nodes[to].vy -= dy * pull * share;
            self.nodes[from].vx += dx * pull * (1.0 - share);
            self.nodes[from].vy += dy * pull * (1.0 - share);
        }

        // Отталкивание всех от всех и столкновения.
        for i in 0..count {
            for j in i + 1..count {
                let (a, b) = (&self.nodes[i], &self.nodes[j]);
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let distance = (dx * dx + dy * dy).sqrt().max(0.5);
                let (ux, uy) = (dx / distance, dy / distance);
                // Отталкивание короткое (∝ 1/d²) и сильнее у крупных.
                let reach = a.r + b.r + GAP * 3.0;
                let charge = 12.0 * alpha * (reach / distance.max(reach * 0.5)).powi(2);
                let (mass_a, mass_b) = (a.r * a.r, b.r * b.r);
                let share = mass_a / (mass_a + mass_b);
                let mut push = charge;
                let touching = a.r + b.r + GAP;
                if distance < touching {
                    push += (touching - distance) * 0.5;
                }
                self.nodes[i].vx -= ux * push * (1.0 - share);
                self.nodes[i].vy -= uy * push * (1.0 - share);
                self.nodes[j].vx += ux * push * share;
                self.nodes[j].vy += uy * push * share;
            }
        }

        // Притяжение к центру: исполнители не разлетаются, альбомы держатся у своих.
        for node in &mut self.nodes {
            let gravity = if node.album.is_none() { 0.08 } else { 0.006 };
            node.vx += (cx - node.x) * gravity * alpha;
            node.vy += (cy - node.y) * gravity * alpha;
        }

        for node in &mut self.nodes {
            if node.pinned {
                node.vx = 0.0;
                node.vy = 0.0;
                continue;
            }
            node.vx *= 1.0 - VELOCITY_DECAY;
            node.vy *= 1.0 - VELOCITY_DECAY;
            node.x += node.vx;
            node.y += node.vy;
        }
        self.keep_inside();
        self.moving()
    }

    fn keep_inside(&mut self) {
        for node in &mut self.nodes {
            node.x = node.x.clamp(node.r, (self.width - node.r).max(node.r));
            node.y = node.y.clamp(node.r, (self.height - node.r).max(node.r));
        }
    }

    /// Линии пружин в виде команд SVG-пути для одного `Path` на весь холст.
    pub fn links_path(&self) -> String {
        let mut path = String::with_capacity(self.links.len() * 32);
        for &(from, to) in &self.links {
            let (a, b) = (&self.nodes[from], &self.nodes[to]);
            path.push_str(&format!("M {:.1} {:.1} L {:.1} {:.1} ", a.x, a.y, b.x, b.y));
        }
        path
    }
}

/// Радиус по доле прослушиваний: площадь пропорциональна прослушиваниям.
fn radius(share: f32, smallest: f32, largest: f32) -> f32 {
    smallest + (largest - smallest) * share.clamp(0.0, 1.0).sqrt()
}

/// Цвет исполнителя: оттенки разнесены «золотым углом», чтобы соседи не сливались.
pub fn hue(artist: usize) -> f32 {
    (artist as f32 * 137.508) % 360.0
}

/// HSL → RGB (0–255).
pub fn hsl(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    let byte = |value: f32| ((value + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (byte(r), byte(g), byte(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyanoto_library::GraphAlbum;

    fn artists() -> Vec<GraphArtist> {
        (0..12)
            .map(|index| GraphArtist {
                name: format!("Исполнитель {index}"),
                tracks: 200 / (index + 1),
                listened_ms: 200_000 / (index as u64 + 1),
                albums: (0..(index % 5 + 1))
                    .map(|album| GraphAlbum {
                        title: Some(format!("Альбом {album}")),
                        plays: 4.0 / (album as f32 + 1.0),
                        listened_ms: 40_000 / (album as u64 + 1),
                        folder: format!("/music/{index}/{album}").into(),
                    })
                    .collect(),
                folder: None,
            })
            .collect()
    }

    fn settle(layout: &mut Layout) -> usize {
        let mut steps = 0;
        while layout.tick() {
            steps += 1;
            assert!(steps < 2_000, "граф не успокаивается");
        }
        steps
    }

    #[test]
    fn graph_settles_inside_the_canvas_without_overlaps() {
        let mut layout = Layout::new(&artists(), 900.0, 500.0);
        let steps = settle(&mut layout);
        assert!(steps > 50, "{steps} шагов");
        for node in &layout.nodes {
            assert!(node.x >= node.r - 0.01 && node.x <= 900.0 - node.r + 0.01);
            assert!(node.y >= node.r - 0.01 && node.y <= 500.0 - node.r + 0.01);
        }
        let nodes = &layout.nodes;
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                assert!(distance > (a.r + b.r) * 0.8, "узлы наползли: {distance} при {} + {}", a.r, b.r);
            }
        }
    }

    #[test]
    fn albums_stay_by_their_artist_and_the_biggest_artist_is_biggest() {
        let mut layout = Layout::new(&artists(), 900.0, 500.0);
        settle(&mut layout);
        for &(from, to) in &layout.links {
            let (a, b) = (&layout.nodes[from], &layout.nodes[to]);
            let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            assert!(distance < (a.r + b.r + GAP * 2.0) * 2.5, "альбом ушёл от исполнителя: {distance}");
        }
        let artists: Vec<&Node> = layout.nodes.iter().filter(|node| node.album.is_none()).collect();
        assert!(artists.windows(2).all(|pair| pair[0].r >= pair[1].r));
    }

    #[test]
    fn dragged_artist_pulls_its_albums_and_comes_back_near_the_others() {
        let mut layout = Layout::new(&artists(), 900.0, 500.0);
        settle(&mut layout);
        let album = layout.links[0].1;
        let before = (layout.nodes[album].x, layout.nodes[album].y);
        // Тянем самого крупного исполнителя на 200 точек вправо за 20 шагов.
        let (x, y) = (layout.nodes[0].x, layout.nodes[0].y);
        layout.grab(0, x, y);
        for step in 1..=20 {
            layout.drag_to(0, x + 10.0 * step as f32, y);
            layout.tick();
        }
        // Упругость: узел отстаёт от мыши, но далеко не уходит.
        let moved = layout.nodes[0].x - x;
        assert!(moved > 60.0 && moved < 200.0, "{moved}");
        assert!(layout.nodes[album].x > before.0 + 50.0, "альбом не потянулся за исполнителем");
        layout.release(0);
        settle(&mut layout);
        // Отпущенный исполнитель не улетел: он недалеко от центра холста.
        let node = &layout.nodes[0];
        let from_center = ((node.x - 450.0).powi(2) + (node.y - 250.0).powi(2)).sqrt();
        assert!(from_center < 300.0, "{from_center}");
    }

    #[test]
    fn colors_are_valid() {
        assert_eq!(hsl(0.0, 1.0, 0.5), (255, 0, 0));
        assert_eq!(hsl(120.0, 1.0, 0.5), (0, 255, 0));
        assert_eq!(hsl(240.0, 1.0, 0.5), (0, 0, 255));
    }
}
