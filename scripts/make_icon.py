"""app-icon.png로 Windows용 멀티사이즈 BMP ICO를 만듭니다.

app-icon.png가 없으면 위젯 모티프(갱신 화살표)로 먼저 그립니다.
"""
from __future__ import annotations

import math
import struct
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "app-icon.png"
ICONS = ROOT / "src-tauri" / "icons"
OUT_ICO = ICONS / "icon.ico"
# Standard Windows shell sizes — BMP (not PNG) entries embed reliably via winres.
SIZES = [16, 32, 48, 256]

BRAND = (232, 163, 23, 255)


def draw_app_icon(dest: Path, size: int = 512) -> None:
    """갱신 버튼 모티프 — 가운데 점(내 IP)을 도는 새로고침 화살표."""
    img = Image.new("RGBA", (size, size), BRAND)
    draw = ImageDraw.Draw(img)

    white = (255, 255, 255, 245)
    c = size / 2
    r = size * 0.29
    width = round(size * 0.085)

    # PIL 각도는 3시 방향 0도에서 시계 방향으로 늘어납니다. 오른쪽 위를 비워 화살촉 자리를 둡니다.
    start, end = 0, 300
    draw.arc([c - r, c - r, c + r, c + r], start=start, end=end, fill=white, width=width)

    theta = math.radians(end)
    mid = r - width / 2
    px, py = c + mid * math.cos(theta), c + mid * math.sin(theta)
    tx, ty = -math.sin(theta), math.cos(theta)  # 시계 방향 접선
    nx, ny = math.cos(theta), math.sin(theta)  # 바깥쪽 법선
    head_len = size * 0.15
    head_half = size * 0.11
    draw.polygon(
        [
            (px + tx * head_len, py + ty * head_len),
            (px + nx * head_half, py + ny * head_half),
            (px - nx * head_half, py - ny * head_half),
        ],
        fill=white,
    )

    dot = size * 0.09
    draw.ellipse([c - dot, c - dot, c + dot, c + dot], fill=white)

    img.save(dest)
    print(f"drew {dest}")


def to_bmp_dib(im: Image.Image) -> bytes:
    """Return BITMAPINFOHEADER + BGRA XOR bitmap + empty AND mask (ICO style)."""
    im = im.convert("RGBA")
    w, h = im.size
    pixels = im.split()
    r, g, b, a = [ch.tobytes() for ch in pixels]
    bgra = bytearray(w * h * 4)
    for i in range(w * h):
        bgra[i * 4 + 0] = b[i]
        bgra[i * 4 + 1] = g[i]
        bgra[i * 4 + 2] = r[i]
        bgra[i * 4 + 3] = a[i]

    stride = w * 4
    xor = bytearray(stride * h)
    for row in range(h):
        src_off = row * stride
        dst_off = (h - 1 - row) * stride
        xor[dst_off : dst_off + stride] = bgra[src_off : src_off + stride]

    and_row = ((w + 31) // 32) * 4
    and_mask = bytes(and_row * h)

    header = struct.pack(
        "<IIIHHIIIIII",
        40,
        w,
        h * 2,
        1,
        32,
        0,
        len(xor) + len(and_mask),
        0,
        0,
        0,
        0,
    )
    return header + bytes(xor) + and_mask


def write_ico(frames: list[Image.Image], dest: Path) -> None:
    dibs = [to_bmp_dib(im) for im in frames]
    count = len(frames)
    offset = 6 + 16 * count
    entries = bytearray()
    blob = bytearray()
    for im, dib in zip(frames, dibs):
        w, h = im.size
        entries += struct.pack(
            "<BBBBHHII",
            0 if w >= 256 else w,
            0 if h >= 256 else h,
            0,
            0,
            1,
            32,
            len(dib),
            offset + len(blob),
        )
        blob += dib
    dest.write_bytes(struct.pack("<HHH", 0, 1, count) + entries + blob)


def main() -> None:
    if not SRC.is_file():
        draw_app_icon(SRC)

    ICONS.mkdir(parents=True, exist_ok=True)
    img = Image.open(SRC).convert("RGBA")
    # Opaque tile — Explorer shows this more reliably than a transparent bg.
    bg = Image.new("RGBA", img.size, BRAND)
    composed = Image.alpha_composite(bg, img)

    frames: list[Image.Image] = []
    for s in SIZES:
        im = composed.resize((s, s), Image.Resampling.LANCZOS)
        frames.append(im)
        if s == 32:
            im.save(ICONS / "32x32.png")
        if s == 256:
            # Tauri conf also references these PNGs.
            im.resize((128, 128), Image.Resampling.LANCZOS).save(ICONS / "128x128.png")
            im.save(ICONS / "128x128@2x.png")
            im.save(ICONS / "icon.png")

    write_ico(frames, OUT_ICO)
    print(f"wrote {OUT_ICO} bytes={OUT_ICO.stat().st_size} sizes={SIZES}")


if __name__ == "__main__":
    main()
