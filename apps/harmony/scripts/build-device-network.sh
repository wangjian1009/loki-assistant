#!/bin/sh
set -eu
app_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
device_deveco=${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}
device_ndk="$device_deveco/sdk/default/openharmony/native"
device_rust="$app_root/entry/src/main/rust"
device_libs="$app_root/entry/libs/arm64-v8a"
export CC_aarch64_unknown_linux_ohos="$device_ndk/llvm/bin/clang"
export AR_aarch64_unknown_linux_ohos="$device_ndk/llvm/bin/llvm-ar"
export CFLAGS_aarch64_unknown_linux_ohos="--target=aarch64-linux-ohos --sysroot=$device_ndk/sysroot"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$app_root/scripts/ohos-linker.sh"
cargo build --locked --manifest-path "$device_rust/Cargo.toml" --target aarch64-unknown-linux-ohos
mkdir -p "$device_libs"
# 清理改名前的生成库，避免 HAP 同时携带旧设备管理模块。
rm -f "$device_libs/liblokidevices.so"
"$device_ndk/llvm/bin/clang++" --target=aarch64-linux-ohos \
  --sysroot="$device_ndk/sysroot" -shared -fPIC -std=c++17 \
  "$app_root/entry/src/main/cpp/workspace.cpp" \
  "$device_rust/target/aarch64-unknown-linux-ohos/debug/libloki_mobile_network.a" \
  -lace_napi.z -ldl -lm -pthread \
  -Wl,-soname,liblokiworkspace.so -Wl,-z,max-page-size=16384 \
  -o "$device_libs/liblokiworkspace.so"
"$device_ndk/llvm/bin/llvm-strip" --strip-debug "$device_libs/liblokiworkspace.so"
cp "$device_ndk/llvm/lib/aarch64-linux-ohos/libc++_shared.so" "$device_libs/"
