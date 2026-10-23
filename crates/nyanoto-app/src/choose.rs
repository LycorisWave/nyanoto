//! Выбор папки или файла. На компьютере — системный диалог, на телефоне — свой список:
//! системного выбора путей там нет, а доступ ко всем файлам у плеера есть.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::{ModelRc, VecModel};

use crate::{App, BrowseRow, with_app};

#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) enum Want {
    Folder,
    /// Файл: название фильтра и расширения (строчными, без точки).
    File(&'static str, &'static [&'static str]),
}

type Then = Box<dyn FnOnce(&Rc<App>, PathBuf)>;

/// Открытый список: где мы, что ищем и что в папке.
struct Browser {
    dir: PathBuf,
    want: Want,
    then: Then,
    entries: Vec<(PathBuf, bool)>,
}

thread_local! {
    static BROWSER: RefCell<Option<Browser>> = const { RefCell::new(None) };
}

/// Общая память телефона.
const PHONE_HOME: &str = "/storage/emulated/0";

impl App {
    /// Спрашивает папку или файл; `then` — когда выбрали.
    pub(crate) fn choose(&self, want: Want, title: &str, then: impl FnOnce(&Rc<App>, PathBuf) + 'static) {
        #[cfg(not(target_os = "android"))]
        {
            let title = title.to_string();
            crate::pick(
                move || {
                    let mut dialog = rfd::AsyncFileDialog::new();
                    if !title.is_empty() {
                        dialog = dialog.set_title(&title);
                    }
                    let chosen: std::pin::Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>> = match want {
                        Want::Folder => Box::pin(dialog.pick_folder()),
                        Want::File(name, extensions) => Box::pin(dialog.add_filter(name, extensions).pick_file()),
                    };
                    chosen
                },
                then,
            );
        }
        #[cfg(target_os = "android")]
        self.browse_start(want, title, Box::new(then));
    }

    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn browse_start(&self, want: Want, title: &str, then: Then) {
        let home = Path::new(PHONE_HOME);
        let dir = if home.is_dir() { home.to_path_buf() } else { std::env::current_dir().unwrap_or_else(|_| "/".into()) };
        self.window.set_browse_title(title.into());
        self.window.set_browse_folder(matches!(want, Want::Folder));
        BROWSER.with(|browser| *browser.borrow_mut() = Some(Browser { dir, want, then, entries: Vec::new() }));
        self.browse_show();
        self.window.set_browse_open(true);
    }

    /// Перечитывает папку и показывает её.
    fn browse_show(&self) {
        let shown = BROWSER.with(|browser| {
            let mut browser = browser.borrow_mut();
            let browser = browser.as_mut()?;
            browser.entries = list(&browser.dir, &browser.want);
            let rows: Vec<BrowseRow> = browser
                .entries
                .iter()
                .map(|(path, folder)| BrowseRow {
                    name: path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default().into(),
                    folder: *folder,
                })
                .collect();
            Some((browser.dir.display().to_string(), rows))
        });
        if let Some((path, rows)) = shown {
            self.window.set_browse_path(path.into());
            self.window.set_browse_rows(ModelRc::new(VecModel::from(rows)));
        }
    }

    pub(crate) fn browse_enter(&self, index: usize) {
        let picked = BROWSER.with(|browser| {
            let mut browser = browser.borrow_mut();
            let browser = browser.as_mut()?;
            let (path, folder) = browser.entries.get(index)?.clone();
            if folder {
                browser.dir = path;
                None
            } else {
                Some(path)
            }
        });
        match picked {
            Some(file) => self.browse_finish(file),
            None => self.browse_show(),
        }
    }

    pub(crate) fn browse_up(&self) {
        BROWSER.with(|browser| {
            if let Some(browser) = browser.borrow_mut().as_mut()
                && let Some(parent) = browser.dir.parent()
            {
                browser.dir = parent.to_path_buf();
            }
        });
        self.browse_show();
    }

    /// «Выбрать эту папку».
    pub(crate) fn browse_pick(&self) {
        let dir = BROWSER.with(|browser| browser.borrow().as_ref().map(|browser| browser.dir.clone()));
        if let Some(dir) = dir {
            self.browse_finish(dir);
        }
    }

    pub(crate) fn browse_close(&self) {
        BROWSER.with(|browser| browser.borrow_mut().take());
        self.window.set_browse_open(false);
        self.window.set_browse_rows(ModelRc::default());
    }

    fn browse_finish(&self, path: PathBuf) {
        let browser = BROWSER.with(|browser| browser.borrow_mut().take());
        self.window.set_browse_open(false);
        self.window.set_browse_rows(ModelRc::default());
        if let Some(browser) = browser {
            // Не изнутри нажатия: `then` может открыть другое окно.
            slint::Timer::single_shot(std::time::Duration::ZERO, move || with_app(|app| (browser.then)(app, path)));
        }
    }
}

/// Папки, а для файла — ещё и подходящие файлы; скрытые не показываем.
fn list(dir: &Path, want: &Want) -> Vec<(PathBuf, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(PathBuf, bool)> = read
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| {
            let path = entry.path();
            let folder = path.is_dir();
            let fits = match want {
                Want::Folder => false,
                Want::File(_, extensions) => path
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_lowercase())
                    .is_some_and(|extension| extensions.contains(&extension.as_str())),
            };
            (folder || fits).then_some((path, folder))
        })
        .collect();
    entries.sort_by(|(a, a_folder), (b, b_folder)| {
        b_folder.cmp(a_folder).then_with(|| {
            nyanoto_library::natural_cmp(&a.file_name().unwrap_or_default().to_string_lossy(), &b.file_name().unwrap_or_default().to_string_lossy())
        })
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_first_then_fitting_files() {
        let dir = std::env::temp_dir().join(format!("nyanoto-choose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for folder in ["b", "A10", "A2", ".hidden"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for file in ["x.txt", "y.TXT", "z.jpg"] {
            std::fs::write(dir.join(file), b"").unwrap();
        }
        let names = |want| list(&dir, &want).into_iter().map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(names(Want::Folder), ["A2", "A10", "b"]);
        assert_eq!(names(Want::File("", &["txt"])), ["A2", "A10", "b", "x.txt", "y.TXT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
