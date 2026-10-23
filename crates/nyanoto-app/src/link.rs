//! Связка ПК и телефона через папку, которую пользователь синхронизирует сам.

use std::path::{Path, PathBuf};

use nyanoto_library::{SyncFile, SyncReport};

use crate::App;

pub const SETTING_SYNC: &str = "sync";
pub const SETTING_SYNC_FOLDER: &str = "sync_folder";
const SETTING_DEVICE_ID: &str = "device_id";
/// Подпапка внутри выбранной папки: там лежат файлы всех устройств.
const SUBFOLDER: &str = "nyanoto-sync";
/// Старое имя, переименовывается.
const OLD_SUBFOLDER: &str = "nekori-sync";

/// Пишет файл через временный: оборванная запись не оставит половину.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let temporary = path.with_extension("json-tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

impl App {
    fn sync_folder(&self) -> Option<PathBuf> {
        let library = self.library.as_ref()?;
        let folder = library.setting(SETTING_SYNC_FOLDER).ok().flatten()?;
        if folder.is_empty() {
            return None;
        }
        let (folder, old) = (PathBuf::from(&folder).join(SUBFOLDER), PathBuf::from(&folder).join(OLD_SUBFOLDER));
        if !folder.exists() && old.is_dir() {
            let _ = std::fs::rename(&old, &folder);
        }
        Some(folder)
    }

    /// Постоянный номер этого устройства: имя файла в папке связки.
    fn device_id(&self) -> String {
        let library = self.library.as_ref();
        if let Some(id) = library.and_then(|library| library.setting(SETTING_DEVICE_ID).ok().flatten()) {
            return id;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);
        let id = format!("{:016x}", nanos ^ (u64::from(std::process::id()) << 32));
        self.save_setting(SETTING_DEVICE_ID, &id);
        id
    }

    fn device_name() -> String {
        // В Linux `HOSTNAME` — переменная оболочки: программе, запущенной с
        // рабочего стола, она не достаётся. Имя машины знает ядро.
        std::env::var("COMPUTERNAME")
            .ok()
            .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
            .or_else(|| std::env::var("HOSTNAME").ok())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Nyanoto".into())
    }

    /// Показывает в настройках выбранную папку.
    pub(crate) fn show_sync_folder(&self) {
        let lex = self.lex();
        let folder = self
            .library
            .as_ref()
            .and_then(|library| library.setting(SETTING_SYNC_FOLDER).ok().flatten())
            .filter(|folder| !folder.is_empty())
            .unwrap_or_else(|| lex.sync_none.to_string());
        self.window.set_sync_folder(folder.into());
    }

    pub(crate) fn choose_sync_folder(&self) {
        self.choose(crate::choose::Want::Folder, "", |app, folder| {
            app.save_setting(SETTING_SYNC_FOLDER, &folder.to_string_lossy());
            app.show_sync_folder();
            app.sync_now();
        });
    }

    /// Принимает файлы других устройств и кладёт свой.
    pub(crate) fn sync_now(&self) {
        let lex = self.lex();
        let (Some(library), Some(folder)) = (self.library.as_ref(), self.sync_folder()) else {
            return;
        };
        if !self.sync_on.get() {
            return;
        }
        if let Err(error) = std::fs::create_dir_all(&folder) {
            self.window.set_sync_status(format!("{}: {error}", lex.sync_failed).into());
            return;
        }
        let own = format!("{}.json", self.device_id());
        let mut total = SyncReport::default();
        let mut problems = Vec::new();
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == own || path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let parsed = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| serde_json::from_str::<SyncFile>(&text).map_err(|error| error.to_string()));
            match parsed.and_then(|file| library.sync_import(&file).map_err(|error| error.to_string())) {
                Ok(report) => {
                    total.likes += report.likes;
                    total.unlikes += report.unlikes;
                    total.playlists += report.playlists;
                    total.plays += report.plays;
                    total.unknown += report.unknown;
                }
                Err(error) => problems.push(format!("{name}: {error}")),
            }
        }
        self.export_sync();
        // Принятое сразу видно в окне.
        self.liked.replace(library.liked_ids().unwrap_or_default());
        self.refresh_playlists();
        self.render_all();
        let mut text = format!(
            "{}: {} {}, {} {}, {} {}, {} {}",
            lex.sync_done,
            lex.sync_likes,
            total.likes,
            lex.sync_unlikes,
            total.unlikes,
            lex.sync_playlists,
            total.playlists,
            lex.sync_plays,
            total.plays
        );
        if total.unknown > 0 {
            text.push_str(&format!("; {}: {}", lex.sync_unknown, total.unknown));
        }
        if !problems.is_empty() {
            text.push_str(&format!("; {}: {}", lex.sync_failed, problems.join("; ")));
        }
        self.window.set_sync_status(text.into());
    }

    /// Кладёт в папку свой файл: при выходе и после приёма чужих.
    pub(crate) fn export_sync(&self) {
        let (Some(library), Some(folder)) = (self.library.as_ref(), self.sync_folder()) else {
            return;
        };
        if !self.sync_on.get() || std::fs::create_dir_all(&folder).is_err() {
            return;
        }
        let path = folder.join(format!("{}.json", self.device_id()));
        let written = library
            .sync_export(&Self::device_name())
            .map_err(|error| error.to_string())
            .and_then(|file| serde_json::to_string(&file).map_err(|error| error.to_string()))
            .and_then(|text| write_atomically(&path, &text).map_err(|error| error.to_string()));
        if let Err(error) = written {
            self.window
                .set_sync_status(format!("{}: {error}", self.lex().sync_failed).into());
        }
    }
}
