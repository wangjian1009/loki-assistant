#!/usr/bin/env python3
"""读取私有环境文件，验证中继上的双向消息和令牌拒绝行为。"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--env-file', type=Path, required=True)
parser.add_argument('--check-denied', action='store_true')
args = parser.parse_args()
environment = dict(os.environ)
for line in args.env_file.read_text().splitlines():
    if line and not line.startswith('#'):
        key, value = line.split('=', 1)
        if key not in {'LOKI_ASSISTANT_RELAY_URL', 'LOKI_ASSISTANT_RELAY_TOKEN'}:
            raise SystemExit('环境文件包含不支持的配置项。')
        environment[key] = value
binary = str(ROOT / 'services/target/debug/loki-services')
log = args.env_file.with_suffix('.services.log')
with log.open('w') as output:
    service = subprocess.Popen([binary, 'network-probe'], env=environment,
                               stdout=output, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 40
        while 'SERVICES_READY' not in log.read_text():
            if service.poll() is not None:
                raise SystemExit(log.read_text())
            if time.monotonic() > deadline:
                raise SystemExit('等待 services 连接中继超时。')
            time.sleep(0.2)
        client = subprocess.run([binary, 'network-probe-client'], env=environment,
                                capture_output=True, text=True, timeout=70, check=True)
        report = json.loads(client.stdout)
        if report.get('ok') is not True or report.get('transport') != 'relay-only':
            raise SystemExit('双向消息验证未通过。')
        deadline = time.monotonic() + 5
        while 'PROBE_PASS' not in log.read_text():
            if time.monotonic() > deadline:
                raise SystemExit('services 未确认双向回执。')
            time.sleep(0.1)
        if args.check_denied:
            invalid = dict(environment)
            invalid['LOKI_ASSISTANT_RELAY_TOKEN'] = 'invalid-local-verification-token'
            denied = subprocess.run([binary, 'network-probe-client'], env=invalid,
                                    capture_output=True, text=True, timeout=70)
            if denied.returncode == 0 or '中继连接超时' not in denied.stderr:
                raise SystemExit('无效访问令牌未按预期被拒绝。')
            report['invalid_token_denied'] = True
        args.env_file.with_suffix('.result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n')
        print(json.dumps(report, ensure_ascii=False))
    finally:
        service.terminate()
        try:
            service.wait(timeout=5)
        except subprocess.TimeoutExpired:
            service.kill()
            service.wait()
