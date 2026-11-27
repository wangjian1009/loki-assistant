#!/bin/sh
set -eu
device_deveco=${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}
device_ndk="$device_deveco/sdk/default/openharmony/native"
exec "$device_ndk/llvm/bin/clang" --target=aarch64-linux-ohos --sysroot="$device_ndk/sysroot" "$@"
