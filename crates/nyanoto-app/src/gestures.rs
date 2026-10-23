//! Жесты тачпада и сенсорного экрана: вбок — назад/вперёд, по треку — другой трек,
//! вниз в полном плеере — свернуть, вверх по треку — развернуть. Жест идёт за пальцами.
//!
//! Тачпад приходит как точная прокрутка; колёсико мыши — не жест. Направления как у прокрутки.

/// Где начался жест.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// Страница (списки).
    Page,
    /// Трек в нижней панели (обложка и название).
    BarTrack,
    /// Остальная нижняя панель: там ползунки, жестов нет.
    Bar,
    /// Полный плеер.
    Full,
    /// Текст песни в полном плеере: вверх и вниз он прокручивается сам.
    FullLyrics,
}

/// Что сделать, когда жест довели.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Back,
    Forward,
    Next,
    Previous,
    Collapse,
    Expand,
}

/// Подсказка, пока пальцы на тачпаде.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Feedback {
    /// Что выйдет, если отпустить сейчас.
    pub action: Option<Action>,
    /// Сколько пройдено до порога: 0…1 и больше.
    pub progress: f32,
    /// На сколько точек увести полный плеер вниз.
    pub drag: f32,
    /// На сколько точек сдвинуть обложку полного плеера вбок.
    pub shift: f32,
}

/// Итог очередного шага жеста.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Step {
    /// Не отдавать прокрутку окну (жест забрал её себе).
    pub swallow: bool,
    pub feedback: Feedback,
    /// Сделать сейчас.
    pub action: Option<Action>,
}

/// Что разрешено там, где начался жест.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowed {
    pub back: bool,
    pub forward: bool,
    /// Есть трек: переключать, сворачивать и разворачивать.
    pub track: bool,
}

/// Пройти вбок, чтобы «Назад» или «Вперёд» сработали.
const NAVIGATE: f32 = 140.0;
/// Вбок по треку — переключить.
const SWITCH: f32 = 110.0;
/// Вниз в полном плеере — свернуть.
const COLLAPSE: f32 = 120.0;
/// Вверх по треку в панели — развернуть (сразу, без следования).
const EXPAND: f32 = 50.0;
/// Пока пальцы прошли меньше, направление не решено.
const DECIDE: f32 = 8.0;
/// Вбок — если по горизонтали заметно больше, чем по вертикали.
const SIDEWAYS: f32 = 1.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    Undecided,
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug)]
struct Swipe {
    region: Region,
    allowed: Allowed,
    dx: f32,
    dy: f32,
    axis: Axis,
    /// Действие уже сделано (разворачивание — сразу у порога).
    done: bool,
}

/// Одним пальцем жест «Назад» и «Вперёд» начинается только у края окна.
const TOUCH_EDGE: f32 = 28.0;

/// Пальцы на сенсорном экране.
#[derive(Default)]
pub struct Touches {
    /// Номер касания и где оно сейчас.
    fingers: Vec<(u64, f32, f32)>,
    /// Сколькими пальцами идёт жест: 0 — жеста нет.
    count: usize,
    /// Где был жест на прошлом шаге (палец или середина между двумя).
    last: (f32, f32),
    /// Жест забрал касание у окна.
    taken: bool,
}

impl Touches {
    fn center(&self) -> (f32, f32) {
        let n = self.fingers.len().max(1) as f32;
        let (sx, sy) = self.fingers.iter().fold((0.0, 0.0), |(sx, sy), (_, x, y)| (sx + x, sy + y));
        (sx / n, sy / n)
    }
}

#[derive(Default)]
pub struct Tracker {
    swipe: Option<Swipe>,
}

impl Tracker {
    /// Пальцы коснулись (или первая прокрутка без явного начала).
    pub fn begin(&mut self, region: Region, allowed: Allowed) {
        self.swipe = Some(Swipe { region, allowed, dx: 0.0, dy: 0.0, axis: Axis::Undecided, done: false });
    }

    pub fn active(&self) -> bool {
        self.swipe.is_some()
    }

    /// Пальцы сдвинулись на `dx`, `dy` точек.
    pub fn moved(&mut self, dx: f32, dy: f32) -> Step {
        let Some(swipe) = self.swipe.as_mut() else {
            return Step::default();
        };
        swipe.dx += dx;
        swipe.dy += dy;
        if swipe.axis == Axis::Undecided && swipe.dx.hypot(swipe.dy) >= DECIDE {
            swipe.axis = if swipe.dx.abs() > swipe.dy.abs() * SIDEWAYS { Axis::Horizontal } else { Axis::Vertical };
        }
        let feedback = feedback(swipe);
        let mut step = Step { swallow: swallows(swipe), feedback, action: None };
        // Разворачивание — сразу у порога: дальше его ведёт своя анимация.
        if feedback.action == Some(Action::Expand) && feedback.progress >= 1.0 && !swipe.done {
            swipe.done = true;
            step.action = Some(Action::Expand);
        }
        step
    }

    /// Пальцы отпустили (или прокрутка стихла): действие, если дошли до порога.
    pub fn end(&mut self) -> Step {
        let Some(swipe) = self.swipe.take() else {
            return Step::default();
        };
        let feedback = feedback(&swipe);
        let action = feedback.action.filter(|action| feedback.progress >= 1.0 && *action != Action::Expand && !swipe.done);
        Step { swallow: swallows(&swipe), feedback: Feedback::default(), action }
    }
}

/// Забирает ли жест прокрутку себе (иначе списки чуть ездили бы).
fn swallows(swipe: &Swipe) -> bool {
    match (swipe.axis, swipe.region) {
        (Axis::Undecided, _) => false,
        (Axis::Horizontal, Region::Page) => {
            if swipe.dx > 0.0 { swipe.allowed.back } else { swipe.allowed.forward }
        }
        (Axis::Horizontal, Region::Bar) => false,
        (Axis::Horizontal, _) => swipe.allowed.track,
        (Axis::Vertical, Region::Full | Region::BarTrack) => swipe.allowed.track,
        (Axis::Vertical, _) => false,
    }
}

fn feedback(swipe: &Swipe) -> Feedback {
    let none = Feedback::default();
    match (swipe.axis, swipe.region) {
        (Axis::Horizontal, Region::Page) => {
            let (action, allowed) = if swipe.dx > 0.0 {
                (Action::Back, swipe.allowed.back)
            } else {
                (Action::Forward, swipe.allowed.forward)
            };
            if !allowed {
                return none;
            }
            Feedback { action: Some(action), progress: swipe.dx.abs() / NAVIGATE, ..none }
        }
        (Axis::Horizontal, Region::BarTrack | Region::Full | Region::FullLyrics) if swipe.allowed.track => {
            // Содержимое влево — следующий трек (как карусель), вправо — предыдущий.
            let action = if swipe.dx < 0.0 { Action::Next } else { Action::Previous };
            let full = matches!(swipe.region, Region::Full | Region::FullLyrics);
            Feedback {
                action: Some(action),
                progress: swipe.dx.abs() / SWITCH,
                shift: if full { (swipe.dx * 0.35).clamp(-70.0, 70.0) } else { 0.0 },
                ..none
            }
        }
        (Axis::Vertical, Region::Full) if swipe.allowed.track && swipe.dy > 0.0 => {
            // Плеер едет за пальцами, дальше порога — с сопротивлением.
            let drag = if swipe.dy <= COLLAPSE { swipe.dy } else { COLLAPSE + (swipe.dy - COLLAPSE) * 0.4 };
            Feedback { action: Some(Action::Collapse), progress: swipe.dy / COLLAPSE, drag, ..none }
        }
        (Axis::Vertical, Region::BarTrack) if swipe.allowed.track && swipe.dy < 0.0 => {
            Feedback { action: Some(Action::Expand), progress: -swipe.dy / EXPAND, ..none }
        }
        _ => none,
    }
}

// ───────────────────────────── окно ─────────────────────────────

use std::time::Duration;

use slint::{TimerMode, ComponentHandle};

use crate::keys::{Pointer, TouchPhase};
use crate::{App, BAR_HEIGHT, FULL_LYRICS_BOTTOM, GESTURE_IDLE, PAGE_ARTISTS, with_app};

/// Где показать стрелку: у страницы, у панели или в полном плеере.
fn place_of(region: Region) -> i32 {
    match region {
        Region::Page => 0,
        Region::BarTrack | Region::Bar => 1,
        Region::Full | Region::FullLyrics => 2,
    }
}

impl App {
    /// Движение указателя и прокрутка из окна; `true` — прокрутку забрал жест.
    pub(crate) fn pointer_event(&self, pointer: Pointer) -> bool {
        match pointer {
            Pointer::Moved(x, y) => {
                self.pointer.set((x, y));
                false
            }
            Pointer::Scroll { dx, dy, edge, precise } => self.scroll_gesture(dx, dy, edge, precise),
            Pointer::Focused(focused) => {
                self.look_focus(focused);
                false
            }
            Pointer::Scale(system) => {
                self.system_scale_changed(system);
                false
            }
            Pointer::Touch { id, phase, x, y } => self.touch_gesture(id, phase, x, y),
            Pointer::Resized => {
                // Значок «развернуть/вернуть». Повтор через 150 мс: при стаскивании мышью система
                // сообщает о развороте позже нового размера.
                self.follow_maximized();
                slint::Timer::single_shot(std::time::Duration::from_millis(150), || {
                    crate::with_app(|app| app.follow_maximized());
                });
                false
            }
        }
    }

    fn scroll_gesture(&self, dx: f32, dy: f32, edge: Option<bool>, precise: bool) -> bool {
        if !self.gestures_on.get() {
            return false;
        }
        let active = self.gesture.borrow().active();
        // Колёсико мыши — не жест.
        if !precise && !active {
            return false;
        }
        if edge.is_some() {
            self.gesture_edges.set(true);
        }
        if edge == Some(true) || !active {
            let Some(region) = self.gesture_region() else {
                return false;
            };
            self.window.set_swipe_place(place_of(region));
            self.gesture.borrow_mut().begin(region, self.gesture_allowed());
        }
        let mut step = self.gesture.borrow_mut().moved(dx, dy);
        if edge == Some(false) {
            self.gesture_idle.stop();
            let end = self.gesture.borrow_mut().end();
            step = Step { swallow: step.swallow || end.swallow, feedback: end.feedback, action: step.action.or(end.action) };
            self.apply_gesture(step, false);
        } else {
            // Без явного конца (X11, Windows) — конец, когда прокрутка стихла.
            let idle = if self.gesture_edges.get() { Duration::from_secs(1) } else { GESTURE_IDLE };
            self.gesture_idle.start(TimerMode::SingleShot, idle, || with_app(|app| app.gesture_end()));
            self.apply_gesture(step, true);
        }
        step.swallow
    }

    /// Касание сенсорного экрана; `true` — жест забрал его у окна.
    fn touch_gesture(&self, id: u64, phase: TouchPhase, x: f32, y: f32) -> bool {
        if !self.gestures_on.get() {
            return false;
        }
        let mut touches = self.touches.borrow_mut();
        match phase {
            TouchPhase::Started => {
                touches.fingers.retain(|(known, ..)| *known != id);
                touches.fingers.push((id, x, y));
                let fingers = touches.fingers.len();
                if fingers > 2 || touches.taken {
                    return touches.taken;
                }
                // Одним пальцем — только там, где не мешает нажатиям; второй палец начинает заново.
                let center = touches.center();
                self.pointer.set(center);
                let region = self.gesture_region().filter(|region| fingers == 2 || self.one_finger_allowed(*region, x, y));
                match region {
                    Some(region) => {
                        self.window.set_swipe_place(place_of(region));
                        self.gesture.borrow_mut().begin(region, self.gesture_allowed());
                        touches.count = fingers;
                        touches.last = center;
                    }
                    None => {
                        if touches.count > 0 {
                            *self.gesture.borrow_mut() = Tracker::default();
                        }
                        touches.count = 0;
                    }
                }
                false
            }
            TouchPhase::Moved => {
                if let Some(finger) = touches.fingers.iter_mut().find(|(known, ..)| *known == id) {
                    (finger.1, finger.2) = (x, y);
                }
                if touches.count == 0 || touches.fingers.len() != touches.count {
                    return touches.taken;
                }
                let center = touches.center();
                let (dx, dy) = (center.0 - touches.last.0, center.1 - touches.last.1);
                touches.last = center;
                let step = self.gesture.borrow_mut().moved(dx, dy);
                if step.swallow && !touches.taken {
                    // Чёт тут костыль: отпускаем то, что окно успело принять за нажатие.
                    touches.taken = true;
                    self.window.window().dispatch_event(slint::platform::WindowEvent::PointerExited);
                }
                drop(touches);
                self.apply_gesture(step, true);
                self.touches.borrow().taken
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                touches.fingers.retain(|(known, ..)| *known != id);
                let taken = touches.taken;
                if touches.count > 0 && touches.fingers.len() < touches.count {
                    touches.count = 0;
                    drop(touches);
                    let step = self.gesture.borrow_mut().end();
                    let step = if phase == TouchPhase::Cancelled { Step { action: None, ..step } } else { step };
                    self.apply_gesture(step, false);
                    touches = self.touches.borrow_mut();
                }
                if touches.fingers.is_empty() {
                    touches.taken = false;
                }
                taken
            }
        }
    }

    /// Где можно начать жест одним пальцем.
    fn one_finger_allowed(&self, region: Region, x: f32, y: f32) -> bool {
        let scale = self.window.window().scale_factor();
        let size = self.window.window().size();
        let (width, height) = (size.width as f32 / scale, size.height as f32 / scale);
        match region {
            Region::Page => x < TOUCH_EDGE || x > width - TOUCH_EDGE,
            Region::BarTrack => true,
            Region::Full => y < height - FULL_LYRICS_BOTTOM,
            Region::Bar | Region::FullLyrics => false,
        }
    }

    fn gesture_end(&self) {
        let step = self.gesture.borrow_mut().end();
        self.apply_gesture(step, false);
    }

    /// Жесты выключили посреди жеста: всё на место.
    pub(crate) fn gesture_cancel(&self) {
        *self.gesture.borrow_mut() = Tracker::default();
        self.gesture_idle.stop();
        self.apply_gesture(Step::default(), false);
    }

    /// Где указатель; `None` — открыто окно поверх.
    fn gesture_region(&self) -> Option<Region> {
        let window = &self.window;
        let modal = window.get_info_open()
            || window.get_picker_open()
            || window.get_editor_open()
            || window.get_warning_open()
            || window.get_confirm_open()
            || window.get_quality_open()
            || window.get_smart_open()
            || window.get_eq_open()
            || window.get_convert_open()
            || window.get_chapters_open()
            || window.get_sleep_menu_open();
        if modal {
            return None;
        }
        let (x, y) = self.pointer.get();
        let scale = window.window().scale_factor();
        let size = window.window().size();
        let (width, height) = (size.width as f32 / scale, size.height as f32 / scale);
        Some(if window.get_full_open() {
            if window.get_full_lyrics() && x > width / 2.0 && y < height - FULL_LYRICS_BOTTOM {
                Region::FullLyrics
            } else {
                Region::Full
            }
        } else if y > height - BAR_HEIGHT {
            if x < window.get_bar_track_right() { Region::BarTrack } else { Region::Bar }
        } else {
            Region::Page
        })
    }

    fn gesture_allowed(&self) -> Allowed {
        Allowed {
            back: self.window.get_page() != PAGE_ARTISTS,
            forward: !self.forward_places.borrow().is_empty(),
            track: self.window.get_has_track(),
        }
    }

    /// Подсказка и действие жеста.
    fn apply_gesture(&self, step: Step, swiping: bool) {
        let window = &self.window;
        let feedback = step.feedback;
        let kind = match feedback.action {
            Some(Action::Back) => 1,
            Some(Action::Forward) => 2,
            Some(Action::Next) => 3,
            Some(Action::Previous) => 4,
            _ => 0,
        };
        // Вид стрелки остаётся прежним, пока она уезжает.
        if kind != 0 {
            window.set_swipe_kind(kind);
        }
        window.set_swipe_progress(if kind != 0 && swiping { feedback.progress } else { 0.0 });
        window.set_swiping(swiping);
        window.set_full_dragging(swiping && feedback.drag > 0.0);
        window.set_full_drag(feedback.drag);
        window.set_full_shift(feedback.shift);
        let Some(action) = step.action else {
            return;
        };
        match action {
            Action::Back => self.back(),
            Action::Forward => self.forward(),
            Action::Next => self.go_next(),
            Action::Previous => self.go_previous(),
            Action::Collapse => self.close_full(),
            Action::Expand => {
                if !window.get_full_open() {
                    self.toggle_full();
                }
            }
        }
    }
}

impl App {
    /// Значок «развернуть / вернуть» — по тому, развёрнуто ли окно сейчас.
    pub(crate) fn follow_maximized(&self) {
        let maximized = self.window.window().is_maximized();
        if self.window.get_window_maximized() != maximized {
            self.window.set_window_maximized(maximized);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Allowed = Allowed { back: true, forward: true, track: true };

    fn run(tracker: &mut Tracker, steps: &[(f32, f32)]) -> Step {
        let mut last = Step::default();
        for (dx, dy) in steps {
            last = tracker.moved(*dx, *dy);
        }
        last
    }

    #[test]
    fn sideways_on_a_page_goes_back_and_forward() {
        let mut tracker = Tracker::default();
        tracker.begin(Region::Page, ALL);
        let step = run(&mut tracker, &[(4.0, 1.0); 20]);
        assert!(step.swallow, "вбок списки не дёргаются");
        assert_eq!(step.feedback.action, Some(Action::Back));
        assert!((step.feedback.progress - 80.0 / NAVIGATE).abs() < 1e-3);
        // Не довели — ничего.
        assert_eq!(tracker.end().action, None);

        tracker.begin(Region::Page, ALL);
        run(&mut tracker, &[(10.0, -2.0); 16]);
        assert_eq!(tracker.end().action, Some(Action::Back));

        tracker.begin(Region::Page, ALL);
        run(&mut tracker, &[(-10.0, 0.0); 16]);
        assert_eq!(tracker.end().action, Some(Action::Forward));

        // Вперёд некуда — жест не забирает прокрутку и ничего не делает.
        tracker.begin(Region::Page, Allowed { forward: false, ..ALL });
        let step = run(&mut tracker, &[(-10.0, 0.0); 16]);
        assert!(!step.swallow);
        assert_eq!(tracker.end().action, None);
    }

    #[test]
    fn vertical_scrolling_stays_with_the_lists() {
        let mut tracker = Tracker::default();
        tracker.begin(Region::Page, ALL);
        let step = run(&mut tracker, &[(1.0, 12.0); 20]);
        assert!(!step.swallow);
        assert_eq!(step.feedback.action, None);
        assert_eq!(tracker.end().action, None);
        // Начало жеста — ещё не ясно, куда: окну отдаётся всё.
        tracker.begin(Region::Page, ALL);
        assert!(!tracker.moved(3.0, 0.0).swallow);
    }

    #[test]
    fn full_player_follows_the_fingers_down() {
        let mut tracker = Tracker::default();
        tracker.begin(Region::Full, ALL);
        let step = run(&mut tracker, &[(0.0, 10.0); 6]);
        assert!(step.swallow);
        assert_eq!(step.feedback.drag, 60.0);
        // Отпустили раньше порога — плеер возвращается.
        let end = tracker.end();
        assert_eq!(end.action, None);
        assert_eq!(end.feedback.drag, 0.0);

        tracker.begin(Region::Full, ALL);
        let step = run(&mut tracker, &[(0.0, 10.0); 20]);
        assert!(step.feedback.drag > COLLAPSE && step.feedback.drag < 200.0, "дальше порога — туже");
        assert_eq!(tracker.end().action, Some(Action::Collapse));

        // Над текстом песни вверх-вниз — прокрутка текста.
        tracker.begin(Region::FullLyrics, ALL);
        let step = run(&mut tracker, &[(0.0, 10.0); 20]);
        assert!(!step.swallow);
        assert_eq!(tracker.end().action, None);
    }

    #[test]
    fn sideways_on_the_track_switches_tracks() {
        let mut tracker = Tracker::default();
        tracker.begin(Region::Full, ALL);
        let step = run(&mut tracker, &[(-12.0, 1.0); 12]);
        assert_eq!(step.feedback.action, Some(Action::Next));
        assert!(step.feedback.shift < 0.0 && step.feedback.shift >= -70.0);
        assert_eq!(tracker.end().action, Some(Action::Next));

        tracker.begin(Region::BarTrack, ALL);
        run(&mut tracker, &[(12.0, 0.0); 12]);
        assert_eq!(tracker.end().action, Some(Action::Previous));

        // Без трека — ничего.
        tracker.begin(Region::BarTrack, Allowed { track: false, ..ALL });
        let step = run(&mut tracker, &[(12.0, 0.0); 12]);
        assert!(!step.swallow);
        assert_eq!(tracker.end().action, None);
    }

    #[test]
    fn up_on_the_bar_expands_once() {
        let mut tracker = Tracker::default();
        tracker.begin(Region::BarTrack, ALL);
        let actions: Vec<Option<Action>> = (0..20).map(|_| tracker.moved(0.0, -6.0).action).collect();
        assert_eq!(actions.iter().filter(|action| **action == Some(Action::Expand)).count(), 1);
        assert_eq!(tracker.end().action, None, "второй раз не разворачивает");
        // Остальная панель (громкость, перемотка) — без жестов.
        tracker.begin(Region::Bar, ALL);
        let step = run(&mut tracker, &[(0.0, -6.0); 20]);
        assert!(!step.swallow);
        assert_eq!(tracker.end().action, None);
    }
}


