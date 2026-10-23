# Nyanoto

A music player for your own collection of files. Linux, Windows and Android.

[Русская версия](README.md)

> Status: **0.1.3-pre-alpha**. There are no ready-made builds yet; build it from source (see "Building").
> Most of the work happens on Linux; Windows and Android are built from the same code.

![Full-screen player](docs/screenshots/desktop-player.jpg)

## Why

I wanted a player that opens a big music folder quickly, plays nearly any format and stays light.
Idle, Nyanoto uses about 55 MB of RAM and 0% CPU; while playing, about 1% CPU.

The player does not download anything and does not send anything anywhere. Everything it knows
about your music lives in one SQLite database on your disk. The music files are only read.

Everything extra is a module: modules stay off until you turn them on in the settings.

## Screenshots

| Album | Artists |
|---|---|
| ![Album](docs/screenshots/desktop-album.jpg) | ![Artists](docs/screenshots/desktop-artists.jpg) |

| Phone: full player | Phone: library | Phone: settings |
|---|---|---|
| ![](docs/screenshots/phone-player.jpg) | ![](docs/screenshots/phone-artists.jpg) | ![](docs/screenshots/phone-settings.jpg) |

## Features

### Library
- Several music folders: drives, USB sticks, network shares. An unplugged drive does not wipe your
  likes and history.
- Each folder has its own layout: by folders ("Artist / Album"), by tags, or by composers (for
  classical music).
- Artists and albums as a grid or a list; covers from the album folder or from the tags.
- Artist pictures: your own photo or an image from the artist's folder.
- Search that forgives the wrong keyboard layout and typos.
- Playlists, "Favorites", a queue with history.
- Scanning runs in the background and the window stays responsive (tested on 100,000 files).

### Playback
- Gapless playback.
- Speed from 0.25× to 3×, pitch preserved.
- Crossfade between tracks of different albums.
- Sleep timer: at a time or after the current track.
- A–B repeat.
- Shuffle tracks or albums. When an album ends, the player can go on with another album by the same
  artist.
- Pause when headphones are unplugged.

### Lyrics
- Synced lyrics from an `.lrc` file next to the track or from the tags.
- Two looks: plain lines and "scenes" — animated text in the style of the Cotodama lyric speaker.
  The scene style follows the mood of the track, which the player works out from the audio itself.
- An `.lrc` editor (desktop): type the text, set time marks by ear over the track's waveform, save.

### Statistics and the daily playlist
- Listening statistics and a "map": artists and albums as circles, the more you listened, the bigger.
- Daily playlist: a new mix from your own library every day — mostly familiar tracks plus a few new
  ones, ordered in a wave by tempo.

### Checking files
- Quality authenticity check (beta): looks for "lossless" files made from MP3 and for upsampled
  Hi-Res. It is an educated guess from the spectrum, not proof.
- Library health: broken files, duplicates, gaps in albums.

### Look
- Themes: light, dark, black for OLED, "Vinyl", "Synthwave", "Terminal" and "Cover" (colours taken
  from the cover of the playing track).
- Interface scale 80–150%.
- Russian and English.

### System integration
- Linux: media keys and the player widget over MPRIS (GNOME, KDE, `playerctl`).
- Windows: media keys and the volume flyout (SMTC).
- Android: a notification with buttons, the lock screen, headphone buttons.
- Touchpad and touchscreen gestures: sideways for back and forward, down to collapse the player.

### On the phone
Its own touch interface: tabs at the bottom, a mini player above them, a full-screen player (swipe
down to collapse, sideways for another track). Music keeps playing in the background; the Back
gesture on the main screen sends the player to the background instead of closing it.

## Formats

In short: nearly everything you meet in music collections.

| Lossless | Lossy | Other |
|---|---|---|
| FLAC, ALAC, WAV, AIFF, WavPack, Monkey's Audio (APE), TTA, DSD (DSF, DFF), TAK¹, WMA Lossless¹, TrueHD¹ | MP3, AAC, Opus, Vorbis, Musepack, WMA¹, AC3¹, DTS¹ | ATRAC¹, RealAudio¹, speech codecs¹ |

¹ through FFmpeg, if it is present (see below). The full table — what decodes each format and how
well it is tested — is in [docs/FORMATS.md](docs/FORMATS.md) (in Russian).

FLAC, WavPack, APE and TTA are checked bit for bit against the reference decoders; Opus length is
exact to the sample. DSD is converted to 88.2 or 176.4 kHz PCM with the player's own filter.

## Quick start

1. Build the player (see "Building") and run `nyanoto`.
2. Click "Choose a music folder" and pick the folder.
3. The player asks how to lay out this folder: by folders or by tags. If your music is stored as
   "Artist / Album / tracks", pick "by folders"; if it is all mixed up, pick "by tags".
4. Wait for the scan (the thin bar under the title) — done.

Modules are turned on in the settings. You can also add a folder from the command line:
`nyanoto ~/Music`.

On Android the player asks for "All files access" on first launch: without it, it cannot see
`cover.jpg` covers and `.lrc` lyrics next to the music. Then it offers to add the `Music` folder.

**Where the data lives:** the database, settings and the cover cache are in `~/.local/share/nyanoto`
(Linux), `%APPDATA%\nyanoto` (Windows) or in the app's private folder (Android).

## Keyboard shortcuts

| Key | Action |
|---|---|
| Space | pause / play |
| ← / → | seek 5 seconds (with Shift — 30) |
| Ctrl+← / Ctrl+→ | previous / next track |
| ↑ / ↓ | volume up / down |
| M | mute / unmute |
| L | like / unlike |
| Q | queue |
| Ctrl+F or / | search |
| Esc | back, close a window, leave a text field |

Keyboard media keys work too. Letter keys also work in the Russian layout.

## Building

You need Rust 1.88 or newer (1.92 for Android). All audio libraries (libopus, libwavpack, the
Monkey's Audio SDK, libmpcdec, Signalsmith Stretch) are in `crates/nyanoto-core/vendor` and are built
together with the player; there is nothing extra to install for them.

### Linux

```sh
# Fedora
sudo dnf install gcc-c++ alsa-lib-devel fontconfig-devel
# Debian / Ubuntu
sudo apt install g++ pkg-config libasound2-dev libfontconfig1-dev

cargo build --release -p nyanoto-app
./target/release/nyanoto
```

A desktop entry and icons are in `packaging/linux` and `packaging/icons`.

### Windows

```sh
cargo build --release -p nyanoto-app
```

Builds with MSVC, or from Linux with the llvm-mingw cross compiler (target
`x86_64-pc-windows-gnullvm`). The program icon is `packaging/windows/nyanoto.ico`.

### Android

You need JDK 17, the Android SDK (platform 35, build-tools 35), NDK r28c and the Rust targets:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
export ANDROID_HOME=~/Android/Sdk
export ANDROID_NDK=$ANDROID_HOME/ndk/28.2.13676358
packaging/android/build.sh                           # release, arm64 and x86_64
PROFILE=dev ABIS=x86_64 packaging/android/build.sh   # quick, for the emulator
```

The result is `target/android/nyanoto.apk`. The script signs it with `~/.android/nyanoto-release.jks`
and reads the password from the file next to it, `nyanoto-release.jks.pass`. To create the key once:

```sh
printf '%s' 'choose-a-password' > ~/.android/nyanoto-release.jks.pass
keytool -genkeypair -keystore ~/.android/nyanoto-release.jks -alias nyanoto \
    -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Nyanoto" \
    -storepass:file ~/.android/nyanoto-release.jks.pass
```

Keep the key safe: Android will not install a new version signed with a different key over the old one.

### FFmpeg (optional)

Rare formats (WMA, AC3, DTS, TAK and others) play through FFmpeg 7. FFmpeg is not built into the
player: its libraries are loaded only when such a file shows up. The player looks for them in an
`ffmpeg` folder next to the program, then in the system. You can build your own audio-only FFmpeg 7.1
with `crates/nyanoto-core/ffmpeg/build.sh`. Without FFmpeg the player works fine; those formats just
won't open.

### Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

There is also a console player for testing the engine without a window:
`cargo run -p nyanoto-cli -- play FOLDER` (`nyanoto-cli` without arguments lists all commands).

## Project layout

| Part | What is inside |
|---|---|
| `crates/nyanoto-core` | The engine: decoding, audio output, resampling, speed, equalizer, authenticity check, own decoders (TTA, DSD) and wrappers around the reference libraries |
| `crates/nyanoto-library` | The library: scanner, tags, SQLite database, search, playlists, statistics, daily playlist |
| `crates/nyanoto-app` | The [Slint](https://slint.dev) window: desktop markup in `ui/`, phone markup in `ui/mobile/`, all window logic |
| `crates/nyanoto-android` | The Android entry point |
| `crates/nyanoto-cli` | Console player and test tools |
| `crates/nyanoto-testkit` | Test helpers |
| `packaging/` | Icons, the Linux desktop entry, the Java part and the APK build |
| `docs/` | Development log (`LOG.md`, in English), open issues, ideas, the formats table |

The desktop window and the phone window have the same set of properties and callbacks, so the
logic is shared and only the markup differs. The set is taken from `ui/app.slint` and, when building
for Android, inserted into `ui/mobile/app.slint`.

## License

Nyanoto is released under the [GNU GPL, version 3 or later](LICENSE).

Third-party parts and their licenses:

| What | License |
|---|---|
| [Slint](https://slint.dev) | GPL-3.0 |
| [Symphonia](https://github.com/pdeljanov/Symphonia) | MPL-2.0 |
| libopus | BSD-3-Clause |
| libwavpack 5.9 | BSD-3-Clause |
| Monkey's Audio SDK 13.27 | BSD-3-Clause |
| libmpcdec (Musepack r495) | BSD-3-Clause |
| Signalsmith Stretch | MIT |
| FFmpeg 7.1 (loaded at run time, not built in) | LGPL-2.1+ |
| Fonts Oswald, Playfair Display, Great Vibes, Unbounded, Press Start 2P, JetBrains Mono | SIL OFL 1.1 |

License texts sit next to the sources in `crates/nyanoto-core/vendor/*` and `crates/nyanoto-app/fonts/`.
The main dependencies with their versions are listed in the player itself: "Settings → About →
Components and licenses".
