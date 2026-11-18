#!/usr/bin/env python3
"""校验上游发布包并构建 ECS 使用的最小 Linux AMD64 中继镜像。"""
import hashlib
import pathlib
import shutil
import subprocess
import tarfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
VERSION = '1.4.1'
SHA256 = '75254641e3fad7fcbac6dee4d485556fbe06ef708340a9d8810469270ba626b8'
directory = ROOT / '.build/relay-ecs'
context = directory / 'image'
context.mkdir(parents=True, exist_ok=True)
archive = directory / f'iroh-relay-v{VERSION}-x86_64-unknown-linux-musl.tar.gz'
if not archive.exists():
    subprocess.run(['curl', '--fail', '--location', '--retry', '2', '--max-time', '60',
                    '-o', str(archive),
                    f'https://github.com/n0-computer/iroh/releases/download/v{VERSION}/{archive.name}'], check=True)
if hashlib.file_digest(archive.open('rb'), 'sha256').hexdigest() != SHA256:
    raise SystemExit('中继发布包 SHA256 不匹配，请删除缓存后重新下载。')
with tarfile.open(archive) as package:
    members = [m for m in package.getmembers() if m.isfile() and pathlib.PurePosixPath(m.name).name == 'iroh-relay']
    if len(members) != 1:
        raise SystemExit('发布包中的中继可执行文件不唯一。')
    with package.extractfile(members[0]) as source, (context / 'iroh-relay').open('wb') as target:
        shutil.copyfileobj(source, target)
(context / 'iroh-relay').chmod(0o755)
shutil.copy2(ROOT / 'services/relay/relay.toml', context / 'relay.toml')
subprocess.run(['docker', 'build', '--platform', 'linux/amd64', '-t', f'loki/iroh-relay:{VERSION}',
                '-f', str(ROOT / 'services/relay/Dockerfile'), str(context)], check=True)
print(f'已构建并校验 loki/iroh-relay:{VERSION}，发布包 SHA256：{SHA256}')
