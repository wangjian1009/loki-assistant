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
export OHOS_NDK_HOME="$device_deveco/sdk/default/openharmony"
export NAPI_TYPE_DEF_TMP_FOLDER="$device_rust/target/napi-types"
mkdir -p "$NAPI_TYPE_DEF_TMP_FOLDER"
cargo build --locked --manifest-path "$device_rust/Cargo.toml" --target aarch64-unknown-linux-ohos
mkdir -p "$device_libs"
cp "$device_rust/target/aarch64-unknown-linux-ohos/debug/libloki_mobile_network.so" "$device_libs/liblokiworkspace.so"
"$device_ndk/llvm/bin/llvm-strip" --strip-debug "$device_libs/liblokiworkspace.so"
device_types="$app_root/entry/src/main/cpp/types/liblokiworkspace"
"${NODE_HOME:-$device_deveco/tools/node}/bin/node" "$app_root/scripts/generate-napi-types.cjs" \
  "$NAPI_TYPE_DEF_TMP_FOLDER/loki-mobile-network" "$device_types/models.d.ts" "$device_types/index.d.ts"
