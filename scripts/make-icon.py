"""Draws the HT Plugin Manager app icon in the house style: assets/AppIcon.icns (macOS),
AppIcon.ico (Windows) and AppIcon.png (window icon).

The graphite sheet, an H/T wordmark in Plex Sans Condensed SemiBold with the
slash stepped back to inkFaint, centred, and the four crop marks. Run from the repo root:
    python3 scripts/make-icon.py
"""

import pathlib
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
FONT = ROOT / "assets/fonts/IBMPlexSansCondensed-SemiBold.ttf"

SHEET = (0x11, 0x11, 0x11, 255)
INK = (0xE6, 0xE6, 0xE6, 255)
INK_FAINT = (0x57, 0x57, 0x57, 255)
HAIRLINE = (0x2C, 0x2C, 0x2C, 255)


def draw(size: int = 1024) -> Image.Image:
    s = size / 1024
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # macOS icon grid: 824 px body, 100 px margin, ~185 px corner radius.
    body = (100 * s, 100 * s, 924 * s, 924 * s)
    d.rounded_rectangle(body, radius=185 * s, fill=SHEET, outline=HAIRLINE, width=max(1, round(4 * s)))

    # Crop marks, inset like the sheet's.
    inset, length, w = 214 * s, 64 * s, max(1, round(10 * s))
    for (x, y, dx, dy) in [(inset, inset, 1, 1), (1024 * s - inset, inset, -1, 1),
                           (inset, 1024 * s - inset, 1, -1), (1024 * s - inset, 1024 * s - inset, -1, -1)]:
        d.line([(x, y), (x + length * dx, y)], fill=INK_FAINT, width=w)
        d.line([(x, y), (x, y + length * dy)], fill=INK_FAINT, width=w)

    # H / T, set tight, centred on the body.
    font = ImageFont.truetype(str(FONT), round(330 * s))
    parts = [("H", INK), ("/", INK_FAINT), ("T", INK)]
    gap = 6 * s
    widths = [d.textlength(t, font=font) for t, _ in parts]
    total = sum(widths) + gap * (len(parts) - 1)
    x = (size - total) / 2
    _, top, _, bottom = d.textbbox((0, 0), "H", font=font)
    y = 512 * s - (bottom + top) / 2
    for (t, c), wdt in zip(parts, widths):
        d.text((x, y), t, font=font, fill=c)
        x += wdt + gap
    return img


def main() -> None:
    master = draw(1024)
    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for base in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = base * scale
                name = f"icon_{base}x{base}{'@2x' if scale == 2 else ''}.png"
                master.resize((px, px), Image.LANCZOS).save(iconset / name)
        out = ROOT / "assets/AppIcon.icns"
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)
    master.save(ROOT / "assets/AppIcon.png")
    # Windows: exe resource and installer icon.
    master.save(ROOT / "assets/AppIcon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print(f"wrote {out}, AppIcon.png, AppIcon.ico")


if __name__ == "__main__":
    main()
