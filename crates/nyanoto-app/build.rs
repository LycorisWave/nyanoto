//! Разметка окна и сведения о сборке для «О приложении»: версии зависимостей
//! из Cargo.lock, цель, профиль и версия компилятора.

use std::path::Path;

/// Зависимости, которые показываются в «О приложении».
const SHOWN: &[&str] = &["symphonia", "lofty", "cpal", "rubato", "realfft", "rusqlite", "slint", "image", "rfd"];

fn main() {
    let android = std::env::var("TARGET").is_ok_and(|target| target.contains("android"));
    if android {
        let ui = mobile_ui();
        let config = slint_build::CompilerConfiguration::new().with_include_paths(vec![Path::new("ui").into(), Path::new("ui/mobile").into()]);
        slint_build::compile_with_config(ui, config).expect("разметка телефона не собралась");
    } else {
        slint_build::compile("ui/app.slint").expect("разметка интерфейса не собралась");
    }

    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    println!("cargo::rerun-if-changed={}", lock.display());
    let text = std::fs::read_to_string(&lock).unwrap_or_default();
    let mut found: Vec<(String, String)> = Vec::new();
    for package in text.split("[[package]]") {
        let field = |key: &str| {
            package
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key} = \"")))
                .map(|rest| rest.trim_end_matches('"').to_string())
        };
        if let (Some(name), Some(version)) = (field("name"), field("version"))
            && SHOWN.contains(&name.as_str())
            && !found.iter().any(|(known, _)| *known == name)
        {
            found.push((name, version));
        }
    }
    found.sort_by_key(|(name, _)| SHOWN.iter().position(|shown| shown == name));
    let versions: Vec<String> = found.iter().map(|(name, version)| format!("{name}={version}")).collect();
    println!("cargo::rustc-env=NYANOTO_DEPENDENCIES={}", versions.join(";"));

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let compiler = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo::rustc-env=NYANOTO_RUSTC={compiler}");
    println!("cargo::rustc-env=NYANOTO_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo::rustc-env=NYANOTO_PROFILE={}", std::env::var("PROFILE").unwrap_or_default());
}

/// Разметка телефона: `ui/mobile/app.slint`, куда вместо строки `// @api` встаёт весь набор
/// свойств и колбэков окна компьютера. Rust видит одно и то же окно на обеих платформах.
fn mobile_ui() -> std::path::PathBuf {
    println!("cargo::rerun-if-changed=ui");
    let desktop = std::fs::read_to_string("ui/app.slint").expect("ui/app.slint");
    let template = std::fs::read_to_string("ui/mobile/app.slint").expect("ui/mobile/app.slint");
    let api = window_api(&desktop);
    assert!(template.contains("    // @api\n"), "в ui/mobile/app.slint нет строки `    // @api`");
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("mobile.slint");
    std::fs::write(&out, template.replacen("    // @api\n", &api, 1)).expect("mobile.slint");
    out
}

/// Свойства `in`/`out`/`in-out` и колбэки корня `AppWindow`. Привязки, которые смотрят на
/// элементы окна компьютера (`bar-track.width`), отбрасываются — у телефона их нет.
fn window_api(desktop: &str) -> String {
    let mut api = String::new();
    let mut depth = 0i32;
    let mut inside = false;
    for line in desktop.lines() {
        if line.starts_with("export component AppWindow") {
            inside = true;
        }
        let code = code_of(line);
        if inside && depth == 1 {
            let trimmed = line.trim_start();
            let declared = ["in property", "out property", "in-out property", "callback ", "pure callback "]
                .iter()
                .any(|start| trimmed.starts_with(start));
            if declared && line.starts_with("    ") && !line.starts_with("     ") {
                api.push_str(&strip_foreign_binding(line));
                api.push('\n');
            }
        }
        if inside {
            depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
            if depth == 0 && code.contains('}') {
                break;
            }
        }
    }
    api
}

fn strip_foreign_binding(line: &str) -> String {
    let Some((head, binding)) = line.split_once(">").and_then(|(left, right)| {
        let (name, value) = right.split_once(':')?;
        Some((format!("{left}>{name}"), value))
    }) else {
        return line.to_string();
    };
    let foreign = binding.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '.' || c == '_')).any(|word| {
        word.contains('.') && !["Tr.", "Theme.", "root.", "Ui.", "Icons.", "self."].iter().any(|known| word.starts_with(known))
            && !word.chars().next().is_some_and(|c| c.is_ascii_digit())
    });
    if foreign { format!("{};", head.trim_end()) } else { line.to_string() }
}

/// Строка без комментария и без текста в кавычках: только код, чтобы считать скобки.
fn code_of(line: &str) -> String {
    let mut code = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => quoted = false,
                _ => {}
            }
        } else if c == '"' {
            quoted = true;
        } else if c == '/' && chars.peek() == Some(&'/') {
            break;
        } else {
            code.push(c);
        }
    }
    code
}
