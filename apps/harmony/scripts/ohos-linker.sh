#!/bin/sh
set -eu
probe_deveco=${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}
probe_ndk="$probe_deveco/sdk/default/openharmony/native"
exec "$probe_ndk/llvm/bin/clang" --target=aarch64-linux-ohos --sysroot="$probe_ndk/sysroot" "$@"
