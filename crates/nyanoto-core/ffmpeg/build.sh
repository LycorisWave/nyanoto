#!/bin/sh
# Собирает FFmpeg для модуля редких форматов: только аудиодекодеры, разборщики
# контейнеров и парсеры, без программ, сети, видео и фильтров. Лицензия
# сборки — LGPL 2.1+ (ни --enable-gpl, ни --enable-nonfree), библиотеки
# разделяемые: так LGPL соблюдается и в закрытом, и в открытом виде.
#
#   build.sh <исходники FFmpeg 7.1> <папка установки> [linux|windows|android-arm64]
#
# windows — кросс-сборка из Linux компилятором llvm-mingw (x86_64-w64-mingw32-clang
# в PATH); android-arm64 — NDK (переменная ANDROID_NDK). Ассемблер (nasm) не
# обязателен: без него x86-оптимизации выключаются, звук от этого не меняется.
set -eu

SOURCE=$(cd "$1" && pwd)
PREFIX=$2
TARGET=${3:-linux}
mkdir -p "$PREFIX"
PREFIX=$(cd "$PREFIX" && pwd)

DECODERS="ac3 eac3 dca truehd mlp tak wmav1 wmav2 wmapro wmalossless wmavoice dst
dsd_lsbf dsd_msbf dsd_lsbf_planar dsd_msbf_planar atrac1 atrac3 atrac3al atrac3p atrac3pal atrac9
shorten als ralf cook sipr ra_144 ra_288 qdm2 twinvq metasound imc iac mp1 mp2 mp3 aac aac_latm
alac amrnb amrwb speex nellymoser on2avc dolby_e sbc aptx aptx_hd hca osq apac bonk wavarc rka
qoa xma1 xma2 ftr misc4 gsm gsm_ms g723_1 g729 interplay_acm
pcm_alaw pcm_mulaw pcm_bluray pcm_dvd pcm_lxf pcm_s8 pcm_s8_planar pcm_u8 pcm_s16le pcm_s16be
pcm_s16le_planar pcm_s16be_planar pcm_u16le pcm_u16be pcm_s24le pcm_s24be pcm_s24le_planar
pcm_u24le pcm_u24be pcm_s24daud pcm_s32le pcm_s32be pcm_s32le_planar pcm_u32le pcm_u32be
pcm_s64le pcm_s64be pcm_f16le pcm_f24le pcm_f32le pcm_f32be pcm_f64le pcm_f64be pcm_sga pcm_vidc
adpcm_ima_wav adpcm_ms adpcm_ima_qt adpcm_g722 adpcm_g726 adpcm_g726le adpcm_yamaha adpcm_swf
adpcm_xa adpcm_adx adpcm_psx adpcm_thp adpcm_thp_le adpcm_ima_oki adpcm_ima_dk3 adpcm_ima_dk4
adpcm_sbpro_2 adpcm_sbpro_3 adpcm_sbpro_4 adpcm_ct adpcm_aica"

DEMUXERS="asf xwma ac3 eac3 dts dtshd truehd mlp tak iff wsd dsf oma aea shorten mov matroska
wav w64 rm mpegts mpegps au voc aiff caf mp3 aac loas amr spdif sbc aptx aptx_hd hca osq apac
bonk wavarc rka qoa sox avr ircam xa"

PARSERS="aac aac_latm ac3 amr cook dca dolby_e g723_1 g729 gsm mlp mpegaudio sbc sipr tak xma"

list() { printf '%s' "$1" | tr -s ' \n' ','; }

set -- --prefix="$PREFIX" --disable-everything --disable-programs --disable-doc \
  --disable-avdevice --disable-avfilter --disable-swscale --disable-swresample --disable-postproc \
  --disable-network --disable-autodetect --enable-shared --disable-static --enable-pic \
  --enable-decoder="$(list "$DECODERS")" --enable-demuxer="$(list "$DEMUXERS")" \
  --enable-parser="$(list "$PARSERS")"
command -v nasm >/dev/null 2>&1 || set -- "$@" --disable-x86asm

case "$TARGET" in
  linux)
    # Соседние библиотеки ищутся рядом с собой.
    set -- "$@" --extra-ldflags='-Wl,-rpath,$$ORIGIN'
    ;;
  windows)
    set -- "$@" --enable-cross-compile --target-os=mingw32 --arch=x86_64 \
      --cross-prefix=x86_64-w64-mingw32- --cc=x86_64-w64-mingw32-clang --cxx=x86_64-w64-mingw32-clang++
    ;;
  android-arm64)
    TOOLS="$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
    set -- "$@" --enable-cross-compile --target-os=android --arch=aarch64 \
      --cc="$TOOLS/aarch64-linux-android24-clang" --cxx="$TOOLS/aarch64-linux-android24-clang++" \
      --ar="$TOOLS/llvm-ar" --nm="$TOOLS/llvm-nm" --strip="$TOOLS/llvm-strip" --ranlib="$TOOLS/llvm-ranlib"
    ;;
  *) echo "неизвестная цель: $TARGET" >&2; exit 2 ;;
esac

BUILD="$PREFIX/build"
mkdir -p "$BUILD"
cd "$BUILD"
"$SOURCE/configure" "$@"
make -j"$(nproc)"
make install
