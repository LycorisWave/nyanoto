# Nyanoto

A player for music you keep as files. Runs on Linux, Windows and Android.

[Русский](README.md)

Version 0.1.3, pre-alpha. There are no ready-made builds yet, so you'll have to build it yourself;
instructions are below. I work mostly on Linux. Windows and Android are built from the same code,
but I test them less often.

![Full-screen player](docs/screenshots/desktop-player.jpg)

I wanted a player that opens a big music folder fast, plays almost any format and doesn't eat
memory. Right now it sits at about 55 MB when idle, barely touches the CPU, and uses around 1% of it
while playing.

Everything the player knows about your music lives in one SQLite database on your disk. It only
reads your files and never writes to them. Extra features are modules, and they stay off until you
turn them on in the settings.

| Album | Artists |
|---|---|
| ![Album](docs/screenshots/desktop-album.jpg) | ![Artists](docs/screenshots/desktop-artists.jpg) |

| Full player on a phone | Library | Settings |
|---|---|---|
| ![](docs/screenshots/phone-player.jpg) | ![](docs/screenshots/phone-artists.jpg) | ![](docs/screenshots/phone-settings.jpg) |

## What it does

The library is built from folders: on a drive, on a USB stick, on the network. For each folder you
choose how to read it: by folders like "Artist / Album", by tags, or by composers if it's classical
music. Unplug a drive and your likes and history stay put. Covers come from the album folder or the
tags, and you can give an artist your own photo. Search forgives typos and the wrong keyboard layout.
Scanning runs in the background and the window doesn't freeze (I tried it on 100,000 files).

Tracks play back to back without gaps. Speed goes from 0.25× to 3× without changing the pitch. There's
also crossfade between albums, a sleep timer, A–B repeat, and shuffle for tracks or whole albums.
When an album ends, the player can carry on with another album by the same artist. Pull out your
headphones and the music pauses.

Lyrics come from an `.lrc` file next to the track or from the tags. You can show them as plain lines
or as scenes, animated text inspired by the Cotodama lyric speaker. The scene style depends on the
mood of the track, and the player works that mood out from the audio itself. The desktop version has
an `.lrc` editor where you set the time marks by ear, right on the track's waveform.

The player keeps listening statistics and draws a map from them: the longer you've listened to an
artist, the bigger their circle. The same statistics feed a daily playlist built from your own
library, mostly familiar tracks plus a few new ones, arranged in a wave by tempo.

If you like digging into your files, there's an authenticity check (still in beta). It looks for
"lossless" files that were transcoded from MP3 and for upsampled Hi-Res. It's an estimate from the
spectrum, not a verdict. The Health tab shows broken files, duplicates and gaps in albums.

There are seven themes: light, dark, black for OLED screens, Vinyl, Synthwave, Terminal, and Cover,
which takes its colours from the cover of the playing track. The interface comes in English or
Russian, scaled from 80 to 150%.

It talks to the system in the usual ways. On Linux that's MPRIS: media keys, the GNOME and KDE
widgets, `playerctl`. On Windows it's media keys and the volume flyout through SMTC, and on Android a
notification with buttons and the lock screen. On a laptop, touchpad gestures work too: swipe
sideways to go back and forward, swipe down to collapse the full player.

On a phone it has its own interface made for fingers: tabs at the bottom, a mini player above them,
tap it to open the full player, swipe down to close it. Music keeps playing in the background, and
the Back gesture on the main screen sends the player to the background instead of closing it.

## Formats

| Lossless | Lossy | Rare stuff |
|---|---|---|
| FLAC, ALAC, WAV, AIFF, WavPack, Monkey's Audio (APE), TTA, DSD (DSF, DFF), TAK¹, WMA Lossless¹, TrueHD¹ | MP3, AAC, Opus, Vorbis, Musepack, WMA¹, AC3¹, DTS¹ | ATRAC¹, RealAudio¹, speech codecs¹ |

¹ Through FFmpeg, if it's installed (see below).

I checked FLAC, WavPack, APE and TTA bit for bit against the reference decoders; Opus length matches
to the sample. DSD is converted to 88.2 or 176.4 kHz PCM with the player's own filter. The detailed
table of what decodes each format and how well it's tested is in [docs/FORMATS.md](docs/FORMATS.md)
(in Russian).

## Getting started

1. Build the player (see below) and run `nyanoto`.
2. Click "Choose a music folder".
3. The player asks how to read it. If your music is sorted into artist and album folders, pick
   "By folders". If it's all in one pile, pick "By tags".
4. Wait for the scan to finish; you'll see a thin bar under the title while it runs.

You can also pass a folder when starting it: `nyanoto ~/Music`.

On Android the player asks for access to all files on first launch. Without it, it can't see the
`cover.jpg` covers and `.lrc` lyrics sitting next to your music. After that it offers to add the
`Music` folder.

The database, settings and cover cache are in `~/.local/share/nyanoto` on Linux, in
`%APPDATA%\nyanoto` on Windows, and in the app's private folder on Android.

## Keyboard shortcuts

| Key | Action |
|---|---|
| Space | pause and play |
| ← / → | seek 5 seconds, 30 with Shift |
| Ctrl+← / Ctrl+→ | previous and next track |
| ↑ / ↓ | volume up and down |
| M | mute and unmute |
| L | add to Favorites or remove |
| Q | queue |
| Ctrl+F or / | search |
| Esc | back, close a window, leave a text field |

Media keys work too. Letter keys also work in the Russian layout.

## Building

You need Rust 1.88 or newer, 1.92 for Android. The audio libraries (libopus, libwavpack, the Monkey's
Audio SDK, libmpcdec, Signalsmith Stretch) live in `crates/nyanoto-core/vendor` and get built along
with the player, so there's nothing extra to install for them.

### Linux

```sh
# Fedora
sudo dnf install gcc-c++ alsa-lib-devel fontconfig-devel
# Debian, Ubuntu
sudo apt install g++ pkg-config libasound2-dev libfontconfig1-dev

cargo build --release -p nyanoto-app
./target/release/nyanoto
```

A desktop entry and icons are in `packaging/linux` and `packaging/icons`.

### Windows

```sh
cargo build --release -p nyanoto-app
```

It builds with MSVC. From Linux you can cross-compile with llvm-mingw for the
`x86_64-pc-windows-gnullvm` target. The program icon is `packaging/windows/nyanoto.ico`.

### Android

You'll need JDK 17, the Android SDK with platform 35 and build-tools 35, NDK r28c, and the Rust targets:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
export ANDROID_HOME=~/Android/Sdk
export ANDROID_NDK=$ANDROID_HOME/ndk/28.2.13676358
packaging/android/build.sh                           # release for arm64 and x86_64
PROFILE=dev ABIS=x86_64 packaging/android/build.sh   # quick build for the emulator
```

The APK ends up in `target/android/nyanoto.apk`. The script signs it with
`~/.android/nyanoto-release.jks` and reads the password from `nyanoto-release.jks.pass` next to it.
You create the key once:

```sh
printf '%s' 'choose-a-password' > ~/.android/nyanoto-release.jks.pass
keytool -genkeypair -keystore ~/.android/nyanoto-release.jks -alias nyanoto \
    -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Nyanoto" \
    -storepass:file ~/.android/nyanoto-release.jks.pass
```

Don't lose it: Android won't install a version signed with a different key over the old one.

### FFmpeg

FFmpeg is only needed for rare formats like WMA, AC3, DTS and TAK. It isn't built into the player;
its libraries are loaded when such a file turns up. The player looks for them in an `ffmpeg` folder
next to the program, then in the system, and it needs version 7. You can build your own audio-only
FFmpeg 7.1 with `crates/nyanoto-core/ffmpeg/build.sh`. Without FFmpeg the player works as usual; those
files just won't open.

### Tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

To poke at the engine without a window there's a console player:
`cargo run -p nyanoto-cli -- play FOLDER`. Run it without arguments to see the other commands.

## How the code is laid out

| Part | What's there |
|---|---|
| `crates/nyanoto-core` | the engine: decoding, audio output, resampling, speed, equalizer, authenticity check, own TTA and DSD decoders, wrappers around the reference libraries |
| `crates/nyanoto-library` | scanner, tags, SQLite database, search, playlists, statistics, daily playlist |
| `crates/nyanoto-app` | the [Slint](https://slint.dev) window: desktop markup in `ui/`, phone markup in `ui/mobile/`, and all the logic |
| `crates/nyanoto-android` | the Android entry point |
| `crates/nyanoto-cli` | console player and test utilities |
| `crates/nyanoto-testkit` | test helpers |
| `packaging/` | icons, the Linux desktop entry, the Java part and the APK build |
| `docs/` | development log `LOG.md` (in English), open issues, ideas, formats table |

The desktop and the phone share the same logic; only the markup differs. The window's set of
properties and callbacks is defined in `ui/app.slint`, and when building for Android it gets copied
into `ui/mobile/app.slint`.

## License

Nyanoto is released under the [GNU GPL, version 3 or later](LICENSE).

Other people's code and fonts inside:

| What | License |
|---|---|
| [Slint](https://slint.dev) | GPL-3.0 |
| [Symphonia](https://github.com/pdeljanov/Symphonia) | MPL-2.0 |
| libopus | BSD-3-Clause |
| libwavpack 5.9 | BSD-3-Clause |
| Monkey's Audio SDK 13.27 | BSD-3-Clause |
| libmpcdec (Musepack r495) | BSD-3-Clause |
| Signalsmith Stretch | MIT |
| FFmpeg 7.1, loaded at run time | LGPL-2.1+ |
| fonts Oswald, Playfair Display, Great Vibes, Unbounded, Press Start 2P, JetBrains Mono | SIL OFL 1.1 |

License texts sit next to the sources in `crates/nyanoto-core/vendor/*` and `crates/nyanoto-app/fonts/`.
You can see the versions of the main dependencies in the player itself: Settings → About →
Components and licenses.
