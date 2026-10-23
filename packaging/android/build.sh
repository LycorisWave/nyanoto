#!/bin/bash
# Сборка APK: библиотека Rust под нужные процессоры, Java-часть, ресурсы, подпись.
#   packaging/android/build.sh              — release, arm64 и x86_64
#   PROFILE=dev ABIS=x86_64 packaging/android/build.sh — быстро, для эмулятора
# Ключ подписи: KEYSTORE (по умолчанию ~/.android/nyanoto-release.jks) и пароль в KEYSTORE.pass.
set -e
cd "$(dirname "$0")/../.."
. packaging/android/env.sh
PROFILE=${PROFILE:-release}
ABIS=${ABIS:-"arm64-v8a x86_64"}
KEYSTORE=${KEYSTORE:-$HOME/.android/nyanoto-release.jks}
BUILD_TOOLS=$ANDROID_HOME/build-tools/35.0.0
PLATFORM=$ANDROID_HOME/platforms/android-35/android.jar
OUT=target/android
VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
# Номер версии растёт сам: ГГММДДЧЧ.
CODE=$(date -u +%y%m%d%H)

rm -rf "$OUT/apk" "$OUT/classes" "$OUT/gen"
mkdir -p "$OUT/apk/lib" "$OUT/classes" "$OUT/gen"

for abi in $ABIS; do
    case $abi in
        arm64-v8a) target=aarch64-linux-android ;;
        x86_64) target=x86_64-linux-android ;;
        *) echo "неизвестный процессор: $abi"; exit 1 ;;
    esac
    cargo build --profile "$PROFILE" -p nyanoto-android --target "$target" -j "${JOBS:-4}"
    dir=target/$target/$([ "$PROFILE" = dev ] && echo debug || echo "$PROFILE")
    mkdir -p "$OUT/apk/lib/$abi"
    llvm-strip --strip-unneeded "$dir/libnyanoto.so" -o "$OUT/apk/lib/$abi/libnyanoto.so"
    # C++ (Monkey's Audio, Signalsmith) тянет общую libc++ из NDK.
    if llvm-readelf -d "$OUT/apk/lib/$abi/libnyanoto.so" | grep -q 'libc++_shared'; then
        cp "$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/$target/libc++_shared.so" "$OUT/apk/lib/$abi/"
    fi
done

"$BUILD_TOOLS/aapt2" compile --dir packaging/android/res -o "$OUT/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$OUT/base.apk" -I "$PLATFORM" \
    --manifest packaging/android/AndroidManifest.xml --java "$OUT/gen" \
    --min-sdk-version 26 --target-sdk-version 35 \
    --version-code "$CODE" --version-name "$VERSION" "$OUT/res.zip"
javac -encoding UTF-8 -nowarn -Xlint:-options -source 8 -target 8 -bootclasspath "$PLATFORM" -d "$OUT/classes" \
    $(find packaging/android/java "$OUT/gen" -name '*.java')
"$BUILD_TOOLS/d8" --release --min-api 26 --lib "$PLATFORM" --output "$OUT/apk" $(find "$OUT/classes" -name '*.class')

cp "$OUT/base.apk" "$OUT/unsigned.apk"
# Библиотеки — без сжатия: Android грузит их прямо из APK.
(cd "$OUT/apk" && zip -q -u ../unsigned.apk classes.dex && zip -q -0 -r -u ../unsigned.apk lib)
"$BUILD_TOOLS/zipalign" -f -p 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"
"$BUILD_TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass "file:$KEYSTORE.pass" --out "$OUT/nyanoto.apk" "$OUT/aligned.apk"
rm -f "$OUT/unsigned.apk" "$OUT/aligned.apk" "$OUT/base.apk" "$OUT/res.zip"
ls -la "$OUT/nyanoto.apk"
