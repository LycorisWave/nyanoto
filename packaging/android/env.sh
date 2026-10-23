# Инструменты Android для сборки: `. packaging/android/env.sh`.
# ANDROID_HOME — SDK с платформой 35, build-tools 35 и NDK r28c (на сервере — /root/android/sdk).
export ANDROID_HOME=${ANDROID_HOME:-/root/android/sdk}
export ANDROID_NDK=${ANDROID_NDK:-$ANDROID_HOME/ndk/28.2.13676358}
export ANDROID_API=${ANDROID_API:-26}
TOOLS=$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
for target in aarch64-linux-android x86_64-linux-android; do
    upper=$(echo "$target" | tr 'a-z-' 'A-Z_')
    lower=$(echo "$target" | tr '-' '_')
    export CC_$lower=$TOOLS/$target$ANDROID_API-clang
    export CXX_$lower=$TOOLS/$target$ANDROID_API-clang++
    export AR_$lower=$TOOLS/llvm-ar
    export CARGO_TARGET_${upper}_LINKER=$TOOLS/$target$ANDROID_API-clang
done
export PATH=$TOOLS:$PATH
