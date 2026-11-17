#!/usr/bin/env python3
"""从已确认的 SVG 母版导出 macOS 应用与菜单栏图标。"""

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


def main() -> None:
    """使用固定母版导出图标，不依赖当前工作目录。"""
    for executable in ("rsvg-convert", "iconutil"):
        if shutil.which(executable) is None:
            raise SystemExit(f"缺少图标导出工具：{executable}")

    assets = Path(__file__).resolve().parent.parent / "Resources" / "AppIcon"
    app_source = assets / "loki-assistant-v1.svg"
    menu_source = assets / "loki-assistant-menu-v1.svg"
    with tempfile.TemporaryDirectory(prefix="loki-icon-export-") as temporary:
        staging = Path(temporary)
        iconset = staging / "loki-assistant-v1.iconset"
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                suffix = "@2x" if scale == 2 else ""
                render(app_source, iconset / f"icon_{points}x{points}{suffix}.png", points * scale)
        render(app_source, staging / "loki-assistant-v1.png", 1024)
        for scale in (1, 2):
            suffix = "@2x" if scale == 2 else ""
            render(menu_source, staging / f"loki-assistant-menu-v1{suffix}.png", 20 * scale)
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(staging / "loki-assistant-v1.icns")],
            check=True,
        )
        # 标准 AppIcon asset catalog 供系统通知等按资源名请求图标。
        catalog = assets.parent / "Assets.xcassets"
        appicon = catalog / "AppIcon.appiconset"
        appicon.mkdir(parents=True, exist_ok=True)
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
        # 全部生成成功后再更新导出文件，SVG 母版始终保留。
        shutil.copytree(iconset, assets / iconset.name, dirs_exist_ok=True)
        for generated in staging.iterdir():
            if generated.is_file():
                shutil.copyfile(generated, assets / generated.name)
    print(f"图标已导出：{assets}")


if __name__ == "__main__":
    main()
