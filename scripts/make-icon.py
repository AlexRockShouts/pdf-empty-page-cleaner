#!/usr/bin/env python3
"""Generates assets/AppIcon.png (1024x1024) with only the Python stdlib."""

import math
import struct
import zlib
from pathlib import Path

SIZE = 1024
SS = 2
N = SIZE * SS


def clamp(value, low=0.0, high=1.0):
    return max(low, min(high, value))


def lerp(a, b, t):
    return a + (b - a) * t


def mix(c1, c2, t):
    return tuple(round(lerp(a, b, t)) for a, b in zip(c1, c2))


def in_rounded_rect(x, y, x1, y1, x2, y2, radius):
    if not (x1 <= x <= x2 and y1 <= y <= y2):
        return False
    cx = min(max(x, x1 + radius), x2 - radius)
    cy = min(max(y, y1 + radius), y2 - radius)
    dx = x - cx
    dy = y - cy
    return dx * dx + dy * dy <= radius * radius


def render():
    pixels = bytearray(N * N * 4)
    page_color_top = (252, 253, 255)
    page_color_bottom = (238, 242, 248)
    line_color = (186, 200, 219)
    badge = (34, 197, 94)

    line_rows = [(0.355, 0.615, 0.33), (0.355, 0.615, 0.43),
                 (0.355, 0.615, 0.53), (0.355, 0.615, 0.63),
                 (0.355, 0.545, 0.73)]

    for py in range(N):
        y = (py + 0.5) / N
        for px in range(N):
            x = (px + 0.5) / N
            r = g = b = 0
            a = 0

            if in_rounded_rect(x, y, 0.055, 0.055, 0.945, 0.945, 0.205):
                t = clamp((y - 0.055) / 0.89)
                r, g, b = mix((74, 144, 255), (29, 78, 216), t)
                a = 255

                if in_rounded_rect(x, y, 0.305, 0.175, 0.705, 0.855, 0.03) and not in_rounded_rect(
                    x, y, 0.30, 0.16, 0.70, 0.84, 0.03
                ):
                    r, g, b = (round(r * 0.62), round(g * 0.62), round(b * 0.62))

                if in_rounded_rect(x, y, 0.30, 0.16, 0.70, 0.84, 0.03):
                    t = clamp((y - 0.16) / 0.68)
                    r, g, b = mix(page_color_top, page_color_bottom, t)

                    for x1, x2, yc in line_rows:
                        if in_rounded_rect(x, y, x1, yc - 0.013, x2, yc + 0.013, 0.013):
                            r, g, b = line_color
                            break

                dx = x - 0.735
                dy = y - 0.77
                distance = math.hypot(dx, dy)
                if distance <= 0.152:
                    r, g, b = (15, 23, 42) if distance > 0.143 else badge
                    if abs(dx) <= 0.068 and abs(dy) <= 0.016:
                        r, g, b = (255, 255, 255)

            index = (py * N + px) * 4
            pixels[index] = r
            pixels[index + 1] = g
            pixels[index + 2] = b
            pixels[index + 3] = a

    output = bytearray(SIZE * SIZE * 4)
    for y in range(SIZE):
        for x in range(SIZE):
            r = g = b = a = 0
            for sy in range(SS):
                for sx in range(SS):
                    index = ((y * SS + sy) * N + (x * SS + sx)) * 4
                    r += pixels[index]
                    g += pixels[index + 1]
                    b += pixels[index + 2]
                    a += pixels[index + 3]
            samples = SS * SS
            index = (y * SIZE + x) * 4
            output[index] = r // samples
            output[index + 1] = g // samples
            output[index + 2] = b // samples
            output[index + 3] = a // samples
    return bytes(output)


def write_png(path, width, height, rgba):
    raw = b"".join(
        b"\x00" + rgba[row * width * 4 : (row + 1) * width * 4] for row in range(height)
    )

    def chunk(kind, payload):
        data = struct.pack(">I", len(payload)) + kind + payload
        return data + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    Path(path).write_bytes(png)


if __name__ == "__main__":
    target = Path(__file__).resolve().parent.parent / "assets" / "AppIcon.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    write_png(target, SIZE, SIZE, render())
    print(f"wrote {target}")
