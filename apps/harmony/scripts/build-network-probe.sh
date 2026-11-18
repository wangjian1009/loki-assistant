#!/bin/sh
set -eu
app_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
deveco_root=${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}
probe_ndk="$deveco_root/sdk/default/openharmony/native"
probe_rust="$app_root/entry/src/main/rust"
probe_libs="$app_root/entry/libs/arm64-v8a"
export CC_aarch64_unknown_linux_ohos="$probe_ndk/llvm/bin/clang"
export AR_aarch64_unknown_linux_ohos="$probe_ndk/llvm/bin/llvm-ar"
export CFLAGS_aarch64_unknown_linux_ohos="--target=aarch64-linux-ohos --sysroot=$probe_ndk/sysroot"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$app_root/scripts/ohos-linker.sh"
cargo build --manifest-path "$probe_rust/Cargo.toml" --target aarch64-unknown-linux-ohos
mkdir -p "$probe_libs"
"$probe_ndk/llvm/bin/clang++" --target=aarch64-linux-ohos \
  --sysroot="$probe_ndk/sysroot" -shared -fPIC -std=c++17 \
  "$app_root/entry/src/main/cpp/probe.cpp" \
  "$probe_rust/target/aarch64-unknown-linux-ohos/debug/libloki_mobile_probe.a" \
  -lace_napi.z -lhilog_ndk.z -ldl -lm -pthread \
  -Wl,-soname,liblokiprobe.so -Wl,-z,max-page-size=16384 \
  -o "$probe_libs/liblokiprobe.so"
"$probe_ndk/llvm/bin/llvm-strip" --strip-debug "$probe_libs/liblokiprobe.so"
cp "$probe_ndk/llvm/lib/aarch64-linux-ohos/libc++_shared.so" "$probe_libs/"
