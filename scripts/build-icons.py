#!/usr/bin/env python3
"""Regenerate Mori's icons from the supplied branding artwork.

Development tool only (not used by the app at runtime). Requires Pillow and
NumPy; building the macOS .icns additionally requires Apple's `iconutil`
(ships with macOS).

Inputs (original artwork, kept out of Git because of its size):
    assets/branding/source/mori-logo-light-back.png   dark folder on a light background
    assets/branding/source/mori-logo-dark-back.png    light folder on a dark background

Outputs:
    assets/branding/mori-app-icon.png    1024 macOS app icon master (squircle)
    assets/branding/mori-icon-black.png   512 transparent dark folder (primary mark)
    assets/branding/mori-icon-white.png   512 transparent light folder
    public/mori-icon-black.png            160 in-app mark for the light UI
    public/mori-icon-white.png            160 in-app mark for the dark UI
    src-tauri/icons/icon.icns             macOS (squircle; built with iconutil)
    src-tauri/icons/icon.ico              Windows (free-form folder, 16-256)
    src-tauri/icons/{32x32,128x128,128x128@2x,icon}.png   Linux / window icon

Why not just `tauri icon`? It scales one image to every size. A free-form
(non-squircle) icon is shrunk onto a grey plate by macOS 26, and plain
downscaling turns the pixel-art character into a smudge at 16-32 px. Here the
macOS icon follows Apple's icon grid and small sizes get a tighter crop plus
light sharpening.
"""

import shutil
import subprocess
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "assets/branding/source"
BRAND = ROOT / "assets/branding"
PUBLIC = ROOT / "public"
ICONS = ROOT / "src-tauri/icons"

LIGHT_BACK = SRC / "mori-logo-light-back.png"  # dark folder on light background
DARK_BACK = SRC / "mori-logo-dark-back.png"  # light folder on dark background


# ----------------------------------------------------------------- cut-outs


def cut_out(path: Path, tolerance: int) -> Image.Image:
    """Remove the flat background around the folder.

    The background is flood-filled from the image edges, so pixels *inside*
    the folder (the character's black or white areas) are never touched.
    Semi-transparent edge pixels are then colour-decontaminated: they take
    their colour from neighbouring solid folder pixels instead of keeping the
    old background colour, which would otherwise show up as a light or dark
    fringe on the opposite UI background.
    """
    rgb = Image.open(path).convert("RGB")
    w, h = rgb.size
    px = np.asarray(rgb).astype(np.int32)
    # 1. Pixels close to the background colour (sampled at a corner); the
    #    distance is summed over R+G+B.
    near_bg = np.abs(px - px[2, 2]).sum(-1) <= tolerance
    # 2. Keep only the near-background regions connected to the image border,
    #    via an exact flood fill on that binary map (so nothing enclosed by the
    #    folder outline can ever be removed).
    # (.copy(): images made by fromarray share NumPy's read-only buffer, and
    # an in-place flood fill on them is silently lost.)
    work = Image.fromarray(np.where(near_bg, 255, 0).astype(np.uint8), "L").copy()
    for seed in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1), (w // 2, 0), (w // 2, h - 1), (0, h // 2), (w - 1, h // 2)]:
        if work.getpixel(seed) == 255:
            ImageDraw.floodfill(work, seed, 128)
    bg = np.asarray(work) == 128

    alpha = Image.fromarray(np.where(bg, 0, 255).astype(np.uint8))
    alpha = alpha.filter(ImageFilter.MinFilter(3)).filter(ImageFilter.GaussianBlur(0.9))
    a = np.asarray(alpha).astype(np.float32)

    # Decontaminate: colour of partially transparent pixels = weighted average
    # of nearby fully-opaque pixels.
    src = np.asarray(rgb).astype(np.float32)
    solid = (a >= 254).astype(np.float32)
    weight = np.asarray(Image.fromarray((solid * 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(4))).astype(np.float32)
    clean = src.copy()
    for c in range(3):
        premult = Image.fromarray(np.clip(src[..., c] * solid, 0, 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(4))
        spread = np.asarray(premult).astype(np.float32) / np.maximum(weight / 255.0, 1e-3)
        edge = (a < 254) & (a > 0)
        clean[..., c] = np.where(edge, np.clip(spread, 0, 255), src[..., c])
    out = Image.fromarray(np.dstack([clean, a]).astype(np.uint8), "RGBA")
    return out.crop(out.getchannel("A").point(lambda v: 255 if v > 8 else 0).getbbox())


def fit_square(img: Image.Image, size: int, fill: float, sharpen: bool = False) -> Image.Image:
    """Centre `img` on a transparent square, its longest side = fill * size."""
    scale = fill * size / max(img.size)
    w, h = max(1, round(img.width * scale)), max(1, round(img.height * scale))
    small = img.resize((w, h), Image.LANCZOS)
    if sharpen:
        small = small.filter(ImageFilter.UnsharpMask(radius=0.8, percent=90, threshold=1))
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.alpha_composite(small, ((size - w) // 2, (size - h) // 2))
    return canvas


# ------------------------------------------------------------- macOS icon


def squircle_mask(size: int, inset: int, radius: int) -> Image.Image:
    ss = 4
    m = Image.new("L", (size * ss, size * ss), 0)
    ImageDraw.Draw(m).rounded_rectangle((inset * ss, inset * ss, (size - inset) * ss - 1, (size - inset) * ss - 1), radius=radius * ss, fill=255)
    return m.resize((size, size), Image.LANCZOS)


def mac_icon(size: int, folder_fill: float) -> Image.Image:
    """The supplied light-background composition inside Apple's icon grid
    (824/1024 squircle with transparent margins), folder centred."""
    src = Image.open(LIGHT_BACK).convert("RGB")
    # Locate the folder: everything clearly different from the background colour.
    rgb = np.asarray(src).astype(np.int16)
    bg_colour = rgb[5, 5]
    ys, xs = np.where(np.abs(rgb - bg_colour).sum(-1) > 60)
    cx, cy = (xs.min() + xs.max()) / 2, (ys.min() + ys.max()) / 2
    side = (xs.max() - xs.min()) / folder_fill
    # Square crop centred on the folder (padded with the background if needed).
    box = (round(cx - side / 2), round(cy - side / 2), round(cx + side / 2), round(cy + side / 2))
    plate_src = Image.new("RGB", (box[2] - box[0], box[3] - box[1]), tuple(int(v) for v in bg_colour))
    plate_src.paste(src, (-box[0], -box[1]))

    inset = round(size * 100 / 1024)
    plate = plate_src.resize((size - 2 * inset, size - 2 * inset), Image.LANCZOS)
    if size <= 64:
        plate = plate.filter(ImageFilter.UnsharpMask(radius=0.8, percent=90, threshold=1))
    icon = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    icon.paste(plate, (inset, inset))
    icon.putalpha(squircle_mask(size, inset, round(size * 185 / 1024)))
    return icon


def build_icns(dest: Path) -> None:
    if not shutil.which("iconutil"):
        raise SystemExit("iconutil not found: the .icns can only be built on macOS")
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "Mori.iconset"
        iconset.mkdir()
        for pt in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = pt * scale
                # Folder fills more of the plate at tiny sizes so the character stays legible.
                fill = 0.92 if px <= 32 else 0.86 if px <= 64 else 0.80
                name = f"icon_{pt}x{pt}{'@2x' if scale == 2 else ''}.png"
                mac_icon(px, fill).save(iconset / name, optimize=True)
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(dest)], check=True)


# ------------------------------------------------------------------- main


def main() -> None:
    for p in (LIGHT_BACK, DARK_BACK):
        if not p.exists():
            raise SystemExit(f"missing source artwork: {p.relative_to(ROOT)}")

    # Tolerances are summed over R+G+B. 540 removes the light background *and*
    # its soft drop shadow down to ~75 luminance, well above the folder body
    # (~42); the folder's near-black outline encloses the character.
    dark_folder = cut_out(LIGHT_BACK, 540)  # dark folder, light character
    light_folder = cut_out(DARK_BACK, 70)  # light folder, dark character (keeps its dark back panel)

    # Branding masters.
    fit_square(dark_folder, 512, 0.86).save(BRAND / "mori-icon-black.png", optimize=True)
    fit_square(light_folder, 512, 0.86).save(BRAND / "mori-icon-white.png", optimize=True)
    mac_icon(1024, 0.80).save(BRAND / "mori-app-icon.png", optimize=True)

    # In-app marks (shown at 20-72 CSS px, so 160 covers 2x displays).
    fit_square(dark_folder, 160, 0.94).save(PUBLIC / "mori-icon-black.png", optimize=True)
    fit_square(light_folder, 160, 0.94).save(PUBLIC / "mori-icon-white.png", optimize=True)

    # Linux / non-Windows window icon PNGs: free-form folder.
    for name, px in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256), ("icon.png", 512)]:
        small = px <= 32
        fit_square(dark_folder, px, 0.96 if small else 0.90, sharpen=small).save(ICONS / name, optimize=True)

    # Windows: free-form folder, every standard size, tighter crop when tiny.
    frames = [fit_square(dark_folder, px, 0.98 if px <= 48 else 0.92, sharpen=px <= 48) for px in (16, 24, 32, 48, 64, 128, 256)]
    frames[-1].save(ICONS / "icon.ico", format="ICO", sizes=[f.size for f in frames], append_images=frames[:-1])

    build_icns(ICONS / "icon.icns")
    print("icons regenerated")


if __name__ == "__main__":
    main()
