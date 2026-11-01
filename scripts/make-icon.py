# /// script
# dependencies = ["pillow", "resvg-py"]
# ///
"""Renders Kiku's icon from assets/icon/kiku.svg into kiku.ico and preview PNGs beside it.

Small sizes come from simplified cuts (kiku-24.svg for 20 and 24 px, kiku-16.svg for 16 px) with fewer
highlights and a stronger rim, so they stay legible in the tray and title bars.
The .ico is committed and embedded in kiku.exe by crates/app/build.rs; run this after changing an SVG.

Usage: uv run scripts/make-icon.py
"""

import io
from pathlib import Path

import resvg_py
from PIL import Image

ICON = Path(__file__).resolve().parent.parent / "assets" / "icon"
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
PREVIEW_SIZES = [16, 32, 256]


def render(size: int) -> Image.Image:
    source = ICON / ("kiku-16.svg" if size <= 16 else "kiku-24.svg" if size <= 24 else "kiku.svg")
    png = resvg_py.svg_to_bytes(svg_path=str(source), width=size, height=size)
    return Image.open(io.BytesIO(bytes(png))).convert("RGBA")


def main() -> None:
    images = {size: render(size) for size in ICO_SIZES}
    largest = images[max(ICO_SIZES)]
    # Each size is its own rendering; Pillow would otherwise scale the largest down.
    others = [image for size, image in images.items() if image is not largest]
    largest.save(ICON / "kiku.ico", sizes=[(size, size) for size in ICO_SIZES], append_images=others)
    for size in PREVIEW_SIZES:
        images[size].save(ICON / f"kiku-{size}.png")


if __name__ == "__main__":
    main()
