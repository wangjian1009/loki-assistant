#!/usr/bin/env python3
"""从公共 SVG 母版导出 macOS、HarmonyOS 与 Chrome 插件所需的图标。"""

from pathlib import Path
import shutil
import json
import subprocess
import tempfile


def render(source: Path, output: Path, size: int, *, stylesheet: str = "") -> None:
    """按指定像素尺寸渲染 PNG，保留透明通道。"""
    command = ["rsvg-convert", "-w", str(size), "-h", str(size), "-o", str(output)]
    if stylesheet:
        command.extend(["--stylesheet", "/dev/stdin"])
    subprocess.run([*command, str(source)], input=stylesheet, text=True, check=True)


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


def export_harmony(app_source: Path, app_png: Path, media: Path) -> None:
    """导出由 HarmonyOS 裁切的桌面分层图标及静态启动图标。"""
    media.mkdir(parents=True)
    shutil.copyfile(app_png, media / "app_icon.png")
    render(app_source, media / "app_icon_background.png", 1024, stylesheet="""
        #rounded-background, #foreground { display: none !important; }
        #harmony-background { display: inline !important; }
    """)
    render(app_source, media / "app_icon_foreground.png", 1024, stylesheet="""
        #rounded-background { display: none !important; }
        #foreground { transform: matrix(1.25, 0, 0, 1.25, -128, -145); }
    """)


def export_chrome(app_source: Path, icons: Path) -> None:
    """使用 Chrome 专用彩色母版，确保放大的主体在深浅主题下均可辨认。"""
    icons.mkdir(parents=True)
    for size in (16, 20, 24, 32, 48, 64, 128):
        render(app_source, icons / f"icon-{size}.png", size)


def main() -> None:
    """所有格式生成成功后再更新各应用资源，不依赖当前工作目录。"""
    for executable in ("rsvg-convert", "iconutil"):
        if shutil.which(executable) is None:
            raise SystemExit(f"缺少图标导出工具：{executable}")

    root = Path(__file__).resolve().parent.parent
    sources = root / "resources" / "icons"
    mac_resources = Path("apps/center/mac/Resources")
    harmony_media = Path("harmony/AppScope/resources/base/media")
    chrome_icons = Path("chrome-plugin/extension/icons")
    with tempfile.TemporaryDirectory(prefix="loki-icon-export-") as temporary:
        staging = Path(temporary)
        app_png = export_macos(sources / "loki-assistant-v1.svg",
                               sources / "loki-assistant-menu-v1.svg",
                               sources / "loki-assistant-menu-sleep-v1.svg", staging / mac_resources)
        export_harmony(sources / "loki-assistant-v1.svg", app_png, staging / harmony_media)
        export_chrome(sources / "loki-assistant-chrome-v1.svg", staging / chrome_icons)
        notifications = staging / mac_resources / "NotificationAppIcons"
        notifications.mkdir()
        for app in ("mail", "slack"):
            render(sources / f"{app}.svg", notifications / f"{app}.png", 128)
        for generated in staging.rglob("*"):
            if generated.is_file():
                output = root / generated.relative_to(staging)
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(generated, output)
    print(f"macOS 图标已导出：{root / mac_resources}")
    print(f"HarmonyOS 图标已导出：{root / harmony_media}")
    print(f"Chrome 图标已导出：{root / chrome_icons}")


if __name__ == "__main__":
    main()
