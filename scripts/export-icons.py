#!/usr/bin/env python3
"""从公共 SVG 母版导出 macOS 与 HarmonyOS 应用所需的图标。"""

from pathlib import Path
import shutil
import json
import subprocess
import tempfile


def render(source: Path, output: Path, size: int) -> None:
    """按指定像素尺寸渲染 PNG，保留透明通道。"""
    subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), str(source), "-o", str(output)],
        check=True,
    )


def export_macos(app_source: Path, menu_source: Path, sleep_source: Path, resources: Path) -> Path:
    """生成 macOS iconset、ICNS、asset catalog 和菜单栏图标。"""
    assets = resources / "AppIcon"
    iconset = assets / "loki-assistant-v1.iconset"
    iconset.mkdir(parents=True)
    for points in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            suffix = "@2x" if scale == 2 else ""
            render(app_source, iconset / f"icon_{points}x{points}{suffix}.png", points * scale)
    app_png = assets / "loki-assistant-v1.png"
    render(app_source, app_png, 1024)
    for scale in (1, 2):
        suffix = "@2x" if scale == 2 else ""
        render(menu_source, assets / f"loki-assistant-menu-v1{suffix}.png", 20 * scale)
        render(sleep_source, assets / f"loki-assistant-menu-sleep-v1{suffix}.png", 20 * scale)
    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset), "-o", str(assets / "loki-assistant-v1.icns")],
        check=True,
    )
    # 标准 AppIcon asset catalog 供系统通知等按资源名请求图标。
    catalog = resources / "Assets.xcassets"
    appicon = catalog / "AppIcon.appiconset"
    appicon.mkdir(parents=True)
    images = []
    for points in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            suffix = "@2x" if scale == 2 else ""
            filename = f"icon_{points}x{points}{suffix}.png"
            shutil.copyfile(iconset / filename, appicon / filename)
            images.append({"filename": filename, "idiom": "mac",
                           "scale": f"{scale}x", "size": f"{points}x{points}"})
    metadata = {"author": "xcode", "version": 1}
    (catalog / "Contents.json").write_text(
        json.dumps({"info": metadata}, indent=2) + "\n", encoding="utf-8")
    (appicon / "Contents.json").write_text(
        json.dumps({"images": images, "info": metadata}, indent=2) + "\n", encoding="utf-8")
    return app_png


def export_harmony(app_png: Path, media: Path) -> None:
    """HarmonyOS 应用、Ability 和启动窗口复用同一张 1024 像素 PNG。"""
    media.mkdir(parents=True)
    shutil.copyfile(app_png, media / "app_icon.png")


def main() -> None:
    """所有格式生成成功后再更新各应用资源，不依赖当前工作目录。"""
    for executable in ("rsvg-convert", "iconutil"):
        if shutil.which(executable) is None:
            raise SystemExit(f"缺少图标导出工具：{executable}")

    root = Path(__file__).resolve().parent.parent
    sources = root / "resources" / "icons"
    mac_resources = Path("apps/center/mac/Resources")
    harmony_media = Path("apps/harmony/AppScope/resources/base/media")
    with tempfile.TemporaryDirectory(prefix="loki-icon-export-") as temporary:
        staging = Path(temporary)
        app_png = export_macos(sources / "loki-assistant-v1.svg",
                               sources / "loki-assistant-menu-v1.svg",
                               sources / "loki-assistant-menu-sleep-v1.svg", staging / mac_resources)
        export_harmony(app_png, staging / harmony_media)
        for generated in staging.rglob("*"):
            if generated.is_file():
                output = root / generated.relative_to(staging)
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(generated, output)
    print(f"macOS 图标已导出：{root / mac_resources}")
    print(f"HarmonyOS 图标已导出：{root / harmony_media / 'app_icon.png'}")


if __name__ == "__main__":
    main()
