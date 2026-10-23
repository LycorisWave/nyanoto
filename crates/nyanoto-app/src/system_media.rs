//! Медиаклавиши и виджет плеера системы.
//! - Linux/BSD — MPRIS по D-Bus сеанса (сеть не нужна).
//! - Windows — SystemMediaTransportControls.
//!
//! Команды уходят в `act`, он сам перекидывает их в поток окна.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nyanoto_core::Repeat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playback {
    Playing,
    Paused,
    Stopped,
}

/// Команда от системы. У Windows нет «пауза/играть» одной кнопкой, громкости и «выйти».
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(any(windows, target_os = "android"), allow(dead_code))]
pub enum Action {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// Перемотать на столько секунд вперёд (назад — отрицательное).
    SeekBy(f64),
    /// Перейти к секунде.
    SeekTo(f64),
    /// Громкость 0–1, как у ползунка окна.
    Volume(f64),
    Repeat(Repeat),
    /// Показать окно.
    Raise,
    Quit,
}

pub type Act = Arc<dyn Fn(Action) + Send + Sync>;

/// Что показать системе.
#[derive(Clone, Debug, PartialEq)]
pub struct Now {
    pub playback: Playback,
    /// Номер элемента очереди: по нему система отличает треки.
    pub track: Option<u64>,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Длина и позиция в секундах.
    pub length: f64,
    pub position: f64,
    /// Файл обложки.
    pub art: Option<PathBuf>,
    pub rate: f64,
    pub volume: f64,
    pub repeat: Repeat,
    pub can_next: bool,
    pub can_previous: bool,
}

impl Default for Now {
    fn default() -> Self {
        Self {
            playback: Playback::Stopped,
            track: None,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            length: 0.0,
            position: 0.0,
            art: None,
            rate: 1.0,
            volume: 1.0,
            repeat: Repeat::Off,
            can_next: false,
            can_previous: false,
        }
    }
}

pub struct SystemMedia {
    controls: imp::Controls,
}

impl SystemMedia {
    /// Окно уже должно быть показано (в Windows панель цепляется к нему).
    pub fn start(window: &slint::Window, act: Act) -> Result<Self, String> {
        Ok(Self { controls: imp::Controls::start(window, act)? })
    }

    /// Новое состояние; система узнаёт только то, что поменялось.
    pub fn update(&self, now: Now) {
        self.controls.update(now);
    }

    /// Позиция прыгнула (перемотка), а не дошла сама.
    pub fn seeked(&self, position: f64) {
        self.controls.seeked(position);
    }

    /// Соединение с системой живо: медиаклавиши придут оттуда.
    pub fn active(&self) -> bool {
        self.controls.active()
    }
}

#[cfg_attr(target_os = "android", allow(dead_code))]
/// `file://` адрес файла: всё, кроме латиницы, цифр и `-._~/`, — в %XX.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if text.starts_with('/') { "file://" } else { "file:///" });
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
mod imp {
    //! MPRIS: https://specifications.freedesktop.org/mpris-spec/latest/

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::Instant;

    use nyanoto_core::Repeat;
    use zbus::fdo::RequestNameFlags;
    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    use super::{Act, Action, Now, Playback, file_url};

    const PATH: &str = "/org/mpris/MediaPlayer2";
    /// Сколько ждать следующих изменений, чтобы отправить их одним сигналом.
    const COALESCE: std::time::Duration = std::time::Duration::from_millis(60);
    const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
    const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

    /// Состояние и когда пришла позиция: пока играет, она идёт сама.
    struct State {
        now: Now,
        at: Instant,
    }

    impl State {
        fn position_us(&self) -> i64 {
            let mut seconds = self.now.position;
            if self.now.playback == Playback::Playing {
                seconds += self.at.elapsed().as_secs_f64() * self.now.rate;
            }
            if self.now.length > 0.0 {
                seconds = seconds.min(self.now.length);
            }
            (seconds.max(0.0) * 1e6) as i64
        }
    }

    type Shared = Arc<Mutex<State>>;

    fn lock(shared: &Shared) -> MutexGuard<'_, State> {
        shared.lock().unwrap_or_else(|error| error.into_inner())
    }

    enum Message {
        Changed(Vec<&'static str>),
        Seeked(i64),
    }

    pub struct Controls {
        shared: Shared,
        messages: Sender<Message>,
        alive: Arc<AtomicBool>,
    }

    impl Controls {
        pub fn start(_window: &slint::Window, act: Act) -> Result<Self, String> {
            let shared: Shared = Arc::new(Mutex::new(State { now: Now::default(), at: Instant::now() }));
            let (messages, inbox) = mpsc::channel::<Message>();
            let alive = Arc::new(AtomicBool::new(false));
            let (thread_shared, thread_alive) = (shared.clone(), alive.clone());
            // Подключение к шине занимает миллисекунды — не в потоке окна.
            std::thread::Builder::new()
                .name("nyanoto-mpris".into())
                .spawn(move || {
                    let connection = match connect(act, thread_shared.clone()) {
                        Ok(connection) => connection,
                        Err(error) => {
                            crate::trace(&format!("MPRIS: {error}"));
                            return;
                        }
                    };
                    thread_alive.store(true, Ordering::Relaxed);
                    // Окно закрыло канал — соединение закрывается, имя на шине освобождается.
                    while let Ok(first) = inbox.recv() {
                        // Шлём одним сигналом: GNOME на каждый сигнал перечитывает обложку.
                        let mut names: Vec<&'static str> = Vec::new();
                        let mut seeks = Vec::new();
                        let mut take = |message| match message {
                            Message::Changed(changed) => {
                                for name in changed {
                                    if !names.contains(&name) {
                                        names.push(name);
                                    }
                                }
                            }
                            Message::Seeked(position) => seeks.push(position),
                        };
                        take(first);
                        while let Ok(more) = inbox.recv_timeout(COALESCE) {
                            take(more);
                        }
                        let mut sent = Ok(());
                        if !names.is_empty() {
                            let changed: HashMap<&str, Value> = {
                                let state = lock(&thread_shared);
                                names.iter().map(|name| (*name, property(&state, name))).collect()
                            };
                            sent = connection.emit_signal(
                                None::<()>,
                                PATH,
                                "org.freedesktop.DBus.Properties",
                                "PropertiesChanged",
                                &(PLAYER, changed, Vec::<&str>::new()),
                            );
                        }
                        if let Some(position) = seeks.last() {
                            sent = sent.and(connection.emit_signal(None::<()>, PATH, PLAYER, "Seeked", &(*position,)));
                        }
                        if let Err(error) = sent {
                            crate::trace(&format!("MPRIS: {error}"));
                        }
                    }
                    thread_alive.store(false, Ordering::Relaxed);
                })
                .map_err(|error| error.to_string())?;
            Ok(Self { shared, messages, alive })
        }

        pub fn update(&self, now: Now) {
            let names = {
                let mut state = lock(&self.shared);
                let old = &state.now;
                let mut names = Vec::new();
                if old.playback != now.playback {
                    names.extend(["PlaybackStatus", "CanPlay", "CanPause", "CanSeek"]);
                }
                if (old.track, &old.title, &old.artist, &old.album, old.length, &old.art)
                    != (now.track, &now.title, &now.artist, &now.album, now.length, &now.art)
                {
                    names.push("Metadata");
                }
                if old.rate != now.rate {
                    names.extend(["Rate", "MinimumRate", "MaximumRate"]);
                }
                if old.volume != now.volume {
                    names.push("Volume");
                }
                if old.repeat != now.repeat {
                    names.push("LoopStatus");
                }
                if old.can_next != now.can_next {
                    names.push("CanGoNext");
                }
                if old.can_previous != now.can_previous {
                    names.push("CanGoPrevious");
                }
                state.now = now;
                state.at = Instant::now();
                names
            };
            if !names.is_empty() {
                let _ = self.messages.send(Message::Changed(names));
            }
        }

        pub fn seeked(&self, position: f64) {
            {
                let mut state = lock(&self.shared);
                state.now.position = position;
                state.at = Instant::now();
            }
            let _ = self.messages.send(Message::Seeked((position.max(0.0) * 1e6) as i64));
        }

        pub fn active(&self) -> bool {
            self.alive.load(Ordering::Relaxed)
        }
    }

    fn connect(act: Act, shared: Shared) -> zbus::Result<zbus::blocking::Connection> {
        let connection = zbus::blocking::connection::Builder::session()?
            .serve_at(PATH, Root { act: act.clone() })?
            .serve_at(PATH, Player { act, shared })?
            .build()?;
        // Второй плеер берёт имя с номером процесса. zbus отвечает NameTaken, а не Exists.
        let name = "org.mpris.MediaPlayer2.nyanoto";
        let primary = match connection.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into()) {
            Ok(reply) => reply == zbus::fdo::RequestNameReply::PrimaryOwner,
            Err(zbus::Error::NameTaken) => false,
            Err(error) => return Err(error),
        };
        if !primary {
            connection.request_name(format!("{name}.instance{}", std::process::id()))?;
        }
        Ok(connection)
    }

    fn track_path(now: &Now) -> ObjectPath<'static> {
        match now.track {
            Some(id) => ObjectPath::try_from(format!("/org/nyanoto/track/{id}")).unwrap_or_else(|_| ObjectPath::from_static_str_unchecked(NO_TRACK)),
            None => ObjectPath::from_static_str_unchecked(NO_TRACK),
        }
    }

    fn metadata(now: &Now) -> HashMap<String, OwnedValue> {
        let mut map: HashMap<String, OwnedValue> = HashMap::new();
        let mut put = |key: &str, value: Value<'_>| {
            if let Ok(value) = OwnedValue::try_from(value) {
                map.insert(key.to_string(), value);
            }
        };
        put("mpris:trackid", Value::from(track_path(now)));
        if now.track.is_none() {
            return map;
        }
        if now.length > 0.0 {
            put("mpris:length", Value::from((now.length * 1e6) as i64));
        }
        put("xesam:title", Value::from(now.title.clone()));
        if !now.artist.is_empty() {
            put("xesam:artist", Value::from(vec![now.artist.clone()]));
        }
        if !now.album.is_empty() {
            put("xesam:album", Value::from(now.album.clone()));
        }
        if let Some(art) = &now.art {
            put("mpris:artUrl", Value::from(file_url(art)));
        }
        map
    }

    fn playback_status(now: &Now) -> &'static str {
        match now.playback {
            Playback::Playing => "Playing",
            Playback::Paused => "Paused",
            Playback::Stopped => "Stopped",
        }
    }

    fn loop_status(now: &Now) -> &'static str {
        match now.repeat {
            Repeat::Off => "None",
            Repeat::One => "Track",
            Repeat::All => "Playlist",
        }
    }

    /// Значение свойства для сигнала об изменении.
    fn property(state: &State, name: &str) -> Value<'static> {
        let now = &state.now;
        let has_track = now.track.is_some();
        match name {
            "PlaybackStatus" => Value::from(playback_status(now)),
            "LoopStatus" => Value::from(loop_status(now)),
            "Rate" => Value::from(now.rate),
            "MinimumRate" => Value::from(now.rate.min(1.0)),
            "MaximumRate" => Value::from(now.rate.max(1.0)),
            "Volume" => Value::from(now.volume),
            "CanGoNext" => Value::from(now.can_next),
            "CanGoPrevious" => Value::from(now.can_previous),
            "CanPlay" | "CanPause" | "CanSeek" => Value::from(has_track),
            _ => {
                let mut dict = zbus::zvariant::Dict::new(&zbus::zvariant::Signature::Str, &zbus::zvariant::Signature::Variant);
                for (key, value) in metadata(now) {
                    let _ = dict.append(Value::from(key), Value::Value(Box::new(Value::from(value))));
                }
                Value::Dict(dict)
            }
        }
    }

    struct Root {
        act: Act,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl Root {
        fn raise(&self) {
            (self.act)(Action::Raise);
        }

        fn quit(&self) {
            (self.act)(Action::Quit);
        }

        #[zbus(property)]
        fn can_quit(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn can_raise(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn has_track_list(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn identity(&self) -> String {
            "Nyanoto".into()
        }

        #[zbus(property)]
        fn desktop_entry(&self) -> String {
            "nyanoto".into()
        }

        #[zbus(property)]
        fn supported_uri_schemes(&self) -> Vec<String> {
            Vec::new()
        }

        #[zbus(property)]
        fn supported_mime_types(&self) -> Vec<String> {
            Vec::new()
        }
    }

    struct Player {
        act: Act,
        shared: Shared,
    }

    impl Player {
        fn state(&self) -> MutexGuard<'_, State> {
            lock(&self.shared)
        }
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl Player {
        fn next(&self) {
            (self.act)(Action::Next);
        }

        fn previous(&self) {
            (self.act)(Action::Previous);
        }

        fn pause(&self) {
            (self.act)(Action::Pause);
        }

        fn play_pause(&self) {
            (self.act)(Action::PlayPause);
        }

        fn stop(&self) {
            (self.act)(Action::Stop);
        }

        fn play(&self) {
            (self.act)(Action::Play);
        }

        /// Сдвиг в микросекундах.
        fn seek(&self, offset: i64) {
            (self.act)(Action::SeekBy(offset as f64 / 1e6));
        }

        /// Только для текущего трека: запрос к уже сменившемуся игнорируется.
        fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
            let state = self.state();
            if track_path(&state.now) == track_id && position >= 0 && (position as f64) / 1e6 <= state.now.length {
                drop(state);
                (self.act)(Action::SeekTo(position as f64 / 1e6));
            }
        }

        fn open_uri(&self, _uri: String) -> zbus::fdo::Result<()> {
            Err(zbus::fdo::Error::NotSupported("Nyanoto plays its own library".into()))
        }

        #[zbus(property)]
        fn playback_status(&self) -> String {
            playback_status(&self.state().now).into()
        }

        #[zbus(property)]
        fn loop_status(&self) -> String {
            loop_status(&self.state().now).into()
        }

        #[zbus(property)]
        fn set_loop_status(&self, value: String) {
            let repeat = match value.as_str() {
                "Track" => Repeat::One,
                "Playlist" => Repeat::All,
                _ => Repeat::Off,
            };
            (self.act)(Action::Repeat(repeat));
        }

        #[zbus(property)]
        fn rate(&self) -> f64 {
            self.state().now.rate
        }

        /// Скорость меняется только в окне (модуль «Скорость»).
        #[zbus(property)]
        fn set_rate(&self, _value: f64) {}

        #[zbus(property)]
        fn minimum_rate(&self) -> f64 {
            self.state().now.rate.min(1.0)
        }

        #[zbus(property)]
        fn maximum_rate(&self) -> f64 {
            self.state().now.rate.max(1.0)
        }

        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            metadata(&self.state().now)
        }

        #[zbus(property)]
        fn volume(&self) -> f64 {
            self.state().now.volume
        }

        #[zbus(property)]
        fn set_volume(&self, value: f64) {
            (self.act)(Action::Volume(value.clamp(0.0, 1.0)));
        }

        #[zbus(property(emits_changed_signal = "false"))]
        fn position(&self) -> i64 {
            self.state().position_us()
        }

        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            self.state().now.can_next
        }

        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            self.state().now.can_previous
        }

        #[zbus(property)]
        fn can_play(&self) -> bool {
            self.state().now.track.is_some()
        }

        #[zbus(property)]
        fn can_pause(&self) -> bool {
            self.state().now.track.is_some()
        }

        #[zbus(property)]
        fn can_seek(&self) -> bool {
            self.state().now.track.is_some()
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn can_control(&self) -> bool {
            true
        }
    }
}

#[cfg(windows)]
mod imp {
    //! SMTC для обычного окна — через ISystemMediaTransportControlsInterop::GetForWindow.

    use std::cell::RefCell;

    use nyanoto_core::Repeat;
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Foundation::{TimeSpan, TypedEventHandler, Uri};
    use windows::Media::{
        AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode, MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        SystemMediaTransportControls, SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    };
    use windows::Storage::Streams::RandomAccessStreamReference;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
    use windows::core::{HSTRING, Ref};

    use super::{Act, Action, Now, Playback, file_url};

    pub struct Controls {
        smtc: SystemMediaTransportControls,
        last: RefCell<Option<Now>>,
    }

    /// Секунды в единицах TimeSpan (по 100 нс).
    fn span(seconds: f64) -> TimeSpan {
        TimeSpan { Duration: (seconds.max(0.0) * 1e7) as i64 }
    }

    impl Controls {
        pub fn start(window: &slint::Window, act: Act) -> Result<Self, String> {
            let hwnd = window
                .with_winit_window(|window| match window.window_handle().map(|handle| handle.as_raw()) {
                    Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get()),
                    _ => None,
                })
                .flatten()
                .ok_or("no window handle")?;
            let smtc = Self::open(hwnd, act).map_err(|error| error.message())?;
            Ok(Self { smtc, last: RefCell::new(None) })
        }

        fn open(hwnd: isize, act: Act) -> windows::core::Result<SystemMediaTransportControls> {
            let interop = windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
            // Окно живёт дольше панели: панель закрывается вместе с плеером.
            let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd as *mut core::ffi::c_void))? };
            smtc.SetIsEnabled(true)?;
            smtc.SetIsPlayEnabled(true)?;
            smtc.SetIsPauseEnabled(true)?;
            smtc.SetIsStopEnabled(true)?;
            let buttons = act.clone();
            smtc.ButtonPressed(&TypedEventHandler::new(
                move |_, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                    let action = match args.ok()?.Button()? {
                        SystemMediaTransportControlsButton::Play => Some(Action::Play),
                        SystemMediaTransportControlsButton::Pause => Some(Action::Pause),
                        SystemMediaTransportControlsButton::Stop => Some(Action::Stop),
                        SystemMediaTransportControlsButton::Next => Some(Action::Next),
                        SystemMediaTransportControlsButton::Previous => Some(Action::Previous),
                        _ => None,
                    };
                    if let Some(action) = action {
                        buttons(action);
                    }
                    Ok(())
                },
            ))?;
            let seeks = act.clone();
            smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
                    let position = args.ok()?.RequestedPlaybackPosition()?;
                    seeks(Action::SeekTo(position.Duration as f64 / 1e7));
                    Ok(())
                },
            ))?;
            smtc.AutoRepeatModeChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<AutoRepeatModeChangeRequestedEventArgs>| {
                    let repeat = match args.ok()?.RequestedAutoRepeatMode()? {
                        MediaPlaybackAutoRepeatMode::Track => Repeat::One,
                        MediaPlaybackAutoRepeatMode::List => Repeat::All,
                        _ => Repeat::Off,
                    };
                    act(Action::Repeat(repeat));
                    Ok(())
                },
            ))?;
            Ok(smtc)
        }

        pub fn update(&self, now: Now) {
            let last = self.last.borrow().clone();
            if last.as_ref() == Some(&now) {
                return;
            }
            let _ = self.apply(last.as_ref(), &now);
            *self.last.borrow_mut() = Some(now);
        }

        fn apply(&self, last: Option<&Now>, now: &Now) -> windows::core::Result<()> {
            let smtc = &self.smtc;
            smtc.SetPlaybackStatus(match (now.playback, now.track) {
                (_, None) => MediaPlaybackStatus::Closed,
                (Playback::Playing, _) => MediaPlaybackStatus::Playing,
                (Playback::Paused, _) => MediaPlaybackStatus::Paused,
                (Playback::Stopped, _) => MediaPlaybackStatus::Stopped,
            })?;
            smtc.SetIsNextEnabled(now.can_next)?;
            smtc.SetIsPreviousEnabled(now.can_previous)?;
            smtc.SetAutoRepeatMode(match now.repeat {
                Repeat::Off => MediaPlaybackAutoRepeatMode::None,
                Repeat::One => MediaPlaybackAutoRepeatMode::Track,
                Repeat::All => MediaPlaybackAutoRepeatMode::List,
            })?;
            let same_track = last.is_some_and(|last| {
                (last.track, &last.title, &last.artist, &last.album, &last.art) == (now.track, &now.title, &now.artist, &now.album, &now.art)
            });
            if !same_track {
                let updater = smtc.DisplayUpdater()?;
                updater.ClearAll()?;
                updater.SetType(MediaPlaybackType::Music)?;
                let music = updater.MusicProperties()?;
                music.SetTitle(&HSTRING::from(now.title.as_str()))?;
                music.SetArtist(&HSTRING::from(now.artist.as_str()))?;
                music.SetAlbumTitle(&HSTRING::from(now.album.as_str()))?;
                if let Some(art) = &now.art {
                    let uri = Uri::CreateUri(&HSTRING::from(file_url(art)))?;
                    updater.SetThumbnail(&RandomAccessStreamReference::CreateFromUri(&uri)?)?;
                }
                updater.Update()?;
            }
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            timeline.SetStartTime(span(0.0))?;
            timeline.SetMinSeekTime(span(0.0))?;
            timeline.SetEndTime(span(now.length))?;
            timeline.SetMaxSeekTime(span(now.length))?;
            timeline.SetPosition(span(now.position))?;
            smtc.UpdateTimelineProperties(&timeline)?;
            Ok(())
        }

        pub fn seeked(&self, position: f64) {
            let now = self.last.borrow().clone();
            if let Some(mut now) = now {
                now.position = position;
                self.update(now);
            }
        }

        pub fn active(&self) -> bool {
            true
        }
    }

    impl Drop for Controls {
        fn drop(&mut self) {
            let _ = self.smtc.SetIsEnabled(false);
        }
    }
}

/// Телефон: уведомление и экран блокировки через Java (`crate::android`).
#[cfg(target_os = "android")]
mod imp {
    use std::cell::RefCell;
    use std::sync::Mutex;

    use super::{Act, Now};

    /// Куда отдавать кнопки из уведомления: Java зовёт нас из своего потока.
    pub(super) static ACT: Mutex<Option<Act>> = Mutex::new(None);

    pub struct Controls {
        last: RefCell<Option<Now>>,
    }

    impl Controls {
        pub fn start(_window: &slint::Window, act: Act) -> Result<Self, String> {
            *ACT.lock().map_err(|error| error.to_string())? = Some(act);
            Ok(Self { last: RefCell::new(None) })
        }

        pub fn update(&self, now: Now) {
            if self.last.borrow().as_ref() == Some(&now) {
                return;
            }
            crate::android::show(&now);
            *self.last.borrow_mut() = Some(now);
        }

        pub fn seeked(&self, position: f64) {
            let now = self.last.borrow().clone();
            if let Some(mut now) = now {
                now.position = position;
                crate::android::show(&now);
                *self.last.borrow_mut() = Some(now);
            }
        }

        pub fn active(&self) -> bool {
            true
        }
    }

    impl Drop for Controls {
        fn drop(&mut self) {
            if let Ok(mut act) = ACT.lock() {
                act.take();
            }
            crate::android::hide();
        }
    }
}

/// Кнопка из уведомления: 1 пауза/игра, 2 игра, 3 пауза, 4 стоп, 5 дальше, 6 назад, 7 перемотка (мс).
#[cfg(target_os = "android")]
pub fn android_command(code: i32, value: i64) {
    let act = imp::ACT.lock().ok().and_then(|act| act.clone());
    let Some(act) = act else {
        return;
    };
    act(match code {
        1 => Action::PlayPause,
        2 => Action::Play,
        3 => Action::Pause,
        4 => Action::Stop,
        5 => Action::Next,
        6 => Action::Previous,
        7 => Action::SeekTo(value as f64 / 1000.0),
        _ => return,
    });
}

#[cfg(not(any(windows, target_os = "android", all(unix, not(any(target_os = "macos", target_os = "ios"))))))]
mod imp {
    use super::{Act, Now};

    pub struct Controls;

    impl Controls {
        pub fn start(_window: &slint::Window, _act: Act) -> Result<Self, String> {
            Err("not supported on this system".into())
        }

        pub fn update(&self, _now: Now) {}

        pub fn seeked(&self, _position: f64) {}

        pub fn active(&self) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::file_url;
    use std::path::Path;

    #[test]
    fn file_url_escapes_everything_but_plain_ascii() {
        assert_eq!(file_url(Path::new("/music/Кино/cover 1.jpg")), "file:///music/%D0%9A%D0%B8%D0%BD%D0%BE/cover%201.jpg");
        assert_eq!(file_url(Path::new("C:\\Music\\a#b.jpg")), "file:///C:/Music/a%23b.jpg");
    }
}
