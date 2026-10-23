fn main() {
    println!("cargo::rerun-if-changed=src/stretch.cpp");
    println!("cargo::rerun-if-changed=vendor/signalsmith");
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .file("src/stretch.cpp")
        .include("vendor/signalsmith")
        .warnings(false)
        // Вычислительное ядро оптимизируется и в отладочной сборке, чтобы тесты не ползли.
        .opt_level(2)
        .compile("nyanoto-stretch");

    build_opus();
    build_wavpack();
    build_musepack();
    build_ape();
    build_ffmpeg_shim();
}

/// Прослойка к FFmpeg: сам он не линкуется, нужны только заголовки.
fn build_ffmpeg_shim() {
    println!("cargo::rerun-if-changed=src/native/ffmpeg.c");
    println!("cargo::rerun-if-changed=vendor/ffmpeg");
    cc::Build::new()
        .file("src/native/ffmpeg.c")
        .include("vendor/ffmpeg/include")
        .warnings(false)
        .opt_level(2)
        .compile("nyanoto-ffmpeg-shim");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "linux" || os == "android" {
        println!("cargo::rustc-link-lib=dl");
    }
}

/// libopus из исходников, float, без ассемблера. Список — из её `*_sources.mk`.
fn build_opus() {
    const ROOT: &str = "vendor/opus";
    println!("cargo::rerun-if-changed={ROOT}");
    let mut build = cc::Build::new();
    for (list, variables) in [
        ("celt_sources.mk", &["CELT_SOURCES"][..]),
        ("silk_sources.mk", &["SILK_SOURCES", "SILK_SOURCES_FLOAT"][..]),
        ("opus_sources.mk", &["OPUS_SOURCES", "OPUS_SOURCES_FLOAT"][..]),
    ] {
        let text = std::fs::read_to_string(format!("{ROOT}/{list}")).expect("список исходников libopus");
        for file in sources(&text, variables) {
            build.file(format!("{ROOT}/{file}"));
        }
    }
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    build
        .include(format!("{ROOT}/include"))
        .include(format!("{ROOT}/celt"))
        .include(format!("{ROOT}/silk"))
        .include(format!("{ROOT}/silk/float"))
        .include(format!("{ROOT}/src"))
        .define("OPUS_BUILD", None)
        // Версию подставляет configure официальной сборки; без неё — «unknown».
        .define("PACKAGE_VERSION", "\"1.6.1\"")
        .define("ENABLE_HARDENING", None)
        .define("HAVE_LRINT", None)
        .define("HAVE_LRINTF", None)
        // Массивы на стеке: в MSVC нет массивов переменной длины из C99.
        .define(if msvc { "USE_ALLOCA" } else { "VAR_ARRAYS" }, None)
        .warnings(false)
        .opt_level(2)
        .compile("nyanoto-opus");
}

/// libwavpack: только C, со старыми файлами и DSD. Читает через наши колбэки — `open_filename.c` не нужен.
fn build_wavpack() {
    const ROOT: &str = "vendor/wavpack";
    println!("cargo::rerun-if-changed={ROOT}");
    let mut build = cc::Build::new();
    for entry in std::fs::read_dir(format!("{ROOT}/src")).expect("исходники libwavpack") {
        let path = entry.expect("исходники libwavpack").path();
        if path.extension().is_some_and(|extension| extension == "c") {
            build.file(path);
        }
    }
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    build
        .include(format!("{ROOT}/include"))
        .include(format!("{ROOT}/src"))
        .define("ENABLE_LEGACY", None)
        .define("ENABLE_DSD", None)
        .define(if msvc { "_CRT_SECURE_NO_WARNINGS" } else { "HAVE___BUILTIN_CLZ" }, None)
        .warnings(false)
        .opt_level(2)
        .compile("nyanoto-wavpack");
}

/// libmpcdec (SV7, SV8); правки — в `vendor/musepack/VERSION.txt`.
fn build_musepack() {
    const ROOT: &str = "vendor/musepack";
    println!("cargo::rerun-if-changed={ROOT}");
    let mut build = cc::Build::new();
    for entry in std::fs::read_dir(format!("{ROOT}/libmpcdec")).expect("исходники libmpcdec") {
        let path = entry.expect("исходники libmpcdec").path();
        if path.extension().is_some_and(|extension| extension == "c") {
            build.file(path);
        }
    }
    build
        .file(format!("{ROOT}/nyanoto_crc32.c"))
        .include(format!("{ROOT}/include"))
        .include(format!("{ROOT}/libmpcdec"))
        .warnings(false)
        .opt_level(2)
        .compile("nyanoto-musepack");
}

/// Monkey's Audio SDK. SSE/AVX/NEON — отдельно со своими флагами, выбор при запуске.
fn build_ape() {
    const ROOT: &str = "vendor/monkeys-audio";
    println!("cargo::rerun-if-changed={ROOT}");
    println!("cargo::rerun-if-changed=src/native/ape.cpp");
    const SIMD: &[&str] = &[
        "NNFilterSSE2.cpp",
        "NNFilterSSE4.1.cpp",
        "NNFilterAVX2.cpp",
        "NNFilterAVX512.cpp",
        "NNFilterNeon.cpp",
        "NNFilterRVV.cpp",
        "NNFilterAltiVec.cpp",
    ];
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // SDK пишет `<Windows.h>`, у MinGW — `windows.h`. Переходник только при кросс-сборке.
    let host_windows = std::env::var("HOST").is_ok_and(|host| host.contains("windows"));
    let compat = (windows && !host_windows).then(|| {
        let compat = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("ape-compat");
        std::fs::create_dir_all(&compat).expect("папка переходника");
        std::fs::write(compat.join("Windows.h"), "#include <windows.h>\n").expect("переходник Windows.h");
        compat
    });
    let base = || {
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .std("c++14")
            .include(format!("{ROOT}/Shared"))
            .include(format!("{ROOT}/MACLib"))
            .warnings(false)
            .opt_level(2);
        if let Some(compat) = &compat {
            build.include(compat);
        }
        build
    };

    let mut build = base();
    let shared = [
        "BufferIO", "CharacterHelper", "CircleBuffer", "CPUFeatures", "CRC", "GlobalFunctions", "MemoryIO",
        "Semaphore", "Thread", "WholeFileIO",
    ];
    for name in shared {
        build.file(format!("{ROOT}/Shared/{name}.cpp"));
    }
    build.file(format!("{ROOT}/Shared/{}.cpp", if windows { "WinFileIO" } else { "StdLibFileIO" }));
    for dir in ["MACLib", "MACLib/Old"] {
        for entry in std::fs::read_dir(format!("{ROOT}/{dir}")).expect("исходники Monkey's Audio") {
            let path = entry.expect("исходники Monkey's Audio").path();
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            if name.ends_with(".cpp") && !SIMD.contains(&name) {
                build.file(&path);
            }
        }
    }
    build.file("src/native/ape.cpp").compile("nyanoto-ape");

    // Флаги для GCC и Clang; MSVC разрешает встроенные функции без флагов.
    let simd: &[(&str, &[&str])] = match arch.as_str() {
        "x86" | "x86_64" => &[
            ("NNFilterSSE2.cpp", &["-msse2"]),
            ("NNFilterSSE4.1.cpp", &["-msse4.1"]),
            ("NNFilterAVX2.cpp", &["-mavx2"]),
            ("NNFilterAVX512.cpp", &["-mavx512dq", "-mavx512bw"]),
        ],
        "aarch64" => &[("NNFilterNeon.cpp", &[])],
        "arm" => &[("NNFilterNeon.cpp", &["-mfpu=neon"])],
        _ => &[],
    };
    for (index, (file, flags)) in simd.iter().enumerate() {
        let mut build = base();
        build.file(format!("{ROOT}/MACLib/{file}"));
        if !msvc {
            for flag in *flags {
                build.flag(flag);
            }
        }
        build.compile(&format!("nyanoto-ape-simd{index}"));
    }
}

/// Файлы из переменных `ИМЯ = \` makefile-списка, по одному в строке.
fn sources<'a>(text: &'a str, variables: &[&str]) -> Vec<&'a str> {
    let mut files = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some((name, _)) = line.split_once('=') {
            inside = variables.contains(&name.trim());
            continue;
        }
        if inside {
            let file = line.trim_end_matches('\\').trim();
            if file.ends_with(".c") {
                files.push(file);
            }
            inside = line.ends_with('\\');
        }
    }
    files
}
