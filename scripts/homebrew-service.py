#!/usr/bin/env python3
"""通过本地 Homebrew formula 安装和管理 services，迁移旧的自管 LaunchAgent。"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[1]
TAP = "loki-assistant/local"
NAME = "loki-assistant-services"
FORMULA = f"{TAP}/{NAME}"
LEGACY_LABEL = "cc.loki-assistant.services"
MARKER = "# 由 scripts/homebrew-service.py 为当前本机构建生成。"
ENV_KEYS = ("LOKI_ASSISTANT_HTTP_ADDR", "LOKI_ASSISTANT_CENTER_EXECUTABLE", "LOKI_ASSISTANT_RELAY_TOKEN")


def run(command, *, capture=False, check=True):
    environment = os.environ.copy()
    environment.update(HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_INSTALL_CLEANUP="1", HOMEBREW_NO_AUTOREMOVE="1")
    return subprocess.run(command, cwd=ROOT, env=environment, check=check,
                          text=True, stdout=subprocess.PIPE if capture else None,
                          stderr=subprocess.PIPE if capture else None)


def legacy_settings():
    path = Path.home() / "Library/LaunchAgents" / f"{LEGACY_LABEL}.plist"
    if not path.exists():
        return path, {}
    with path.open("rb") as source:
        settings = plistlib.load(source)
    if settings.get("Label") != LEGACY_LABEL:
        raise RuntimeError("旧服务 plist 的 Label 不匹配，停止迁移")
    return path, settings.get("EnvironmentVariables", {})


def preserve_environment(settings):
    values = {key: os.environ.get(key, settings.get(key)) for key in ENV_KEYS}
    values = {key: value for key, value in values.items() if value is not None}
    if not values:
        return
    for key, value in values.items():
        if "\n" in value or "\r" in value:
            raise RuntimeError(f"{key} 包含换行，无法迁移环境设置")
    directory = Path(os.environ.get("HOMEBREW_USER_CONFIG_HOME", str(Path.home() / ".homebrew"))) / "services"
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = directory / f"{NAME}.env"
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        return  # 保留已有的 Homebrew 服务环境设置。
    with os.fdopen(descriptor, "w") as target:
        for key, value in values.items():
            target.write(f"{key}={value}\n")
    print("已将旧服务的环境设置保存到 Homebrew 用户配置，权限为 0600。", flush=True)


def remove_legacy(path):
    target = f"gui/{os.getuid()}/{LEGACY_LABEL}"
    loaded = run(["/bin/launchctl", "print", target], capture=True, check=False).returncode == 0
    if loaded:
        run(["/bin/launchctl", "bootout", target])
    if path.exists():
        path.unlink()
    if loaded:
        print("已卸载旧 services LaunchAgent；托盘 LaunchAgent 保留。", flush=True)


def install(brew, cargo):
    legacy_path, settings = legacy_settings()
    manifest = ROOT / "services/Cargo.toml"
    run([cargo, "build", "--manifest-path", str(manifest)])
    metadata = json.loads(run([cargo, "metadata", "--no-deps", "--format-version", "1", "--manifest-path", str(manifest)], capture=True).stdout)
    package = next(package for package in metadata["packages"] if package["name"] == "loki-services")
    binary = Path(metadata["target_directory"]) / "debug/loki-services"
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    version = f'{package["version"]}-{digest[:12]}'
    directory = ROOT / ".build/homebrew"
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory / f"{NAME}-{version}.tar.gz"
    if not archive.exists():
        with tarfile.open(archive, "w:gz", compresslevel=1) as target:
            target.add(binary, arcname="loki-services")
    taps = run([brew, "tap"], capture=True).stdout.splitlines()
    if TAP not in taps:
        run([brew, "tap-new", "--no-git", TAP])
    tap = Path(run([brew, "--repository", TAP], capture=True).stdout.strip())
    formula = tap / "Formula" / f"{NAME}.rb"
    if formula.exists() and not formula.read_text().startswith(MARKER):
        raise RuntimeError(f"已有同名 formula 不属于本项目：{formula}")
    template = (ROOT / "services/homebrew/loki-assistant-services.rb.in").read_text()
    replacements = {
        "ARCHIVE_URL": archive.as_uri(), "VERSION": version,
        "SHA256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "CENTER_EXECUTABLE": str(ROOT / "apps/center/mac/.build/Debug/LokiAssistant.app/Contents/MacOS/LokiAssistant"),
    }
    for key, value in replacements.items():
        template = template.replace(f"@{key}@", json.dumps(value, ensure_ascii=False))
    formula.parent.mkdir(parents=True, exist_ok=True)
    formula.write_text(template)
    installed = run([brew, "list", "--versions", "--formula", FORMULA], capture=True, check=False)
    versions = installed.stdout.split()[1:]
    if version not in versions:
        run([brew, "reinstall" if versions else "install", "--formula", "--build-from-source", FORMULA])
    (Path.home() / "Library/Logs/LokiAssistant").mkdir(parents=True, exist_ok=True)
    preserve_environment(settings)
    # 新 formula 安装完成后再卸载旧任务，避免安装失败影响正在运行的服务。
    remove_legacy(legacy_path)
    run([brew, "services", "restart", FORMULA])
    run([brew, "services", "info", FORMULA])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("install", "uninstall", "start", "stop", "restart", "status"))
    parser.add_argument("--brew", default="brew")
    parser.add_argument("--cargo", default="cargo")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("当前服务安装入口仅支持 macOS")
    if args.action in ("install", "restart"):
        install(args.brew, args.cargo)
    elif args.action == "uninstall":
        installed = run([args.brew, "list", "--versions", "--formula", FORMULA], capture=True, check=False)
        if installed.stdout.strip():
            run([args.brew, "services", "stop", FORMULA])
            run([args.brew, "uninstall", "--formula", FORMULA])
        print("已注销 services；用户配置、身份、托盘设置与日志保留。")
    else:
        action = "info" if args.action == "status" else args.action
        run([args.brew, "services", action, FORMULA])


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        print(f"服务管理失败：{error}", file=sys.stderr)
        sys.exit(1)
