//! «Сеть»: обмен как по TCP и UDP.
//! * пакеты — спокойно: TCP с рукопожатием, громко: UDP с потерями;
//! * терминал — лог как в tcpdump со спетыми словами;
//! * узлы и лог — сеть сверху, лог снизу.

use tiny_skia::{PathBuilder, Rect, Transform};

use super::{Ctx, Lazy, Part, Scene, draw_word, faces, ink_center, is_key};
use crate::stage::motion::{self, Entry, Exit, Rng, apply, ease_out, progress, tempo};
use crate::stage::paint::{self, Canvas, faded};
use crate::stage::text::{Face, Fonts, Word};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Packets,
    Terminal,
    Both,
}

/// Порог напряжения: выше — UDP.
const UDP_FROM: f32 = 0.55;

/// Узел — слово строки.
struct Node {
    word: Word,
    at: f32,
    x: f32,
    y: f32,
    size: f32,
    half: (f32, f32),
    address: Word,
}

/// Пакет: откуда, куда (номера узлов), когда вылетел и прилетел, вид.
struct Packet {
    from: usize,
    to: usize,
    start: f32,
    arrive: f32,
    kind: PacketKind,
    /// Потерян на этой доле пути (UDP).
    lost: Option<f32>,
    /// Подпись (SYN, ACK…).
    label: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum PacketKind {
    Data,
    Ack,
    Control,
    Datagram,
}

struct NetLine {
    nodes: Vec<Node>,
    packets: Vec<Packet>,
    udp: bool,
    entry: Entry,
    exit: Exit,
}

/// Строка лога: когда появляется, текст и слово-нагрузка.
struct Entry2 {
    at: f32,
    text: Word,
    payload: Option<Word>,
    /// Строка песни после приглашения (печатается по словам).
    prompt: Option<(usize, Vec<(Word, f32)>)>,
    /// Строка лога картинкой: буквы каждый кадр — 20 мс, копия — даром.
    strip: Option<(StripKey, tiny_skia::Pixmap)>,
}

/// С чем нарисована картинка строки лога.
#[derive(Clone, Copy, PartialEq)]
struct StripKey {
    size: f32,
    scale: f32,
    ink: [u8; 4],
    ground: [u8; 4],
    sung: usize,
}

pub struct Net {
    part: Part,
    variant: Variant,
    lines: Lazy<NetLine>,
    log: Vec<Entry2>,
    logged: usize,
    labels: Vec<Word>,
    glyph: Option<Word>,
    size: (f32, f32),
}

const LABELS: [&str; 4] = ["SYN", "SYN-ACK", "ACK", "UDP"];

impl Net {
    pub fn new(part: &Part, variant: Variant) -> Self {
        Self { part: *part, variant, lines: Lazy::default(), log: Vec::new(), logged: 0, labels: Vec::new(), glyph: None, size: (0.0, 0.0) }
    }

    fn region(&self, ctx: &Ctx) -> (Rect, Rect) {
        let (width, height) = (ctx.width, ctx.height);
        let full = Rect::from_xywh(0.0, 0.0, width, height).expect("размер больше нуля");
        match self.variant {
            Variant::Packets => (full, full),
            Variant::Terminal => (full, full),
            Variant::Both => {
                let split = height * 0.6;
                (Rect::from_xywh(0.0, 0.0, width, split).expect("часть"), Rect::from_xywh(0.0, split, width, height - split).expect("часть"))
            }
        }
    }

    fn net_line(&self, fonts: &mut Fonts, ctx: &Ctx, region: Rect, index: usize) -> NetLine {
        let line = &ctx.lines[index];
        let mut rng = Rng::new(motion::mix(self.part.seed, index as u64));
        let unit = region.width().min(region.height());
        let energy = ctx.mood.mean(line.start, line.end);
        let udp = energy >= UDP_FROM;
        let (key_face, small_face, upper) = faces(&self.part);
        let words: Vec<&str> = line.text.split_whitespace().collect();
        let mut nodes: Vec<Node> = Vec::new();
        let mut shrink = 1.0;
        for _ in 0..4 {
            nodes.clear();
            let mut ok = true;
            for (number, raw) in words.iter().enumerate() {
                let key = is_key(raw);
                let text = if upper { raw.to_uppercase() } else { raw.to_string() };
                let word = fonts.word(&text, if key { key_face } else { small_face });
                let size = unit * if key { 0.1 } else { 0.07 } * shrink;
                let half = (word.ink_width() * size / 2.0 + unit * 0.025, word.ink_height() * size / 2.0 + unit * 0.025);
                let address = fonts.word(&format!("10.0.{}.{}", rng.below(250) + 1, rng.below(250) + 2), Face::Mono);
                let margin = unit * 0.04;
                let mut found = None;
                for _ in 0..50 {
                    let x = rng.range(region.left() + half.0 + margin, (region.right() - half.0 - margin).max(region.left() + half.0 + margin + 1.0));
                    let y = rng.range(region.top() + half.1 + margin * 1.6, (region.bottom() - half.1 - margin).max(region.top() + half.1 + margin * 1.6 + 1.0));
                    let clear = nodes.iter().all(|other| (other.x - x).abs() > other.half.0 + half.0 + margin || (other.y - y).abs() > other.half.1 + half.1 + margin * 1.6);
                    if clear {
                        found = Some((x, y));
                        break;
                    }
                }
                let Some((x, y)) = found else {
                    ok = false;
                    break;
                };
                nodes.push(Node { word, at: line.words.get(number).copied().unwrap_or(line.start), x, y, size, half, address });
            }
            if ok {
                break;
            }
            shrink *= 0.8;
        }
        let pace = tempo(energy);
        let mut packets = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            if index == 0 {
                if !udp && nodes.len() > 1 {
                    // Рукопожатие между первыми двумя узлами к началу строки.
                    let step = 0.14 * pace;
                    let begin = node.at - 3.0 * step;
                    for (number, (from, to)) in [(0, 1), (1, 0), (0, 1)].into_iter().enumerate() {
                        let start = begin + number as f32 * step;
                        packets.push(Packet { from, to, start, arrive: start + step, kind: PacketKind::Control, lost: None, label: Some(number) });
                    }
                }
                continue;
            }
            let gap = (node.at - nodes[index - 1].at).max(0.08);
            let travel = (0.45 * pace).min(gap * 0.9);
            if udp {
                // Пачка датаграмм, прилетает к слову; часть теряется.
                let burst = 2 + (energy * 2.0) as usize;
                for number in 0..burst {
                    let start = node.at - travel - number as f32 * 0.08;
                    let lost = rng.chance(0.1).then(|| rng.range(0.3, 0.8));
                    packets.push(Packet { from: index - 1, to: index, start, arrive: start + travel, kind: PacketKind::Datagram, lost, label: (number == 0).then_some(3) });
                }
            } else {
                packets.push(Packet { from: index - 1, to: index, start: node.at - travel, arrive: node.at, kind: PacketKind::Data, lost: None, label: None });
                packets.push(Packet { from: index, to: index - 1, start: node.at + 0.05, arrive: node.at + 0.05 + travel * 0.8, kind: PacketKind::Ack, lost: None, label: Some(2) });
            }
        }
        // Фоновый поток по связям строки (UDP — чаще).
        if nodes.len() > 1 {
            let every = if udp { 0.7 - 0.3 * energy } else { 1.2 };
            let mut at = line.start;
            while at < line.end {
                let from = rng.below(nodes.len() - 1);
                let (from, to) = if rng.chance(0.5) { (from, from + 1) } else { (from + 1, from) };
                let travel = rng.range(0.3, 0.6) * pace;
                let lost = (udp && rng.chance(0.1)).then(|| rng.range(0.3, 0.8));
                packets.push(Packet { from, to, start: at, arrive: at + travel, kind: if udp { PacketKind::Datagram } else { PacketKind::Ack }, lost, label: None });
                at += every * rng.range(0.6, 1.4);
            }
        }
        NetLine { nodes, packets, udp, entry: Entry::pick(&mut rng, energy), exit: Exit::pick(&mut rng, energy) }
    }

    /// Строки лога части по порядку до строки `upto`.
    fn grow_log(&mut self, fonts: &mut Fonts, ctx: &Ctx, upto: usize) {
        while self.part.first + self.logged <= upto.min(self.part.last) {
            let index = self.part.first + self.logged;
            let line = &ctx.lines[index];
            let mut rng = Rng::new(motion::mix(self.part.seed ^ 0x106, index as u64));
            let energy = ctx.mood.mean(line.start, line.end);
            let udp = energy >= UDP_FROM;
            let upper = self.part.strong;
            let stamp = |at: f32| {
                let ms = (at.max(0.0) * 1000.0) as u32;
                format!("{:02}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
            };
            let (client, server) = (format!("10.0.{}.{}", rng.below(250) + 1, rng.below(250) + 2), format!("10.0.{}.{}", rng.below(250) + 1, rng.below(250) + 2));
            let words: Vec<(String, f32)> = line
                .text
                .split_whitespace()
                .enumerate()
                .map(|(number, raw)| (if upper { raw.to_uppercase() } else { raw.to_string() }, line.words.get(number).copied().unwrap_or(line.start)))
                .collect();
            let lead = 0.12;
            // Приглашение со строкой песни — печатается по словам.
            let prompt_words: Vec<(Word, f32)> = words.iter().map(|(text, at)| (fonts.word(text, Face::MonoBold), *at)).collect();
            self.log.push(Entry2 { at: line.start - lead, text: fonts.word("> ", Face::MonoBold), payload: None, prompt: Some((index, prompt_words)), strip: None });
            let push = |log: &mut Vec<Entry2>, fonts: &mut Fonts, at: f32, text: String, payload: Option<Word>| {
                log.push(Entry2 { at, text: fonts.word(&text, Face::Mono), payload, prompt: None, strip: None });
            };
            if !udp {
                for (number, flags) in ["[S]", "[S.]", "[.]"].iter().enumerate() {
                    let at = line.start - lead + 0.04 * (number + 1) as f32;
                    let (from, to) = if number == 1 { (&server, &client) } else { (&client, &server) };
                    push(&mut self.log, fonts, at, format!("{} {from} > {to} {flags}", stamp(at)), None);
                }
            }
            for (text, at) in &words {
                let length = text.chars().count();
                let header = if udp {
                    format!("{} {client} > 224.0.0.251 UDP len {length}", stamp(*at))
                } else {
                    format!("{} {client} > {server} [P.] len {length}", stamp(*at))
                };
                push(&mut self.log, fonts, *at - 0.02, header, None);
                // Нагрузка — строкой ниже, как у tcpdump -A: так слово не уходит за край.
                let payload = fonts.word(text, Face::MonoBold);
                push(&mut self.log, fonts, *at, "    └".to_string(), Some(payload));
                // Громко — между словами шум сети.
                if udp {
                    for number in 0..usize::from(energy > 0.7) {
                        let when = at + 0.06 * (number + 1) as f32;
                        push(&mut self.log, fonts, when, format!("{} {client} > {server} UDP len {}", stamp(when), 200 + rng.below(1200)), None);
                    }
                }
            }
            self.logged += 1;
        }
        self.log.sort_by(|a, b| a.at.total_cmp(&b.at));
        // Далеко позади лог не нужен.
        let t = ctx.t;
        if self.log.len() > 400 {
            let cut = self.log.iter().position(|entry| entry.at > t - 60.0).unwrap_or(0).min(self.log.len() - 200);
            self.log.drain(..cut);
        }
    }

    fn draw_packets(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, region: Rect, visible: f32) {
        let part = self.part;
        let t = ctx.t;
        let unit = region.width().min(region.height());
        let ink = ctx.palette.ink;
        let hair = canvas.hair();
        if self.labels.is_empty() {
            self.labels = LABELS.iter().map(|label| fonts.word(label, Face::Mono)).collect();
        }
        // Под узлами — карта сети: сетка плывёт, по ней бегут импульсы.
        if self.variant == Variant::Packets {
            super::fx::grid(canvas, ctx, part.seed, visible);
        }
        let current = ctx.current(&part);
        let indices: Vec<usize> = ctx.recent(&part, 0.8).collect();
        self.lines.keep(|line| indices.contains(&line) || current.is_some_and(|current| line + 1 >= current && line <= current + 1));
        // Камера чуть покачивается.
        let sway = Transform::from_rotate_at(0.6 * motion::wander(t, part.seed, 0.03), region.left() + region.width() / 2.0, region.top() + region.height() / 2.0);
        for index in indices {
            if self.lines.get(index).is_none() {
                let built = self.net_line(fonts, ctx, region, index);
                self.lines.get_or(index, || built);
            }
            let net = self.lines.get(index).expect("построено выше");
            let line = &ctx.lines[index];
            let pace = tempo(ctx.mood.mean(line.start, line.end));
            let leave_at = line.end - 0.08 * pace;
            let leave = progress(t, leave_at, 0.45 * pace);
            let alpha = visible * (1.0 - ease_out(leave));
            if alpha <= 0.003 {
                continue;
            }
            // Связи: TCP — сплошные, UDP — пунктиром.
            let mut edges = PathBuilder::new();
            for pair in net.nodes.windows(2) {
                let grow = ease_out(progress(t, pair[1].at - 0.5 * pace, 0.4 * pace));
                if grow > 0.0 {
                    edges.move_to(pair[0].x, pair[0].y);
                    edges.line_to(pair[0].x + (pair[1].x - pair[0].x) * grow, pair[0].y + (pair[1].y - pair[0].y) * grow);
                }
            }
            if let Some(path) = edges.finish() {
                let dash = net.udp.then_some((unit * 0.012, unit * 0.01));
                canvas.stroke(&path, faded(ink, 0.45 * alpha), hair, sway, dash);
            }
            // Пакеты.
            let mut solid = PathBuilder::new();
            let mut hollow = PathBuilder::new();
            let mut crosses = PathBuilder::new();
            let box_size = unit * 0.012;
            for packet in &net.packets {
                if t < packet.start || packet.from >= net.nodes.len() || packet.to >= net.nodes.len() {
                    continue;
                }
                let p = progress(t, packet.start, packet.arrive - packet.start);
                let (a, b) = (&net.nodes[packet.from], &net.nodes[packet.to]);
                let (x, y) = (a.x + (b.x - a.x) * p, a.y + (b.y - a.y) * p);
                if let Some(lost) = packet.lost
                    && p >= lost
                {
                    // Потерянная датаграмма: крестик гаснет на месте.
                    let fade = progress(t, packet.start + (packet.arrive - packet.start) * lost, 0.4);
                    if fade < 1.0 {
                        let (lx, ly) = (a.x + (b.x - a.x) * lost, a.y + (b.y - a.y) * lost);
                        let r = box_size * (1.0 + fade);
                        crosses.move_to(lx - r, ly - r);
                        crosses.line_to(lx + r, ly + r);
                        crosses.move_to(lx - r, ly + r);
                        crosses.line_to(lx + r, ly - r);
                    }
                    continue;
                }
                if p >= 1.0 {
                    continue;
                }
                let rect = Rect::from_xywh(x - box_size / 2.0, y - box_size / 2.0, box_size, box_size);
                match (packet.kind, rect) {
                    (PacketKind::Ack, Some(rect)) => hollow.push_rect(rect),
                    (_, Some(rect)) => solid.push_rect(rect),
                    _ => {}
                }
                if let Some(label) = packet.label.and_then(|label| self.labels.get(label)) {
                    let size = unit * 0.026;
                    let transform = Transform::from_scale(size, size).post_translate(x + box_size, y - box_size).post_concat(sway);
                    canvas.word(label, faded(ink, 0.7 * alpha), transform, None);
                }
            }
            if let Some(path) = solid.finish() {
                canvas.fill(&path, faded(ink, 0.9 * alpha), sway, None);
            }
            if let Some(path) = hollow.finish() {
                canvas.stroke(&path, faded(ink, 0.8 * alpha), hair, sway, None);
            }
            if let Some(path) = crosses.finish() {
                canvas.stroke(&path, faded(ink, 0.7 * alpha), hair, sway, None);
            }
            // Узлы: рамка, слово, адрес; пришёл пакет — рамка вспыхивает.
            for node in &net.nodes {
                let p = progress(t, node.at - 0.25 * pace, 0.3 * pace);
                if p <= 0.0 {
                    continue;
                }
                let flash = motion::kick(t - node.at, 7.0) * if t >= node.at { 1.0 } else { 0.0 };
                if let Some(frame) = Rect::from_xywh(node.x - node.half.0, node.y - node.half.1, node.half.0 * 2.0, node.half.1 * 2.0) {
                    let mut builder = PathBuilder::new();
                    builder.push_rect(frame);
                    if let Some(path) = builder.finish() {
                        if flash > 0.02 {
                            canvas.fill(&path, faded(ink, 0.25 * flash * alpha), sway, None);
                        }
                        canvas.stroke(&path, faded(ink, 0.7 * ease_out(p) * alpha), hair, sway, Some((unit * 0.02 * ease_out(p) + 0.01, unit * 0.02 * (1.0 - ease_out(p)) + 0.001)));
                    }
                }
                let size = unit * 0.024;
                let address = Transform::from_scale(size, size).post_translate(node.x - node.half.0, node.y - node.half.1 - size * 0.4).post_concat(sway);
                canvas.word(&node.address, faded(ink, 0.55 * ease_out(p) * alpha), address, None);
                let q = progress(t, node.at, net.entry.length(pace));
                if q > 0.0 {
                    let (ix, iy) = ink_center(&node.word);
                    let base = Transform::from_translate(-ix, -iy).post_scale(node.size, node.size).post_translate(node.x, node.y).post_concat(sway);
                    let mut look = motion::combine(net.entry.look(q), net.exit.look(leave));
                    look.alpha *= visible;
                    draw_word(canvas, &node.word, base, apply(sway, node.x, node.y), node.size, &look, ink, None);
                }
            }
        }
    }

    fn draw_terminal(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, region: Rect, visible: f32) {
        let part = self.part;
        let t = ctx.t;
        let ink = ctx.palette.ink;
        // Развёртка экрана ползёт сверху вниз (быстрее, когда громко).
        super::fx::scanline(canvas, ctx, region.top(), region.bottom(), visible);
        let current = ctx.current(&part).unwrap_or(part.first);
        self.grow_log(fonts, ctx, current + 1);
        let glyph = self.glyph.get_or_insert_with(|| fonts.word("M", Face::Mono));
        let char_width = glyph.advance.max(0.3);
        // Кегль: строк ~15 по высоте, но строка лога (~42 знака) — по ширине.
        let size = (region.height() / 15.0).min(region.width() / (char_width * 44.0)).clamp(7.0, 22.0);
        let prompt_size = size * 1.5;
        let line_height = size * 1.45;
        let left = region.left() + size * 0.8;
        let bottom = region.bottom() - line_height * 0.6;
        let shown: Vec<usize> = (0..self.log.len()).filter(|index| self.log[*index].at <= t).collect();
        let Some(&newest) = shown.last() else {
            return;
        };
        if self.variant == Variant::Both
            && let Some(separator) = paint::segment(region.left(), region.top(), region.right(), region.top())
        {
            canvas.stroke(&separator, faded(ink, 0.4 * visible), canvas.hair(), Transform::identity(), None);
        }
        // Новая строка въезжает снизу, остальные поднимаются.
        let scroll = line_height * (1.0 - ease_out(progress(t, self.log[newest].at, 0.12)));
        let latest_prompt = shown.iter().rev().copied().find(|index| self.log[*index].prompt.is_some());
        let color_key = |color: tiny_skia::Color| {
            let color = color.to_color_u8();
            [color.red(), color.green(), color.blue(), color.alpha()]
        };
        let (ink_key, ground_key) = (color_key(ink), color_key(ctx.palette.ground));
        let scale = canvas.root.sx;
        let mut baseline = bottom + scroll;
        for &index in shown.iter().rev() {
            if baseline < region.top() + line_height * 0.8 {
                break;
            }
            let entry = &mut self.log[index];
            let is_prompt = entry.prompt.is_some();
            let height = if is_prompt { prompt_size * 1.3 } else { line_height };
            let from_bottom = (bottom + scroll - baseline) / line_height;
            let alpha = visible * (1.0 - 0.5 * (from_bottom / 14.0).min(1.0));
            // Новая строка печатается: картинка открывается слева направо.
            let typed = if index == newest && !is_prompt { progress(t, entry.at, 0.12) } else { 1.0 };
            let sung = entry.prompt.as_ref().map_or(0, |(_, words)| words.iter().filter(|(_, at)| *at <= t).count());
            let key = StripKey { size, scale, ink: ink_key, ground: ground_key, sung };
            if entry.strip.as_ref().is_none_or(|(known, _)| *known != key) {
                entry.strip = render_strip(entry, size, prompt_size, scale, ink, ctx.palette.ground, t, region.width() - size * 1.6);
            }
            if let Some((_, strip)) = &entry.strip {
                let top = baseline - (if is_prompt { prompt_size } else { size }) * 1.05;
                canvas.blit(strip, (left * scale).round() as i32, (top * scale).round() as i32, typed, alpha);
            }
            // Курсор мигает по тому, что слышно сейчас, а не по опережающему кадру.
            let beat = ctx.mood.phase(ctx.heard());
            if Some(index) == latest_prompt && beat < 0.5 {
                let words = entry.prompt.as_ref().map_or(0.0, |(_, words)| {
                    words.iter().filter(|(_, at)| *at <= t).map(|(word, _)| word.advance + 0.6).sum::<f32>()
                });
                let x = left + (entry.text.advance + words) * prompt_size;
                if let Some(rect) = Rect::from_xywh(x.min(region.right() - prompt_size), baseline - prompt_size * 0.78, prompt_size * char_width, prompt_size * 0.95) {
                    let mut cursor = PathBuilder::new();
                    cursor.push_rect(rect);
                    if let Some(path) = cursor.finish() {
                        canvas.fill(&path, faded(ink, (0.95 - 0.8 * beat) * visible), Transform::identity(), None);
                    }
                }
            }
            baseline -= height;
        }
    }
}

/// Строка лога: текст, слово на плашке, у приглашения — спетые слова.
#[allow(clippy::too_many_arguments)]
fn render_strip(
    entry: &Entry2,
    size: f32,
    prompt_size: f32,
    scale: f32,
    ink: tiny_skia::Color,
    ground: tiny_skia::Color,
    t: f32,
    room: f32,
) -> Option<(StripKey, tiny_skia::Pixmap)> {
    let is_prompt = entry.prompt.is_some();
    let font = if is_prompt { prompt_size } else { size };
    let height = font * 1.4;
    // Картинка — по ширине содержимого: пустое справа копировать незачем.
    let content = entry.text.advance * font
        + entry.payload.as_ref().map_or(0.0, |payload| payload.advance * font + font * 1.2)
        + entry.prompt.as_ref().map_or(0.0, |(_, words)| words.iter().map(|(word, _)| (word.advance + 0.6) * font).sum::<f32>());
    let width = content.min(room).max(10.0);
    let mut pixmap = tiny_skia::Pixmap::new((width * scale).ceil() as u32, (height * scale).ceil() as u32)?;
    let baseline = font * 1.05;
    let mut canvas = Canvas {
        pixmap: pixmap.as_mut(),
        root: Transform::from_scale(scale, scale),
        spare: None,
        blank: true,
    };
    let sung = entry.prompt.as_ref().map_or(0, |(_, words)| words.iter().filter(|(_, at)| *at <= t).count());
    canvas.word(&entry.text, ink, Transform::from_scale(font, font).post_translate(0.0, baseline), None);
    let mut x = entry.text.advance * font;
    if let Some((_, words)) = &entry.prompt {
        // Строка песни: если не влезает — мельче.
        let total: f32 = words.iter().map(|(word, _)| word.advance + 0.6).sum::<f32>() * font;
        let squeeze = ((width - x) / total.max(1.0)).min(1.0);
        for (word, _) in words.iter().take(sung) {
            let size = font * squeeze;
            canvas.word(word, ink, Transform::from_scale(size, size).post_translate(x, baseline), None);
            x += (word.advance + 0.6) * size;
        }
    }
    if let Some(payload) = &entry.payload {
        // Нагрузка — инверсией: плашка цветом букв, слово цветом фона.
        let x = x + font * 0.6;
        let pad = font * 0.2;
        let fit = ((width - x - pad * 2.0) / (payload.advance * font).max(1.0)).min(1.0);
        let size = font * fit.max(0.4);
        if let Some(rect) = Rect::from_xywh(x - pad, baseline - size * 0.82, payload.advance * size + pad * 2.0, size * 1.1) {
            let mut plate = PathBuilder::new();
            plate.push_rect(rect);
            if let Some(path) = plate.finish() {
                canvas.fill(&path, ink, Transform::identity(), None);
            }
        }
        canvas.word(payload, ground, Transform::from_scale(size, size).post_translate(x, baseline), None);
    }
    let color = |color: tiny_skia::Color| {
        let color = color.to_color_u8();
        [color.red(), color.green(), color.blue(), color.alpha()]
    };
    let key = StripKey { size, scale, ink: color(ink), ground: color(ground), sung };
    Some((key, pixmap))
}

impl Scene for Net {
    fn draw(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, ctx: &Ctx, visible: f32) {
        if self.size != (ctx.width, ctx.height) {
            self.size = (ctx.width, ctx.height);
            self.lines.clear();
        }
        let (top, bottom) = self.region(ctx);
        match self.variant {
            Variant::Packets => self.draw_packets(canvas, fonts, ctx, top, visible),
            Variant::Terminal => self.draw_terminal(canvas, fonts, ctx, bottom, visible),
            Variant::Both => {
                self.draw_packets(canvas, fonts, ctx, top, visible);
                self.draw_terminal(canvas, fonts, ctx, bottom, visible);
            }
        }
    }
}
