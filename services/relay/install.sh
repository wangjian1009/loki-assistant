#!/bin/sh
set -eu
if [ "$(id -u)" != 0 ]; then
  echo '请使用 sudo 运行安装脚本。' >&2
  exit 1
fi
relay_bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
relay_root=/opt/loki-relay
test -s "$relay_bundle/relay.env"
test -s "$relay_bundle/iroh-relay-image.tar.gz"
command -v docker >/dev/null
command -v caddy >/dev/null
if docker container inspect loki-iroh-relay >/dev/null 2>&1; then
  echo '中继容器已存在，请先检查现有部署，不自动覆盖。' >&2
  exit 1
fi
install -d -m 700 "$relay_root"
install -m 600 "$relay_bundle/relay.env" "$relay_root/relay.env"
docker load -i "$relay_bundle/iroh-relay-image.tar.gz"
systemctl enable --now docker
docker run -d --name loki-iroh-relay --restart unless-stopped \
  --read-only --cap-drop ALL --security-opt no-new-privileges:true \
  --pids-limit 128 --memory 384m --cpus 0.5 \
  --env-file "$relay_root/relay.env" \
  -p 127.0.0.1:3340:3340 loki/iroh-relay:1.4.1
curl --fail --retry 10 --retry-connrefused --retry-delay 1 --max-time 3 \
  --silent http://127.0.0.1:3340/ >/dev/null
printf '%s\n' '中继已安装：容器开机自启，后端仅监听 127.0.0.1:3340。'
