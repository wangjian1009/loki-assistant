#!/bin/sh
set -eu
if [ "$(id -u)" != 0 ]; then
  echo '请使用 sudo 运行 HTTPS 配置脚本。' >&2
  exit 1
fi
relay_domain=${1:?需要指定已解析到本机的域名}
relay_port=${2:-8443}
python3 - "$relay_domain" "$relay_port" <<'PY'
import re, sys
if not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?',sys.argv[1]) or '.' not in sys.argv[1]:
    raise SystemExit('域名格式无效。')
if not sys.argv[2].isdigit() or not 1024 <= int(sys.argv[2]) <= 65535:
    raise SystemExit('请选择 1024 至 65535 之间的 HTTPS 端口。')
PY
relay_candidate=$(mktemp /etc/caddy/Caddyfile.loki.XXXXXX)
trap 'rm -f "$relay_candidate"' EXIT HUP INT TERM
relay_marker="# BEGIN LOKI RELAY $relay_domain:$relay_port"
if grep -Fq "$relay_marker" /etc/caddy/Caddyfile; then
  echo '该中继 HTTPS 入口已配置，不重复写入。'
  exit 0
fi
cp /etc/caddy/Caddyfile "$relay_candidate"
cat >> "$relay_candidate" <<EOF

$relay_marker
https://$relay_domain:$relay_port {
    reverse_proxy 127.0.0.1:3340
}
# END LOKI RELAY
EOF
caddy validate --adapter caddyfile --config "$relay_candidate"
relay_backup="/opt/loki-relay/Caddyfile.before-https.$(date +%Y%m%d%H%M%S)"
cp -p /etc/caddy/Caddyfile "$relay_backup"
install -m 644 "$relay_candidate" /etc/caddy/Caddyfile
if ! systemctl reload caddy; then
  cp -p "$relay_backup" /etc/caddy/Caddyfile
  systemctl reload caddy
  echo 'Caddy 重载失败，已恢复原配置。' >&2
  exit 1
fi
printf '中继 HTTPS 入口：https://%s:%s\n' "$relay_domain" "$relay_port"
