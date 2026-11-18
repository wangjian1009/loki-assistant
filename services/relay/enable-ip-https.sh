#!/bin/sh
# 使用公网 IP 证书配置 HTTPS，不依赖 DNS，不升级宿主机 Caddy。
set -eu
if [ "$(id -u)" != 0 ]; then
  echo '请使用 sudo 运行 IP HTTPS 配置脚本。' >&2
  exit 1
fi
relay_ip=${1:?需要指定本机公网 IPv4 地址}
relay_port=${2:-8443}
relay_root=/opt/loki-relay
relay_webroot=/var/lib/loki-relay/acme
relay_certdir=/etc/caddy/loki-relay
python3 - "$relay_ip" "$relay_port" <<'PY'
import ipaddress, sys
ip = ipaddress.ip_address(sys.argv[1])
if ip.version != 4 or not ip.is_global:
    raise SystemExit('需要公网 IPv4 地址。')
if not sys.argv[2].isdigit() or not 1024 <= int(sys.argv[2]) <= 65535:
    raise SystemExit('端口必须介于 1024 和 65535 之间。')
PY
command -v caddy >/dev/null
command -v docker >/dev/null
install -d -m 700 "$relay_root"
install -d -m 755 "$relay_webroot/.well-known/acme-challenge"
install -d -m 750 -o root -g caddy "$relay_certdir"

# 为 HTTP-01 单独增加 IP 站点，保留现有域名站点。
relay_candidate=$(mktemp /etc/caddy/Caddyfile.loki-ip.XXXXXX)
trap 'rm -f "$relay_candidate"' EXIT HUP INT TERM
python3 - "$relay_ip" "$relay_webroot" "$relay_candidate" <<'PY'
from pathlib import Path
import re,sys
ip, root, output = sys.argv[1:]
text = Path('/etc/caddy/Caddyfile').read_text()
text = re.sub(r'(?ms)^# BEGIN LOKI IP ACME\n.*?^# END LOKI IP ACME\n?', '', text)
text += f'''\n# BEGIN LOKI IP ACME
http://{ip} {{
    handle /.well-known/acme-challenge/* {{
        root * {root}
        file_server
    }}
    handle {{
        respond 404
    }}
}}
# END LOKI IP ACME
'''
Path(output).write_text(text)
PY
caddy validate --adapter caddyfile --config "$relay_candidate"
cp -p /etc/caddy/Caddyfile "$relay_root/Caddyfile.before-ip.$(date +%Y%m%d%H%M%S)"
install -m 644 "$relay_candidate" /etc/caddy/Caddyfile
systemctl reload caddy
printf '%s\n' loki-relay-acme-check > "$relay_webroot/.well-known/acme-challenge/loki-check"
relay_check=$(curl --fail --silent --show-error --noproxy '*' --connect-timeout 5 --max-time 10 \
  "http://$relay_ip/.well-known/acme-challenge/loki-check")
test "$relay_check" = loki-relay-acme-check
rm -f "$relay_webroot/.well-known/acme-challenge/loki-check"

# 使用独立 venv，避免更改系统 Python 包或升级既有 Caddy。
if ! test -x "$relay_root/certbot/bin/python"; then
  if ! python3 -m venv "$relay_root/certbot"; then
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0 install -y --no-install-recommends python3-venv
    python3 -m venv "$relay_root/certbot"
  fi
fi
"$relay_root/certbot/bin/python" -m pip install --disable-pip-version-check \
  --index-url https://mirrors.aliyun.com/pypi/simple --timeout 30 --retries 2 'certbot==5.10.0'

cat > "$relay_root/deploy-ip-certificate.sh" <<'EOF'
#!/bin/sh
set -eu
test -n "${RENEWED_LINEAGE:-}"
install -m 640 -o root -g caddy "$RENEWED_LINEAGE/fullchain.pem" /etc/caddy/loki-relay/fullchain.pem
install -m 640 -o root -g caddy "$RENEWED_LINEAGE/privkey.pem" /etc/caddy/loki-relay/privkey.pem
caddy validate --adapter caddyfile --config /etc/caddy/Caddyfile
systemctl reload caddy
EOF
chmod 700 "$relay_root/deploy-ip-certificate.sh"

# 先使用测试 CA 验证，再申请正式短期 IP 证书。
"$relay_root/certbot/bin/certbot" certonly --staging --non-interactive --agree-tos \
  --register-unsafely-without-email --preferred-profile shortlived \
  --webroot --webroot-path "$relay_webroot" --ip-address "$relay_ip" \
  --cert-name loki-relay-ip --keep-until-expiring \
  --config-dir "$relay_root/letsencrypt-staging" \
  --work-dir "$relay_root/certbot-staging-work" --logs-dir "$relay_root/certbot-staging-logs"
"$relay_root/certbot/bin/certbot" certonly --non-interactive --agree-tos \
  --register-unsafely-without-email --preferred-profile shortlived \
  --webroot --webroot-path "$relay_webroot" --ip-address "$relay_ip" \
  --cert-name loki-relay-ip --keep-until-expiring \
  --config-dir "$relay_root/letsencrypt" \
  --work-dir "$relay_root/certbot-work" --logs-dir "$relay_root/certbot-logs" \
  --deploy-hook "$relay_root/deploy-ip-certificate.sh"

# 只替换本工具维护的中继入口，不触及既有站点。
python3 - "$relay_ip" "$relay_port" "$relay_certdir" "$relay_candidate" <<'PY'
from pathlib import Path
import re,sys
ip, port, certdir, output = sys.argv[1:]
text = Path('/etc/caddy/Caddyfile').read_text()
text = re.sub(r'(?ms)^# BEGIN LOKI RELAY [^\n]+\n.*?^# END LOKI RELAY\n?', '', text)
# IP 连接通常不发送 SNI，NAT 下的本机地址又不同于公网 IP，必须指定回退名称。
existing = re.search(r'(?m)^\s*default_sni\s+(\S+)', text)
if existing and existing.group(1) != ip:
    raise SystemExit('已有不同的 default_sni 配置，不自动覆盖。')
if not existing:
    if re.match(r'^\s*\{', text):
        text = re.sub(r'^(\s*\{)', lambda m: m.group(1) + f'\n    default_sni {ip}', text, count=1)
    else:
        text = f'{{\n    default_sni {ip}\n}}\n\n' + text
text += f'''\n# BEGIN LOKI RELAY {ip}:{port}
https://{ip}:{port} {{
    tls {certdir}/fullchain.pem {certdir}/privkey.pem
    reverse_proxy 127.0.0.1:3340
}}
# END LOKI RELAY
'''
Path(output).write_text(text)
PY
caddy validate --adapter caddyfile --config "$relay_candidate"
cp -p /etc/caddy/Caddyfile "$relay_root/Caddyfile.before-ip-tls.$(date +%Y%m%d%H%M%S)"
install -m 644 "$relay_candidate" /etc/caddy/Caddyfile
systemctl reload caddy

cat > /etc/systemd/system/loki-relay-cert-renew.service <<EOF
[Unit]
Description=Loki relay IP certificate renewal
After=network-online.target caddy.service
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=$relay_root/certbot/bin/certbot renew --quiet --config-dir $relay_root/letsencrypt --work-dir $relay_root/certbot-work --logs-dir $relay_root/certbot-logs --deploy-hook $relay_root/deploy-ip-certificate.sh
EOF
cat > /etc/systemd/system/loki-relay-cert-renew.timer <<'EOF'
[Unit]
Description=Loki relay IP certificate renewal schedule

[Timer]
OnBootSec=10min
OnCalendar=*-*-* 00,12:00:00
RandomizedDelaySec=1h
Persistent=true

[Install]
WantedBy=timers.target
EOF
systemctl daemon-reload
systemctl enable --now loki-relay-cert-renew.timer
printf 'IP 中继已配置：https://%s:%s；证书每日两次检查自动续期。\n' "$relay_ip" "$relay_port"
