"""Build Nikao Dock icons from assets/brand/nikao-dock-icon.png.

Writes:
  - assets/brand/nikao-dock.ico (dock exe, NSIS installer)
  - apps/settings/src-tauri/icons/* (Tauri bundle)
  - apps/settings/public/favicon* (Settings dev/build favicons)

Usage: python scripts/make_nikao_icon.py
"""

from __future__ import annotations

import shutil
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "brand" / "nikao-dock-icon.png"
BRAND = ROOT / "assets" / "brand"
SETTINGS_ICONS = ROOT / "apps" / "settings" / "src-tauri" / "icons"
SETTINGS_PUBLIC = ROOT / "apps" / "settings" / "public"

ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)
TAURI_PNGS = {
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 512,
}


def rgba_master() -> Image.Image:
    master = Image.open(SOURCE).convert("RGBA")
    if master.size != (1024, 1024):
        master = master.resize((1024, 1024), Image.LANCZOS)
    return master


def resize(master: Image.Image, size: int) -> Image.Image:
    return master.resize((size, size), Image.LANCZOS)


def write_ico(master: Image.Image, path: Path) -> None:
    frames = [resize(master, size) for size in ICO_SIZES]
    frames[-1].save(
        path,
        sizes=[(size, size) for size in ICO_SIZES],
        append_images=frames[:-1],
        bitmap_format="png",
    )


def main() -> None:
    if not SOURCE.is_file():
        raise SystemExit(f"missing source icon: {SOURCE}")
    master = rgba_master()
    SETTINGS_ICONS.mkdir(parents=True, exist_ok=True)
    SETTINGS_PUBLIC.mkdir(parents=True, exist_ok=True)

    write_ico(master, BRAND / "nikao-dock.ico")
    write_ico(master, SETTINGS_ICONS / "icon.ico")

    for name, size in TAURI_PNGS.items():
        resize(master, size).save(SETTINGS_ICONS / name, optimize=True)

    for name, size in (
        ("Square30x30Logo.png", 30),
        ("Square44x44Logo.png", 44),
        ("Square71x71Logo.png", 71),
        ("Square89x89Logo.png", 89),
        ("Square107x107Logo.png", 107),
        ("Square142x142Logo.png", 142),
        ("Square150x150Logo.png", 150),
        ("Square284x284Logo.png", 284),
        ("Square310x310Logo.png", 310),
        ("StoreLogo.png", 50),
    ):
        resize(master, size).save(SETTINGS_ICONS / name, optimize=True)

    resize(master, 32).save(SETTINGS_PUBLIC / "favicon-32x32.png", optimize=True)
    resize(master, 16).save(SETTINGS_PUBLIC / "favicon-16x16.png", optimize=True)
    resize(master, 180).save(SETTINGS_PUBLIC / "apple-touch-icon.png", optimize=True)
    resize(master, 512).save(SETTINGS_PUBLIC / "favicon.png", optimize=True)
    shutil.copyfile(SOURCE, SETTINGS_PUBLIC / "app-icon.png")

    print("wrote nikao-dock.ico, Tauri icons and Settings favicons")


if __name__ == "__main__":
    main()