# Nyanoto — development log

Chronological engineering log: what was done, what broke, how it was fixed,
what was measured. Written in English so any developer can pick up the
project. Newest entries at the bottom of each day.

Companion journals (Russian, the owner reads them):

- [ISSUES.md](ISSUES.md) — open, unresolved problems (bugs, unverified things, platform gaps).
- [IDEAS.md](IDEAS.md) — feature ideas with status.
- [../PLAN.md](../PLAN.md) — the agreed plan, stages, decisions (Russian, authoritative for product decisions).

## Start here

**What nyanoto is.** A local, offline audio player written in Rust with a Slint UI.
Targets: Windows, Linux, Android — *equally*, no platform is the priority
(owner's statement 2026-10-16, replaces "Windows first" from PLAN.md of 2026-10-14).
It is meant to become a quality product for the community, not a personal toy.

**Philosophy (from the owner, 2026-10-16).**
- Born from frustration with AIMP: limited features and lag/crash on big libraries (many FLAC).
- Must be very energy-efficient, low RAM, fast. Memory must not grow with library size.
- Cross-platform only for Windows, Linux, Android, and all three must work equally well.
  Android may get a different frontend later.
- Packed with features, but every feature is an opt-in module, OFF by default,
  toggled in settings; the user decides. Nothing is imposed.
- Frontend is deliberately minimal right now; features first, beauty later.
- Strictly offline: no sockets, no network crates, no telemetry (see PLAN.md "Принципы").
- Owner wants product decisions agreed with them before implementation.
- The owner writes Russian (often in a Russian keyboard layout, with typos); answer in Russian.

**Constitution — priority order (owner, 2026-10-17):** sound quality → support (exotic formats and
cross-platform) → speed → resource load. Performance still matters: the trigger was Yandex Music using
2 GB RAM and 30 % GPU on an i9-12900KF / RTX 3080 / 32 GB box. When options conflict, pick the better one
by the first criterion; on a tie, by the next.

**Layout.** Cargo workspace, `crates/`:
`nyanoto-core` (decode via Symphonia, resample via rubato, output via cpal, queue, gapless,
EQ, stretch via vendored Signalsmith C++ through `cc`, FLAC writer, inspect/authenticity),
`nyanoto-library` (SQLite via rusqlite bundled, scanner, tags via lofty, search, sync, smart playlists, health),
`nyanoto-app` (Slint window, binary `nekori`), `nyanoto-cli` (console player/tools, binary `nyanoto-cli`),
`nyanoto-testkit` (synthetic FLAC writer for tests).

**Build on Linux (Fedora 43).**
```
# once: rustup (user-level) + system packages
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt
sudo dnf install -y gcc-c++ alsa-lib-devel fontconfig-devel
# then
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p nyanoto-app -p nyanoto-cli
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Native deps on Linux are exactly: `alsa-sys` (cpal), `yeslogic-fontconfig-sys` (fontique, not dlopen),
`cc` for `stretch.cpp`, bundled SQLite. Wayland/X11/xkbcommon are dlopen'ed at runtime by winit.

**Data locations.** DB: `$XDG_DATA_HOME/nyanoto/library.db` (fallback `~/.local/share/nyanoto/`),
on Windows `%APPDATA%\nyanoto\library.db`. Wave cache: `waves/` next to the DB.

**Testing rules learned the hard way (from the Windows days).**
- Never play audible test tones: use silent files or set volume 0 first. When the speed
  module is on, slider #1 in UI automation is *speed*, not volume.
- Test folders and DB copies go to a scratch dir, never into the owner's music folders.
- Never modify or delete the owner's music.

## 2026-10-14 … 2026-10-15 (Windows — summary)

Reconstructed from PLAN.md and the Windows notes.
Stages 0–4 done: engine, CLI, library, folder-based UI, 12 opt-in modules
(CP1251 tags, classical metadata, long recordings, smart playlists, library health,
waveform seekbar, parametric EQ + AutoEQ import, per-device profiles, crossfade,
WASAPI exclusive, PC↔phone sync through a folder, conversion to FLAC/WAV),
LRC editor with waveform, speed control (Signalsmith Stretch), authenticity check (beta).
114 tests + clippy clean on Windows at that time. See PLAN.md "Состояние" for details.

Windows work stopped in the middle of:
- startup timing via `NYANOTO_TRACE=<file>` env var — marked TEMPORARY in `nyanoto-app/src/main.rs` (`fn trace`);
- switched Slint to software renderer only (`default-features = false`, `renderer-software`)
  — measured on Windows: much faster window creation and ~3x less RAM (no GPU driver);
- `[profile.release] lto = "fat"`, `codegen-units = 1`, `nyanoto-app` at `opt-level = "s"` (exe 45 MB → 28.8 MB);
- scan progress bar in the status line (`scan-progress` property + `show_scan_progress`);
- was about to add an on-disk cover thumbnail cache (not started in code).
Measured on Windows: cold start ~2.3 s to window (mostly antivirus scanning a fresh exe),
warm start ~150 ms; RSS ~87 MB.

Owner's answers at that time (still valid):
- keep third-party libraries (Slint, SQLite, Symphonia…), no rewrite to "100% own code";
- focus instead on making the player faster and lighter; startup progress only if it is real;
- **must support the full set of formats, "from WAV to Opus and MP3"** (Opus is not in Symphonia 0.5 — open issue);
- rewrite of Signalsmith stretch in Rust only if it gives better performance.

## 2026-10-16 (Linux, Fedora 43)

Environment found:
- Project copied from Windows into `/home/lycoris/Документы/nyanoto`.
- `.git` is broken/empty: only `HEAD`, `config`, hooks; no `objects/`, no `refs/`
  (the Windows repo had zero commits anyway; empty dirs were lost in the copy).
  → `git` says "not a git repository". No history exists.
- `target/` contained 28 GB of Windows-only artifacts (`.exe`, `.pdb`). Deleted with the owner's
  approval ("можно удалить, но помни, что в релизе будет поддержка windows").
- No Rust on the machine. Installed rustup (user-level) → rustc/cargo 1.99.1 in `~/.cargo/bin`.
- No C++ compiler / dev headers; `sudo` needs a password, so the owner installed
  `gcc-c++ alsa-lib-devel fontconfig-devel` manually.
- Machine: 4 cores, 7.7 GB RAM (≈3 GB free), ~84 GB free disk after cleanup. Disk is tight — the owner
  keeps backups on the phone; do NOT create backup copies of the project.
- Test music: `/home/lycoris/Документы/music` — 3 artists, 92 FLAC (Qobuz layout:
  `Artist/Album (year) (Clean|Explicit) [UPC…]/NN. Artist - Title.flac`, `Cover.jpg`), 1 `.lrc`, 1.6 GB.
  Folder names contain `ʔ` (U+0294) instead of `?` — glyph availability to be checked in the UI.

### Code review for Linux (before the first Linux build finished)

Findings and changes (all compile-checked later, see below):

1. **Scanner: folder symlink loops** (`nyanoto-library/src/scan.rs`, `walk_tree`). A symlinked dir
   was followed blindly up to `MAX_DEPTH = 64`: `music/A/loop -> music` duplicates every track
   ~64×, two such links → exponential walk (effectively a hang). Fix: canonicalize symlinked dirs;
   skip links whose target is inside the (canonical) root or was already walked; links to outside
   folders are walked once. Test `folder_links_do_not_loop_or_duplicate` (unix only).
2. **Scanner: hidden entries** — dot-prefixed names are now skipped (`.Trash-1000` on USB sticks,
   macOS AppleDouble `._01.flac` which are not audio and showed up as broken tracks).
   Test `hidden_entries_are_skipped`. Windows `$RECYCLE.BIN` / `System Volume Information`
   are NOT handled yet (hidden+system attributes) — noted in ISSUES.md.
3. **Sync device name** (`nyanoto-app/src/link.rs`, `device_name`): used `COMPUTERNAME`/`HOSTNAME`
   env; `HOSTNAME` is a bash variable, not exported to GUI apps → every Linux device was named
   "nyanoto". Now reads `/proc/sys/kernel/hostname` (Linux/Android). Android will still say
   "localhost" — use `Build.MODEL` via JNI later.
4. **Quiet mode on Linux** (`nyanoto-app/src/quiet.rs`): was only `nice(19)`. Now
   `sched_setscheduler(0, SCHED_IDLE)` (fallback `nice(19)`) + `ioprio_set(IOPRIO_WHO_PROCESS, 0,
   IOPRIO_CLASS_IDLE)` via raw `syscall` (numbers for x86_64/aarch64/x86/arm). Both affect only
   the calling thread on Linux. Test `background_touches_only_this_thread` reads policy from
   `/proc/thread-self/stat` (field 41).
5. **Blocking file dialogs** (`rfd::FileDialog` sync in 4 places: add folder, sync folder,
   conversion folder, AutoEQ import). rfd's xdg-portal backend does `pollster::block_on` on the
   calling (UI) thread → the Slint/winit event loop stops; on GNOME/Wayland the compositor ping
   goes unanswered → "not responding" dialog. Replaced with `rfd::AsyncFileDialog` +
   `slint::spawn_local` through one helper `pick()` in `main.rs` (also prevents a second dialog
   while one is open). rfd's async impl on Linux runs the portal call on its own thread.

### First Linux build and runtime checks

- First full debug build (`cargo build --workspace --all-targets`): 20.5 min on 4 cores; **0 errors**.
  Only 2 warnings: `exclusive::encode` / `exclusive::candidates` unused off Windows (they are
  platform-neutral and unit-tested everywhere) → `#[cfg_attr(not(windows), allow(dead_code))]`.
  One clippy `needless_borrow` from my own refactor (`eq_import_file`). Clippy `-D warnings` clean on Linux.
- `cargo test --workspace`: **all 118 tests pass on Linux** (incl. 3 new ones).
- `nyanoto-cli check ~/Документы/music`: 92/92 FLAC intact, MD5 OK, 0 decoder errors, 221 min of audio
  in 33.6 s (debug-ish build), peak RSS 7 MB.
- `nyanoto-cli scan` into a scratch DB: 92 files in 0.05 s, RSS 9 MB; rescan 0.00 s.
  Layout-fixing search works (`pfgf[` → «Запах цветов вчерашних»).
- GUI starts on GNOME (tested under Xwayland for screenshots): HiDPI ×2, Cyrillic, symbol glyphs
  (|◄ ► ‖ ►| ♥ ♡) and `ʔ` (U+0294) in folder names all render with Noto Sans. Playback, covers,
  quality line work.
- Startup trace (debug build, cold): window created at 1.53 s, first scan done at 1.6 s.

**Testing setup on Linux (reuse it):**
- Scratch data: `XDG_DATA_HOME=<scratch>/data` → DB at `<scratch>/data/nyanoto/library.db`.
- Silent audio: `pactl load-module module-null-sink sink_name=nyanoto_null` and run the app with
  `PIPEWIRE_PROPS='{ target.object = nyanoto_null }'` (pipewire-alsa honours it). Verify with
  `pactl list short sinks` (null sink RUNNING, real sink SUSPENDED). Also set app volume to 0.
  Unload the module afterwards.
- UI automation through AT-SPI: accesskit only exposes the tree when the session a11y flag is on:
  `busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true`
  (restore to `false` afterwards). Python `gi` + `Atspi` can press buttons by name and set slider values.
- Screenshots: run with `env -u WAYLAND_DISPLAY` (X11 via Xwayland), window id from
  `xprop -root _NET_CLIENT_LIST`, then `import -window <id> shot.png`. Native Wayland windows can't be captured this way.
- The GNOME file chooser portal is served by Nautilus (`org.gnome.Nautilus` in AT-SPI). Careful:
  the owner's own Nautilus windows live in the same app — identify the chooser by its «Выбрать» button.
- Measurements: per-thread CPU and context switches from `/proc/<pid>/task/*/{stat,status}`;
  PipeWire quantum per node from `pw-top -b -n 3`.

**Energy: device buffer on Linux** (`nyanoto-core/src/output.rs`, `roomy_buffer`).
Measured (debug build, playing FLAC 44.1 kHz into the null sink, 20 s):
| | cpal default (100 ms / 25 ms period) | Fixed 200 ms (50 ms period) |
|---|---|---|
| PipeWire quantum for our stream | 1024 @ 48 kHz | 2048 (= max-quantum) |
| `data-loop.0` (PipeWire client thread) | 47.5 wakeups/s | 23.9/s |
| `cpal_alsa_out` | 28.0/s | 20.5/s |
| whole process | 104/s, 1.40 % of one core | 73/s, 1.25 % |
Idle (paused/stopped): 2 wakeups/s, 0 % CPU. UI thread ~20 wakeups/s while playing (4 Hz position ticker).
Trade-off accepted for now: pause/volume take effect after up to 0.2 s instead of 0.1 s
(shown to the owner as a decision). Falls back to cpal's default if the device rejects the size.
Not applied on Windows/Android yet (untested there).

**Volume persistence.** Without the per-device profiles module the volume was reset to 100 % on every
start (a loud surprise if the user listened at 20 %). Now setting `volume` (0…1) is saved 1 s after the
slider stops and on exit, restored on start; per-device profile volume still wins when that module is on.
Verified via AT-SPI: set 0.3 → restart → slider 0.3, label «Громкость 30%».

**Async dialogs verified on GNOME:** while the folder chooser was open, the player's position kept
advancing (73 → 75 → 76 s) — the UI thread is no longer blocked; cancelling resets the
"one dialog at a time" guard (second press opened a new chooser).

**Accessibility:** the search field and the seek/speed/volume sliders had no accessible names
(screen readers announced an empty "slider"). Added `accessible-label`s (new string `seek`
«Перемотка»/«Seek»).

**Startup (debug build, warm):** window created 54 ms after `main` under X11, 46 ms under native
Wayland (the 1.5 s seen earlier was the very first cold launch: 115 MB debug binary + font cache from disk).
**Native Wayland playback (debug):** 66 wakeups/s, 1.25 % of one core, RSS 122 MB
(X11: 95 MB). Breakdown: 61.5 MB anonymous (software-renderer frame buffers at 2200×1440 HiDPI, fonts,
glyph caches, cover pixels), 23.6 MB debug binary text, 10.8 MB `memfd:softbuffer`, 6.4 MB
`memfd:smithay-client-toolkit` (client-side decorations). Release numbers below.

### Release build on Linux and the memory fix

- `cargo build --release -p nyanoto-app -p nyanoto-cli`: 15 min from scratch (fat LTO, codegen-units=1 →
  the final `nekori` crate is ~5 min single-threaded on every app change). Binaries:
  `target/release/nyanoto` 34.7 MB (not stripped), `target/release/nyanoto-cli` 7.3 MB.
- Release, native Wayland: window created 37 ms after `main`; idle in the artists list 48 MB RSS,
  0 % CPU, ~2 wakeups/s.

**Memory bug: freed cover-decoding memory kept by glibc** (`nyanoto-app/src/main.rs`,
`return_big_blocks_to_system`). Opening an artist with 22 albums raised RSS 48 → 105 MB and it never
came back. `gdb -p <pid> -batch -ex 'call (void)malloc_stats()'` (prints into the app's stderr) showed
arenas 8 and 9 (the two `nyanoto-covers-*` threads) with ~29–31 MB "system bytes" but only 0.5–0.8 MB in use.
Cause: glibc's dynamic mmap threshold — after a big mmapped chunk is freed the threshold (and with it the
trim threshold) is raised, so later 7–9 MB decode buffers (1500×1500 JPEG) come from the per-thread arena,
and the arena's top chunk is never returned. `malloc_trim(0)` after each cover did NOT help (it does not
shrink the top of non-main arenas) — tried and removed. Fix: `mallopt(M_MMAP_THRESHOLD, 128 KiB)` at the
start of `main` (linux-gnu only) — fixing the threshold disables the dynamic adjustment, big blocks are
mmapped and unmapped immediately. Same effect as `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072`.
| release, Wayland, HiDPI ×2 | before | after |
|---|---|---|
| start (artists list) | 47.7 MB | 47.6 MB |
| artist page (22 covers) | 105.4 MB | 50.5 MB |
| album page | 105.2 MB | 50.8 MB |
| playing | 114.5 MB | 59.7 MB |
| peak (VmHWM) | 138.7 MB | 112.1 MB |
CPU while playing unchanged: 1.03–1.10 % of one core, ~62 wakeups/s (UI 14/s, PipeWire client 24/s,
cpal 16.5/s, engine 2/s). The remaining peak is two covers decoded at full size in parallel
(~30 MB transient each) — the on-disk thumbnail cache would remove it (IDEAS.md).

**State at the end of the day:** clippy `-D warnings` clean on Linux, 118/118 tests pass,
release binaries built. Not compile-checked for Windows/Android targets from Linux (would need
cross C++ toolchains for `stretch.cpp`); the Windows-only code paths were not touched except
`#[cfg_attr(not(windows), allow(dead_code))]` and the shared `pick()` dialog helper (rfd's async
dialog on Windows runs on its own thread too — verify on Windows).

## 2026-10-17

Owner's answers to the questions of 2026-10-16:
1. All four decisions (skip hidden entries, don't follow symlinks into the root, 200 ms device
   buffer on Linux, persistent volume) — keep.
2. Formats: choose by the constitution (quality → support → speed → load). → Opus through the
   reference libopus (vendored C, built with `cc` like Signalsmith) rather than a young pure-Rust decoder;
   exotic formats wanted.
3. On-disk cover thumbnail cache: "можно, но рискованно — не засорит ли память?" → build it as an
   experiment, measure disk and RAM, report pros/cons before deciding the default.
4. cpal 0.15 → 0.18: try it.
5. No git for now (don't init, don't commit).

### Opus support (libopus, vendored)

Chosen by the constitution: quality first → the reference decoder, not a young pure-Rust one.
- Source: `opus-1.6.1.tar.gz` (2026-01-15) from downloads.xiph.org, SHA256
  `6ffcb593207be92584df15b32466ed64bbec99109f007c82205f0194572411a1` matched the official SHA256SUMS.txt.
  Vendored into `crates/nyanoto-core/vendor/opus/` (4.1 MB): `include/ src/ celt/ silk/` + `COPYING`, `AUTHORS`,
  the three `*_sources.mk`; no `dnn/` (23 MB of neural models — DRED/deep PLC/OSCE are off by default
  upstream too), no tests, no build-system files. BSD-3 licence.
- Build (`nyanoto-core/build.rs`, `build_opus`): `cc`, file list parsed from upstream `*_sources.mk`
  (`CELT_SOURCES`, `SILK_SOURCES`, `SILK_SOURCES_FLOAT`, `OPUS_SOURCES`, `OPUS_SOURCES_FLOAT`), float build,
  no SIMD/asm (portable to Windows MSVC, Linux, Android NDK identically). Defines: `OPUS_BUILD`,
  `ENABLE_HARDENING`, `HAVE_LRINT(F)`, `VAR_ARRAYS` (or `USE_ALLOCA` on MSVC). Encoder is compiled too
  (unused symbols are dropped by the linker) — it makes Opus *conversion* possible later and is used by tests.
- Decoder (`nyanoto-core/src/opus.rs`): implements Symphonia 0.5 `Decoder` for `CODEC_TYPE_OPUS`;
  parses OpusHead from `extra_data` (Ogg and Matroska both put it there), multistream API for families 0/1
  (mono … 7.1), header output gain applied via `OPUS_SET_GAIN`, pre-skip trimmed by packet timestamp,
  `packet.trim_start/trim_end` honoured, Vorbis channel order remapped to Symphonia planes.
  Registry: `opus::codecs()` = Symphonia defaults + ours; used by `TrackDecoder` (open + rebuild).
- `TrackDecoder` (`source.rs`): duration minus pre-skip; Opus seek = `seek_opus()`: seek 200 ms early
  (`OPUS_PREROLL_FRAMES`) and discard up to the target. Measured on a pure tone, SNR of the first 20 ms
  after a seek: no preroll 1.3 dB, 80 ms (RFC minimum, what ffmpeg/opusfile use) 14.7 dB, continuous decode
  46 dB; convergence is ~5 dB per 20 ms → 200 ms gives > 35 dB. Costs < 1 ms CPU per seek.
- Matroska/WebM timestamps are in ms, Ogg in samples — `opus_samples(ts, time_base)` converts before any
  pre-skip arithmetic (first version trimmed the first ~16 WebM packets → "0:02" instead of 0:03).
- Tests `nyanoto-core/tests/opus.rs` (self-contained: the vendored libopus *encoder* + a tiny Ogg page writer
  with CRC): exact length incl. non-multiple-of-packet end trim, waveform alignment (no shift), header gain
  −6.02 dB → ×0.5, seek lands on the exact sample with > 35 dB, 5.1 channel order (Goertzel per channel).
  Plus unit tests for OpusHead parsing. Workspace: 125 tests pass, clippy clean.
- Cross-check against ffmpeg (system libopus) on ffmpeg-encoded files: Ogg stereo/mono(44.1k source)/5.1 —
  identical length (144000 frames) and 71–76 dB agreement (= the 16-bit dither floor of the WAV comparison).
- **Known gap:** Opus in WebM/MKA keeps up to ~20 ms of encoder padding at the end — Symphonia 0.5's mkv
  demuxer reads `DiscardPadding` and throws the value away (`segment.rs`, `BlockGroupElement::read`).
  Start trimming is correct. Options: patch symphonia-format-mkv, or move to Symphonia 0.6 (check it there).
- Library: lofty cannot parse Matroska/WebM (it guessed "Mpeg" and failed → the track was marked as a
  problem). New `nyanoto-library/src/probe.rs`: when lofty fails, Symphonia probes the container (no decoding)
  for duration/rate/channels/bit depth/average bitrate and tags (StandardTagKey → our fields); used by both
  `tags::parse` (scanner) and `read_fields` (track info dialog). Opus channel count is taken from OpusHead
  when Matroska doesn't state it. `quality()` learned "alac" and "pcm" (lossless).

### cpal 0.15 → 0.18.2 (PulseAudio host on Linux)

- Workspace: `cpal = { version = "0.18", features = ["pulseaudio"] }`. The PulseAudio host is the pure-Rust
  `pulseaudio` 0.3.2 crate (native protocol over the local unix socket; it explicitly refuses
  `PULSE_SERVER=tcp:…`, so the "strictly offline" principle holds; no libpulse, no build deps).
  With it, `cpal::default_host()` on Linux = PulseAudio (via pipewire-pulse) when its socket exists, else ALSA.
  The `pipewire` feature was NOT enabled: it needs libpipewire headers + bindgen/libclang (sudo, heavier build).
- `nyanoto-core` `windows` dependency 0.54 → 0.62 with exactly cpal's feature list (crate built once).
  The WASAPI exclusive code (`exclusive.rs`) compiled unchanged against 0.62.
- API changes handled in `output.rs`: `device.name()` → `device.description()?.name()` (human names:
  «Встроенное аудио Аналоговый стерео» instead of ALSA's `default`), `SampleRate` is now `u32`,
  `build_output_stream` takes `StreamConfig` by value, unified `cpal::Error`. Added F64 and I24 device formats.
  The stream error callback now gets `ErrorKind::Xrun` for plain underruns → counted as underruns,
  no longer treated as a lost device (otherwise every xrun would reopen the device).
- `BufferSize::Fixed(x)` semantics in 0.18 (ALSA and Pulse alike): period = x, buffer = 2x. So the
  Linux constant became `DEVICE_PERIOD_SECONDS = 0.05` (period 50 ms, buffer 100 ms) — same wakeup rate
  as the earlier "200 ms buffer / 50 ms period" ALSA setting, but pause/volume latency back to ≤ 0.1 s.
- The engine keeps one `cpal::Host` per worker (`Worker::host`, created lazily): the Pulse host connects
  to the server in `Host::new()` (thread + handshake), and `follow_default_device()` runs every 2 s while
  playing — creating a host each time would mean a new server connection every 2 s. If opening the device
  fails, the cached host is dropped and the open retried once (the sound server may have restarted).
  `default_device_name()` (used once by the app at startup) still creates a fresh host.
- Cross-checking Windows from Linux: `rustup target add x86_64-pc-windows-gnu` + a fake C compiler for
  build scripts (only `cargo check`/`clippy` — no linking). Script: a `fakecc` that just creates the file
  given by `-o`/`-Fo`; run with
  `CC_x86_64_pc_windows_gnu=fakecc CXX_x86_64_pc_windows_gnu=fakecc AR_x86_64_pc_windows_gnu=ar
   cargo clippy --target x86_64-pc-windows-gnu --workspace --all-targets -- -D warnings`.
  Baseline before the migration and the result after: clean. This type-checks Windows-only code
  (WASAPI, quiet mode, dialogs) but does not run it — runtime checks on Windows are still needed.
- Runtime (Linux, silent FLAC + volume 0): `nyanoto-cli play` → «вывод: Встроенное аудио Аналоговый стерео,
  48000 Гц, 2 кан.», no stutters. Default config is now 48 kHz (cpal 0.18 prefers it).

### Audio path on Linux: hybrid (ALSA stream + PulseAudio names) — measured

Release build, playing a silent FLAC album at volume 0, 30 s, per-thread counters:
| path | nyanoto CPU | nyanoto wakeups/s | extra server work | stream name in the mixer |
|---|---|---|---|---|
| cpal 0.15 ALSA (`default` → pipewire-alsa), period 50 ms | 1.03–1.10 % | ~62 | — | «PipeWire ALSA [nyanoto]» |
| cpal 0.18 PulseAudio host (→ pipewire-pulse) | 1.37 % | 79.6 | pipewire-pulse: 72 wakeups/s, 0.3 % | `cpal-pulseaudio-<PID>` |
| **cpal 0.18 hybrid: ALSA stream + Pulse host for names** | **0.90 %** | 71.5 | — | «PipeWire ALSA [nyanoto]» |
The Pulse host path costs more (its reactor + callback threads, plus the pipewire-pulse middleman), and
cpal hard-codes the client name `cpal-pulseaudio-{pid}` (`host/pulseaudio/mod.rs:138`) — a new name every
launch, so WirePlumber's stream-restore gets a new entry per run and the per-app mixer volume never sticks.
Quality is equal on both paths (float into PipeWire). By the constitution (equal quality and support →
speed → load) the hybrid wins:
- `output::Audio` (`nyanoto-core/src/output.rs`): on Linux `host = cpal::host_from_id(HostId::Alsa)`,
  `names = host_from_id(HostId::PulseAudio).ok()`; elsewhere `default_host()` and no separate names.
  `default_name()` asks `names` (sink description) or falls back to the stream host.
- `follows_default()` = names available = a sound server routes ALSA `default`. Then
  `Worker::follow_default_device()` does NOT reopen the output on a default-device change (PipeWire already
  moved the stream): it renames `output.device`, applies the known device volume and emits `OutputOpened`
  (the app applies the device's EQ profile). Without a server (plain ALSA) or on Windows it reopens as before.
- Verified with `nyanoto-cli play` (same engine) + a temporary null sink: default sink switched
  analog → null → analog; the stream moved (sink 50 → 320 → 50), the engine reported «вывод: nyanoto-null»
  then «вывод: Встроенное аудио Аналоговый стерео», 0 underruns, no reopen. Default sink restored afterwards.
- Per-device profiles therefore now get real, distinct device names on Linux (logic itself unchanged).

### On-disk cover thumbnail cache (experiment the owner asked for)

`nyanoto-app/src/covers.rs`: one JPEG copy per cover, 240 px (the largest size the UI uses: album header at
HiDPI ×2), quality 90, in `<data>/nyanoto/covers/<fnv(path|len|mtime|240)>.jpg`; written via a temp file +
rename; list (120 px) and bar (128 px) sizes are downscaled from it in memory. A background quiet thread
(`sweep`) keeps the folder ≤ 64 MiB (deletes oldest down to ¾, removes stray `.tmp`). Unit tests:
`copy_is_made_once_and_reused`, `sweep_keeps_the_newest_under_the_limit`.
Also the in-memory cover map (`App::cover_cache`) was unbounded — every browsed album added 57–230 KB of
RGBA pixels forever (≈150 MB after browsing 2700 albums). Now capped at 300 entries, FIFO
(`COVERS_IN_MEMORY`, `cover_order`); evicted covers come back from the disk copy in ~2 ms.
Measured (release, Wayland, real music: artist with 22 albums, 1500×1500 covers; `covers_bench.py` opens the
artist and reads per-cover timings from `NYANOTO_TRACE`):
| | cold (no cache yet) | warm (cached) |
|---|---|---|
| all 22 covers shown after | 856 ms | 24 ms |
| work per cover | 76.2 ms | 2.1 ms |
| peak RSS | 113.5 MB | 52.0 MB |
| RSS after | 52.1 MB | 52.0 MB |
| disk | 22 files, 470 KB (~21 KB/album) | same |
Extrapolation: 64 MiB cap ≈ 3000 albums of real covers. The trace line `cover N µs` in `covers.rs` is part
of the temporary `NYANOTO_TRACE` instrumentation.

## 2026-10-17 (evening) — owner's answers, cover cache settings, lighter debug builds

Owner's answers to the three questions:
1. Cover cache: keep it, but as a setting «Кэш обложек» (on by default) with sub-settings «Сохранять кэш на
   диск» (default yes) and «Качество кэша» high/medium/minimal (default medium), plus a dialog that cannot be
   skipped, like the authenticity-check one. (The owner thought the cache disappears on restart — it does
   not: the disk copies survive, which is what the "warm" measurement above was.)
2. Exotic formats: "follow the constitution" — the choice is ours, by quality → support → speed → load
   (decision and reasoning in the formats section below).
3. Disk: delete the debug builds and make them smaller — done.

**Debug builds.** `cargo clean --profile dev` freed 23 GiB. Workspace `Cargo.toml`: `[profile.dev]
debug = "line-tables-only"` (backtraces keep file:line), `[profile.dev.package."*"] debug = false`.
A full `cargo test --workspace --no-run` from scratch now takes 2.4 GB (was 21 GB) and 14m46s.

**Cover cache settings** (`nyanoto-app/src/covers.rs`, `main.rs`, `ui/app.slint`, `lang.rs`):
- `covers::Quality` High/Medium/Minimal = copy side 480/240/128 px, JPEG 92/90/80, folder cap
  256/64/16 MiB (≈3000 albums each). Measured on the 34 real covers in ~/Документы/music (debug build):
  80.3 / 21.3 / 5.3 KB per album; making a copy 145/122/113 ms (decoding the 1–5 MB original dominates);
  reusing it 4.2/0.6/0.2 ms.
- Copy names are `{h-|m-|s-}{fnv(path|len|mtime|side|jpeg)}.jpg`. `Loader::new(folder)` starts with disk
  copies off; `Loader::set_disk(Option<Quality>)` (an `AtomicU8` read by the workers per job) also spawns a
  quiet sweep that deletes `.tmp`, copies of other levels (and the unprefixed copies of the first
  experiment), or all copies when `None`, then trims to the level's cap. It only touches `.jpg`/`.tmp`.
- A request bigger than the copy (e.g. the 240 px header with «минимальное») decodes the original; no copy.
- App: settings `cover_cache`, `cover_disk`, `cover_quality` («0» = off; absent = on / medium).
  `apply_covers()` → `covers.set_disk((cache && disk).then(quality))`; with the cache off the memory map is
  cleared and `remember_cover` stores nothing (every show decodes the original). Changing the quality
  forgets the memory map so the next view uses the new copies.
- The warning dialog became shared: `warning-kind` 0 = authenticity, 1 = covers (`enum Warning`,
  `open_warning()`); the cover cache turns off at once and turns on only through the dialog, whose «Включить»
  unlocks after 5 s — the same rule as the authenticity check.
- Checked live (Xwayland + AT-SPI, fresh data dir): off writes `cover_cache=0` at once; pressing «Включить (4)»
  early does nothing; after the countdown it writes `1`; «Сохранять кэш на диск» writes 0/1; «Качество кэша»
  is greyed out when the disk copies are off. Screenshots looked right. Note: AT-SPI does not report the
  CHECKED state of Slint `Switch` (always false) — see ISSUES.md.
- Tests: `copy_is_made_once_and_reused`, `each_level_has_its_own_copy_and_big_covers_come_from_the_original`,
  `sweep_keeps_the_newest_copies_of_the_current_level`, `quality_codes_round_trip`.
- Temporary files of this work live in `target/scratch/` (UI script `tools/covers_ui.py`, screenshots,
  downloaded codec sources) — removed by `cargo clean`.

### Exotic formats: WavPack, Monkey's Audio, Musepack, TTA, DSD (owner: "follow the constitution")

**Decision** (quality → support → speed → load). For each format the decoder that reads *all* of its variants
bit-exactly wins:
- WavPack → reference **libwavpack 5.9.0** (only it reconstructs hybrid files with their `.wvc` correction file;
  also floats, 1–32 bit, multichannel, DSD inside, legacy < 4.0 files).
- Monkey's Audio → reference **SDK 13.27** (all levels incl. Insane, 8–32 bit int, float ("MACF"), up to
  32 channels; FFmpeg's decoder lacks some of these). 13.30 was published the day before — took the newest
  version older than two weeks.
- Musepack → reference **libmpcdec r495** (the Debian/Ubuntu snapshot; SV7 + SV8, exact true-gapless length).
- TTA → **own Rust decoder**: the format is tiny, verifiable bit-exactly against an independent encoder, and
  the reference libtta is LGPL.
- DSD (DSF, DFF, DSD-in-WavPack) → **own DSD→PCM converter** (the point of DSD support is the conversion).
- Not done: WMA/AC3/DTS/TAK/DST/ATRAC — no open reference libraries; proposed as an optional FFmpeg module
  (IDEAS.md, awaiting the owner).
All vendored code is permissive (BSD-3); the project still has no license chosen, so nothing restricts it.
Sources downloaded to `target/scratch/dl/`; checksums in each `vendor/*/VERSION.txt`.

**Architecture.** `nyanoto-core/src/native/` — formats that read the file themselves. `native::open()` sniffs
the first bytes (skipping an ID3v2 tag): `TTA1`, `wvpk`, `DSD `, `FRM8`, `MPCK`/`MP+`, `MAC `/`MACF`; otherwise
Symphonia. `TrackDecoder` became a wrapper over `Inner::{Symphonia, Native}` (the old struct is
`SymphoniaDecoder`), so playback, peaks, inspection, health check and conversion all get the new formats.
`native::Stream` = `read/seek/decode_errors/finalize`; `NativeDecoder` cuts output into ≤4096-frame chunks and
reports `missing` when a file ends before its header says. The C libraries read through Rust callbacks
(BufReader<File>), never `fopen` — same on Windows (any path characters) and later Android.
`native::tags`: APEv2 (end of file, before ID3v1) + ID3v2 via `symphonia-metadata`; APE keys mapped to
StandardTagKey; ReplayGain for all native formats comes from there.
`nyanoto_core::probe(path)` gives format/rate/bits/channels/duration/tags without decoding; `nyanoto-library`
now depends on `nyanoto-core` and uses it when neither lofty nor Symphonia knows the file (TTA, DSF, DFF), and to
detect DSD inside WavPack (lofty reports it as "8 bit, 352.8 kHz"). Library format names: `tta` → "TTA",
`dsd` → "DSD64/128/256/512" (bit depth and MHz rate hidden). Schema 13 forces a re-read of `wavpack`
rows with bit_depth 8. **Scanner change:** files whose last read failed (`problem`) are re-read on every scan
(test extended), so new formats are picked up without touching the files.

**Per format — details and findings.**
- TTA (`native/tta.rs`): header CRC, seek table (CRC mismatch → counted, table still used), per-frame CRC →
  damaged frame becomes silence, cut file → stops and reports `missing`. Encrypted (format 2) → Unsupported.
  *Bug found only on real music:* the LSB-first bit reader shifted a u64 by 64 when a unary run of 63 ones
  filled the whole cache (debug panic, wrong data in release). Fixed with `checked_shr`, unit test added.
  Verified bit-exact: three fixtures (8/16/24 bit, 1/2/3 ch) made by FFmpeg 7.1.5's TTA encoder, and three real
  tracks (16/44.1, 24/48, 24/96) converted FLAC → TTA by FFmpeg.
- WavPack (`native/wavpack.rs`): `OPEN_WVC` when `name.wvc` exists, `OPEN_DSD_NATIVE`, no OPEN_NORMALIZE
  (floats scaled by 2^(127−normexp) ourselves, so MD5 sees raw bits). MD5 computed like `wvunpack -m` over
  WAV-layout bytes (8-bit unsigned), only for files read straight through. Failed seek kills a libwavpack
  context → reopen and retry. *Library quirk found:* stereo without a channel mask is stored as two mono
  streams; a damaged second-channel block is muted but **not counted** in `WavpackGetNumErrors`. When `verify`
  is set we therefore also scan all blocks with `WavpackVerifySingleBlock` (what `wvunpack -vv` does);
  decode_errors = max(library count, scan). Verified: system `wavpack` 5.8.1 (default, `-hh -x6`, hybrid
  `-b3 -c`) on two real tracks → bit-exact vs FLAC; DSD WavPack made by `wavpack` from a DSF → same PCM as
  the DSF bit for bit.
- Musepack (`native/musepack.rs`): `mpc_reader` callbacks; the library drops synth delay + beg_silence and
  trims by true-gapless itself → exact length. Patches/own files (in VERSION.txt): `minimax.h` replaced
  (the original is LGPL; two macros used), `common/crc32.c` replaced by a table-less `nyanoto_crc32.c`, and
  `mpc_decoder_init` no longer calls `huff_init_lut` (its own FIXME; shared tables rewritten on every open =
  data race with playback + waveform threads) — we call it once via `Once`. FFmpeg's decoder, by contrast,
  outputs the stream 481 frames late (synth delay not removed) and 666 frames too long. Our SNR on pure tones
  equals FFmpeg's (SV8 62/27 dB, SV7 63/38 dB — the codec hides noise under the tone). Third-party samples
  (samples.ffmpeg.org, SV7 and SV8 with tags/chapters) decode; vs FFmpeg after aligning 481 frames: 70–81 dB,
  Choral.mpc 38 dB (likely PNS — random noise substitution differs between decoders).
- Monkey's Audio (`native/ape.rs` + `native/ape.cpp`): C++ shim with a callback `IAPEIO`; frame CRC errors are
  reported (SDK outputs silence for the frame); `nyanoto_ape_md5` repeats the SDK's quick verify (MD5 of WAV
  header + frames + APE header + seek table) on the open decoder. Channel mask parsed from the stored WAV
  header (WAVE_FORMAT_EXTENSIBLE). SIMD filters (SSE2/SSE4.1/AVX2/AVX512, NEON) compiled per file with their
  flags like the SDK's CMake; the SDK picks one at run time. Note: the SDK decodes in its own worker thread
  and calls our read callbacks from it (we don't touch the file meanwhile). Float files start with `MACF` —
  first test run found that sniffing only `MAC ` missed them. Verified: official `mac` 13.27 console tool built
  in scratch, Extra High and Insane on two real tracks → bit-exact vs FLAC; damaged copy → "ПОВРЕЖДЁН".
- DSD (`native/dsd.rs`): DSF (4096-byte blocks per channel, LSB-first, channel type → mask), DFF (big-endian
  chunks, CHNL order mapped to mask order with a permutation; DST → Unsupported), DSD in WavPack.
  Converter: stage 1 = 95-tap Kaiser FIR (+1 zero tap) applied per byte through 12 tables × 256 entries,
  ÷8; stage 2 = Kaiser FIR, ÷4/8/16 to 88.2 kHz (DSD64) or 176.4 kHz (DSD128+; 48 kHz family → 96/192);
  passband flat to 35 kHz (DSD64) / 50 kHz, stop-band ≈130 dB, no gain (DSD full scale = PCM full scale,
  SACD peaks at +3.1 dB over 50 % don't clip). Delay is compensated *exactly*: stage-1 delay is 5 bytes,
  stage-2 length chosen so the total is a whole number of frames — first test run showed a 0.73-frame phase
  error from counting the delay from the wrong reference point; fixed. Silence (0x69) primes and flushes the
  filters; seeking reads a warm-up so a sought chunk equals the same chunk played through, bit for bit.
  Tests: DSF = DFF = WavPack-DSD bit-exact; level error < 0.01 dB, phase error < 0.05 frame, in-band noise
  below −75 dBFS (set by the test's 2nd-order modulator); DSD silence stays < −120 dB; DSD128 → 176.4 kHz.

**Tests added:** `tests/formats.rs` (TTA + manual tools `same_audio` and `dump_audio`), `tests/wavpack.rs`,
`tests/dsd.rs`, `tests/musepack.rs`, `tests/ape.rs`, shared `tests/common/mod.rs` (libwavpack encoder FFI,
2nd-order sigma-delta modulator, DSF/DFF writers), unit tests in `native/`. Every format has a "mutated file
never panics" test. Fixtures: `tests/fixtures/*.tta` (91 KB), `mpc_sv7.mpc`, `mpc_sv8.mpc` (33 KB).
Tools in scratch: `target/scratch/build/musepack/{mpcenc,mppenc}`, `target/scratch/build/mac/mac`,
`target/scratch/tools/{compare,align,make_tagged}.py`.
Scanner check (`nyanoto-cli scan --db …` on a folder with DSF+ID3v2, DFF+ID3, TTA+APEv2, APE, WavPack+wvc,
Musepack): 6 found, 0 problems, tags read.

**Speed** (release `nyanoto-cli check`, one file at a time, this laptop):
| file | audio | decode speed | CPU |
|---|---|---|---|
| FLAC 24/96 (reference) | 3:38 | 226× real time | 0.95 s |
| TTA 24/96 | 3:38 | 137× | 1.55 s |
| WavPack 24/48 `-hh -x6` | 1:46 | 109× | 0.96 s |
| APE 24/48 Extra High | 1:46 | 74× | 1.41 s |
| APE 24/48 Insane | 1:46 | 41× | 2.55 s |
| Musepack SV7 | 3:45 | 612× | 0.36 s |
| DSD64 → 88.2 kHz (first version) | 1:00 | 19× | 3.09 s |
| DSD128 → 176.4 kHz (first version) | 1:00 | 16× | 3.70 s |
| DSD256 → 176.4 kHz (first version) | 1:00 | 7× | 8.00 s |
DSD was the outlier (≈5 % of a core for DSD64 while playing): stage 2 was a scalar reversed f64 loop.
Optimised without changing the math: stage-1 history as a u128 shift register (no Vec rotate), stage-2 dot
product forward with four independent sums (vectorised). Results after: see below.
After the optimisation (release `nyanoto-cli check`, end to end): DSD64 **49×** (1.20 s CPU per minute, was
19×/3.09 s), DSD128 31× (1.87 s), DSD256 16× (3.69 s). The converter alone (`dsd_bench`, an ignored test in
`native/dsd.rs`): DSD64 17.6 ms, DSD128 26.6 ms, DSD256 51.7 ms per second of audio. Stage 1 is now bound by
its 12 table lookups per byte (shift-vs-array made no difference); further gains would need symmetric folding
or a half-band cascade in stage 2 (≈25 % more) — noted, not needed now.
Whole engine path (`nyanoto-cli play --null`, decode + resample to 48 kHz, as fast as possible; length on the
output equals the headers to the millisecond): CPU per second of audio — DSD64 2.6 %, APE Insane 24/48 2.4 %,
FLAC 16/44.1 0.39 % of one core; peak RSS 9 MB / 40 MB (the APE SDK's Insane buffers) / 21 MB.

**Other changes in this step:** the signal path ("тракт") now shows «DSD → PCM 88,2 кГц» as the first step for
DSD and never claims bit-perfect for it (it would have: lossless + no other steps). `nyanoto_core::dsd_pcm_rate`
exposes the converter's choice. A first version of `Converter::new` decided the 35 kHz band by
`dsd_rate / pcm_rate == 32`, which is also true for DSD128 → caught while writing the signal-path code; now
`passband(dsd_rate)` with a unit test over all rates. Clippy (Rust 1.99) wanted `as_chunks` and
`is_multiple_of` — applied. Workspace: 168 tests pass (+6 ignored manual tools), clippy clean on Linux and on
`x86_64-pc-windows-gnu` (Rust only: C/C++ stubbed by `target/scratch/tools/fakecc`; no MinGW here).

**Disk note.** After a day of rebuilds `target/debug` grew back from 2.4 GB to 8.2 GB, 4.7 GB of it incremental
data of `nyanoto-app` (the Slint-generated code is huge; one incremental dir per build flavour: bin, test,
clippy). `cargo clean -p nyanoto-app -p nyanoto-core -p nyanoto-library -p nyanoto-cli -p nyanoto-testkit` removed
7 GiB of our own crates' artifacts only (dependencies stay; first rebuild ≈2 min). If it becomes a nuisance:
consider `[profile.dev.package.nyanoto-app] incremental = false` (slower UI iteration) — not done.

## 2026-10-18 — owner: warning on turning the cover cache *off*; finish the remaining formats; Windows

**Cover cache warning moved** (owner: "предупреждение нужно сделать на выключение"): turning the cache on is
immediate; turning it off opens the shared warning (`warning-kind` 1) — «Выключить кэш обложек?», what
changes, the 0.86 s/113 MB vs 0.02 s/52 MB example, what gets deleted, and the advice to switch off only
«Сохранять кэш на диск» instead. The confirm button reads «Выключить (5)» (`disable` string). The owner looked
at a debug build launched with their own (fresh) data dir; demo folder `target/scratch/demo`.

**FFmpeg module** — the remaining formats (WMA incl. Pro/Lossless/Voice, AC3, E-AC3, DTS incl. DTS-HD MA,
TrueHD/MLP, TAK, DST, ATRAC, RealAudio, Shorten, ALS, AMR, Speex, ADPCM/PCM variants, and codecs inside known
containers that Symphonia can't decode). By the constitution there is no alternative: no open reference
libraries, and Windows Media Foundation is Windows-only.
- `src/native/ffmpeg.c` (shim, compiled against vendored FFmpeg 7.1.5 public headers `vendor/ffmpeg`, LGPL)
  loads `libavutil.so.59/libavcodec.so.61/libavformat.so.61` (Windows `avutil-59.dll`…, Android `libavutil.so`…)
  at run time with dlopen/LoadLibraryExW, only when a file needs them, and refuses another major version
  (struct layouts). Custom AVIO over Rust callbacks; output converted to interleaved f32 for every sample
  format; exact seeking (av_seek_frame backward + flush + discard by best_effort_timestamp); metadata → tags.
  Search order: `NYANOTO_FFMPEG_DIR`, `<exe>/ffmpeg`, `<exe>`, `<exe>/../ffmpeg` (tests), system.
- `src/native/ffmpeg.rs`: `set_enabled`, `status()` (Disabled/NotLoaded/Loaded{version, place}/Missing), open and
  describe (for the library). Lengths from FFmpeg are estimates, so `missing` is not reported for them.
- `TrackDecoder::open`: native → Symphonia → **FFmpeg as a fallback** (on any open error except a real I/O
  error), the first error is returned if FFmpeg fails too. DST in DFF (our parser says Unsupported) also goes
  to FFmpeg.
- **Bug found by the first FFmpeg test:** a raw `.dts` file was opened by *Symphonia as MP3* (its MP3 probe found a
  false frame sync in DTS data) and "played" 10 s of garbage for a 6 s file with 1107 errors. Fix: `native::sniff`
  recognises FFmpeg-only formats by signature first (ASF GUID, AC3/E-AC3 0x0B77 both byte orders, all four DTS
  sync words, `DTSHDHDR`, TrueHD/MLP major sync at offset 4, `tBaK`, `ajkg`, `ea3`, `.ra\xfd`, `.RMF`) — unit test.
- `crates/nyanoto-core/ffmpeg/build.sh <src> <prefix> linux|windows|android-arm64`: decoders/demuxers/parsers list,
  no programs/network/video/filters/swresample, shared libs, LGPL, `--disable-x86asm` when nasm is absent.
  FFmpeg 7.1.5 tarball verified with the FFmpeg release key (FCF9 86EA … D676 58D8). Sizes: Linux 4.2 MB,
  Windows 4.1 MB (imports only UCRT/kernel32/bcrypt), Android arm64 3.9 MB. Built: `target/ffmpeg/<target>/`;
  copied to `target/{debug,release}/ffmpeg/` for development (`cargo clean` removes them — rerun build.sh).
- Settings: «Редкие форматы через FFmpeg» (Library section, on by default — nothing is loaded until needed) with a
  status line (loaded version and folder / not found / off). Setting `ffmpeg` («0» = off).
- Library: `Probe.format` is now a String (FFmpeg codec names: wmalossless, eac3, dts-hd-ma, truehd…) with
  display names and lossless flags in `quality()`; scanner extensions added (eac3, ec3, thd, mlp, dtshd, tak, shn,
  oma, aa3, at3, ra, rka, w64, au).
- Verified: own test files (system ffmpeg encoders) — MLP and TrueHD bit-exact, AC3/DTS/WMA play (length differs by
  codec frame padding, same as the FFmpeg CLI). samples.ffmpeg.org "luckynight" set vs its reference WAV: WMA
  Lossless, Shorten, MPEG-4 ALS, ALAC, APE, TTA, WavPack **bit-exact**; MLP bit-exact except the format's end
  padding; RKA — FFmpeg itself fails on the last block (identical to the FFmpeg CLI). TAK 2.2, E-AC3 (raw and in
  MP4), DTS 5.1, DTS-HD MA (m2ts), AC3 5.1, WMA Pro 5.1 — open and decode without errors. No DST sample found.
- Tests: `tests/ffmpeg.rs` (MLP bit-exact + exact seeking, AC3/WMA levels; skipped with a message if FFmpeg is
  absent), fixtures `ff.mlp/ff.ac3/ff.wma` (88 KB). `dsd.rs`: DST test now asserts "not decoded as plain DSD".

**Windows and Android, real compilers now.**
- Windows: portable llvm-mingw 20260922 (sha256 verified) in `target/scratch/toolchains`, Rust target
  `x86_64-pc-windows-gnullvm`. Env: `CC_/CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang(++)`,
  `AR_…=llvm-ar`, `CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang`. `nyanoto.exe` and
  `nyanoto-cli.exe` build and link with every vendored C/C++ library. Found and fixed: the Monkey's Audio SDK
  includes `<Windows.h>` — MinGW headers are lowercase, so build.rs writes an `OUT_DIR/ape-compat/Windows.h`
  shim **only when the host isn't Windows** (on Windows it would include itself); the FFmpeg loader used ANSI
  `LoadLibraryExA` — paths with Cyrillic user names would fail — now UTF-8 → `LoadLibraryExW`. The llvm-mingw
  executables import `libc++.dll` and `libunwind.dll` (ship them or link statically; an MSVC build won't need
  them). Clippy clean on this target with real compilers. Not run on Windows (no Wine/Windows here).
- Android: NDK r30 (sha1 verified), `aarch64-linux-android`, API 26 clang: nyanoto-core, nyanoto-library and
  nyanoto-cli build with all libraries (Monkey's Audio with NEON); FFmpeg built for arm64. The window (Slint
  desktop backend) is not built for Android — a separate Android frontend is planned.
- Linux: 171 tests pass (+6 ignored manual tools), clippy clean.
- Format table for people: `docs/FORMATS.md`.

## 2026-10-18 (2) — «О приложении», split screen, listening map

**Owner's requests:** an «О приложении» item first in Settings with the app and library versions; auto-update
("a library got updated → update it from the app") — owner's decision after the trade-offs (no public release
location yet, only FFmpeg is separately updatable, strict-offline principle): **postpone to future releases**
(IDEAS.md). Version **0.1.0-pre-alpha**. Split screen did not work on Linux. Statistics: an Obsidian-like map.

**Version and «О приложении».** Workspace version `0.1.0-pre-alpha`. Settings start with: «nyanoto <version>», build
line (profile · target · rustc, from `nyanoto-app/build.rs`), data folder, «Интерфейс сделан на Slint» (the Slint
Royalty-free 2.0 license requires the attribution in the About screen — now satisfied) and a collapsible
«Компоненты и лицензии»: native libraries report their own versions at run time (`nyanoto_core::components()`:
`opus_get_version_string`, `WavpackGetLibraryVersionString`, `nyanoto_ape_version` from the SDK's Version.h,
libmpcdec r495, Signalsmith 1.3.1, FFmpeg from its loader status), Rust crates come from Cargo.lock parsed in
`nyanoto-app/build.rs` (`NYANOTO_DEPENDENCIES`), plus SQLite (`nyanoto_library::sqlite_version()`). Found: libopus
said "unknown" — `PACKAGE_VERSION` is normally set by its configure; `build_opus()` now defines "1.6.1".

**Split screen on Linux.** The window had `min-width: 900px`. The laptop is 2256×1504 at 133 % (Mutter
`GetCurrentState`) = 1692 logical px, half = 846 < 900, so GNOME refused to tile it. Now `min-width: 560px` (half
of a 1366-wide or 1280-wide screen also fits). The layout had to learn to shrink: the search field (fixed 320 px →
min 120 / preferred 320) made the top bar ~800 px wide, which pushed every page wider than the window (that's
also why long album titles didn't elide); settings combo boxes fixed 220/240 → min 140 / preferred 220/240
(the «Чувствительность» row was >600 px). Checked by screenshots at 560 px (music, album, settings, statistics):
everything fits. Mutter's tiling itself wasn't triggered from here (no input injection), only the size hints
(`WM_NORMAL_HINTS` min 1120×920 physical = 560×460 logical).

**Listening map** (`nyanoto-app/src/graph.rs`, statistics page, tabs «Карта» / «Списки»):
- Data: `Library::listen_graph(grouping, 30, 10)` — plays grouped by the browse grouping's owner and album
  (`Grouping::album()` made `pub(crate)`), top 30 artists, top 10 albums each.
- Model (d3-force style): node area ∝ plays (artists r = 3.5–10 % of the canvas' smaller side, albums 1.2–5 %);
  springs artist↔album (the album, being lighter, moves more); short-range repulsion ∝ ((r₁+r₂+30)/d)² plus
  collision push; gravity to the centre (artists 0.08, albums 0.006) so unconnected artists stay together;
  velocity decay 0.4; alpha cools 1 → 0.002 (~300 steps) and the animation stops. Dragging is elastic:
  displacement from the grab point is `reach·tanh(d/reach)`, reach = ¼ of the smaller canvas side; on release
  the model re-heats (alpha 0.8) so the node returns to the cluster. Grab/drag pass absolute canvas
  coordinates (with an elastic node, relative deltas would accumulate error). A press that moved < 5 px is a click:
  artist → its albums page, album → its track list.
- Rendering: one Slint `Path` with all links as SVG commands + a `Rectangle` per node; artist names in a separate
  layer above all circles (albums used to cover them); album names on hover with play counts. Hues per artist by
  the golden angle.
- Energy: 30 frames/s with two model steps per frame (the cost is repainting, not physics); the timer stops when
  the graph has settled or the map isn't visible. Release, scratch library with 600 synthetic plays (3 artists,
  33 albums): opening the map costs ~0.9 s CPU once, then **0.00 s** over 10 s idle.
- Tests: settles inside the canvas without overlaps; albums stay near their artist; artist size order follows
  plays; dragging pulls the albums, is elastic (60–200 px for a 200 px pull) and the node comes back near the
  centre; HSL colours. Library test for `listen_graph`.
- Not checked: dragging with a real mouse (AT-SPI can't drag; xdotool is not installed).
- 175 tests pass (+6 ignored manual tools); clippy clean on Linux and `x86_64-pc-windows-gnullvm`.

## 2026-10-18 (3) — covers and avatars on the listening map, artist avatars

User request: the map must show the real album covers (square) and artist avatars (round), and the user must be
able to set artist avatars.

- **Library.** `GraphArtist.albums` is now `Vec<GraphAlbum { title, plays, folder }>`; `listen_graph` also takes
  `MIN(r.path || '/' || t.rel_path)` per album and runs it through `album_folder` (made `pub(crate)`), same as the
  album list. `GraphArtist.folder` = `artist_folder(name, album_folder)` — new public helper: the nearest
  ancestor of the album folder named like the artist (case-insensitive); always found when browsing by folders,
  found by tags when the music is laid out by artist.
- **Avatars** (`nyanoto-app/src/avatars.rs`). Stored in `<data>/avatars/<fnv64(lowercase trimmed name)>.jpg|png`
  — no DB table: one avatar per artist across groupings (folders/tags names differ mostly by case), survives
  the original picture being moved. `make()` decodes by content (`with_guessed_format`, downloaded pictures lie
  about extensions), crops a square (portraits: from the upper quarter, faces are usually there; otherwise the
  centre), downsizes to 512 px with Lanczos3, writes PNG if any pixel is transparent else JPEG q90 (temp file +
  rename; the file with the other extension is removed). `Avatars::open` scans the folder once at start-up, so
  checking an artist is a hash-map lookup (no disk access per list row). Picking: «Выбрать аватар…» on the artist
  page via the existing async `pick()` (jpg/jpeg/png/webp); the work runs in a thread, then `avatar_changed`
  forgets all round pictures in memory and refreshes the artist header, matching rows of the artist list and
  search results, and the artist node on the map. «Убрать аватар» deletes the file.
- **image crate**: `webp` feature added (pure Rust `image-webp`) — artist photos from the web are often WebP;
  `cover.webp`/`folder.webp`/`front.webp` are now also album covers.
- **Loader** (`covers.rs`): jobs carry `Source::{Album(folder), Artist { own, folder }}` and
  `Shape::{Fit, Square, Round}`. Artist source: own avatar, else `artist|folder.(jpg|png|webp)` in the artist
  folder (Kodi/foobar2000/MusicBee convention). Square = centre crop + `resize_exact` (Triangle); Round = square
  with an anti-aliased circular alpha mask done in Rust (doesn't depend on how the renderer clips rounded
  rectangles). Own avatars skip the disk copies (already small); everything else uses them as before.
  Memory cache key became `(path, size, shape)`.
- **Map** (`GraphNode` + `image`, `letter`): each node gets a picture of exactly its on-screen size
  (`picture_side`: physical px rounded up to 8, ≤ 512) so the renderer doesn't scale; artists without a picture
  are coloured circles with the initial, albums without a cover — coloured squares. `graph_view` generation
  drops late pictures of a previous map. Hover labels got a background (they can lie over covers). Hint text
  updated (circles = artists, squares = albums, avatar is changed on the artist page).
- **Artist page** got a header: 96 px round avatar (or initial), «N альбомов · M треков», buttons. Artist list
  rows show the user's avatars (folder photos only on the page and the map — see ISSUES).
- Checked with screenshots (scratch library, two synthetic avatars): list, artist header, map with 33 real
  covers, removal → initial everywhere. The first map opening in a debug build missed one cover (3000×3000
  original decoded slower than the screenshot delay; the disk copy appeared a moment later) — not a bug.
  Release: opening the map ~1.2 s CPU once (was ~0.9 s without pictures), idle 0.01 s / 10 s.
- Tests: avatars (portrait crop from the top, case-insensitive key, reopen, remove, PNG kept for transparency,
  no upscaling of small pictures, broken path → error), loader shapes (square from the centre, transparent
  circle corners, own avatar preferred over folder photo), `artist_folder`, `listen_graph` folders.
  179 tests pass (+6 ignored); clippy clean on Linux and `x86_64-pc-windows-gnullvm`. Release rebuilt.

## 2026-10-18 (4) — hotkeys, MPRIS/SMTC, queue, several music folders, bug fixes

User request: hotkeys, MPRIS, a YouTube-like queue ("add to queue" as a temporary playlist), support for several
folders, fix bugs. Done in this order; product details decided by the constitution and listed here.

- **Hotkeys** (`app.slint` root `FocusScope` → `callback hotkey(text, ctrl, shift) -> bool`, `App::hotkey`).
  They live in the bubbling `key-pressed` of the root scope, so text fields keep Space/arrows; std `Button`'s
  FocusScope is 0 px wide (does not take focus on click), so Space is not stolen by the last clicked button.
  Space play/pause; ←/→ seek 5 s (Shift 30 s); Ctrl+←/→ prev/next; ↑/↓ volume ±5 %; M mute (remembers volume);
  L like; Q queue page; Ctrl+F or `/` focuses search (`search-box.focus()`); Esc now also returns focus to
  the root scope. Letters accept the Cyrillic key of the same physical key (м→ь, l→д, q→й); `keys.rs` maps
  Ctrl+F like Ctrl+C/V. Media keys (`NamedKey::MediaPlayPause…`) are caught in the winit hook (Slint drops
  them) and ignored when system media controls are active (otherwise Windows would toggle twice).
  Not pressed live: AT-SPI cannot inject keys into an unfocused window on Wayland.
- **System media controls** (`nyanoto-app/src/system_media.rs`, setting `system_media`, on by default).
  Linux/BSD: MPRIS via zbus 5.19 blocking API (already in the build through accesskit_unix, so ~no size cost).
  A `nyanoto-mpris` thread builds the connection (not on the UI thread), serves `org.mpris.MediaPlayer2` and
  `.Player`, requests `org.mpris.MediaPlayer2.nyanoto` (`.instance<pid>` if taken), and emits
  `PropertiesChanged`/`Seeked` from a channel; the UI only diffs a `Now` snapshot. Position is extrapolated from
  the last update × rate. Metadata: trackid `/org/nyanoto/track/<queue id>`, title, artist (tag → album artist →
  folder), album, length, `mpris:artUrl` (percent-encoded file URL of the folder cover). Windows: SMTC through
  `ISystemMediaTransportControlsInterop::GetForWindow` (HWND from winit's raw handle) with buttons, timeline,
  seek requests, repeat mode, thumbnail; started by a zero-delay timer because the winit window exists only
  once the event loop runs. Verified live on Linux with `gdbus`: properties, Play/Next/Seek/Pause/Quit, metadata
  with cover. Windows: clippy + full link with llvm-mingw, not run.
- **Engine** (`nyanoto-core`): `Engine::cue(paths, index, position)` — replace the queue and remember a track +
  position without opening anything; Play starts there; Next/Previous/Seek while cued only move the cue (emit
  `QueueChanged`); `Engine::move_item(id, index)`; `Engine::current_id()` (playing or cued, via `Status.cued`).
  Test `cued_queue_waits_silently_and_starts_where_it_was` (silence before play, exact first sample, order after
  a move).
- **Queue in the app**: «+» of a track now opens the picker with «Играть следующим» / «В очередь» on top (then
  playlists); album and playlist pages got «В очередь». If nothing plays or waits, adding starts a fresh queue
  *cued* (no sound until Play — the user decides when audio starts). Queue page (`PAGE_QUEUE = 9`, chip
  «Очередь · N» in the bar, Q): played rows dimmed, current highlighted, ▲/▼/× per row, click jumps, «Очистить»
  keeps the current track, «Сохранить как плейлист» reuses the picker. A user-edited queue becomes «список»
  (repeat label) and stops the album chain. Many `QueueChanged` events in one drain re-render once.
- **Queue persistence**: schema 14 `queue_items(position, track_id → tracks ON DELETE CASCADE)` + settings
  `queue_current/position_ms/kind`; saved 2 s after edits/track change/state change and on exit; restored at
  start with `cue` and shown in the bar (`show_cued`). Lost tracks are skipped; if the current one is gone, the
  next one starts from 0. Checked live: quit through MPRIS at 0:31.9 → restart → cued at 0:31.9.
- **Several music folders** (they were already possible, but nested ones duplicated music): `add_root`
  normalizes the path; same folder → same id; inside an existing root → `LibraryError::InsideRoot` (status
  message, nothing added); containing existing roots → one transaction moves their tracks (`root_id`,
  `rel_path` prefixed, FTS `path` updated, `folder_artist/album` recomputed with the migration-3 SQL) and deletes
  the old roots — track ids, likes, playlists, stats survive; non-UTF-8 names → `LibraryError::FolderName`.
  Windows paths compare case-insensitively. `remove_root` (+FTS/track_values cleanup) behind a confirmation
  dialog with the track count; refused while scanning. Settings list each folder with «Убрать». Note: sync keys
  use `rel_path` (falls back to tags), so a merge changes keys for untagged tracks.
- **Bugs fixed**:
  - Opus in WebM/MKA tail: `opus::matroska_end` parses the last Cluster from the file tail (4 MiB window,
    EBML children, SimpleBlock/BlockGroup of the track, DiscardPadding ns) and computes the end on the packet
    timestamp scale (`opus_samples(tc) + opus_packet_get_nb_samples − padding`); `SymphoniaDecoder::opus_keep`
    cuts the last packet. Fixture `tests/fixtures/opus.webm` (FFmpeg libopus, 1 s): exactly 48 000 frames;
    duration within 1 ms (Matroska timestamps are whole ms). Note: FFmpeg's WebM starts at −7 ms; Symphonia's
    and our timestamps are the raw block ones (+7 ms), consistently.
  - HE-AAC: Symphonia 0.5 parses SBR/PS signalling but never applies it (core band only, v2 mono).
    `needs_sbr(params)`: AOT 5/29, backward-compatible 0x2B7 extension with sbrPresent, or core rate ≤ 24 kHz
    (implicit signalling) → `TrackDecoder::open` prefers FFmpeg, falls back to Symphonia without it. No real
    HE-AAC sample (Fedora's fdk-aac-free refuses AOT 5); unit test on hand-made AudioSpecificConfigs.
  - Hybrid WavPack without `.wvc`: libwavpack `MODE_LOSSLESS` → codec `wavpack-lossy`, probe `lossless: false`,
    library format `wavpack-lossy` («WavPack Hybrid», lossy); schema 15 re-reads WavPack files once.
  - Scanner skips `$RECYCLE.BIN`, `RECYCLER`, `System Volume Information` (any OS).
  - Core file errors are shown in the UI language (`rows::track_error`): playback, conversion, health.
  - `NYANOTO_TRACE` kept as a debug journal (env read once).
- 186 tests pass (+6 ignored); clippy clean on Linux and `x86_64-pc-windows-gnullvm`; Windows app links.
  Release rebuilt; idle CPU with MPRIS on: 0.00 s / 10 s.

## 2026-10-18 (5) — track journal, artist/album links, full-screen player

User request (choices made by the user in a questionnaire): journal of what played / what will play as tabs
«Дальше» / «История» on the queue page, history kept between launches (last 500), independent of statistics;
artist and album clickable in the player bar and in the full-screen player (not in list rows); full-screen
player over the whole window with blurred cover background, lyrics, quality, next track, like and «+».

- **Engine**: `Engine::play_now(paths)` — insert after the current item and start the first one (the queue is
  kept). Test `play_now_keeps_the_queue` (paced capture, otherwise the first track ends before the command).
- **History** (schema 16 `history(id, track_id → tracks ON DELETE CASCADE, at)`): `add_history` on every
  `TrackStarted` with a known track; the same track twice in a row is one entry (time updated) — repeat-one does
  not flood it; trimmed to `HISTORY_LIMIT` = 500. `history(limit)` skips missing files; `clear_history`.
  UI: `HistoryLine` with a day heading on the first entry of a day («Сегодня», «Вчера», date) and local time;
  click → `play_now`. Local time: new `clock.rs` — offset from `localtime_r().tm_gmtoff` (unix, via libc, already
  in the tree) or `GetTimeZoneInformation` (Windows), taken once at start; `rows::date` now uses local days
  (it was UTC). Known limitation: entries before a DST switch show ±1 h (ISSUES).
- **Links**: bar line «artist · album» became two `Link`s (`now-owner`, `now-album` from `now_parts`, same
  words as the old line per grouping). `open_now(album_too)`: `album_of(track, grouping)` → owner → the artist
  as the browse list has it (case-insensitive over all spellings) → `open_artist`, then `open_album` by title.
- **Full-screen player** (`full-open`, overlay declared before the dialogs so «+» opens above it): Esc, «Свернуть»
  or a click on the cover closes it; «На весь экран» toggles the OS full screen (`Window::set_fullscreen`; the
  name `full-screen` is taken by Slint's Window property, hence `toggle-fullscreen`). Cover loaded at the size it
  is shown (window height − 330 pt, 256…1024 px); background: new `Shape::Backdrop` — cover squeezed to 24 px,
  upscaled to 320 px with Catmull-Rom, light blur, ×0.42 darkness (first try 40 px was blocky: the software
  renderer scales without smoothing). Lyrics column when the lyrics module is on and the track has lyrics —
  `lyrics_visible()` now covers both the lyrics window and this column (line timer included). Images are dropped
  on close. Q (queue) closes it; focus returns to the root scope (`focus-keys`).
- Checked live (AT-SPI + `gdbus`): history tab with day heading/time, full-screen view (cover, smooth background,
  links, quality, next), artist link → artist page, album link → album page, lyrics column with the current line
  (scratch library, the one `.lrc` in the user's music, queue cued at 0:20 via the DB). CPU while playing
  (release, volume 0): window 0.11 s / 10 s, full-screen with lyrics 0.19 s / 10 s.
- 188 tests pass (+6 ignored); clippy clean on Linux and `x86_64-pc-windows-gnullvm`. Release rebuilt.

## 2026-10-18 (6) — bug and efficiency review

User request: fix bugs like the time-zone shift, analyse the whole app for bugs, make it more efficient.

**Method.** (1) static: `unwrap/expect` outside tests (3, all invariants), string slices on external text (LRC,
CUE — safe: ASCII boundaries / digit-validated; CUE `line[9..]` hardened with `get`), clippy with runtime lints
(`significant_drop_in_scrutinee`, `unchecked_time_subtraction`, `await_holding_refcell_ref`, … — clean), manual
search for RefCell borrows held across callbacks (Rust 2024 keeps `if let` scrutinee temporaries through the
then-block). (2) dynamic: synthetic library `nyanoto-cli gen-library … 200000` (4000 artists, 20 000 albums,
782 MB) scanned in 55 s / 14 MB; new ignored bench `nyanoto-library/tests/bench.rs`
(`NYANOTO_BENCH_DB=… cargo test --release -p nyanoto-library --test bench -- --ignored --nocapture`) timing every
DB call the window makes; startup trace (`NYANOTO_TRACE`), RSS/threads, idle wakeups per thread
(`target/scratch/tools/wakeups.py`, voluntary+involuntary context switches over 20 s).

**Bugs fixed:**
- History times ±1 h across DST: `clock::offset_at(unix)` per moment (`localtime_r` / Windows
  `FileTimeToSystemTime` + `SystemTimeToTzSpecificLocalTime`); `rows::date` uses local days. Tests with
  `TZ=Europe/Berlin` (summer 7200, winter 3600) and `Asia/Kolkata`.
- Folder added during a scan was not scanned until restart (`start_scan` returned early): pending roots are
  queued, the running scan is cancelled (incomplete walk ⇒ nothing marked missing), roots are added in
  `scan_finished`, then a new scan; any `start_scan` during a scan sets `rescan_pending`.
- Memory cache of pictures was capped by count only (300) while full-screen covers are up to 4 MB: pictures
  > 256 px are not kept, total ≤ 24 MB (byte accounting in `cover_bytes`).
- After «Убрать папку» the app still holds those track ids (queue, current track): `save_queue`, `add_history`,
  `record_play`, `set_liked`, `add_to_playlist` inserts now use `SELECT … WHERE EXISTS (tracks)` — no FK errors.
- `show_picture` called `show` while holding the cache borrow (latent BorrowMutError) — borrow released first.
- M (mute) after moving the volume slider restored the old volume; full-screen lyrics column stayed after
  disabling the lyrics module; bar labels didn't follow language/grouping changes (`refresh_now_labels`).

**Efficiency (200k tracks, release):**
- Browse queries filtered on `COALESCE(t.folder_artist, '')` etc., which plain-column indexes can't serve:
  schema 17 expression indexes `(COALESCE(owner…), album…, missing)` per grouping. Albums 49 → 0.2 ms, album
  tracks 51 → 0.1 ms, artists 165 → 58 ms (index-only scan). Side effect found: without `sqlite_stat1` the planner
  then drove history queries from tracks into empty `plays` (90 ms) — `CROSS JOIN` fixes the order.
- Search: relaxed (layout/typo) widening only when exact matches < 50; typo matching without allocations
  (3 rolling rows, reused char buffer, digit-only vocabulary terms skipped for word queries); bm25 ranking
  skipped when a pattern matches ≥ 4×limit rows (counted cheaply with a LIMITed subquery) — the window sorts
  results by path anyway. 375–600 ms → 6–37 ms; first typo search 140 ms (vocabulary load). Artists filter via
  prepared `TextMatcher` 18 → 6 ms. Track search moved to a `nyanoto-search` thread (own connection and
  vocabulary, latest query wins, 100 ms debounce, stale answers dropped by id); test in `searcher.rs`.
- DB size: schema 18 drops `track_values_by_value` (no query used it, 12%) and rebuilds FTS5 as contentless
  with `contentless_delete=1` (SQLite 3.53; search reads only rowid and rank; 14%); VACUUM once after the step.
  319 → 191 MB (0.95 KB/track). Folder merge no longer UPDATEs FTS paths (impossible contentless) — moved tracks
  get `mtime = -1` and the following scan re-reads them (test updated). One-time migration: 7.8 s at 200k.
- Background checks (authenticity, health) fetched each batch with `ORDER BY id LIMIT n` from the start —
  quadratic; now `*_after(last_id, n)` keyset pagination (tests).
- MPRIS no longer clones the queue per update (`Engine::queue_position()`); the queue is rewritten only when it
  changed (`queue_dirty`), otherwise `save_queue_place` stores index + position.
- Measured, fine: startup on 200k — window 42 ms, artists 117 ms, unchanged rescan 1.4 s in background;
  RSS 52 MB, 13 threads; idle: window/engine/our threads 0 wakeups, CPU 0.00 s/20 s. Only zbus's `blocking-N`
  threads wake every 500 ms forever (blocking 1.7 never retires idle threads below its 500-thread limit): one from
  accesskit, one from MPRIS — left as is (ISSUES). Binary 39 MB: 23 MB `.text`, ~5 MB symbols (strip is an open
  idea needing the user's decision — function names in crash reports).
- 190 tests pass (+7 ignored manual tools); clippy clean on Linux and `x86_64-pc-windows-gnullvm`. Release rebuilt.

## 2026-10-18 (7) — GNOME freeze on launch, pause icon

User report: launching the player froze the whole GNOME session; the pause button looked like «stop».

- **Investigation.** Journal: player started 18:18:59, first track 18:19:05 (history table), power key 18:19:13,
  suspend/resume — i.e. ~8 s of frozen desktop right at playback start; no OOM, PSI memory/io/cpu near zero, no
  RT thread (portal refused). Objective probe (`target/scratch/tools/shell_probe.py`): `Properties.Get
  ShellVersion` on org.gnome.Shell every 100 ms measures gnome-shell main-loop latency; baseline ≤ 30 ms. Launching
  a scratch instance and driving Play/Next via MPRIS (`mpris_probe.sh`): stalls of 0.9–1.9 s at launch, Play and
  Next; with the cover removed from metadata (temporary env switch): ≤ 26 ms. Reason, from gnome-shell 49 sources
  (`gresource extract …/libshell-17.so /org/gnome/shell/ui/messageList.js`, `mpris.js`): every
  `g-properties-changed` → `_updateState` → `changed` → `MediaMessage._update` builds a new `Gio.FileIcon` from
  `mpris:artUrl` and the image is decoded on the shell's main thread; our art was the original cover file.
- **Fix.** `covers::system_thumbnail(folder, <data>/mpris)` — 256 px JPEG (~20 KB), name from the original's
  path/size/mtime (shell caches by URI), only the current one kept; made on a background thread on album change,
  MPRIS/SMTC get it with the next update (`system_art`). The MPRIS thread coalesces messages arriving within 60 ms
  into one `PropertiesChanged` (+ last `Seeked`). Probe after: ≤ 86 ms (3 runs).
- Found on the way: a second instance never registered MPRIS — zbus returns `Error::NameTaken` instead of
  `RequestNameReply::Exists`; handled → `org.mpris.MediaPlayer2.nyanoto.instance<pid>`.
- **Pause icon.** Pause/resume itself was verified correct (MPRIS and the window button via AT-SPI, incl. a 20 s
  pause past the 3 s idle-stream close). The glyph «‖» rendered as a thin double line. New `MediaButton` draws
  play/pause/previous/next with `Path` (bar, full-screen player, lyrics editor); accessible names «Играть»,
  «Пауза», «Предыдущий трек», «Следующий трек».
- Cleaned up gpg-agent/scdaemon left from the FFmpeg signature check (`target/scratch/dl/ffmpeg/gnupg`).
- 191 tests pass (+7 ignored); clippy clean on Linux and `x86_64-pc-windows-gnullvm`. Release rebuilt.

## 2026-10-18 (8) — lyrics lead and smooth lyrics, slider steps, safe tests

User request: lyrics lines should appear a bit earlier with a smooth animation; a setting for the lead, default
150 ms.

- **Lead.** Setting `lyrics_lead_ms` (0–1000, default 150; slider under the lyrics module, saved on release).
  Applied in real time: track-time lead = lead × speed (`lyrics_lead_track_ms`) — `show_lyrics` looks up the line
  at `position + lead`, `schedule_line` fires `lead` earlier. The LRC editor keeps exact times (no lead).
- **Smooth.** The lyrics model is built once per text (`lyrics_rows`, dropped in `load_lyrics`) with all lines;
  a line change only flips two `current` flags via `set_row_data`, so elements persist and can animate. New Slint
  components: `LyricLine` (Text; size/colour/opacity animated 300 ms; constant weight — weight can't animate and
  jumped) reports its centre via `centered(y)` on `current`/`y`/`height` changes; `LyricsScroll` (clip) moves the
  whole column so the current line sits in the middle, `animate y 400 ms ease-in-out`. Used by the lyrics window
  (centered) and the full-screen column (left, white). Verified by screenshots, one caught mid-transition.
- **Bug: slider steps.** Slint's Slider `step` defaults to 1 and clicking a slider focuses it; then → (seek hotkey)
  was consumed by the volume slider (0..1) → volume 100% (← → 0%). Steps set: volume 0.05, seek 5 s, lead 10 ms.
- **Bug:** lyrics of a cued (restored) track weren't loaded until it played — `show_cued` calls `load_lyrics`.
- **Test-safety incident (my fault).** A scratch test instance at 18:54 played ~5 s at 100% volume (the scratch DB
  volume had become 1 — likely the slider-step bug via AT-SPI/arrows), and at 19:18 a test sent MPRIS Play/Pause to
  the well-known name `org.mpris.MediaPlayer2.nyanoto`, which at that moment belonged to the user's own player.
  New `target/scratch/tools/safe.py`: every test forces volume 0 in the scratch DB before launch, finds its own
  MPRIS name by `GetConnectionUnixProcessID == pid` and refuses to send commands otherwise, and checks Volume is 0
  before Play. All new UI tests use it.

## 2026-10-18 (9) — Cotodama-style lyrics looks

User request: lyrics like the Cotodama Lyric Speaker (several kinds), selectable in settings.

- **Research** (web): Cotodama publishes no list of modes; its engine (AIST mood/structure analysis, chorus
  detection) picks motion and typeface per song. Described behaviour: ballads/folk — delicate serif or handwritten
  type, words gently float/drift on and off; rock — bold gothic, words slam onto the screen and vanish; electronic /
  up-tempo — chunky words flash on and off to the beat, lines switch quickly; chorus — «powerful» graphics;
  unsynced lyrics — plain scroll; no lyrics — abstract graphics. Sources: Sound & Vision, Mixmag (Canvas),
  Impress Watch (kaden), Digital Trends, retailer copy for the Lyric Speaker Box.
- **Looks** (setting `lyrics_style`: auto / column / calm / punch / beat; «Вид текста» under the lyrics module):
  column — the existing scroll; calm — one line, serif (`serif-family`: Noto Serif, Georgia on Windows), rises from
  below and melts upwards (enter 900 ms after a 250 ms delay, leave 450 ms); punch — weight 900, enters at 1.7× size
  and lands in 150 ms; beat — one uppercase word at a time, pops from 0.55× and bursts out at 1.45×, the whole line
  small underneath, size fitted to the width via `character-count` (no-wrap). Auto: genre tag → beat (electronic,
  hip-hop…), punch (rock, metal, punk, alternative, emo…), else calm. Chorus = a line whose normalised text occurs
  more than once (`Lyrics::chorus`) → 1.25× and accent colour.
- **Mechanics.** `KineticLyrics` holds three `KineticSlot`s whose roles (0 sounding, 1 next, 2 gone) rotate; the
  app only sets text/chorus/role (`kinetic_step`), all motion is declarative `animate` (font-size, opacity, y — the
  software renderer has no transforms, `supports_transformations() == false`). The incoming slot gets its text only
  when it becomes «sounding» (pre-filled text showed as a ghost when fast words left a slot still fading).
- **Word timing** (`Lyrics::word_starts`): enhanced-LRC `<mm:ss.xx>` word stamps are now parsed (`word_stamps`,
  shifted with repeated line stamps and the offset); without stamps the line span (to the next line, ≤ 8 s) is split
  by word length, the last word starting by 85%. `schedule_line` also wakes for the next word in beat look. Lead
  (150 ms) applies to lines and words.
- Verified with screenshots for calm (mid-transition), punch, beat (word fitting, burst); tests for word times,
  stamps, repeated stamps and chorus. Tests run through `target/scratch/tools/safe.py` (volume 0, own MPRIS name).

## 2026-10-18 (10) — Cotodama scenes (rotated text, lines, rings, fill)

User feedback on (9): «beat is cool, but Cotodama also has visualisations — lines, fills, text at 90°, 80°…».

- **Research (images).** Text sources describe no modes, so I looked at frames: the w0w studio project page
  (promo film stills) and press photos (mikeshouts, hifipig, geeknewscentral, stuff.tv, displaydaily, YouTube
  thumbnails `I9dJOGbO4H0`). Catalogue: a justified *block* of words of mixed size (key words big), tilted, sometimes
  standing on edge (90°, «Up And Down»); a *giant word* cropped by the screen edges («PLACE») with thin orbit
  circles, dotted circles and dots; *scattered* words at 0/80/90/180° with a tick ring, hatching and text on a circle
  («YOU'LL … MEDITATION»); *concentric rings* with ticks, hatching and a progress arc labelled «57%»; low-poly
  translucent triangles and a translucent disc; shattered letters (Canvas). Saved under `target/scratch/cotodama/`.
- **Why our own renderer.** Slint's software renderer ignores rotation (`SoftwareRenderer::rotate` is a TODO
  stub, `supports_transformations() == false`), so text at an angle is impossible in .slint. New module
  `src/stage/` draws frames itself and hands them to the window as an image (`StageLyrics`, `stage-frame`):
  - `text.rs` — parley 0.11 lays out a word (shaping, kerning, per-script fallback via fontique system fonts),
    skrifa 0.44 gives glyph outlines → tiny-skia paths at 1 em. Faces: bundled **Oswald** variable (OFL 1.1,
    Cyrillic, 172 KB, `fonts/Oswald.ttf` + `Oswald-OFL.txt`, listed in «About») for punch/beat — the user's system
    has no condensed grotesque; Georgia / Noto Serif for calm. parley/skrifa/tiny-skia were already linked by Slint
    (same versions, features unified): the binary grew ~0.6 MB, mostly the font and our code.
  - `paint.rs` — arcs (cubic, both directions), ring sectors, ticks, dotted circles, hatching via a 16 px stripe
    tile as a `Pattern` shader, text on a circle (glyphs merged into one path), masks for inverted text.
  - `scenes.rs` — four layouts per line (`Shot`) and per-part backdrops (`Part`): Block (binary search of the font
    size so the justified block fits, each block row stretched to the full width ≤1.8×, 28% stand on edge at −90°),
    Orbits (longest key word giant, at most 1.3× the width so it stays readable; letters rise one by one; other
    words in rows above), Scatter (4×3 grid, angles 0/±90/180/±5–12/±78–86, rotated ink box kept inside the frame
    and free of other words, words fly in with streaks and linger faded until the next line ends), Rings (current
    word in the centre, the line on an arc, accent progress arc with percent, waves on each word). Chorus parts get a
    band (or a disc for rings) that wipes in; words under it are redrawn in the ground colour through a mask.
  - `mod.rs` — plan: lines → parts (≤6/4/3 lines for calm/punch/beat, a new part at chorus change or a gap > 5 s);
    «mixed» picks a kind per part by mood with chorus preferring stronger kinds, never the same kind twice in a row;
    a random salt per track load (the speaker is different every time). `Clock` smooths the engine position (it
    moves in audio-buffer steps): system clock + upper envelope of the steps, snap on seeks > 0.3 s.
- **Settings.** `lyrics_style` gains scenes / block / orbits / scatter / rings («Сцены — вперемешку», «Сцена: …»);
  mood (font and pace) still follows the genre (calm/punch/beat). `LOOK_STAGE = 4` shows `StageLyrics` in the
  lyrics window and the full-screen column; it reports its size (`stage-resized(place, w, h)`).
- **Cost.** tiny-skia AA is not cheap (half-screen translucent triangle 1.9 ms, 1.3 px circle 0.6 ms, seven 60 px
  words 1.5 ms at 720×460; zeno with own blending was no faster). Measures: 1-physical-pixel lines (tiny-skia
  hairline path), a cached still layer per part (triangles, discs, static circles, progress arc keyed by percent;
  copied with memcpy when fully visible), double pixel buffers, and pacing by motion windows: 60 fps only during
  sharp motion (word pop, streaks, line exit, cover wipe), 30 for soft fades and orbit drawing, 15 for ambient
  rotation, 1 s idle checks without a backdrop; paused — one frame, not redrawn while nothing changes; hidden or
  minimised — the timer stops and buffers are freed. Result on i5-7200U, release, Wayland ×1.33: mixed scenes
  31–34% of one core while playing (column look: 8–9%), half of it our drawing (~5 ms/frame at 684×600 px, ~30
  frames/s), half Slint showing the image; X11 ×2 (2200×1440 window): 42–46%. Pause: 0%. RSS ~71 MB.
  `NYANOTO_TRACE` logs «сцена: 120 кадров, рисование N мс» every 120 frames.
- Verified: 4 stage tests (parts/chorus/gaps, every kind draws and nothing before the first line, clock), font test,
  ignored `stage_frames` (PNG frames + per-frame cost) and `paint_costs`; live screenshots of all kinds in the
  lyrics window and full-screen (`target/scratch/tools/stage_ui.py`, `stage_cpu.py`, volume 0, own MPRIS name);
  196 tests pass; clippy clean for Linux and Windows (gnullvm).

## 2026-10-19 (11) — Scenes rebuilt: mood from audio, 15 clean styles, always in motion

User feedback on (10): scenes must follow the song's own mood (calm parts calmer, aggressive parts harsher), there
are many more Cotodama modes, mine looked static and repeated one pattern, styles were mixed into one; the main
missing style is the «web» (word at −30…30°, a line to the next word, like a web); add a TCP/UDP «internet» style.
Questionnaire answers: two auto modes — one style per song chosen by its mood, and a style that changes with the
song's current state, both «cleaner», not tied to genre; web — all three variants (camera follows / on-screen /
constellation) as separate styles; network — all three (packets / terminal log / both); add flight, pinwheel 90°,
ink & shards, bubbles.

- **More reference frames.** YouTube storyboards (sprite sheets from the watch page's `playerStoryboardSpecRenderer`,
  tool `target/scratch/tools/storyboard.py`): promo `I9dJOGbO4H0` (drifting tilted blocks, the «Up And Down /
  Hang / With The Groove» pinwheel, serif text on a wavy line, particles), concept film «Never Catch Me»
  `HGuppwaUMsM` (camera flying through words in perspective with echo trails, 3D ribbons), Lyric Speaker Caravan demo
  `8lFbntx2tMM` (minimal: word inside a thin circle, «•••» triplets, overlapping circles), «Mirage» `rL8qzwuPYlo`
  (ink blobs, brush strokes, serif).
- **Mood from audio** — `nyanoto_core::feel` (new `feel.rs` in core): whole file decoded, mono, Hann window ≈23 ms
  (power of two by rate), hop ½; per window RMS, SuperFlux-style spectral flux (log magnitude, previous frame
  max-filtered over ±1 bin — a steady tone gave false onsets without it), energy share above 3 kHz, spectral flatness
  below 11 kHz; summarised to 10 values/s; onsets = flux peaks > 1.35× the ±0.4 s mean and > 10% of the max, ≥ 100 ms
  apart. 0.3–0.6 s per song (release). Calibrated on the user's library (17 tracks, `feel_report`): loudness, bright
  share and flatness separate «calm» from «aggressive» (Элис/Пошлая Молли −8…−11 dB, bright 0.05–0.13, flat
  0.06–0.13; Zhanulka −13…−17 dB, 0.003–0.03, 0.001–0.04); onset density does not (dense distorted rock yields fewer
  detectable onsets than pop), so it only drives pulses. App side `stage/feel.rs` `Mood`: timbre = 0.4·loud(−30…−8)
  + 0.3·bright(0…0.1) + 0.3·flat(0…0.1); moment = (0.55·timbre + 0.25·flux/p90 + 0.2·loudness relative to the song's
  p10–p90) × silence gate, smoothed (max of ±0.25 s and ±1 s means); song = 60th percentile of timbre over audible
  frames. Results: punk 0.76–0.98, Zhanulka 0.28–0.48; inside songs intros 0.15–0.35 vs choruses 0.5–0.84.
  Thresholds CALM 0.4 / FIERCE 0.62; serif below 0.5 (song) / part energy. Cached on disk: `<data>/feel/<hash>.nkf`
  (key = path+size+mtime, shared with the wavebar cache key, ~6 KB per 4 min), computed in a background thread
  (`quiet::enter_background`) when scenes are shown; plan is rebuilt when it arrives.
- **Modes** (`stage::Choice`): Song — one part, style picked from the song-energy family; Moment — parts split when a
  line's energy differs from the part mean by > 0.16, at chorus boundaries, after gaps > 5 s, or after 6/4/3 lines
  (calm/mid/fierce); style from the part's family, never the same twice in a row; parts cross-fade 0.7 s; Only(kind).
  Families: calm — bubbles, stars, ink, web-screen, orbits, block; mid — web, pinwheel, packets, terminal, block,
  orbits, network, rings; fierce — scatter, flight, shards, rings, network, web, block, packets.
- **Architecture** `src/stage/`: `motion.rs` (SplitMix rng, easings incl. back/elastic, `wander` noise, `kick`,
  `Camera`, 10 `Entry` and 6 `Exit` animations picked per line by energy, `tempo(energy)`), `styles/` — one file per
  style implementing `Scene { draw(canvas, fonts, ctx, visible), pace(ctx) }`; scenes live per part, build line
  layouts lazily (`Lazy`), cache expensive still layers (`Still`), and are pure functions of `t` (seeking works).
  `styles/mod.rs` — `Kind` (15, codes for settings), `Line`, `Part`, `Ctx` (t, size, palette, lines, mood, progress,
  hatch tile; `energy()`, `beat()`), `draw_word` (whole / letter-by-letter / letters bursting), `chorus_band`.
- **Styles.** block (drifting, swaying, breathing block; per-line arrival slide/zoom/unfold), orbits (rotating arcs
  with planets, dotted circles, beat-pulsing dots, giant word drifting, letters rising), scatter (field rotates,
  words fly towards the viewer, streaks, ring with ticks/hatch/text on circle), rings (rings spin at energy speed,
  waves on beats, arc text orbits), web (camera = Gaussian-kernel average of nodes around `t`, threads grow to the
  word by the time it is sung, cross-links, zoom/turn by energy), web-screen (per line, floating words, threads
  unravel at line end, rotated extents for overlap), stars (sky rotates and breathes, twinkling background, glow ring
  on the sung star), packets (word nodes with IP labels; TCP: SYN/SYN-ACK/ACK handshake, data + ACK per word; UDP:
  bursts and background stream, 15% lost with an ×), terminal (tcpdump-like log, track time as timestamps, lyric line
  typed after `>`, payload line `└ WORD` inverse; log lines cached as pixmaps and blitted — 21 → 2.6 ms per frame),
  network (both), flight (perspective corridor, words arrive from depth exactly when sung, approach and pass, echo
  trails when loud), pinwheel (words split into chunks per square side, spiral of 8 sides, rotates, flies to the
  viewer and restarts), ink (blobs with droplets on lines and loud beats, brush strokes, letters typed, inverted over
  ink via mask), shards (word rasterised once at break time, cut into jittered-grid triangles textured with the
  word, flying with spin and gravity; cracks before), bubbles (circle drawn around each word, «•••» typing dots, beat
  rings, bubbles rise and fade).
- **Settings/UI.** «Вид текста» back to 6 items (auto, column, calm, punch, beat, «Сцены как у колонки»); when scenes
  are chosen: «Сцена» (17: two auto modes + 15 styles, names from `Lex::stage_scenes`, setting `stage_scene` =
  song/moment/kind code; yesterday's codes block/orbits/scatter/rings migrate) and «Плавность сцен» (60 / 30, setting
  `stage_fps`, caps the frame interval).
- **Cost** (i5-7200U, release, Wayland ×1.33, window 2200×1440): scenes always move now — 60 fps: 45–53% of one core
  while playing, 30 fps: 26–28% (column look 8–9%); our drawing 1–6 ms per frame (web ~1, terminal ~3–5, flight ~5),
  the rest is Slint redrawing the region under the image every frame (`SoftwareRenderer::render` has no occlusion
  culling; the line-by-line path has, for opaque RGB images, but winit uses `render`). Paused 0%, hidden — nothing.
- Verified: 200 tests (+11 ignored), clippy clean Linux + Windows; PNG frames and motion strips for all 15 styles
  (`stage_frames`, `STAGE_KINDS`, `STAGE_STRIP=<sec>`), live screenshots of web/terminal/moment/song on punk
  («Ты довольна?») and a calm song («ты похож на кота»: serif, web then orbits in moment mode, web-screen in song mode).

## 2026-10-19 (12) — Readable angles, live backgrounds, fast hairlines, temporary quick picker

User feedback: in «Ханнамонтана» text stood at 180° and was nearly unreadable; scenes are almost static — words
change but the background does not («почти ко всем сценам»); asked for a temporary picker «колонка — конкретная
тема» because their library is too small to see every theme in the auto modes.

- **No upside-down text.** Sources found: scatter picked 180°; pinwheel's top side was 180° and the whole square spun
  without bound; flight's roll and scatter's field turn accumulated with time; rings' line orbited into the lower
  half of the circle; constellation labels rotated with the sky; text on a circle in the lower half was upside down.
  Now: scatter angles 0/±90/±5–12/±78–86, field sways ±(4+6e)°; pinwheel sides bottom 0°, top 0° (hanging from the
  edge), left and right −90° (both read bottom-up), square sways ±(8+7e)°; flight roll ±(6+10e)°; rings' line sways
  on the upper arc; star labels stay upright at the star's screen position; `text_on_circle` flips glyphs (tops to
  the centre, reading left→right) when the middle angle is in the lower half, scatter's ring text then runs CCW.
- **Live backgrounds** (`styles/fx.rs`, energy- and beat-driven): dust with parallax (web camera), warp streaks
  (scatter), beat ripples (block, web, web-screen), wandering low-poly mesh with beat-flashing fills (block), moving
  disc, drifting orbit centres and breathing arcs (orbits), beat spokes and dust (rings), meteors on beats (stars),
  network grid with data pulses in the accent colour (packets), CRT scanline (terminal), concentric squares spinning
  both ways (pinwheel), falling ink drops (ink), floating glass fragments (shards), rising fizz (bubbles). The
  per-part still layer is gone (nothing is static now).
- **Fast hairlines.** tiny-skia's AA hairline costs ~0.5 µs per pixel of length (five 300 px squares: 2.8 ms; 1.2 px
  stroke and thin filled quads the same; non-AA 0.58 ms). `Canvas::stroke` now sends 1-device-pixel strokes to an own
  Xiaolin Wu rasteriser writing premultiplied RGBA directly (curves flattened to ≤ 6 px segments, dashes via
  `Path::dash`): 0.38 ms. Small dots (`Canvas::dots`) are plotted directly too (dust 0.21 → 0.04 ms). Effect costs at
  720×460: mesh 1.6, grid 0.7, ripples 0.24, scanline 0.2, the rest ≤ 0.2 ms. Styles: 1–6 ms per frame; chorus
  bands cost ~2.4 ms (band fill + mask); inverse redraw is now limited to words near the band (`near_band`).
- **Temporary quick picker** (ComboBox in the lyrics window header and the full-screen top bar): auto, column, calm,
  punch, beat, then the two scene modes and the 15 styles; synced both ways with Settings (`show_quick_look`).
  Marked temporary in code and ISSUES — remove once the user has looked through the themes.
- Trace line now names the shown style(s). Live check on «Ханнамонтана» (got scatter in song mode): readable, streaks
  moving; picker visible in both places (Slint's ComboBox exposes no value/actions over AT-SPI, so switching was
  verified by code). Wayland ×1.33: light style at 60 fps ~41–45% of a core, a heavy one (9 ms/frame) at 30 fps
  ~33–38%. 200 tests, clippy clean on Linux and Windows.

## 2026-10-19 (13) — Restraint: fewer effects, no beat jitter

User feedback on (12): «перегружено сильно, много спецэффектов… нужно утончённо, изысканно, чтобы подходило, а не
чтобы всё дребезжало».

- **Beat response.** `Ctx::beat()` was a 12/s-decay kick on every onset (2–4 per second) feeding sizes, dots, spokes,
  ripples, word shake — the «rattle». Now: decay 4/s, scaled by ((energy − 0.55)/0.45)², i.e. zero below 0.55 and
  only gentle (≤ 1–2% scale) in loud parts. Per-beat ripples/spokes/flashes removed; one soft `fx::swell` at a
  line start (rings) and one ring when a bubble's word is sung.
- **One quiet motif per style** (`fx.rs` rewritten): dust 22–30 faint points (parallax for the web camera), grid at
  α 0.045 with 2–6 slow pulses, 5–10 rising bubbles, 1–4 ink drops, two slow squares, seven glass outlines, rare
  meteors only above energy 0.6, faint slow scanline. Removed entirely: low-poly mesh, warp streaks, beat ripples,
  ring spokes, background dust in orbits/rings/web-screen. Rotation/drift speeds roughly halved or more (orbits
  0.04+0.16e rad/s, rings 0.03+0.15e, block drift 0.008+0.02e·unit/s, sway ≤ 0.6°, field sway ≤ 4°, pinwheel ≤ 9°,
  flight roll ≤ 6°, corridor 4 rings at α 0.1, 2–8 streaks).
- **Words.** Entry set reduced to Fade, Rise, Type, Zoom (calm), Rise/Zoom/Slide(left/right)/Pop (lively), Slam/Pop
  only above energy 0.8; overshoot softened (`soft_back`), scales stay within 0.6…1.3 (asserted in a test); Spin,
  Drop, Flip removed; exits Fade/Sink/Fly(+25%)/Slide, Burst only rarely above 0.8; giant letters rise 0.15 em.
- **Chorus fill** only in strong (loud, Oswald) parts and thinner (0.2 unit; orbits 0.14 h); calm songs mark the
  chorus by size only. Constellation: labels of earlier lines fade out (stars and threads stay). Flight: previous
  line fades as the next starts; words placed by best-of-16 to avoid overlaps at reading distance. Ink: blobs
  0.08–0.15 unit, 3 droplets, brush 0.05–0.08 unit and only above 0.65, beat blobs rare above 0.6. Network: UDP
  bursts 2–4, background stream every 0.4–0.7 s, 10% loss, at most one noise line per word in the log.
- Costs at 720×460: 0.6–4.7 ms per frame. 200 tests, clippy clean (Linux, Windows).
- Follow-up: on «Ханнамонтана» (block, song mode) two consecutive chorus lines drew their own bands at different
  angles — a white «X» during the line change. The band is now one per chorus run (`band_angle`: angle from the
  run's first line, through the scene centre for block, at 0.72 h for orbits), so the leaving and the arriving line
  share it.

## 2026-10-19 (14) — LRC tapping bug; scenes: less technical, in time with the music

User: still «something is off», asked for questions. Answers: problems = «too technical» and «the music isn't felt»;
text behaviour = depends on the theme; place = as now (the question was unclear to them); colour = monochrome.
Bug report: in the lyric editor «marked 1, erased 1 → the next tap marks 2».

- **Bug.** `Draft::stamp` advances `next`; `unstamp` (Backspace / «снять последнюю») steps back, but clearing a line's
  time (the line's × button or an emptied time field → `set_time(index, "")`) did not touch `next`. Now clearing a mark
  moves `next` back to that line when no later line up to `next` has a mark (i.e. it was the last stamped one);
  clearing a mark in the middle leaves `next` alone (otherwise taps would overwrite the rest). The × handler also
  selects the line. Test `clearing_the_last_mark_returns_tapping_to_that_line`.
- **Music.** `Mood` now has a fast loudness envelope `level(t)` (song p10…p95 → 0…1, instant attack, ~0.4 s release)
  and a tempo: onset inter-interval histogram (10 ms bins up to 2 s, ±1 bin spread), period 0.33–1.0 s scored with
  2×/3×/4× support and a log-normal prior around 0.5 s; phase = circular mean of onset phases. On the user's songs:
  92–167 BPM. `Ctx::level()`, `pulse()` (cos² bump at each beat), `beats()`, `steps()` (one eased step per beat).
  Used gently: glows breathe with level; block arc turns one step per beat; orbit planets and the dotted circle step
  per beat; rings breathe as a delayed wave and a metronome dot walks the outer ring; web-screen words sway as a wave
  every two beats; stars twinkle in time; packet-grid pulses move one/two cells per beat; terminal cursor blinks per
  beat and the scanline crosses every four bars; flight passes one corridor ring per beat and words approach faster
  when louder; pinwheel swings once per four bars. Estimated word times (no word stamps in the LRC) snap to the
  nearest onset within 0.12 s (first word stays on the line stamp, order kept) — words light up with the sound.
- **Less technical.** Removed ticks, hatching (and the hatch tile), dashed rings, the «57%» label, sector rings, the
  scatter corner ring's ticks/hatch/dashes, the flight's rectangular corridor (now thin circles). Hard translucent
  discs became soft radial glows (`Canvas::glow`).
- **Line vs word by theme** (`line_look`): block, orbits (row visible, the giant word waits dim and its letters rise
  when sung), bubbles, pinwheel, ink (dim line, letters typed bright), web-screen show the whole line at 20 % from the
  line start and light words as they are sung; scatter, rings, flight, shards, web camera, stars, network stay word
  by word. Pinwheel sides leave the corners free (text no longer collides at corners).
- 203 tests, clippy clean (Linux, Windows). Costs 1.5–5.4 ms per frame at 720×460.

## 2026-10-19 (15) — Fewer, calmer scenes; 3D orbit; calligraphic ink; genre fonts; column click-to-seek

User feedback, point by point: bubbles — remove; shards — change or remove; ink — must feel calligraphic and
disappear «beautifully, with long drips», its background must not follow the music; pinwheel — «too ADHD»; flight —
background too jerky; network nodes+log, terminal and TCP/UDP packets — good; constellation — jerky and overloaded,
«jerks hard when the camera rotates»; web — words jerk to the beat («the idea that everything follows the music is
wrong — lyrics follow the music when it fits»); web camera — better, a bit smoother; rings — «ADHD», on the speaker
photos they are orbits seen from the side; scatter — overloaded; orbits — «should have been one, in 3D»; block — OK
for now. Also: remove the Rhythm/Punch/Calm looks; in the column, clicking a line should seek to it 1–2 s early; fonts
should depend on the song's genre.

- **Root cause of the jerks.** Several scenes moved by `t × speed(energy(t))` or `wander(t, seed, speed(energy))` (stars
  spin, flight roll and streaks, dust, block glow, squares, fragments) — when the momentary energy changes, the
  product jumps (at t = 100 s a 0.1 energy step turned the sky by ~9° in one frame). Same with word approach speed
  from `level(t)` in flight and block drift from `energy()`. Fix: `Mood::travel(t)` = ∫ energy dt (prefix sums,
  quadratic inside a 100 ms sample) and `Ctx::drift()` = 0.5 t + travel(t): «song time» that runs 0.5–1.5× faster
  in tense places but is continuous. Per-line/per-part constants replace momentary energy elsewhere (web/flight
  pace, flight speed per line, block drift per line, roll amplitude per part). Test
  `travel_speeds_up_in_loud_places_without_jumps` (frame step never exceeds 1/60 s of travel).
- **Web camera.** The focus was a Gaussian mean of node positions with a `+1e-6` weight: in a pause with no nodes
  within ±σ the average of near-zero weights hopped between distant nodes; and nodes of the next line were laid only
  when the line before it started, entering the ±4σ window with real weight → a jump at every line start. Now
  `focus()` mixes in (weight 0.02) the eased path from word to word, no epsilon, window 4.5σ, and lines are laid
  10 s ahead. σ 1.1 s (web), 1.3 s (stars); web zoom/turn constant per part, smaller wander. Test
  `camera_focus_moves_without_jumps` (≤ 3 px per 1/120 s while flying 1000 px across a 20 s pause).
- **Beat motion removed** (`Ctx::beat`, `pulse`, `Mood::since_beat` deleted): no web-screen sway wave, no stars
  twinkle in time, no pinwheel swing/breath, no block arc steps/breath, no flight push/echo, no ring metronome.
  Kept where it fits: words light up when sung (onset-snapped times), glow brightness follows `level`, the network
  (packets per word, grid pulses per beat, cursor blink, scanline per 4 bars) untouched.
- **Removed:** Bubbles, Shards, Rings (code `rings` → Orbits); fx fizz/drops/squares/fragments/meteors/swell; paint
  fill_picture/brush/dotted_circle/text_on_circle/blob/smooth_closed; `View.progress`. Kinds: 12.
- **Orbit (3D).** One orbit around a planet, seen from slightly above (elevation 9–13°, roll ±3–8°, both wander
  ±1.5° slowly), perspective distance 3 R. Words are written along the orbit in order (arc positions from word
  widths); the orbit turns so the sung word arrives at the front (`turn(t)` = sum of eased moves, each word's move
  0.18–0.75 s starting 0.6 of it before the word — continuous, seek-safe). Text lies on the ring: squeezed by |sin θ|
  and sheared along the tangent, faded at the edges and behind; back half of the orbit, back beads and back words are
  drawn before the planet (a ground-coloured disc with a soft glow and a rotating wireframe: equator + two meridians,
  front halves only), front half after it. 30 beads ride with the words plus slow drift. ~3 ms/frame.
- **Ink (calligraphy).** Bundled Great Vibes (pointed-pen script). The line is written in the upper part: each word is
  revealed left to right by a slanted pen edge (mask polygon, 0.25 s…0.35+0.07·letters, eased). The previous line
  sinks by its own height (+0.08 h) starting 0.7 s before the next line (so the new one is written on free paper even
  in fast songs — checked on «Ханнамонтана»), fades over 2.6 s, and long drips run from ~38 % of the letters (from the
  glyph's ink bottom; length 0.08–0.34 h, 2.2–3.8 s, smooth in-out; neck + growing drop; dry and fade). Background:
  one soft ink cloud drifting by time only, static spatter. 4–5 ms/frame.
- **Pinwheel.** Reading order is now continuous around the square: left top→down (90°), bottom left→right, right
  bottom→up (−90°), top left→right. Static except a ±1.2° slow drift; no squares, no breath. A line that would cross
  into the next cycle starts a new cycle (cycles change between lines); the old cycle fades in 1 s ending just after
  the new first word, the new cycle's first line appears 0.35 s later.
- **Scatter.** Only the words (0°, ±90°, ±80–86°, ±4–9°), one static thin circle and the chorus band in strong parts.
  Gentle entries (Zoom/Fade), tiny constant outward drift. New words avoid the previous line's boxes (3 passes: full
  size avoiding all, 0.7 avoiding all, 0.7 avoiding own); the previous line fades 0.4–1.6 s after the next starts.
- **Flight.** Corridor rings move by `drift` (no per-beat stepping), fade in from depth; 3–6 streaks per part by drift;
  roll amplitude per part, slow wander; per-line approach speed; no beat push, no echo trail.
- **Constellation.** Constant spin (0.012–0.015 rad/s), camera follows the focus fully, stars closer together
  (0.13–0.26 unit steps) so a line fits the frame, 28 background stars twinkling slowly by time, no meteors, threads
  of lines older than current−2 fade then vanish (stars stay), cross threads only for fresh lines, star pop eased.
- **Genre fonts.** `Letters` {Narrow = Oswald, Wide = Unbounded, Serif = Playfair Display} from the genre tag (wide
  words first: «synthpop», «dance-pop» → Wide; then narrow: «pop punk» → Narrow; then serif: pop, ballad, jazz, folk,
  acoustic…); «Alternative & Indie» and empty → by part mood (strong → Narrow, calm → Serif). Faces per (letters,
  strong): Oswald 700/400 caps or 500/300 caps; Unbounded 800/300 caps or 500/300; Playfair 800 + italic 400 or
  500 + italic. Bundled (OFL-1.1, in «About»): Unbounded 1.701 subset to Latin + Cyrillic (fontTools, 196 KB; no
  reserved name), Playfair Display 1.203 roman + italic (301 + 279 KB, unmodified — reserved font name), Great Vibes
  1.103 subset (263 KB; no reserved name). System Georgia/Noto Serif no longer used by scenes. User's tags: Rock,
  Punk / New Wave → Narrow; Pop → Serif; Alternative & Indie → mood. `Stage::set_lyrics(..., genre)`. Test
  `genre_picks_the_font_and_mood_decides_without_it`.
- **Looks.** `lyrics_style` = [column, scenes]; legacy calm/punch/beat → scenes, auto → column, old scene codes →
  scenes. `KineticLyrics`/`KineticSlot`, `kin-*` properties, `serif-family`, genre→look mapping and the per-word line
  timer are gone. `LOOK_STAGE` = 1. Quick picker: column + 14 scene entries.
- **Column click-to-seek.** `LyricLine` has a `TouchArea` (pointer cursor, full opacity on hover) and an accessible
  default action; `LyricsScroll.line-clicked(index)` → `lyrics-seek(index)` → `seek_lyric_line`: line time − 1.5 s
  (`LYRIC_SEEK_LEAD`), updates the slider/MPRIS, refreshes the current line after 80 ms. Checked through AT-SPI on
  «Ханнамонтана» (silent instance): «Колется дезоморфином» (33.04) → 31.95 s after 0.4 s of play; «Но я белый, как
  сметана» (41.46) → 40.37 s. Lines scrolled out of the clipped column are not in the accessibility tree (mouse only).
- 206 tests, clippy clean (Linux, Windows gnullvm), release built. Live (release, X11, 60 fps): fullscreen 42–48 %,
  lyrics window 49–56 % of a core on the i5-7200U (as before); paused 0 %.

## 2026-10-19 (16) — Ink in water (fluid sim), orbit as a 3D band, livelier pinwheel, beat-locked terminal cursor

User: «чернила ужасные — посмотри, как ведут себя чернила в воде: текст должен стираться, будто капля чернил в воду»;
pinwheel «теперь слишком унылая»; orbit «ДА, это то, что нужно, но не доделано»: no planet sphere, the orbit must be a
flat ring like a wedding band with the text written on it, the text «съезжает и дёргается по орбите»; terminal block
cursor should be in time with the music.

- **Beat timing.** Two bugs made beat-tied visuals late/early and drifting: (1) the stage clock runs ahead of the audio
  by the lyrics lead (150 ms default) — fine for words, wrong for beats; `View.lead`/`Ctx.lead`, `Ctx::heard()` = t −
  lead, `beats()`/`steps()` and the cursor use it. (2) one global tempo grid (period + phase) drifts: a 0.5 % period
  error is ~1 s after three minutes. New `Mood.grid`: beat tracking — step one period, snap 70 % toward the nearest
  onset within ±18 % of a period, adapt the period slowly (±8 % of the global estimate); `phase()`/`count()` interpolate
  between tracked beats. `mood_report` now prints onsets landing within ±12 % of a beat: tracked 33–59 % vs global
  20–48 % on the user's songs (random ≈ 24 %; offbeat onsets cap it near 50–60 %). Test
  `beat_grid_follows_a_drifting_tempo` (0.500→0.470 s period over 2 min with offbeats: phase error < 0.12).
  Terminal cursor: on for the first half of each heard beat, brightest on the beat (0.95 → 0.55).
- **Orbit = a band.** Planet removed. A vertical cylindrical ribbon (height 1.5 × letter) seen from 13–17° above, roll
  2–5°, perspective 3.2 R: inner (back) surface and rims first, outer (front) surface and rims after; shading by 24
  flat slices (alpha ∝ |sin θ|^0.8, no AA, rims stroked over — tiny-skia's gradient cost 4.5 ms/frame at scale 2).
  Every glyph is placed on the band individually: affine from the projected tangent (finite difference along θ) and
  the band's vertical (finite difference along height) at its own angle → foreshortening and curvature follow the
  ring. Glyphs fade at |sin θ| < 0.41, back text hidden. Uniform letter size (7.5 % of the scene at the front).
  **Turning:** monotone cubic (Fritsch–Carlson) through knots at the first and last word of each line (time, arc
  position): constant-ish speed inside a line, eases between lines, eases in before the first line and out after the
  last; laid 10 s ahead so new knots never change the past. Test `ring_turns_smoothly_and_brings_each_word_to_the_front`
  (line ends on time, no stop inside a line, per-frame speed change < 15 % of the top speed). Sung words light up over
  0.3 s.
- **Ink in water.** New `stage/fluid.rs`: 2D stable fluids — velocity on a coarse grid (dye grid / 3), dye on 180 cells
  along the long side; buoyancy (dye heavier than water, sink 20), vorticity confinement (12), velocity keep 0.75/s, dye
  keep 0.86/s, semi-Lagrangian advection, Jacobi pressure (24 iterations, warm start), walls. Work limited to a growing
  dye area (+ margin by max velocity). Fixed 1/30 s steps from the impact, deterministic: re-simulated from the impact
  on a seek (back > 0.25 s or forward > 1.5 s; small clock jitter must not restart it — it did, costing ~20 ms/frame).
  Per line: the calligraphic line is written as before (upper/lower half alternately); 0.6 s before the impact a drop
  falls (accelerating, small tail), hits the line 0.15 s before the next line: a dense blot + 0.35 s downward push →
  the water forms the vortex-pair «mushroom» with curling lobes and a stem; letters melt from the impact point outward
  (front speed 0.7 w/s): each glyph is rasterized once into the dye grid (tiny-skia mask), stamped into the water with
  a small seeded push, its crisp version fades over 0.35 s. Dye → cover (1 − (1 + x/4)^−4 instead of exp) → merged over
  all live sims (alpha over) → `Canvas::cover_picture`: own bilinear upscale of one channel, cell by cell, skipping
  empty cells (cost ∝ inked area). At most two waters at once (a line's ink fades by the start of the line after next),
  LIFE 4.5 s, scene paced at 30 fps (the water steps at 30 Hz). Test `a_push_carries_ink_down_into_a_mushroom`;
  ignored `fluid_costs`: 1.4 ms per step with the whole grid active, cover 0.3 ms.
- **Pinwheel, livelier.** Square sways ±(3.5–6.5)° by `drift` (song time); a sung word slides back along its side
  into place (waits 0.25 em ahead, dim) and lights up; the side's rim draws itself under the sung words like a pen
  with a dot at the tip; old cycle turns 30° toward the centre while shrinking to 0.65 and fading (1.2 s); a new cycle
  settles from 1.1× and −8°.
- **Glow** is drawn directly into the buffer ((1 − d²/R²)²): the tiny-skia radial gradient cost ~11 ms/frame at
  scale 2 for a 0.45-unit glow. Ink lost its background glow (specks only). New ignored test `stage_costs` (588×444 at
  scale 2, 30 fps over 6 s): block 10.5, orbits 4.8, ink 7.6, terminal 9.8, others 2.4–5.9 ms per frame.
- `stage_frames`: `STAGE_TIMES=5.5,6.2,…` renders chosen moments.
- 209 tests, clippy clean (Linux, Windows gnullvm), release built.

## 2026-10-19 (17) — Orbit: thick solid rings, random variants (single / chain / stack)

User: «орбиту сделай толстую и чтобы она менялась: 2 орбиты как цепь или 1 — вариации рандома, а не динамики;
текст то на одной, то на другой, то одновременно».

- **Shape per part** (seeded, never changes during the part): Single 40 %, Chain 35 %, Stack 25 %. Random per part also:
  view elevation (Single 13–19°, Chain 10–16°, Stack 16–22°), roll ±1.5–5°, band height 1.5–1.85 × letter, radial
  thickness 0.3–0.5 × band, Stack's upper ring 0.72–0.86 R and gap 1.3–1.7 bands; letter 7.8 % of the scene at the
  front (Chain 6.8 %), computed from the un-wandered elevation so the word layout does not depend on the sway.
- **Geometry.** General 3D rings (centre, in-plane axes `across`/`toward`, axis `up`): Chain = two rings with centres
  ±0.56 R on x, planes tilted ±22–34° about the x-axis in opposite directions — both planes contain the x-axis, so each
  circle crosses the other's plane at x = ∓d ± R, exactly once inside the other's disc (d < R): genuinely linked
  (test `two_rings_share_the_lines_and_the_chain_is_linked` counts the crossings). Stack = two horizontal rings one
  above the other. Camera as before (elevation, roll, perspective 3.2 R); `Eye::toward` for back-face culling.
- **Solid look.** Four faces per ring (outer, inner, top, bottom), 60 slices, culled by normal·view, tone =
  mix(ground, ink, base + gain·light) with light from upper-left-front (top rim brightest, inner/bottom darkest),
  opaque. Painter's algorithm over all pieces of all rings by depth — the chain's links occlude each other correctly.
  Adjacent slices of the same face with the same tone (1/64 steps) are merged into strips of ≤ 8 slices (fills
  halved: 8–11 → 4.4–5.7 ms/frame at scale 2). Glyphs are depth-sorted after the strip they lie on (max of the strip
  depths of ±2 slices + ε) — averaging strip depth had hidden some letters.
- **Lines on rings.** `rings_of(line)`: Single → ring 0; two rings → alternate (20 % stay on the same ring), 12 % and
  every chorus line on both. One `Track` (words + monotone-cubic turning) per ring; idle ring keeps easing between its
  own lines. Text fades 0.6–1.8 s after the next line starts, as before.
- `stage_frames` / `stage_costs`: `STAGE_SALT=n` picks the seed (to see the variants).

## 2026-10-19 (18) — Orbit rings: thin-walled tall bands, four temporary materials, Big and Pair shapes

User on (17): rings «look like old 3D graphics», proportions wrong («different thickness in different places»), «like
squares, not circles» (both faceted edges and rectangular-looking ovals), two rings must never share an angle, and a
big ring seen almost from the front with only half in view. Ten questions asked (AskUserQuestion); answers: section
= flat tall band (wedding ring, rim barely visible); two rings differ by 20–40°; big ring = a sub-type of the single
ring; text outside; rings only drift slightly; shapes: single, big, chain, and «two, never one above the other — they
may overlap a little, not like soldiers in a row: one a bit higher on the right, the other a bit lower on the left»;
materials: «try all, make 4 kinds of orbits temporarily, I'll pick one».

- **Why it looked thick-and-thin / boxy.** The radial wall (0.3–0.5 × band) showed as a wide top rim at the sides and
  a sliver at the front/back; 40–60 slices showed as facets; perspective at 3.2 R squared the oval; a ring tilted
  close to the view axis became a flat bar. Now: wall 0.06–0.1 × band (band 1.9–2.4 × letter), 120 slices (merged into
  ≤ 8-slice strips of equal tone), camera at 6 R (Big: 2.6 R), visible elevation of every ring (view + ring tilt) kept
  ≥ 9° (from above or below) and ≤ 46°.
- **Shapes** (seeded per part): Single 30 % (R 0.4 w), Big 20 % (R 0.85–1.15 w, view 5–8° above, eye centre at 0.3 h
  so the near arc with the text crosses the middle and the far arc is a thin band at the top; letter 10 % of the scene),
  Chain 25 % (linked as before), Pair 25 % (lower-left ring at (−0.42, −0.2, 0.1) R, upper-right (0.45, 0.24, −0.3) R
  with 0.75–0.9 R, small opposite rolls). Two-ring tilts: visible elevations e1 ∈ 10–24°, e2 = e1 ± 20–38° (≥ 9° away
  from edge-on). Test now checks 40 seeds: angle between the ring axes 19–42°, chains linked.
- **Materials** (temporary kinds `orbits` = lines, `orbits-flat`, `orbits-metal`, `orbits-glass`; «Орбита — линии /
  заливка / металл / стекло» in the scene list and quick picker): lines — strips filled with the ground colour
  (hidden-line occlusion) and 1-device-pixel rims (fast Wu path) plus vertical edges where the band turns away;
  flat — opaque ink band (inner side 45 %, top 75 %), letters cut out in the ground colour; metal — tone
  base + diffuse + Blinn highlight (power 14) from a light that slowly circles the ring (`drift`); glass — one fill per
  visible face run (no overlapping strips → no seams; the run starts at an invisible slice), alpha 0.04–0.12, rims
  and edges as hairlines.
- Costs at scale 2 (stage_costs, salts 26/20/13): lines 3.4–6.8, flat 3.1–6.5, metal 3.3–7.1, glass 6.5–8.4 ms.

## 2026-10-19 (19) — Orbit as an LED ring: dot-matrix band, pixel marquee, irregular orbits

User on (18): none of the four materials — «graphics from the 2000s, the rings intersect each other, everything looks
backward»; asked: wide rings carrying a dot display with a pixel marquee, rings not plain circles but slightly uneven
orbits, «on your own taste, but modern». The four temporary kinds are gone (codes `orbits-*` and `rings` map to
`orbits`); `Material`, the solid faces and strip machinery removed.

- **Band = LED matrix.** 11 rows of dots (caps 7 rows, baseline under row 8, descenders below); dot pitch from the band
  height at the front (Single 13 %, Big 16 %, Double 11 % of the scene); columns ≈ 2πR / pitch. Unlit dots: faint grid
  (front 0.12, back 0.05 and every other column); lit dots: crisp (`Canvas::lights`, per-dot alpha) + a soft halo
  (`Canvas::smoke`); the back half shows lit dots faintly from behind; band rims as hairlines (front 0.22, back 0.08).
- **Pixel marquee.** Each word is rasterized once into an 11-row bitmap (tiny-skia mask, AA coverage → brightness
  `(v/255)^0.8`, < 30 off): Oswald 700 caps (Unbounded 800 for «wide» songs — serif is illegible at 7 dots). Words are
  laid in columns (gap 3, line gap 10); the ring itself does not turn — the text steps through the dots one whole
  column at a time (`turn(t).floor()`), like a real sign. `turn` is the same monotone cubic through line starts/ends
  (now in columns). Sung words full brightness, waiting ones 35 %.
- **Uneven orbits.** Radius × (1 + 0.035 sin(2θ + a + 0.02 t) + 0.02 sin(3θ + b − 0.015 t)), band centre lifted by
  0.025 R sin(θ + c + 0.012 t): slightly irregular and very slowly breathing. Shape computed once per column (rows
  offset along the axis) — 9.6 → 6.5 ms/frame at scale 2.
- **Shapes** (seeded per part): Single 40 % (view 14–22°), Big 25 % (R 0.9–1.2 w, view 6–9°, eye centre at 0.3 h — a
  giant LED band crossing the frame), Double 35 %: concentric rings (inner 0.55–0.62 R, so the bands never touch —
  test checks the inner band's farthest point is inside the outer band's nearest point for 40 seeds), outer ring open
  30–42°, inner flatter by 20–33° (≥ 9°): the inner ring's text runs mid-frame, the outer's near the bottom — no
  overlap. Lines alternate between rings, chorus on both.
- Tests: `the_marquee_runs_smoothly_and_letters_become_dots`, `two_rings_never_cross_and_differ_in_angle`.

## 2026-10-20 (20) — Orbits as dot rings 3 dots thick, pixel text riding them; chain back; luck; orange (temporary)

User on (19): «better», but remove the big ring; make the variant choice pseudo-random with bad-luck protection «like in
games» (they never saw a chain while listening); try rings of 3 rows of dots, temporarily orange «once». Clarified:
remove the bands altogether — orbits are rings made of LED dots, 3 dots thick, and the pixel text rides along the
orbit (letters stand on the ring). Chain (two linked rings) — bring back.

- **Look.** Each orbit is a track of LED dots 3 rows thick (front 0.32, back 0.14 alpha; brighter while a chorus line
  sounds); pixel letters (7-dot caps, bitmap rows 0–8 above the track, descenders falling onto the track rows 9–11)
  stand on it and step along one column at a time; the track under the sounding word is lit (0.55). Lit dots: crisp +
  halo; the back half shows lit dots faintly. No bands, no rims. Uneven, slowly breathing orbit shape as in (19).
- **Shapes:** Single (letters 7.5 % of the scene at the front), Double (concentric, outer 30–42° open, inner 20–33°
  flatter; test checks with the text height that the inner never reaches the outer), Chain (centres ±0.56 R on x,
  visible elevations differing by 20–33°, ≥ 9°, linked — test counts crossings). Big removed.
- **Luck** (`stage::Luck`, lives in `Stage` across tracks): a part whose kind has variants (`Kind::variants()`, Orbits →
  3) gets `Part::variant` from a weighted draw, weight (1 + misses)²; drawn once per track load (`salt`) — re-plans
  after the mood arrives reuse it. Test `luck_makes_every_variant_come_soon`: over 300 loads no variant is missing for
  more than 6 in a row.
- **Orange** `#FF8A24` for the orbit dots is temporary (user asked for it «once»).

## 2026-10-20 (21) — Orbit rings as a dot volume (9 high × 3 deep), text on the outer wall, white/orange, real chain

User on (20): the ring was still 1 dot thick (I had made it 3 rows tall); wanted: 3 dots of *thickness* (radial), 6–9
dots of height (chosen 9), length = the circumference; the text written on the side (outer wall), not standing on
top; plain dots white, text dots orange with the orange going into the depth (outer layer most orange, then less,
inner least); the chain must read as linked, not as two overlapping rings. Confirmed by describing back twice.

- **Volume.** 9 rows (caps 7, baseline under row 8, one row for descenders) × 3 radial layers (radius − k·pitch) ×
  columns around. Plain dots: ink (white in the dark theme), alpha 0.28 (0.36 in a chorus) × layer share 1/0.8/0.62,
  back half × 0.35; layers hidden behind the nearest one are drawn only where |facing| ≤ 0.6 (sides, where the
  thickness shows). Text dots: orange `#FF8A24` (temporary), brightness × 1 / 0.55 / 0.28 from the outer layer inward;
  halos only for bright outer front dots.
- **Occlusion.** Each ring is split into 8-column chunks with a depth; chunks are drawn far → near; a chunk first
  erases (`Canvas::erase`, `BlendMode::DestinationOut` — the real window background shows through, works over the
  cover in full screen) its outer- and inner-wall footprints (bounded at half columns, rows −0.5…8.5) — but only if
  a chunk of another ring was already drawn overlapping its bounds; erasing over its own ring left dark seams every
  8 columns. The nearer link of the chain now hides the farther one at one crossing and vice versa at the other.
- **Speed.** `Canvas::lights`: sqrt-free edge ((r² − d²)/2r) and per-row spans (integer blending was slower — the
  divisions). Drawing dominates (≈ 8 of 9 ms at scale 2); the scene paces at 30 fps (the marquee steps by columns
  anyway). stage_costs at scale 2: 8–9 ms per frame.

## 2026-10-20 (22) — Orbit: thickness visible all round, round rings, readable pixel text (Press Start 2P)

User on (21): «yes, that's it», but «why are the 3 dots only at the edges?», «why sometimes ovals, not circles?», and
«think of something for the text — it reads heavily».

- **Thickness everywhere.** The inner layers were drawn only where |facing| ≤ 0.6, so seen from the front the rim
  looked one dot thick. Now the rim row (row 0, or the bottom row when a ring is seen from below — `dot(up, toward)`)
  is drawn on all three layers all round, and the layers are spaced 1.4 × pitch radially (`LAYER_STEP`) so the rim
  reads as three dots; the single ring is viewed from 22–30° (was 14–22°), chain's first ring 14–24°.
- **Round.** The orbit's «unevenness» had a 2θ term (3.5 %) — that is an ellipse. Now 1.2 % 3θ + 0.8 % 5θ ripple and a
  1.2 % height wave: round, still alive.
- **Text.** Rasterizing Oswald into 7 dots gave blobby letters (threshold 100 dropped thin bars, 64 merged letters).
  Bundled **Press Start 2P** 3.000 (OFL-1.1, full Cyrillic, unmodified because of its reserved name, 118 KB; in
  «About»): drawn on an 8-unit grid, caps 7 dots, built-in 1-dot letter spacing; at 8 dots per em with the baseline
  at row 9 every font pixel lands exactly on a ring dot (no AA, ≥ 128 → lit). Pixelify Sans was rejected (no «О»,
  «П»); other Google pixel fonts have no Cyrillic. Word gap 5 columns; the white dots under the current line *and its
  gaps* are dimmed to 30 % (`Track::line_at`) — letters and spaces read like on a real sign; the mirrored text on the
  back is at 12 % instead of 35 %; inner-layer text dots smaller (0.32) and dimmer (0.45 / 0.2).

## 2026-10-20 (23) — Orbit: black letters among white dots, twinkle, even sides, dot background

User on (22): «better», but the sides are still whiter, the background is too bare — «think of something from dots»,
the object's dots could softly dim and brighten, and «the dots — make them black». Asked: background ideas → chose
star dust, motes on orbits, echo of the line; «black» → «we have 2 kinds of dots: keep the white, replace the orange
with black».

- **Letters** are black dots (`Color::BLACK`) among the white ones; the black goes into the depth (1 / 0.75 / 0.5); the
  white dots under the current line and its gaps are 1.6× brighter (contrast); no halos. Back-side letters 25 %.
- **Even sides.** Plain dot alpha × (0.4 + 0.6 |facing|): where the ring turns away the dots crowd and used to add up.
- **Twinkle.** Every plain dot has its own slow wave (0.75…1, period ~6–25 s, hash of ring/column/row/layer); time
  only, not the beat.
- **Background (dots, music-independent):** 90 dust dots with slow twinkle and a slight parallax with the view sway;
  5 motes on their own thin orbits around the rings (0.75–1.3 × the reach, tilts ±25°, ±0.12–0.28 rad/s) with a
  10-dot fading trail, drawn behind or in front of the rings by the head's depth; the previous line as an «echo» —
  its pixel bitmaps at 2.4 % of the scene per dot, 6 % alpha, drifting right→left behind the rings while the current
  line is sung.
- **Light theme.** There the theme ink is dark, so black letters would vanish among dark dots: the letter colour is
  the opposite of the ink by luma (ink luma > 0.5 → black, else white). The full-screen player always uses white ink.
- Clippy: `type Dot = (f32, f32, f32, f32)` (type_complexity on `motes`). Costs at 588×444 ×2, 30 fps: 8–10 ms/frame.

## 2026-10-20 (24) — Orbit: no echo line, motes ride the rings, 1–2 dot figures (cube, rhombohedron, sphere, pyramid)

User on (23): «much better»; remove the «text behind», make the motes follow the orbit instead of circling around
it, and instead of the background put dot figures nearby — cube, rhombohedron, sphere, pyramid, randomly 1–2 of them.
Asked: «text behind» = the echo line (the mirrored text on the ring's back half stays); motes — run along the ring;
figures — a point cloud in the volume; keep the star dust.

- **Echo removed** (`echo()` gone). Star dust stays.
- **Motes** (`struct Mote`, made once in `new()`): Single ring → 3, two rings → 2 each. Each rides its ring's own
  (irregular) path: radius = ring radius + `out` (−1…1.5 pitch), height = ±(4.5 + 3…5) pitch from the band middle —
  always above or below the band, never over the letters — plus a 0.6-pitch wave; 0.1–0.2 rad/s either way; 16-dot
  fading trail along the same path; dimmer on the back half. Drawn *between* ring pieces by depth (merged into the
  far→near piece order), so a near chain link erases a mote behind it.
- **Figures** (`enum Figure`, clouds built once per part): 1 or 2 (50/50), distinct, uniform pick per part seed.
  Clouds (`Figure::cloud`), centred and scaled by the bounding box to −1…1, with per-point weight 0.35 inside / 0.75
  on faces / 1.0 on edges (the shape reads, the volume stays):
  - cube 8³ lattice; rhombohedron = the lattice through edges at 60° with the long diagonal vertical (a crystal);
  - pyramid = stacked square layers (level k is (k+1)², height step 0.85) — straight edges; a pyramid cut from a
    cubic lattice was stair-stepped and looked like a wedge;
  - sphere = Fibonacci shells r = 1, ⅔, ⅓ (150/64/16 dots) + centre — a lattice ball was lumpy.
  - Edge dots exact; face dots shaken ±0.3 cell *within* the face, interior dots in 3D (fixed seed): an exact lattice
    turned toward the viewer collapsed into «rays». The rhombohedron is not shaken (its rows never face the viewer).
  - Placement from the rings' screen box (union of piece bounds): one figure above the rings on a random side, two on
    a diagonal (above + below); size min(0.2u, 0.75·room) but ≥ 0.14u (then it tucks behind the ring edge); centre
    0.03u off the box; slow bob. Rotation about the vertical 0.15–0.25 rad/s, constant tilt toward the viewer (cube
    28–36°, rhombohedron 12–20°, others 16–24°), weak perspective (1/(1 − 0.2z)), nearer dots brighter; alpha 0.34 ×
    weight, dot radius 0.26 pitch; per-dot twinkle. Drawn after the dust, before the rings. Time only, not the beat.
- `stage_frames`: `STAGE_SIZE=WxH`, `STAGE_LIGHT=1`; frames composite over the palette ground. Script
  `target/scratch/tools/orbit_sheet.sh <dir> <t> [salts]` renders orbits at 720×460 and 600×600 and makes sheets.
- Tests: `one_or_two_different_figures_and_all_kinds_come`, `figure_clouds_keep_their_shape`,
  `motes_follow_their_ring_above_or_below_the_text`.
- Checks: 214 tests pass; clippy clean (Linux, Windows gnullvm); costs 7.7–9.4 ms/frame (588×444 ×2, 30 fps). Live
  (Ханнамонтана, i5-7200U): full-screen 44.5 % of a core, lyrics window 51 %, paused 0 %. The first background run
  was killed for low memory (not the build's fault); rerun in the foreground with `-j 3`.

## 2026-10-20 (25) — Orbit: colours swapped (experiment)

User on (24): «отлично»; «swap the colours, white to black and black to white, just quickly». `const SWAPPED: bool =
true` in `orbits.rs` + `Orbits::colors(ctx) -> (dots, letters)`: ring dots, motes, dust and figures take the colour
opposite to the theme ink (black on the dark theme), letters take the ink (white). `false` restores (24). Live: on
the full-screen cover backdrop the black rings read well and the white letters stand out; in the dark lyrics window
the rings, motes, dust and figures almost vanish into the ground — the white letters float, and the mirrored
back-side text (25 %) becomes noticeable. Waiting for the user's verdict.

## 2026-10-20 (26) — Name «Nyanoto» (visible only), 0.1.1-pre-alpha, album paging, scene list, cleanup

User: «отлично»; figures and motes white; look for garbage; renames «колонка» → «обычные», «одна на песню» →
«авто на песню», «меняется по ходу» → «авто по песни»; remove the TCP/UDP packets scene (keep packets inside
«узлы и лог»), hide ink without deleting; a new name not taken on the internet (ask); version 0.1.1-pre-alpha;
UX ideas; next/previous must page across albums (singles 1, 2, 3 play through on their own, but «next» stopped at
the first single). Asked: name — wanted neko/anime («nyanaudio» but easier) → checked web + GitHub API, picked
**Nyanoto** (nyan + 音 oto «sound»; Orbitone, Tembra, Otonoma, Mirumi, Nyamp, Nekoto taken); depth — **visible
only**; the temporary quick look picker — **remove**.

- **Orbit colours**: rings `Orbits::colors` (black on dark, `SWAPPED`), letters ink; dust, figures, motes —
  `ctx.palette.ink` (white). Live: rings black, letters/sphere/motes/dust white.
- **Scenes**: `Kind::ALL` (settings, auto `family`) has 10 kinds — no Packets, no Ink; `Kind::EVERY` (12) for
  `from_code` and the stage tests/frames, so the hidden code stays compiled and tested. `from_code("packets")` →
  Network (packets fly there too); a saved «ink» falls back to «Авто на песню». Labels RU/EN: «Обычные»/«Plain»,
  «Авто на песню»/«Auto per song», «Авто по песне»/«Auto through the song» (the user's «по песни» read as a typo);
  hints updated (no «чернила»). `lex.stage_scenes` is `[&str; 12]`.
- **Removed**: the quick look picker (two ComboBoxes, `quick-look-*`, `show_quick_look`); the legacy lyric-style
  migrations (`LEGACY_SCENES`, `LEGACY_KINETIC`, `rings`/`orbits-*` codes — the user's settings are already new);
  unused `AppWindow.album-title`; `Library::save_queue_position` (superseded by `save_queue_place`; its test now
  uses that). All pub fns of core/library checked for use outside tests: `artists_of`, `FileHealth::is_damaged`
  stay (they test the scan/health rules). Dependencies all used; no unused Slint properties/callbacks left.
- **Disk**: deleted rebuildable caches — `target/debug/incremental` (22 GB), `target/x86_64-pc-windows-gnullvm/
  debug/incremental` (5.8 GB), the obsolete `target/x86_64-pc-windows-gnu` (2.1 GB, replaced by gnullvm), old
  frame/screenshot folders in `target/scratch`: `target` 42 → 16 GB, +23 GB free. Stale `target/*/nyanoto` binaries
  removed (the program is now `nyanoto`).
- **Version** `0.1.1-pre-alpha` (workspace).
- **Name (visible only)**: window title and «About» «Nyanoto»; `slint::set_xdg_app_id("nyanoto")` (Wayland app_id,
  X11 WM_CLASS); MPRIS `org.mpris.MediaPlayer2.nyanoto` (second instance `….instance<pid>`), Identity «Nyanoto»,
  DesktopEntry `nyanoto`, track paths `/org/nyanoto/track/<id>`; binary `[[bin]] name = "nyanoto"`; data folder
  `nyanoto_library::DATA_FOLDER = "nyanoto"` — `default_database()` renames an old `nekori` folder (with
  `library.db`) once, if the new one does not exist (failure → fresh base, old folder untouched); sync subfolder
  `nyanoto-sync` (old `nekori-sync` renamed the same way); `.lrc-nyanoto-tmp`; covers paths in the cache hint.
  Crates, modules, thread names and env vars stay `nekori-*`/`NYANOTO_*`. Test data folders in `target/scratch`
  (`narrow`, `smoke`, `ui-data`) moved to `nyanoto/`; `safe.py` and the tools look for `nyanoto`.
- **Album paging**: `go_next`/`go_previous` (buttons, hotkeys Ctrl+←/→, media keys, MPRIS/SMTC). At the album edge
  (queue kind Album, repeat off; «previous» only within `PREVIOUS_RESTARTS_AFTER` = 3 s, now `pub` in core) →
  `move_to_album(forward, auto)`: forward — next in the artist's list (shuffled auto-next → the chain chooser,
  fallback list), wrapping; backward — `album_trail` (folders we came from, pushed on every forward move, cleared
  when the user picks something) else the previous in the list, wrapping; forward starts at track 1, backward at
  the last track; playing → `play_list`, paused/stopped → `cue` (no sound). `queue_finished` uses the same code
  with `auto = true` (unchanged behaviour: chain without repeats, stop when all played). Album found by folder
  (+title), since singles without album tags share titles. `chain::beside` + test; MPRIS `CanGoNext` true in
  album queues. Live (silent instance, «Куда пропала Элис?», folders grouping = alphabetical like the artist
  page): Сбой → next → «Танцуй…» 1 → 2 → prev → 1 → prev → back to «Сбой» (trail) → prev → «Пришельцы…» last track;
  paused next → cued «Сбой», state Stopped.
- Checks: 215 tests, clippy clean (Linux, Windows gnullvm), release built; live orbit 47 % (full screen), 54 %
  (window), pause 0.2 %.

## 2026-10-20 (27) — Design pass: left icon sidebar, icons instead of words, app icon, quality and lyrics marks, plain-fallback toggles, revealing settings

User: move «the panel» left; replace words with icons across the UI where possible; make an app icon; rename the
lyrics style «как у колонки» → «Современные»; toggles to fall back to «Обычные» when the system is busy (the scenes
cost CPU) and in power-saving mode; a quality badge for every quality (only Hi-Res existed); a lyrics mark in front
of it; dependent settings hidden until their parent is on, sliding out with an animation. Asked: which panel → the
**tabs become a left sidebar** (icons, label on hover); after a fallback → **scenes come back by themselves** when
the load drops / power saving is off; lyrics mark → **only when a track has lyrics**; badges → **4 tiers: Lossy, CD,
Hi-Res, DSD**.

- **`ui/icons.slint`** (new): `global Icons` — 33 stroke icons drawn as SVG path strings in a 24×24 box (Lucide-like,
  1.8 px, round caps; Slint `Path` with `stroke-line-cap/join` works in the software renderer); `Icon`; `IconButton`
  (hover background, `active` = accent, `filled`, `extra` text such as the queue count, `enabled`, tooltip after
  500 ms through `global Tip` + `TipLayer`, the last child of `AppWindow`, positioned from `absolute-position`, above
  the button or below it near the top edge, to the right in the sidebar; auto-hides after 4 s); `NavItem` (sidebar
  tab: accent bar animates in); `Reveal` (animated height/opacity, `clip`, invisible once collapsed so it leaves the
  Tab chain and the accessibility tree). Words stay as `accessible-label` and tooltips.
- **Layout**: a 60 px sidebar (app icon on top; Music, Playlists, Statistics, Health; Settings at the bottom) next to
  the page column; the crumb title, back button (icon), «shuffle all» (icon) and the search field share one 54 px
  row (the separate tab row is gone).
- **Icons instead of words**: bottom bar — repeat (repeat-one variant via new `repeat-mode`), queue (+count), sleep,
  lyrics (active while the window is open), shuffle, equalizer, chapters, like/info, volume (speaker: muted/low/full,
  click = mute via new `mute-toggle`, percentage in the tooltip; the «Громкость N%» text is gone); A–B stays a chip.
  Full player — collapse (chevron), fullscreen (expand/shrink), like, add, repeat. Lyrics window and track info —
  edit (pencil) and close (×). Album — play (`MediaButton`), add to queue, convert. Playlists — create (+), smart
  (sparkle), play, queue, convert, rules, delete (trash). Queue/history — save, clear. Library folder — remove
  (trash). Track rows — heart/plus/info/× as icon buttons. Dialog buttons (Cancel, Save…) and settings actions keep
  their words.
- **Quality badges**: `rows::quality_tier` — DSD (`Quality.dsd`, new, violet #8a4fd0), Hi-Res (gold), CD (lossless
  ≤ 16 bit / 48 kHz, blue #2f6fd0), Lossy (slate #4f5662), none for unreadable files; red «fake?» unchanged; a
  manually chosen label is now outlined. Detected-origin label «MP3» → «Lossy». Test `every_quality_has_its_badge`.
- **Lyrics mark**: `tracks.lyrics` (schema 19): bit 1 — a `.lrc` with the same stem next to the file (the walker now
  reads a folder fully and checks names; unchanged files get `Out::LyricsFile` updates, so a new/removed `.lrc` is
  noticed without touching the audio), bit 2 — lyrics in tags (`ItemKey::Lyrics`/`UnsyncLyrics`; ID3 SYLT is not
  seen). The migration sets `mtime = -1` so every file's tags are re-read once (test library: 453 files). `Track.lyrics`,
  `Library::set_lyrics_file`; the app marks a track when the LRC editor saves or when playback finds a `.lrc` the
  scan has not seen (`mark_lyrics`). Shown before the badge in track rows, the bottom bar and the full player. Test
  `scan_notices_lyrics_next_to_file_and_in_tags`.
- **«Современные»**: the style label (RU «Современные», EN «Modern»); hints updated.
- **Plain fallback** (`src/load.rs`, settings `stage_fallback_load` / `stage_fallback_power`, both on by default):
  while modern scenes are visible a 2 s timer samples the CPU share used by *other* processes (Linux/Android
  `/proc/stat` minus own `/proc/self/stat`; Windows `GetSystemTimes` minus `GetProcessTimes`; Android usually has no
  access → no fallback); ≥ 70 % for 5 s → plain lines, < 50 % for 30 s → scenes again (`Watch`, tested). Power saving:
  a «nyanoto-power» thread asks every 10 s (Linux: power-profiles-daemon `ActiveProfile == power-saver` on the system
  bus, both D-Bus names, else `/sys/firmware/acpi/platform_profile`; Windows: `SYSTEM_POWER_STATUS.SystemStatusFlag`
  bit 0; Android: unknown); it stops when not needed. `lyrics_look()` returns plain while fallen back; the reason
  shows under the toggles in accent colour. Live: scenes → 4 busy processes → plain lines after ~5 s → back ~30 s
  after the load ended. Power detection read «performance» → not saving (profile not changed by me).
- **Revealing settings** (`Reveal` inside a `VerticalLayout { spacing: 0 }` with the parent row, so a closed block
  adds no gap): cover cache → disk cache → quality; authenticity → sensitivity + «check now»; next album → order
  (moved out of the switch row); ReplayGain → mode (moved out); lyrics → lead, style, hint → (Modern) scene, hint,
  smoothness, both fallback toggles, reason; speed → keep pitch; crossfade → length (moved out); sync → folder,
  buttons, status. Former `enabled:` greying removed.
- **App icon**: cat-ear headphones with an «ω» mouth (also a sound wave), mint on dark slate (matches the green
  accent); variants sakura (pink) and night (violet) offered to the user (`target/scratch/icon/preview.png`).
  `assets/icon.svg`, `assets/icon-256.png` (embedded: `Window.icon`, sidebar). `packaging/` (new): icons 16–512, `.ico`,
  `nyanoto.desktop`, README. The Windows file icon is not embedded yet (needs a resource compiler).
- Debug-only hook `NYANOTO_SETTINGS_SCROLL=<px>` (settings `content-y`): AT-SPI cannot scroll Slint views and
  synthetic wheel events do not reach the Xwayland window. Tools: `settings_at.py`, `fallback_ui.py`, `look.py`.
- Checks: 219 tests, clippy clean (Linux, Windows gnullvm), release built; live: sidebar, icons, badges, lyrics mark,
  settings reveals, full player, lyrics window, album paging, fallback.

## 2026-10-20 (28) — Black-and-white logo, «Т» lyrics icon, sleep menu (clock + 12-hour dial), queue count, album plays by minutes, animations

User: simpler minimalist logo, black and white only, just sticking-up cat ears; a new sleep timer — a small menu
with an alarm-clock time on top and a circular timer below, turning either changes the other (now 22:00: set 23:00
on top → the dial shows 1 h; turn the dial 1 h → the top shows 23:00); the lyrics icon as in Yandex Music; the
queue count counted past tracks — count only upcoming, manually queued ones; animations (there were almost none);
statistics: one song of an album listened many times counted as many album plays — count album plays by minutes
(a 50-minute album, one song listened for 50 minutes in total = 1 play). Asked: logo → **head silhouette with
ears**; sleep end → **fade out**; the dial → **12 hours per turn, up to 24 h (next day shown on top), dragged like
Genshin Impact's time skip**; artists → **both tracks and time**.

- **Logo**: a white cat head (ellipse + two ears, round joins) peeking from the bottom of a black rounded tile, no
  grey (`assets/icon.svg`, `icon-256.png`, `packaging/` icons and `.ico` regenerated). Earlier attempts read as «M».
- **Lyrics icon**: the letter «Т» (Yandex Music shows a «Т» button — found by web search, not copied):
  `Icons.lyrics = "M5 7 V5 H19 V7 M12 5 V19 M9 19 H15"`.
- **Queue count**: `QueueItem.manual` (core) — `append`/`insert_at` (play next, add to queue, play now) are manual,
  `replace` (album, playlist, cue/restore) is not; the bar counts manual items after the current one. A restored
  queue has no manual marks (not stored). Test `manual_marks_only_user_additions`.
- **Statistics**: `listen_graph` — per album `SUM(listened_ms)` and the album's full length (`SUM(duration_ms)` of
  all its tracks in the library, CTE + NULL-safe join); `GraphAlbum { plays: f32 = listened / length, listened_ms }`,
  `GraphArtist { tracks, listened_ms }`, sorted by time; `ListenStats.top_artists: (name, tracks, listened_ms)` by
  time. The map sizes circles by listening time; album label «title · 1,3 раза · 52 мин» (`Lex::times`, tenths,
  «раза» for fractions); artists «42 трека · 3 ч 10 мин». Tests updated (`listening_history_feeds_statistics`,
  `album_times_are_fractional`).
- **Sleep timer** (`src/sleep.rs` math + tests; `SleepMenu`/`TimePart` in app.slint; old 15/30/45/60 cycling and
  the deadline check inside the playback ticker removed): the «Сон» button opens a menu above it (click outside or
  × closes). Top: target hours and minutes (wheel, vertical drag, chevrons; ±1 h / ±1 min), «сегодня/завтра».
  Bottom: a 220 px dial — 12-hour face (one turn = 12 h), «now» dot, accent arc from now to the target (first lap on
  the outer ring r = 80, beyond 12 h on the inner ring r = 64), a handle; press = jump to the nearer of the two
  laps, drag = continuous (unwrapped angle, laps accumulate, clamped to 1 min … 24 h; target snapped to 5-minute
  clock marks), wheel = ±5 min; centre text «1 ч 15 мин». The state is the target time (Unix seconds on a minute),
  so the top stays put while time passes and the dial shrinks. Buttons: «После трека» (stop after the current track),
  «Запустить» / «Изменить», «Выключить». A dedicated `sleep_timer` ticks every second (also while paused; it used to
  be checked only while playing), every 100 ms in the last 30 s, fading the engine volume linearly on the slider
  scale (then squared) and pausing at zero; the slider volume is restored. The bar button shows the countdown
  («42:10», «1:15:00») next to the moon.
- **Animations** (were almost none): `Appear` (fade 180 ms + 10 px rise 220 ms on creation via `init`) wraps every
  page, so switching sections/pages animates; every overlay window fades in (160 ms), the full player also rises
  36 px; hover/pressed backgrounds animate on icon buttons, list rows, chips, tabs, mini and media buttons; icon
  stroke/fill animate (like heart, active states); tooltips fade in; the sleep menu fades in; settings reveals and the
  sidebar indicator were already animated. Animations only run during transitions.
- Live (silent instance, debug build): the sleep menu opened over the button («23:40, сегодня, 30 мин», arc from the
  now-dot to the handle); two «Часы +» → «01:40, завтра, 2 ч 30 мин»; set to 1 min and started → the bar showed the
  moon with «0:57»; ~1 min later MPRIS reported Paused. «В очередь» on an 8-track album → the queue icon showed 8
  (0 before: album tracks are not manual). The sidebar shows the new black-and-white logo; the statistics map
  renders with time-based sizes; its hint now says «чем крупнее, тем дольше слушали».
- Checks: 225 tests, clippy clean (Linux, Windows gnullvm), release built.
- Noticed while checking the animations: the plain lyrics window draws 25–48 fps while playing (main window 9 fps,
  paused 0) because of the 19.10 lyric-line animation (font growth re-targets the column scroll every frame); not
  caused by this round's animations (Slint's software renderer applies `opacity` as alpha, no layers). Logged in
  ISSUES, fix offered to the user. Tool `fps_ui.py` (`SLINT_DEBUG_PERFORMANCE=refresh_lazy,console`).

## 2026-10-21 (29) — Fixed-size icon buttons, sleep / A–B / shuffle as separate modules, live shuffle, more animations, 0.1.3-pre-alpha

User: liking highlighted the area around the heart too; split A–B and sleep into separate settings; add a shuffle
tool; more animations — «an animation for every element», including opening the player; «the main horror»: icons
must never change size — the sleep countdown text widened its button and shifted the whole row; version 0.1.3;
propose graphical improvements.

- **`IconButton` never changes size** (`icons.slint`): width = height = `size`. The old `extra` text beside the
  icon is gone: a number/letter is a `badge` pill overlaid on the top-right corner (queue count; «A» while A–B waits
  for its end; «А» for album shuffle); the sleep countdown is a `ring` (accent arc around the moon, `sleep::ring` —
  share of the started time left, radius 11 in a 24 box), the exact time moved to the tooltip. `highlight: false`
  for hearts — liking lights only the heart (fill + pop), not the button background. A–B is an icon button now
  (`Icons.ab`, active while looping); the bar row keeps its width while timers tick.
- **Press / pop feedback**: the glyph shrinks to 84 % while pressed and pops to 122 % for 150 ms when the button
  turns active or its badge changes (size animated inside the fixed button; Slint's software renderer ignores
  `transform-scale` — `scale()`/`rotate()` are TODO stubs in i-slint-renderer-software 1.18).
- **Modules**: settings `sleep_timer`, `ab_repeat` (old `sleep_ab` is read while the new keys are missing),
  `shuffle_tool` (on by default; was tied to smart playlists) — three switches with their own hints; the smart
  playlists hint no longer mentions shuffling. Turning sleep off closes the menu and stops the timer; A–B off clears
  the section; shuffle off restores the order.
- **Live shuffle** (`App::apply_shuffle`, core `Queue::reorder` / `Engine::reorder` + test): cycling the shuffle
  button reorders what has not played yet (current track keeps playing) — tracks, or albums grouped by the original
  order; «off» restores the original order (`shuffle_original`, saved on `play()` or on the first live shuffle).
- **Animations**: the full player slides up from the bottom as a sheet (320 ms, ease-out cubic) and slides back down
  on close (`full-shown` keeps it alive during the exit); covers and the track title cross-fade on track change
  (`CrossImage`, `CrossText`: two layers, the old one fades out) in the bar and the full player; every cover fades in
  when it loads (the `Cover` component); list rows fade in as they appear; play/pause morph (two paths cross-fading)
  and shrink on press; plus the earlier page/overlay/hover/tooltip/reveal animations.
- Version **0.1.3-pre-alpha**.
- Live (silent instance): the bar keeps every icon in place while the sleep ring, the «A» badge of A–B, the queue
  badge and the shuffle highlight change; liking fills only the heart; cycling shuffle reordered the album queue on
  the fly (current track first, the rest shuffled); the full player was caught mid-slide 0.15 s after opening and was
  gone 1 s after «Свернуть»; settings show «Таймер сна», «Повтор A–B», «Перемешивание» and «Умные плейлисты» (renamed)
  as separate switches. Checks: 227 tests, clippy clean (Linux, Windows gnullvm).

## 2026-10-21 (30) — Design pass 2: tokens and radii, new bar layout, seek preview, player expand, settings cards, folders page

User (screenshots): the sleep menu's buttons crowded the right edge and overflowed; repeat and shuffle should always
be there (drop the extra setting) and the other tools sit on the right, slightly apart; no expand animation when
clicking the cover; in the full player the cover should glide to the centre when the next track has no lyrics; the
«Дальше» line vanished on the last track and shifted everything; settings confusing — add separators, give music
folders their own menu; hovering the seek bar should show where and to what time it would seek (Spotify-like
screenshot); the settings icon looked like a ship's wheel; consistent corner radii everywhere — study design rules
and UX pitfalls, keep performance; no animation when opening an album or artist.

- **Research → rules**: concentric radii (outer = inner + padding) on one scale; motion 100–150 ms feedback,
  200–300 ms state, 300–500 ms layout, ease-out in, no bounce; reserve space for dynamic content; consistent
  hover/active/disabled. `global Ui` (icons.slint): radii 4 / 8 / 12 / 16. Applied: covers, icon/media/mini buttons,
  tabs, list rows → 8; badges, small labels → 4; settings cards, the stats map, the full-player cover → 12; all
  dialogs and the sleep menu → 16; tooltips 8. Avatars and pills stay round.
- **Gear**: a real 8-tooth gear (flat tips, hub hole), computed; within the 24 box like the other icons.
- **Bar layout**: left — track (fixed limits), centre — shuffle · prev · play · next · repeat + seek row, right —
  tools (lyrics, queue, sleep, A–B, EQ, chapters) above volume (+ speed). Both side columns share min 140 /
  preferred 330 / max 330 / stretch 0, so they are always equal and the centre is the window's centre whatever is
  enabled (a `root.width`-based width made a binding loop — Slint warned it may panic). Shuffle is always there: the
  `shuffle_tool` setting/switch/words removed (`SETTING_SHUFFLE` mode stays); «Shuffle all» always on the artists page.
- **`SeekBar`** (bar and full player): thicker on hover, a light preview fill up to the pointer, a time bubble
  above the pointer («m:ss»), a thumb on hover/drag; dragging moves the thumb, release seeks; slider accessibility.
  (`aim` not `pointer`: a property named `pointer` shadowed the `pointer` mouse cursor.)
- **Full player**: grows out of the bar cover (x/y/size/radius from the 64 px cover to the window, 320 ms
  ease-out cubic, clipped; content at full size inside so nothing reflows) and shrinks back into it on close
  (`full-shown` keeps it alive 320 ms). The cover slides left ↔ centre when lyrics appear/disappear (360 ms). The
  «Дальше» line always keeps its height (empty → transparent).
- **Animations that were eaten**: page `Appear` and the player expand start from a 1 ms `Timer` after creation — the
  Rust work after a click (loading the album, the blurred backdrop) used to consume the whole animation before the
  first frame. Verified: the album page caught mid-fade 60 ms after the click; the player caught mid-expand.
- **Sleep menu**: «После трека» on its own centred row; «Выключить» / «Изменить» share the width equally.
- **Settings as cards** (`Section` 12 px radius, title, `Divider` between items), regrouped: О приложении; Общие
  (language, media keys, hotkeys, advanced); Библиотека (browse by, cover cache, FFmpeg, classical, CP1251, long
  records, smart playlists, health, authenticity, convert); Воспроизведение (next album, ReplayGain, speed,
  crossfade, exclusive, EQ, device profiles); Панель плеера (sleep, A–B, waveform); Текст песни; Статистика и связка;
  В разработке. **Music folders** moved to their own page (`PAGE_FOLDERS = 10`, a folder icon in the sidebar above
  the gear).
- Checks: 227 tests, clippy clean (Linux, Windows gnullvm); live screenshots of the bar, the sleep menu (running),
  folders, settings cards, the full player and the album page animation.

## 2026-10-21 (31) — Daily playlist: interaction-weighted taste, no repeats of the last four, ordered by tempo and intensity

User: a smart «playlist of the day» — a taste map from likes, what is played most and how much each artist is loved;
remember the 3–4 previous ones so it does not repeat (tracks may recur, not mostly the same); weigh *how* a track
was listened to, not how many times: plays from the daily playlist itself are the algorithm's choice, plays under the
sleep timer may be asleep, «1–4» is noise (answer: «все вместе нельзя» — tracks with 1–4 plays, 1–4-track visits and
skips within 1–4 s, all of them), recency matters; neighbouring tracks must not jump in BPM (140 → 60 → 200 is bad),
genres ordered by aggressiveness; off by default, unavailable until there are statistics, then offered; a minimum
library size; settings for it; lyrics words-per-minute as an automatic, auxiliary calmness hint when many tracks have
`.lrc`. Answers: ~70 % familiar / 30 % new, adjustable; threshold «500 tracks and 2 weeks» by default, others in
settings; first in «Playlists», a new one every day, «Rebuild» button.

- **Signals** (schema 20): `plays.flags` — `PLAY_DAILY` (queue came from the daily playlist and the item was not
  queued by hand), `PLAY_SLEEPY` (sleep timer running at start or finish, or fading), `PLAY_CHOSEN` (the track the
  user clicked — start of `play()`, a queue-page jump, «play now» from history — or a manual queue item). New
  `skips` table: the user switched (next, a click elsewhere, a queue jump) before the play counted; under 1 s is
  ignored (flicking through). Both sync between devices (`#[serde(default)]` keeps old files readable). New
  `daily(day, created, tracks)` keeps the last six lists. Sessions are not stored: a session is a run of plays with
  ≤ 30 min gaps, derived from timestamps (works for old and synced history).
- **Taste** (`nyanoto-library/src/daily.rs`, pure `choose()` + DB wrappers): counted plays only from sessions of ≥ 5
  tracks or ≥ 25 min, minus daily/sleepy; weight = recency (half-life 30 days, nothing older than 180) × completion
  (0.3…1) × 1.4 if chosen. Quick skip (≤ 4 s) −1, later skip −0.5 (half-life 14 days). Likes +2.5 fading to 40 %
  over ~4 months. «Favourite» = liked, or ≥ 5 plays with a recent-enough weight (≥ 1.5, so 5 plays from half a year
  ago are not). Tracks with 1–4 plays feed artist/album/genre affinity at 40 %.
- **Selection**: familiar pool (weight^0.6 × artist affinity, ½ if played in the last 36 h) and new pool (not liked,
  ≤ 4 plays: 0.45 artist + 0.2 album + 0.35 genre affinity, minus skips). Weighted sampling without replacement
  (Efraimidis–Spirakis keys, splitmix seeded by the day; «Rebuild» seeds by time), caps: artist max(2, n/8), album
  max(2, n/10), at most 30 % from the previous list (and from today's when rebuilding). Previous four lists multiply
  their tracks' weight by 0.25 / 0.4 / 0.6 / 0.8; a rebuilt today's list by 0.15. Tracks of 1–15 min only.
- **Order** (`daily_order`): intensity = sound energy (`Mood.song` from the scene analysis) 0.5 + genre
  aggressiveness 0.35 (`genre_aggression`: ~130 English/Russian keywords, the longest match wins — «барокко» is not
  «рок», «пост-рок» < «рок»; several genres averaged) + words per minute 0.15 (only when ≥ 20 % and ≥ 100 tracks have
  lyrics; sung span of synced lyrics). Target wave: calm start → peak at 65 % → ease down to ~60 %. Cost =
  1.5·(intensity − wave)² (+3× above the wave in the first fifth) + neighbours: intensity jump², tempo gap² (1 = 20 %;
  half/double tempo counts as a small step, +0.15), same artist +3. Start: calmest tracks to calmest places, then
  swaps and segment reversals until no gain. *Live bug found*: with the wave weighted 0.5 the loudest track was
  «hidden» at the very start (one neighbour instead of two) — the live order is now a regression test.
- **App** (`src/daily.rs`): built at the first launch of the day (day starts at 05:00 local, so a night session is
  not cut), when Playlists/Settings open, or when a track starts on a new day; in a background thread
  (`quiet::enter_background`, rest after each decoded track). Tempo/energy come from the scene analysis cache
  (`data/feel/*.nkf`); uncached tracks are decoded and cached (scenes benefit too) unless power saving is on.
  Readiness (tracks, counted plays, days of history) is cached for 5 min.
- **UI**: a card first in «Playlists» — offer («enough history — build a daily playlist?» Not now / Turn on),
  «Building…» (the tile breathes), ready (2×2 covers — the first four found among eight albums; title, «25 tracks · 1 h
  4 min · built at 08:57», play), waiting (what is still missing). The list opens like a playlist with «Rebuild»
  (refresh icon; disabled while building; status line «Building…»); tracks cannot be removed from it. Settings card
  «Плейлист дня»: switch «Build every day» (disabled until ready), status («Too early: needs 47 more tracks in the
  library.»), length 15/25/40/60, new 0–60 % (5 % steps), «When to offer» 300 + week / 500 + 2 weeks / 1000 + month
  (100 / 150 / 300 counted plays). Turning it off also stops offering. New icons `sun`, `refresh`; nouns `Play`, `Day`.
- Live (copy of the test library, 453 tracks + 24 days of synthetic sessions): status «needs 47 more tracks» with
  the default threshold; with «300 + week» — the offer card → Turn on → built in 51 s (decoding uncached tracks
  quietly), 0 s next time (cache); list opened; play + next after 3 s → `skips` row with flags DAILY|CHOSEN; queue
  saved as `daily`. Tests: 241 (+14), clippy clean on Linux and Windows gnullvm.
- **Shuffle**: the daily playlist obeys the shuffle switch like any list. User (21.10.2026): «пользователь сам решает,
  наше дело сказать, максимум подсказку» — so only a hint: a muted line on the daily list page while shuffle is on,
  and the status line on play («Shuffle is on — the tracks will play in random order, not in the tempo wave»).

## 2026-10-21 (32) — Pause when the output changes (default on), forced daily playlist in advanced mode, new player expand animation

User: «принудительное создание плейлиста, если включён режим продвинутого пользователя»; «если наушники отключились или
сменилось устройство вывода, то музыка ставится на паузу, но добавь тумблер… по умолчанию — пауза»; «анимация
развёртывания плеера не очень приятная на глаз».

- **Pause on output change** (engine, `nyanoto-core`): `Engine::set_pause_on_device_change(bool)` (engine default off;
  the app sets it from `device_pause`, default **on**, «0» = off). While playing, every 0.5 s (instead of the usual
  2 s name check) the engine compares *what is heard*: on Linux the default PulseAudio/PipeWire sink **and its active
  port** — wired headphones on a laptop jack are the same sink as the speakers, only the port changes
  («analog-output-headphones» → «analog-output-speaker»); elsewhere the default device name. A change → `Paused` at
  once (ramped), `Event::PausedForDevice`, then the usual follow (the output moves to the new device and waits
  paused). A device error while playing (USB/Bluetooth gone; Android/Windows stream invalidated) → `OutputLost` and,
  with the option on, paused too. The baseline is forgotten whenever playback is not running, so unplugging during a
  pause does not pause the next «play». `SwitchWatch` (pure) is unit-tested.
- **Linux names via our own Pulse client**: `Audio` now talks to the server through the `pulseaudio` crate directly
  (the crate cpal used; `futures` executor for its async calls) instead of cpal's PulseAudio host, so one connection
  gives both the human name (`description` or `name`, same as before — device volume profiles keep working) and the
  active port; the connect handshake is guarded by a 2 s timeout like cpal did. cpal's `pulseaudio` feature dropped.
  Measured on this machine: one `GET_SINK_INFO` round trip 0.36 ms (WH-1000XM6 over Bluetooth: identity
  `bluez_output.80_99_E7_FF_C0_18.1|headset-output`); twice a second only while playing.
- **Forced daily playlist** (advanced mode): «Собрать сейчас» in the settings card (with a hint) and on the waiting
  card in Playlists. Ignores thresholds; turns the daily playlist on if it was off (so the result has a home);
  `DailyOptions.force` — after the taste-based passes, the rest is filled from the whole library in random order
  (artist/album caps still apply, then relaxed). «Rebuild» is forced too while the thresholds are still unmet. In
  advanced mode the switch is available regardless of readiness. Live: threshold «500 + 2 weeks», 453 tracks →
  «needs 47 more tracks», «Build now» → 25 tracks, card ready, `daily = 1`.
- **Player expand**: the old «frame growing out of the bar cover» (static content revealed through a growing hole)
  replaced by a shared-element transition: the cover flies from the bar cover (`bar-cover` id, its
  `absolute-position`) to its place — **centre and side are animated, not the corner**, so the growing cover never
  pokes below the window; the backdrop fades in (280 ms), text and controls fade in 60 ms later and rise 28 px
  (380 ms, cubic-bezier(0.2, 0, 0, 1) — fast start, long soft stop, no bounce). Closing is ordered the other way:
  text fades first (140 ms), the dark backdrop holds while the cover flies back and fades after 90 ms, the cover lands
  exactly on the bar cover (the bar's own image is shown until the big one loads). The target ignores the content's
  rise (`- full-content.y`), so the cover flies straight to where it stops. Checked frame by frame with
  `SLINT_SLOW_ANIMATIONS=10` (debug builds stretch the close timer `full-close-time` by the same factor):
  first try caught the cover's bottom edge leaving the window mid-flight (corner animation) and white text hanging
  over the library on close — both fixed.
- Tests: 243, clippy clean on Linux and Windows gnullvm.

## 2026-10-21 (33) — Touchpad gestures: back/forward, track switching, pull the full player down, push the bar up

User: «добавь управление тачпадом, уточни в чате как; двумя пальцами можно, например, свернуть полный плеер и вообще
назад и т.п.» Asked; answers: all four gestures (right — back, left — forward with a new forward history, sideways
over the track — next/previous, down in the full player — collapse / up over the bar track — expand), «follows the
fingers», a switch in General, **on by default**.

- **What reaches the window** (checked in the sources): two-finger scrolling arrives as `WindowEvent::MouseWheel`
  with `PixelDelta` and, on Wayland, `Started/Ended` phases (libinput axis stop); a mouse wheel gives `LineDelta`
  whole lines — not a gesture. Pinch/rotation exist in winit 0.30 only on macOS/iOS; 3–4-finger gestures are the
  desktop's. Windows precision touchpads give fractional `LineDelta` — treated as precise; X11/Windows have no
  phases, so a gesture ends after 160 ms of quiet (with phases — a 1 s safety only).
- **Capture**: Slint has one winit event filter per window, so `keys::latin_shortcuts` now also forwards
  `CursorMoved` (pointer position in logical points) and `MouseWheel` to `App::pointer_event`, which may swallow the
  event (`PreventDefault`). A Slint-level `TouchArea.scroll-event` was not enough: a `Flickable` accepts any wheel
  event with a non-zero `delta_y` when its content is taller, so a slightly diagonal sideways swipe over a list never
  reached a parent.
- **Recogniser** (`src/gestures.rs`, pure `Tracker`, 5 tests): the axis is decided after 8 px (sideways if
  |dx| > 1.3·|dy|). Regions from the pointer: full player (its right half above the controls is lyrics — vertical
  stays with the lyrics scroller), bar track (left of `bar-track-right`, an out property from the bar column's
  `absolute-position`), the rest of the bar (no gestures: sliders), page. Sideways on a page: back (content moving
  right, as browsers do; 140 pt) / forward; sideways over the track: next (content left) / previous (110 pt), the
  full-player cover shifts with the fingers (×0.35, ±70); down in the full player: the content and cover follow the
  fingers 1:1 up to 120 pt, then with resistance (×0.4), the backdrop lightens; up over the bar track: expand at
  50 pt immediately (its own animation is the feedback). Sideways gestures swallow the scroll only when there is
  something to do (so lists don't jiggle); vertical ones only in the full player outside the lyrics and on the bar
  track. Nothing while a dialog/menu is open.
- **Feedback** (`app.slint`): a 48 pt round arrow (`back`, new `forward`, `next`, `previous` icons) slides in from
  the window edge (from the track column in the bar) and fills with the accent at the threshold — release there to
  act, earlier to cancel; it slides out in 200 ms. Following is immediate (animations with 0 ms while
  `swiping` / `full-dragging`), the return uses the usual 380 ms curve. The full cover and blurred background are now
  freed 450 ms after closing (after the animation) instead of at once — the closing backdrop keeps its blur.
- **Forward history**: `back()` remembers where it came from (`Place`: artist page, album, playlist incl. daily,
  search query, queue, other page); `forward()` restores it; any other navigation (`navigated()` in `go`,
  `open_artist`, `show_album` — split out of `open_album` —, `open_playlist`, `open_daily`, `search`, the queue page)
  forgets it; up to 20 places. Back is a gesture everywhere except the artists page (Esc does the same).
- **Checked**: real touchpad events cannot be injected here (XTest wheel does not reach the window), so a debug-only
  hook `NYANOTO_SWIPE="seconds:x,y,dx,dy,steps,hold|end;…"` feeds the same `pointer_event` path. Screenshots:
  half-way back arrow; back → artists, forward → the same album page again; the full player pulled down 90 pt with a
  lighter backdrop; pulled 180 pt → collapsed; next arrow at the bar track; up over the bar → the player opens; the
  full-player cover shifted left with the next arrow at the edge. Tests: 248, clippy clean on Linux and Windows.

## 2026-10-22 (34) — Themes (7), seasonal themes (6) with small scenes, own scale, grid views

User: four regular themes — light, dark, «Тьма» (OLED/AMOLED), «something cool» (asked: all four suggestions taken —
«Винил», «Синтвейв», «Терминал», «Обложка»); seasonal — Halloween, New Year and (asked) Cat Day, Valentine's Day,
Cosmonautics Day, spring sakura; can be switched off, on by default, applied by themselves, «with light animation»;
scale — own control or the system one? (asked: own 80–150 % + «a bit larger by default» + grid by default); artist and
album views — list, grid.

- **Own widgets** (`ui/widgets.slint`): Slint's std widgets paint with the system palette and the *system* accent
  (`accentify` keeps the Fluent lightness and takes hue/chroma from the desktop accent — the yellow seen on this
  machine), so a «Synthwave» would have had yellow switches. `Switch`, `Slider`, `Button`, `ComboBox` (a `PopupWindow`
  list) and `LineEdit` (`TextInput` with the std caret-follow scrolling and a context menu — words from a
  `WidgetWords` global) are re-implemented with the same properties, so call sites did not change; keyboard focus,
  accessibility roles/actions kept. `TextEdit`, `ListView`, `ScrollView` stay std and follow light/dark via
  `Palette.color-scheme` (now set from Rust). A switch must not stretch (std one doesn't): `horizontal-stretch: 0` and
  `max-width` = its content — otherwise rows that wrap the switch in a layout split the width.
- **Theme** (`ui/theme.slint` global, `src/theme.rs`): background, surface, foreground, border, control, deep (full
  player base), dark flag, own accent (or the system one) and a font. All `Palette.*` in our markup replaced by
  `Theme.*`. Light/Dark/Тьма use the system accent; Винил (#1e1712 / amber #e3a23b), Синтвейв (#160e2b / neon pink
  #ff4fd8), Терминал (#030803 / phosphor #3cff6e, **JetBrains Mono** embedded — OFL, Cyrillic; the generic
  «monospace» family is not resolved by Slint), Обложка — from the playing track's bar cover: OKLab, 24 hue buckets
  weighted by chroma (near-black/white count less), the heaviest hue → accent L 0.76 (chroma clipped into sRGB),
  dark tinted background L 0.19 / surface 0.245 / deep 0.13; grey covers → neutral with the system accent. Theme
  changes cross-fade in 450 ms (Rust tween of all colours). Without a saved theme: light or dark as the system says.
- **Seasons**: Halloween 25.10–1.11, New Year 20.12–10.01, Valentine 12–14.02, Cat Day 1.03, Cosmonautics 12.04,
  sakura 20.04–31.05 (checked hourly). Accent + a tint of the background (Тьма stays pure black). Static decorations
  under the pages (web; garland; paw prints; hearts; stars; sakura branch) and a badge on the cat logo (pumpkin,
  Santa hat, bow, heart, star, blossom). Scenes (`SeasonEpisode`, time from `animation-tick()` only while the
  element exists): a bat flies across, sparse snow 22 s, the cat peeks from the bottom, hearts rise, a rocket flies,
  petals drift — the first 12 s after start, then every 2.5–4 min, only while the window has focus (winit
  `Focused`) and power saving is off (checked in a thread before each scene). Debug: `NYANOTO_SEASON=<code>`.
- **Scale**: `slint::platform::WindowEvent::ScaleFactorChanged` with system × own % × 1.07 («100 %» is 7 % larger
  than the markup — «a bit larger by default»). The system factor: from winit; the window is created *after* the
  first event-loop step and the adapter then dispatches the system scale itself, so ours is applied once
  `with_winit_window` returns (retry every 25 ms), and re-applied after every winit `ScaleFactorChanged` (hook →
  `Timer::single_shot(0)`). Slider 80–150 % (step 5) in «Оформление», Ctrl + «+» / «−» / 0. Measured: this laptop
  runs GNOME at **133 %** (2256×1504 panel → ~1692×1128 pt), not 200 % as the X11 test window suggested.
- **Grid views** (default; the list stays): artists — round tiles (150 pt): own avatar, else the artist folder
  photo (only if a file is really there), else the first album cover cropped round; loaded when a tile appears
  (`init` / `changed data` — a rescan replaces the rows); albums — 180 pt covers (`GRID_COVER` 384 px requested in
  grid mode). Rows = `ceil(n / columns)` with columns from the list width; tiles index the flat models. A
  list/grid icon in the page header; remembered per page (`view_artists`, `view_albums`).
- Settings card «Оформление»: theme swatches in two rows (a mini window in the theme colours), the seasonal switch
  with dates and «Now: …», the scale slider. Tests: 254 (+6 theme: seasons and dates, month/day, OLED stays black,
  OKLab round trip, cover colours, scale), clippy clean on Linux and Windows. Live screenshots: all 7 themes, the 6
  seasons (decorations and scenes caught mid-way), grid views, the combobox popup in the light theme.

## 2026-10-22 (35) — Fix: changing the scale crashed the player

User: «когда ставишь масштаб 100 или меняешь масштаб с 100 на любое другое — крашит приложение». The user's journal
(`journalctl --user`) had the panic: `i-slint-renderer-software … lib.rs:591: buffer of size 3192240 with 2256
pixels per line is too small to handle a window of size 2368x1485`. Dispatching
`WindowEvent::ScaleFactorChanged` keeps the window's *logical* size, so its physical size became logical × new
factor — larger than the real surface the winit adapter renders into. `apply_scale` now reads the real
`inner_size()` first and, right after the scale, dispatches `WindowEvent::Resized` with that size divided by the new
factor, so pixels stay the same; and it runs on the next event-loop step (`Timer::single_shot(0)`), not inside the
slider's pointer event or the key handler. Debug hook `NYANOTO_ZOOM="seconds:step;…"`: seven changes in a row
(110 → 120 → 100 → 90 → 80 → 90 → 100 %) survived on X11 and Wayland; screenshots at 130 % and 80 % fill the window.
The journal also shows «Slint: long callback/instantiation chain detected» (already before the themes, a debug log
about >10 instantiation passes) — not the cause; noted in ISSUES.

## 2026-10-22 (36) — Fix: the full player «spread out»; temporary seasonal preview

User: «полноэкранный ты изменил или баг? почему он сейчас так сильно сдвинут, исправь, и сделай предпросмотр
сезонных тем временно, потом уберём».

- **Bug (from session 34)**: the own `Slider` (and `Button`, `ComboBox`, `LineEdit`, `Switch`) had no height limit
  and the default vertical stretch of a `Rectangle`; std widgets cap their height. In the full player the controls
  row (with the volume slider) took a share of the free height from the cover/lyrics area — small cover on top, a
  big gap above the buttons. All own widgets now: `vertical-stretch: 0` and `max-height` = their natural height
  (slider 24, button/combobox 32, field 34, switch its row). Screenshot: the cover is large again, controls compact.
- **Seasonal preview — TEMPORARY** (to be removed): «Оформление» → «Предпросмотр (временно)»: a list «None (by the
  calendar) / Halloween / … / Spring sakura» and «Play the scene». Not saved; shown even with seasonal themes off;
  choosing a holiday applies its accent/tint/decorations and plays its scene right away (a 30 ms gap re-creates the
  scene so its clock restarts). Code marked «ВРЕМЕННО» in `theme.rs` (`preview`, `season_preview`, `season_play`,
  `preview_names`), `app.slint` (row and properties), `lang.rs` (`season_none/preview/play`), `main.rs` (two
  callbacks). The combobox popup items got accessibility roles/labels/actions (screen readers, and the check).
  Checked: «New Year» picked from the list → red accent, green tint, garland, Santa hat, snow.

## 2026-10-22 (37) — Smooth full-player expand, lyrics beside the pages, the window adapts to its width

User: «анимация раскрытия и закрытия полноэкранного плеера всё ещё лаганная и дёрганная»; lyrics in normal mode —
«верхний экран будет разделяться и справа будут субтитры»; when the window is narrowed things go out of bounds —
«меняем сам интерфейс: ползунок звука — сначала полудуга, которая перерастает в круг; значки и прочие функции,
включая настройки, перестраиваются»; narrow + lyrics — only the lyrics column. Asked: lyrics width → «always about
40 %»; volume → bar → half-arc → ring; back from lyrics-only → any section on the left or «Т».

- **Why the expand was jerky** (measured: `SLINT_DEBUG_PERFORMANCE=refresh_lazy,console`, animations ×5 slower,
  CPU of the process from `/proc`): 11–24 fps, the UI thread at 100 %. A frame in which the whole 2200×1440 window
  changes costs this software renderer 40–50 ms (every pixel through the scene; textures are sampled per destination
  pixel, bilinear for `smooth`). Bisected with temporary switches (pages beneath, content rise, flying cover,
  lyrics, backdrop fade): even with all of them off the backdrop *fade* alone kept it at ~20 fps. Rendering notifiers
  don't fire with the software renderer (no per-frame timing that way).
- **Now**: during the flight only the cover's rectangle changes. The dark base and the blurred backdrop appear at
  once (one heavy frame) and stand still; the text and controls fade in (160 ms) when the cover has landed, and on
  close vanish at once; the cover flies back; the library comes back in one frame when the overlay goes. While the
  overlay covers the window, the pages, sidebar and bar are not drawn (`full-covers`; visible again while dragging
  the player down). The blurred backdrop is rendered **at the window's pixel size** in the cover thread
  (`Shape::Screen(height)`: centre crop in the window's aspect → 24 px → blur at quarter size → bilinear up) and drawn
  `image-fit: fill; image-rendering: pixelated` (one sample per pixel). It and the big cover are prepared ahead —
  on every track change once the full player was used in the session, and when the pointer hovers the bar cover
  (`full-warmup`) — so they don't land mid-flight; a backdrop that still arrives late is shown at the landing.
  Result (debug, ×5 slower, X11): open 13–17 → 46–54 fps, close → 63 fps.
- **Lyrics beside the pages**: the modal lyrics card is gone; «Т» splits the area above the bar — pages on the left,
  lyrics on the right, 40 % of the width right of the sidebar (`lyrics-width`, recomputed in `changed width`), with
  the same three looks (lines, scenes, plain text). Narrower than 780 pt: only the lyrics column; a section on the
  left shows the page instead (`lyrics-yield`), «Т» brings the lyrics back. Width-dependent flags are set in
  `changed width`, not bound to `root.width`: a binding made a layout loop (column widths → window layout info →
  width) — Slint warned it may panic — and a width bound to the window would also have pinned the window's size.
- **Bottom bar by width** (`bar-level`): ≥ 1040 pt as before; 860–1040 — volume as a half-arc above the speaker
  (`VolumeArc`: drag along the arc, wheel; the icon still mutes), speed into «⋯»; 700–860 — volume as a ring around
  the icon (`VolumeRing`: drag up/down, wheel, click mutes), sleep, A–B, EQ and chapters into «⋯» (`MenuRow` popup
  above the button; the sleep menu anchors to «⋯»); < 700 — the track column is only the cover and the heart, both
  side columns 104 pt.
- **Settings in a narrow window**: fixed-width labels (160/260 pt) shrink to 90 pt and wrap; theme swatches get
  smaller; the temporary preview row drops its label. Checked at 1100, 950, 780 and 620 pt (screenshots; debug hook
  `NYANOTO_WINDOW=WxH`), the full player and the album grid at 620.
- Tests 254, clippy clean on Linux and Windows.

## 2026-10-22 (38) — The window rearranges continuously: a bending volume stick, folding tools, gliding tiles

User on (37): «не совсем то что нужно: полудугой он должен быть, когда ты сжимаешь окно и он упирается в кнопку
дальше, т. е. он начинает сгибаться не дёргано, а постепенно, будто ты правда палку сжимаешь, и то же самое к
остальному — при недостатке места оно перестраивается плавно и с анимацией. Исполнители и альбомы должны с
анимацией как бы со своего места перемещаться на своё новое место при недостатке места».

- **No more width levels.** `bar-level` and its thresholds are gone. `changed width` sets the side column widths
  continuously: `free = width − 274` (paddings 24, gaps 2 × 12, middle ≥ 226), `even = min(330, free / 2)`; both
  columns are `even` while it is ≥ 106; below that the track column stays 106 (cover + heart) and the right column
  gets the rest (≥ 38). Only preferred/max widths follow the window, minimums are constants (a width bound to the
  window would pin its size). Window minimum 560 → 420 pt so the stick has room to curl into a ring.
- **`BendVolume`** (widgets.slint) replaces the slider, `VolumeArc` and `VolumeRing`: a stick of constant length
  (110 pt bar, 120 pt full player), right end fixed, given `room`. Enough room — straight, speaker icon on the left.
  Less — it bends into a circular arc of the same length whose width is exactly the room left: chord `c` → half
  angle `u` from `sin u / u = c / L` (5 Newton steps in markup, no loops there) up to a half circle; past it the icon
  slides into the middle (`curl` 0 → 1 between room `2L/π + 36` and 38) while the arc closes (`u = L / span`,
  span = room − icon space); the end is a ring open at the bottom with the speaker inside. Drag/click maps along
  the arc (`atan2`; in the gap → nearest end), wheel ±5 %, the icon still mutes, accessible slider actions. The
  bend follows the window width directly — no animation lag, no jumps.
- **`Fold`** (widgets.slint): a slot whose width animates to 0 and back (260 ms) with a fade. Right column tools in
  order lyrics, queue, sleep, A–B, EQ, chapters; `tool-slots = floor(right / 34)`; what doesn't fit folds into «⋯»
  one by one (least important first), «⋯» itself folds in when needed; the menu now also carries lyrics and queue.
  Speed (150 pt) folds into «⋯» while the volume is still straight (right < 296). Track column: the texts fold
  below 200 pt (animated width share), the info button below 136.
- **Grids**: `ArtistGrid` / `AlbumGrid` replace the row-based ListViews. Every tile has its own place —
  `place-column`/`place-row` floats animated 380 ms; cell width follows the window at once, only the place glides,
  so when the column count changes each tile travels from its old spot to its new one. Tiles out of view are
  `visible: false`; artist pictures load when a tile comes near the viewport (`near`, ± half a screen) instead of on
  instantiation (all tiles exist now).
- **Lyrics column**: `lyrics-grow` / `lyrics-keep` (animated 320 ms) — narrowing past 780 pt the column widens over
  the pages (pages fade out, then aren't drawn) and back; «yield» to a section slides it away.
- Settings: label widths and theme swatch size follow the window continuously (`label-short`, `label-long`,
  `swatch-width`); the search field narrows with the window and leaves room for the title. Full player: the volume
  bends there too (white).
- Debug hook `NYANOTO_RESIZE="sec:WxH;…"` (resize at runtime). Checked by screenshots at 1028…393 pt (logical) and
  frame series with `SLINT_SLOW_ANIMATIONS=5`: tiles 5 → 2 columns glide diagonally to their new places, the lyrics
  column widens.
- Pages fade out (`opacity: 1 − lyrics-grow`) while the lyrics column widens. Tests 254, clippy clean on Linux and
  Windows, release build 16:32.

## 2026-10-22 (39) — Fixes after (38), frame-time measurements and cheaper frames, build server

User: «опять баг с полноэкранным плеером (когда элементы управления сдвинуты); когда сжимаешь окно, есть момент,
когда звук и регулятор громкости оказываются на одном и том же месте и ты не можешь регулировать громкость; когда
образуется круг громкости, он обрезается сверху и снизу; интерфейс на всех подобных анимациях очень сильно лагает;
анимация развёртывания плеера уже лучше, но лагает».

- **Full player controls pushed down**: `BendVolume` had no stretch limits, so the controls row took spare height
  (same class of bug as the own widgets in (36)). `horizontal-stretch: 0; vertical-stretch: 0` on `BendVolume` and
  `Fold`.
- **Speaker over the stick**: while curling, the outside speaker button now fades out by `curl` 0.2 and stops taking
  clicks at half opacity; a smaller one (24 pt) fades in at the ring's centre (0.1 → 0.3) — it never overlaps the
  stroke (stroke radius ≥ 19, button radius 12).
- **Ring cut at top and bottom**: a `Path` is rasterized inside its own box; the arc (up to ~40 pt high) was drawn
  in a 34 pt box. Both paths now get a box 10 pt taller on each side (`viewbox-y: -10`).
- **Measuring frames.** RedrawRequested timestamps (`NYANOTO_FRAMES=<file>`, debug, now every window event with its
  kind) showed that on Wayland a client-side `set_size` gives no reliable per-frame timing; test windows also catch
  the user's pointer and wheel. So: `examples/render_bench.rs` — the same `AppWindow` and software renderer on a
  `MinimalSoftwareWindow` with its own clock, fake data (40 artists with round pictures, cover, lyrics), no screen:
  `cargo run -p nyanoto-app --features bench --example render_bench -- [all|still|drag|glide|lyrics|full] [w h scale]`, default the
  user's maximized window (1585 × 1027 pt × 1.4231). `BENCH_LOOP=1` repeats for stack sampling
  (`target/scratch/tools/sample_pid.py`, eu-stack — no perf on this machine). Dev build, user's size, before: whole
  window 21 ms, drag 15.7 ms avg (90 % 20.7), tile glide 8–13, full-player open 15.6 avg (90 % 22).
- **What costs**: image drawing (nearest sampling per destination pixel, per-pixel alpha math) and text (draw +
  re-layout when widths change), then rounded rectangles and the dirty-region/layout pass. Measured and changed:
  - the flying cover's `#ffffff14` underlay was blended under the cover every frame (16 % of the open) — only while
    there is no picture now; `Cover` likewise paints its `Theme.surface` underlay only behind the letter;
  - the invisible half of `CrossImage`/`CrossText` and the transparent full-player content are `visible: false`
    (not walked at all);
  - large translucent panels (sidebar, lyrics column, settings cards) are opaque pre-mixed colours
    (`Theme.panel`, `Theme.card`) unless a seasonal decoration lies beneath (`Theme.decor`);
  - tile captions are as wide as the picture, not the tile — while the window is dragged the tile width changes every
    frame, the picture's usually not, so the captions aren't laid out again each frame;
  - the content fade after the cover lands: 160 → 100 ms (those frames repaint most of the window, 15–18 ms each).
  - Tried and reverted: opaque covers as RGB8 — the renderer's 3-byte path was *slower* (16 vs 13 ms per open frame,
    three runs each), RGBA with alpha 255 takes the same no-blend shortcut.
  - After: full-player open 10.9–12.5 ms avg, drag 14.5–15.0. The rest is the software renderer itself; a GPU
    renderer (femtovg/Skia) would need crates that aren't in the offline cache.
- **Flaky test** `daily::tests::builds_saves_and_rebuilds`: scanning is multi-threaded and ids are handed out in
  completion order, so «the first 12 ids» were different artists each run (3 of 6 runs failed on 8 threads). Now in
  path order: 10 of 10 pass.
- **Build server** (user: use the Proxmox «и распредели нагрузку»): LXC 105 `nyanoto-build` on the user's Proxmox
  (reached through OpenWrt at cow.lycoria.top), Debian 12 / glibc 2.36 so the binary runs on old and new
  distributions, 8 threads, 8 GB, Rust 1.99.1 + windows-gnullvm, llvm-mingw 20260922. Tests, clippy (Linux and
  Windows) and release builds run there (`/root/pipeline.sh`); the laptop does screenshots and frame measurements.
- Release `render_bench` (built on the server, run on the laptop, user's window size): whole window 17.5 ms, drag
  12.6 avg (90 % 16.8), tile glide 7.4 / 11.8, lyrics column 6–11, full-player open 11.1 avg (90 % 17). Server:
  tests 254 (after the daily fix), clippy clean on Linux and Windows (one precedence warning in the bench fixed),
  release in 7 m 23 s (laptop: ~12 min); the binary needs glibc ≥ 2.35 and runs on the laptop.

## 2026-10-22 (40) — GPU rendering as an option: measured, kept off by default, switch in settings

User: «попробовать и замерить», then after the numbers: «переключатель в настройках».

- `renderer-femtovg` (FemtoVG on OpenGL via glutin) is built in next to `renderer-software`; it added five crates
  (femtovg, fnv, gbm, gbm-sys, i-slint-renderer-femtovg — fetched on the build server, copied into the laptop's
  cargo cache, so the laptop still builds offline for Linux and Windows) and +0.8 MB to the release binary.
- **Measured** (debug build, Wayland, window 1500 × 950 pt, `target/scratch/tools/compare.py`; a window near
  screen size gets auto-maximized by GNOME and then ignores `set_size`, so the first attempt measured nothing):
  tile glide — both 16.7 ms median (vsync), 6–7 frames > 20 ms, worst ~35 ms; full-player open/close ×2 — both
  16.7 ms median, worst 33–34 ms software vs 60–66 ms GPU (texture uploads), CPU 18 % vs 15 %; window drag — GPU
  slower (median 24 ms, 90 % 37 ms); memory 82 → 116 MB; first frame 50–150 ms later. On this laptop (Intel HD 620,
  performance profile, on AC) the GPU gives no gain.
- **Switch**: Settings → Look, advanced mode only: «Рисовать видеокартой» (setting `renderer` = `gpu`/`software`,
  default software), takes effect after a restart. Read before the window exists with
  `nyanoto_library::peek_setting` (read-only, no schema update). `NYANOTO_RENDERER=gpu|software` overrides,
  `SLINT_BACKEND` overrides both. **Safety**: with the GPU chosen a marker `renderer-trial` is written next to the
  database before the window and removed after the first rendered frame (rendering notifier works with FemtoVG);
  if it is still there on the next start, that start draws in software, resets the setting and says so in the
  status line. Checked: software — no GL libraries loaded; GPU — GL loaded, marker gone; leftover marker →
  software, setting reset.
- **Build server overload**: `cargo test` built the app's test target and `render_bench` at once — two rustc of
  ~4 GB for 24 min — the container hit its 8 GB limit and the host (15.8 GB, other VMs) went to 4 GB of swap.
  Killed; container now 6 GB + 1 GB swap; `render_bench` builds only with `--features bench` (`test = false` did not keep `cargo test` from building it); tests and release `-j 4`, clippy `-j 2`.
- Server: tests 254, clippy clean on Linux and Windows and for `render_bench` (`--features bench`), release built
  (6 min) and copied to the laptop (`target/release/nyanoto`, 22:53), starts, 56 MB after start.

## 2026-10-22 (41) — Collapse without the lone backdrop; own title bar (window frame was the resize cost)

User: «при закрытии на секунду заново появляется размытая обложка, а при регулировке окна приложение всё равно
лагает прям сильно заметно; тут скорее не в fps дело — может, в среднем и 60, но редкие кадры долгие сильно портят
восприятие».

- **Collapse**: with the backdrop prepared ahead (hover warm-up) it stayed up while the cover flew back — the text and
  buttons vanished, a full-window blurred cover remained for ~0.4 s, then the library. Now `full-covers` is
  `full-shown && full-open && !full-dragging`, and the dark base and the backdrop are visible only while the player
  is up (or dragged): on collapse the library is there at once and the cover flies into the bar over it.
- **Where resize time went**: simulated resizes don't reproduce it (on Wayland GNOME auto-maximizes near-screen-size
  windows and a client-side `set_size` gets few frames; on X11 60 % of the time is synchronous X round trips from
  accesskit-winit and winit property queries — not the user's case). So the user dragged the edge of a test window
  on their own Wayland screen while `target/scratch/tools/live_drag.py` logged window events and sampled stacks:
  of the busy samples during the drag, 20 of 33 were **sctk-adwaita** (winit's client-side decorations on GNOME):
  the title bar and the shadow are cached per part size, every resize changes the sizes, so they are re-rendered
  with tiny-skia from scratch on each step — costlier than the whole player UI (13 of 33).
- **Own title bar** (user: «свой заголовок везде»): `no-frame: true`, `resize-border-width: 6px` (Slint resizes a
  frameless window from its edges through the compositor). `WindowButtons` (minimize, maximize/restore, close —
  close turns red on hover; white over the full player) sit at the top right over everything but tooltips; the
  rightmost top row reserves room for them (pages header, or the lyrics column header, or the full player's top
  row). `DragArea` — empty space of those rows and the logo: a press moved by > 3 pt starts `drag_window`
  (the system moves the window: snapping, maximize at the top edge), double click maximizes/restores, right click
  opens the system window menu (`show_window_menu`). Close goes through `CloseRequested` like the system button.
  `window-maximized` follows `Resized` (`Pointer::Resized`); a 1 px border is drawn while not maximized.
- **Result** (user's own drag, same capture): new sizes arrived every 16.7 ms median, 90 % ≤ 18.4 ms (10 of 302
  steps over 50 ms — pauses of the hand included); no decoration work in the samples; the player busy 31 % of the
  drag. User: «тянется плавнее».
- Server: `CARGO_INCREMENTAL=0` (a single debug build of the app with incremental state did not fit into 6 GB).
  Tests 254, clippy clean (Linux, Windows, bench), release 23:56.

## 2026-10-23 (42) — Feedback round: title bar fixes, crisp avatars, per-folder layout, touch gestures, unfinished modules, shorter texts, renamed to Nyanoto everywhere

User (23.10.2026), point by point:
- **Full player** — «кнопка свернуть находится на одном уровне по высоте с кнопками окна»: the window buttons now
  have their own 48 pt strip at the top of the full player (a `DragArea` — the window is moved by it), the player's
  own row (collapse, fullscreen) starts below it (`padding-top: 52px`, 16 in OS fullscreen).
- **Minimum window size didn't follow the scale**: Slint re-applies the winit minimum only when the constraints change
  in its own (logical) units, and our scale keeps them at 420 × 460. `min-width: 420px + min-nudge`; `apply_scale`
  flips `min-nudge` between 0 and 0.01 so Slint recomputes it in the new physical pixels.
- **Pixelated avatar circles**: `picture_side` rounded the side *up to a multiple of 8* (213.5 px shown → 216 px
  made); the software renderer scales with nearest sampling, dropping rows and columns — stair steps on the circle
  edge. Now the exact rounded size; on a scale change the shown artist pictures are requested again
  (`reload_artist_pictures`). The renderer can't clip with a radius (`combine_clip` ignores it — TODO in Slint).
- **Full player prev/next too dark**: `MediaButton.on-dark` — white glyph, `#ffffff2e` fill, `#ffffff40` border.
- **Sleep icon**: thicker crescent (inner arc radius 6.5 → 9.5).
- **Lyrics column appeared instead of sliding**: the column stays while `lyrics-keep` animates out
  (`if lyrics-open || lyrics-keep > 0.005`), `lyrics-keep` is read even when closed so the animation has a start;
  the content is laid out at full width and pinned to the right edge (no re-wrapping while it slides in and out).
- **Maximize icon stale after un-maximizing by dragging**: re-checked on `Resized` and again 150 ms later
  (`follow_maximized`).
- **Seasonal themes**: off and shown as «В разработке» (switch disabled, `DevBadge`); the temporary preview row hidden.
- **Unfinished modules** (long recordings, CP1251 tags, conversion, ReplayGain, exclusive output + signal path,
  EQ + AutoEQ, per-device profiles, PC↔phone link): `UNFINISHED = true` — switches disabled and grey, title dimmed,
  «В разработке» badge; stored «on» is ignored (the stored CP1251 flag is reset so scans don't apply it).
- **Per-folder layout** (user: «для каждой папки музыки»): schema 21 adds `roots.grouping` (old global choice copied to
  every folder); `Grouping::PerRoot` — owner/album SQL is a `CASE` over the track's folder; albums carry their
  folder's concrete grouping (track order, folder detection, labels); `track_grouping` for the now-playing labels and
  links. Adding a folder asks «Как раскладывать эту папку?» (По папкам / По тегам / По композиторам with the classical
  module); the «Папки» page has a choice per folder; the global «Обзор по» row is gone. New test
  `each_root_has_its_own_grouping`.
- **Touchscreen gestures** (user: «оба варианта»): `WindowEvent::Touch` → `Pointer::Touch` → the same `Tracker` as the
  touchpad. Two fingers — anywhere, like the touchpad (centroid movement); one finger — from the left/right page edge
  (28 pt), on the track in the bar, in the full player above the controls. When a gesture takes the touch the window
  gets `PointerExited` (nothing under the finger is clicked) and further touch events are swallowed. Not tested on a
  touchscreen (none here).
- **Text**: ~60 settings hints and confirmation texts cut to one short sentence (RU and EN).
- **Window close button** accessible label «Закрыть окно» (was «Закрыть», same as the lyrics close — a test pressed
  the window's one).
- **Renamed to Nyanoto everywhere** (user: «везде, и папку тоже»): project folder `~/Документы/nyanoto`, crates
  `nyanoto-{app,core,library,cli,testkit}`, C shim symbols, thread names, temp suffixes, debug variables
  `NYANOTO_*`, docs; the server copy `/root/nyanoto`. Kept on purpose: `OLD_DATA_FOLDER = "nekori"` and
  `OLD_SUBFOLDER = "nekori-sync"` (migration of old data) and journal lines that describe that migration.
- After the rename an Opus test built its OpusTags with a hard-coded vendor length 6 («nekori») — now the length of
  the vendor string. Server: tests 255, clippy clean (Linux, Windows, bench), release 02:19, starts on the laptop.
  Mistake on the server: a new build chain started while the previous release build still ran — two compiles of
  the app at once filled the container's 6 GB (host stayed with ≥ 3 GB free); the stale one was stopped.


## 2026-10-23 (43) — Brevity pass: short dialogs and comments, new icon, release stripped

User: «краткость — сестра таланта, это касается и кода»; the covers-cache warning was «гора бесчеловечного текста».

- **Warnings**: the authenticity and covers warnings are now two sentences each, no countdown (`WARNING_SECONDS`
  and its timer are gone), the dialog is as tall as its text (no scroll view). The confirm dialog is compact too
  and grows to the 760 × 560 list only when it has «what will change» rows. A few more confirmation texts cut.
- **Comments** in every crate (not `vendor/`): ~5 100 → ~3 400 lines. Each block keeps only why, in a line or two;
  dates, quotes and change history moved out of the code (they live here). Stale doc lines left after earlier
  edits removed (a doc of a deleted helper in `source.rs`, `covers.rs`, `lib.rs`; `DATA_FOLDER` said «до 20.10.2026
  — nyanoto» after the rename). C/C++ shims stay in English (MSVC code page).
- **Icon**: black rounded square with two white ears sticking up from the bottom
  (`assets/icon.svg`, `icon-256.png`, `packaging/icons/*`, `nyanoto.ico`).
- **Resources** (release, idle / playing / paused): CPU 0 / 1.1 / 0 %, RSS 53 / 60 / 60 MB, 4 / 83 / 4 wakeups/s.
  RSS at idle: 17 MB clean code pages of the binary, 12 MB the window buffer (softbuffer, shared with the X server),
  heap + anon ~8 MB — little left to win without risk. `strip = true`: 52.5 → 44.6 MB on disk (symbol table only,
  nothing is loaded from it). Gotcha: a package override (`[profile.release.package.nyanoto-app] opt-level = "s"`)
  silently resets `strip` to `debuginfo` for that package (`-C strip=debuginfo` in `cargo -v`), so the override
  repeats `strip = true`. `panic = "abort"` is not possible: covers use `catch_unwind`.
- **app.slint indentation**: 2 561 lines had drifted from the brace depth after earlier wrapper edits (e.g. the title
  row's `HorizontalLayout` looked like a sibling of the `Rectangle` it lives in). Re-indented by depth, expression
  continuations keep their relative offset; only whitespace changed (checked line by line).
- Authenticity warning title is a question now: «Включить проверку подлинности?».
- Checked on the laptop (release + a debug build for `NYANOTO_SETTINGS_SCROLL`): covers, authenticity and remove-folder
  dialogs are a title, one or two sentences and the buttons; the sidebar logo shows the new ears.

## 2026-10-23 (44) — Android: APK with its own touch UI

User: «создай версию для android, на выходе нужен .apk, другой интерфейс, но по стилю похожий». Chosen with the user:
bottom tabs + mini player, «all files access», emulator checks on the laptop; first version has lyrics, stats with the
map, daily playlist and scenes.

- **One logic, two windows.** `nyanoto-app` is now a library (`run()`); `main.rs` (desktop) and the new
  `crates/nyanoto-android` (cdylib `libnyanoto.so`, `android_main`) call it. Slint does not expose inherited
  properties to Rust (public API is collected before inlining), so `build.rs` copies the `in/out/in-out` properties
  and callbacks of the desktop `AppWindow` into `ui/mobile/app.slint` at `// @api` (bindings that point at desktop
  elements are dropped). Shared types, words and parts moved to `ui/common.slint`.
- **Phone UI** (`ui/mobile/`): tabs Музыка / Плейлисты / Статистика / Настройки, mini player, full player (swipe
  down to close, sideways for next/previous, cover or lyrics/scenes), sheets for «+», info, quality, warnings,
  folder layout, confirm, own folder/file chooser (`choose.rs`; desktop keeps rfd), sleep timer.
- **Android glue** (`packaging/android/`): `MainActivity` (NativeActivity), `Bridge` (Rust ↔ Java, natives
  registered from Rust — NativeActivity loads the library past `System.loadLibrary`), `PlaybackService` (foreground
  media service, MediaSession, notification with buttons, audio focus, «becoming noisy», wake lock). Rust side:
  `android.rs`, Android backend in `system_media.rs`; stderr goes to logcat (tag `nyanoto`).
- **Back:** in the app the gesture is ours (`OnBackAnimationCallback`, the page/sheet/full player follows the finger);
  at the root our callback is unregistered and Slint's is filtered out of `getOnBackInvokedDispatcher` by a proxy,
  so Android shows its back-to-home preview and moves the task back — music keeps playing. Swallowing every
  registration (first try) also dropped the activity's own system callback: Back at the root did nothing.
- **Build:** `packaging/android/build.sh` — cargo for arm64-v8a/x86_64 with NDK r28c (API 26), aapt2, javac (Java 8,
  no lambdas — android.jar lacks `LambdaMetafactory`), d8, zipalign, apksigner. Key: `~/.android/nyanoto-release.jks`
  (+ `.pass`), also on the server. Release arm64 APK ≈ 40 MB (Slint's Android backend always brings the Skia renderer).
- **Fixed after the first phone test:** full player rows stretched with the cover (controls spread over the screen) —
  rows `vertical-stretch: 0`; settings hints overlapped the next row — Slint sizes wrapped text inside a
  HorizontalLayout for an unknown width, so `OptionText`/`Toggle` set text heights from their own width; light status
  bar icons on light themes (`setDarkBars`), light over the full player.
- **Server overload:** after the lib split `cargo test` compiled the UI twice at once (~3 GB each) and pushed the whole
  Proxmox host into swap (load 18). `doctest = false` for the lib and `test = false` for the bin; killed from the host.
- Emulator on the laptop: API 35 x86_64, headless, no audio; crashes in Vulkan/SwiftShader — runs with
  `-gpu swangle_indirect -feature -Vulkan,-GLDirectMem` (`target/scratch/tools/emu.sh`). It does not deliver back
  gesture progress, so the in-app follow-the-finger animation is checked on the user's phone only.
- Checks: tests 256, clippy clean (Linux, Windows, bench), desktop release builds.
