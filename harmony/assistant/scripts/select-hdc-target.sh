#!/bin/sh
# 默认选择唯一真机，显式目标可选模拟器；--physical 始终要求真机。
# 标准输出只返回设备 ID，诊断信息写入标准错误。
set -eu

hdc_bin=$1
requested_target=${2:-}
physical_only=${3:-}

fail() {
  echo "$*" >&2
  exit 1
}

target_list=$("$hdc_bin" list targets -v) || fail '读取 hdc 设备列表失败。'
target_list=$(printf '%s\n' "$target_list" | tr -d '\r' | awk '$3 == "Connected" { print $1 }')

is_physical() {
  # 查询设备属性，同时支持 USB 和 TCP 真机，避免仅按连接地址判断。
  model=$("$hdc_bin" -t "$1" shell param get const.product.model) || return 1
  model=$(printf '%s' "$model" | tr -d '\r\n' | tr '[:upper:]' '[:lower:]' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
  case "$model" in
    ''|*emulator*|*simulator*|*fail*|*error*) return 1 ;;
    *) return 0 ;;
  esac
}

if [ -n "$requested_target" ]; then
  printf '%s\n' "$target_list" | awk -v target="$requested_target" '
    $0 == target { found = 1 }
    END { exit !found }
  ' || fail "指定设备未连接或未授权：$requested_target"
  if [ "$physical_only" = '--physical' ]; then
    is_physical "$requested_target" || fail "deploy 仅支持真机，指定设备不是可确认的真机：$requested_target"
  fi
  printf '%s\n' "$requested_target"
  exit 0
fi

selected_target=''
while IFS= read -r candidate; do
  [ -n "$candidate" ] || continue
  if is_physical "$candidate"; then
    [ -z "$selected_target" ] || fail '检测到多台真机，请通过 HDC_TARGET=<设备 ID> 指定目标。'
    selected_target=$candidate
  fi
done <<EOF
$target_list
EOF

[ -n "$selected_target" ] || fail '未检测到已连接的真机，请连接手机、开启 USB 调试并在手机上授权 hdc。'
printf '%s\n' "$selected_target"
