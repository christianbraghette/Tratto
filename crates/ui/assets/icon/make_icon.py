# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Renders the app icon: the title bar's pen nib (Phosphor PEN_NIB, U+E3AC)
in the accent colour on a transparent background.

    python make_icon.py <Phosphor.ttf> <Phosphor-Bold.ttf>

writes icon.png (256 px, the window icon), icon.ico (16-256 px, the .exe
icon) and icon.icns (16-1024 px, the macOS app bundle's icon). Small sizes use the bold weight, so the thin strokes stay visible.
Needs Pillow.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

GLYPH = ""
ACCENT = (77, 139, 255, 255)  # theme::color::ACCENT
SUPERSAMPLE = 1024
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
ICNS_SIZES = [16, 32, 64, 128, 256, 512, 1024]
BOLD_UP_TO = 32


def render(font_path: str, size: int) -> Image.Image:
    """The glyph centred on a square canvas, filling about 90 % of it."""
    big = SUPERSAMPLE
    font = ImageFont.truetype(font_path, int(big * 0.9))
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    left, top, right, bottom = draw.textbbox((0, 0), GLYPH, font=font)
    x = (big - (right - left)) / 2 - left
    y = (big - (bottom - top)) / 2 - top
    draw.text((x, y), GLYPH, font=font, fill=ACCENT)
    return img.resize((size, size), Image.Resampling.LANCZOS)


def main() -> None:
    regular, bold = sys.argv[1], sys.argv[2]
    out = Path(__file__).parent
    render(regular, 256).save(out / "icon.png")
    frames = [render(bold if s <= BOLD_UP_TO else regular, s) for s in ICO_SIZES]
    # Largest first; the other frames are stored as they are, not rescaled.
    frames[-1].save(out / "icon.ico", format="ICO", sizes=[(s, s) for s in ICO_SIZES], append_images=frames[:-1])
    frames = [render(bold if s <= BOLD_UP_TO else regular, s) for s in ICNS_SIZES]
    frames[-1].save(out / "icon.icns", format="ICNS", append_images=frames[:-1])


if __name__ == "__main__":
    main()
