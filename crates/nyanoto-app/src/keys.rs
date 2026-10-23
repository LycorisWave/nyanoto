//! Сочетания в любой раскладке и медиаклавиши.
//! В русской раскладке Ctrl+C даёт «с» — подставляем латинскую букву физической клавиши, как браузеры.
//! Медиаклавиши Slint не передаёт — ловим тут. Здесь же прокрутка для жестов: обработчик у окна один.
//! Всё это — события winit, на телефоне их нет (там свои касания в разметке).

#![cfg_attr(target_os = "android", allow(dead_code))]

#[cfg(not(target_os = "android"))]
pub use desktop::latin_shortcuts;

/// На телефоне окно не winit: ни сочетаний, ни прокрутки для жестов.
#[cfg(target_os = "android")]
pub fn latin_shortcuts(_window: &slint::Window, _media: impl Fn(MediaKey) + 'static, _pointer: impl Fn(Pointer) -> bool + 'static) {}

/// Мультимедийная клавиша клавиатуры.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

/// Указатель для жестов: где он (точки окна) и прокрутка.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Moved(f32, f32),
    /// Окно получило или потеряло фокус.
    Focused(bool),
    /// Система сменила масштаб окна.
    Scale(f32),
    /// Окно сменило размер (развернули, вернули, потянули край).
    Resized,
    /// Палец на сенсорном экране: номер касания, что с ним, где (в точках).
    Touch { id: u64, phase: TouchPhase, x: f32, y: f32 },
    Scroll {
        dx: f32,
        dy: f32,
        /// Начало/конец жеста; `None` — X11 и Windows, конец по паузе.
        edge: Option<bool>,
        /// Точный сдвиг (тачпад), а не щелчки колёсика мыши.
        precise: bool,
    },
}

/// Что с касанием (как у winit, но без него).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

#[cfg(not(target_os = "android"))]
mod desktop {
    use std::cell::Cell;

    use slint::platform::WindowEvent as SlintEvent;
    use slint::winit_030::winit::event::{ElementState, MouseScrollDelta, TouchPhase as Phase, WindowEvent};
    use slint::winit_030::winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
    use slint::winit_030::{EventResult, WinitWindowAccessor};

    use super::{MediaKey, Pointer, TouchPhase};

    /// Сколько точек в одной «строке» прокрутки, когда система даёт строки.
    const LINE: f32 = 40.0;

    /// Прокрутка в точках. Целые строки — колёсико, дробные — тачпад.
    pub fn scroll_of(delta: MouseScrollDelta, scale: f32) -> (f32, f32, bool) {
        match delta {
            MouseScrollDelta::PixelDelta(position) => (position.x as f32 / scale, position.y as f32 / scale, true),
            MouseScrollDelta::LineDelta(x, y) => (x * LINE, y * LINE, x.fract() != 0.0 || y.fract() != 0.0),
        }
    }

    pub fn latin_shortcuts(window: &slint::Window, media: impl Fn(MediaKey) + 'static, pointer: impl Fn(Pointer) -> bool + 'static) {
        let held = Cell::new(ModifiersState::empty());
        let window_event = move |window: &slint::Window, event: &WindowEvent| match event {
            WindowEvent::Focused(focused) => {
                pointer(Pointer::Focused(*focused));
                EventResult::Propagate
            }
            WindowEvent::Resized(_) => {
                pointer(Pointer::Resized);
                EventResult::Propagate
            }
            WindowEvent::Touch(touch) => {
                let scale = window.scale_factor();
                let (x, y) = (touch.location.x as f32 / scale, touch.location.y as f32 / scale);
                // Жест подхватил касание — окну оно больше не достаётся.
                let phase = match touch.phase {
                    Phase::Started => TouchPhase::Started,
                    Phase::Moved => TouchPhase::Moved,
                    Phase::Ended => TouchPhase::Ended,
                    Phase::Cancelled => TouchPhase::Cancelled,
                };
                if pointer(Pointer::Touch { id: touch.id, phase, x, y }) {
                    EventResult::PreventDefault
                } else {
                    EventResult::Propagate
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                pointer(Pointer::Scale(*scale_factor as f32));
                EventResult::Propagate
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = window.scale_factor();
                pointer(Pointer::Moved(position.x as f32 / scale, position.y as f32 / scale));
                EventResult::Propagate
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let (dx, dy, precise) = scroll_of(*delta, window.scale_factor());
                let edge = match phase {
                    Phase::Started => Some(true),
                    Phase::Ended | Phase::Cancelled => Some(false),
                    Phase::Moved => None,
                };
                if pointer(Pointer::Scroll { dx, dy, edge, precise }) {
                    EventResult::PreventDefault
                } else {
                    EventResult::Propagate
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                held.set(modifiers.state());
                EventResult::Propagate
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                if let Key::Named(named) = &event.logical_key {
                    let key = match named {
                        NamedKey::MediaPlayPause => Some(MediaKey::PlayPause),
                        NamedKey::MediaPlay => Some(MediaKey::Play),
                        NamedKey::MediaPause => Some(MediaKey::Pause),
                        NamedKey::MediaStop => Some(MediaKey::Stop),
                        NamedKey::MediaTrackNext => Some(MediaKey::Next),
                        NamedKey::MediaTrackPrevious => Some(MediaKey::Previous),
                        _ => None,
                    };
                    if let Some(key) = key {
                        if event.state == ElementState::Pressed && !event.repeat {
                            media(key);
                        }
                        return EventResult::PreventDefault;
                    }
                }
                let modifiers = held.get();
                let Key::Character(text) = &event.logical_key else {
                    return EventResult::Propagate;
                };
                let Some(letter) = shortcut_letter(event.physical_key) else {
                    return EventResult::Propagate;
                };
                // AltGr на Windows приходит как Ctrl+Alt: это ввод символа, не сочетание.
                if !modifiers.control_key() || modifiers.alt_key() || text.chars().any(|char| char.is_ascii()) {
                    return EventResult::Propagate;
                }
                let text = if modifiers.shift_key() { letter.to_ascii_uppercase() } else { letter };
                let text = text.to_string().into();
                window.dispatch_event(match event.state {
                    ElementState::Pressed => SlintEvent::KeyPressed { text },
                    ElementState::Released => SlintEvent::KeyReleased { text },
                });
                EventResult::PreventDefault
            }
            _ => EventResult::Propagate,
        };
        window.on_winit_window_event(move |window, event| {
            // Проверка плавности (только отладочная сборка): время каждого события окна.
            #[cfg(debug_assertions)]
            frame_mark(event);
            window_event(window, event)
        });
    }

    /// Стандартные сочетания Slint и Ctrl+F.
    fn shortcut_letter(key: PhysicalKey) -> Option<char> {
        let PhysicalKey::Code(code) = key else {
            return None;
        };
        Some(match code {
            KeyCode::KeyA => 'a',
            KeyCode::KeyC => 'c',
            KeyCode::KeyV => 'v',
            KeyCode::KeyX => 'x',
            KeyCode::KeyZ => 'z',
            KeyCode::KeyY => 'y',
            KeyCode::KeyF => 'f',
            _ => return None,
        })
    }

    /// `NYANOTO_FRAMES=<файл>`: событие окна на строку (мс Unix, «R» — перерисовка).
    #[cfg(debug_assertions)]
    fn frame_mark(event: &WindowEvent) {
        use std::io::Write;
        use std::sync::{Mutex, OnceLock};
        static LOG: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        let log = LOG.get_or_init(|| {
            let path = std::env::var_os("NYANOTO_FRAMES")?;
            std::fs::File::create(path).ok().map(Mutex::new)
        });
        if let Some(log) = log
            && let Ok(mut file) = log.lock()
        {
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64() * 1000.0;
            let kind = match event {
                WindowEvent::RedrawRequested => "R".to_string(),
                other => format!("{other:?}").chars().take_while(|c| c.is_alphanumeric()).collect(),
            };
            let _ = writeln!(file, "{ms:.1} {kind}");
        }
    }
}
