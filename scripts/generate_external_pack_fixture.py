#!/usr/bin/env python3
"""Generate the deterministic PNG art for tests/fixtures/packs/external-v3.

Stdlib only (zlib + struct): no Pillow, no image crate, no network. Every
pixel is either fully opaque or fully transparent for cutout art, so cutout
resampling cannot depend on an alpha ramp. Run from the repository root:

    python3 scripts/generate_external_pack_fixture.py

The manifests (pack.toml) are authored by hand from docs/TEXTURE-PACKS.md;
this script only (re)writes the binary art with stable bytes: fixed rows,
filter type 0, no timestamps, no text chunks.
"""

from __future__ import annotations

import pathlib
import struct
import zlib

SIZE = 16
FIXTURE = pathlib.Path("tests/fixtures/packs/external-v3")


def png(width: int, height: int, rgba: bytes) -> bytes:
    assert len(rgba) == width * height * 4

    def chunk(kind: bytes, payload: bytes) -> bytes:
        body = kind + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)  # filter: None
        raw.extend(rgba[y * stride : (y + 1) * stride])
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def solid(colour: tuple[int, int, int, int]) -> bytes:
    return bytes(colour) * (SIZE * SIZE)


def checker(a: tuple[int, int, int, int], b: tuple[int, int, int, int], step: int = 2) -> bytes:
    out = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            out.extend(a if ((x // step) + (y // step)) % 2 == 0 else b)
    return bytes(out)


def bordered(fill: tuple[int, int, int, int], edge: tuple[int, int, int, int], width: int = 1) -> bytes:
    out = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            on_edge = x < width or y < width or x >= SIZE - width or y >= SIZE - width
            out.extend(edge if on_edge else fill)
    return bytes(out)


def diagonal(bg: tuple[int, int, int, int], line: tuple[int, int, int, int]) -> bytes:
    out = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            out.extend(line if (x + y) % 5 == 0 else bg)
    return bytes(out)


CLEAR = (0, 0, 0, 0)


def write(name: str, pixels: bytes) -> None:
    path = FIXTURE / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(png(SIZE, SIZE, pixels))
    print(f"wrote {path} ({path.stat().st_size} bytes)")


def main() -> None:
    # Six distinct built-in stone faces, plus the bare stem that the direction
    # suffixes override. All hard-edged so the precedence is visible.
    write("stone.png", checker((110, 110, 116, 255), (92, 92, 98, 255), step=4))
    write("stone_top.png", bordered((124, 124, 132, 255), (80, 80, 88, 255)))
    write("stone_bottom.png", bordered((96, 96, 104, 255), (70, 70, 78, 255)))
    write("stone_side.png", solid((104, 104, 112, 255)))
    write("stone_east.png", diagonal((132, 128, 120, 255), (74, 72, 68, 255)))
    write("stone_west.png", diagonal((116, 120, 132, 255), (66, 70, 80, 255)))
    write("stone_south.png", checker((140, 118, 104, 255), (88, 72, 62, 255), step=2))
    write("stone_north.png", checker((104, 132, 120, 255), (62, 88, 78, 255), step=2))
    # A second built-in block that only overrides `_top`; every other face
    # keeps the built-in default.
    write("dirt_top.png", bordered((122, 88, 60, 255), (84, 58, 38, 255), width=2))

    # Pack-defined cross/cutout plant: transparent background, opaque blades.
    reed = bytearray(solid(CLEAR))
    for y in range(2, 14):
        x = 4 + (y % 3)
        reed[(y * SIZE + x) * 4 : (y * SIZE + x) * 4 + 4] = bytes((74, 124, 62, 255))
        x2 = 11 - (y % 2)
        reed[(y * SIZE + x2) * 4 : (y * SIZE + x2) * 4 + 4] = bytes((58, 104, 48, 255))
    write("textures/reed.png", bytes(reed))

    # Pack-defined crop/cutout wheat: transparent background, opaque stalk and
    # hard-edged grain heads.
    wheat = bytearray(solid(CLEAR))
    for y in range(2, 15):
        wheat[(y * SIZE + 8) * 4 : (y * SIZE + 8) * 4 + 4] = bytes((196, 168, 62, 255))
    for y in range(3, 9):
        for x in (6, 7, 9, 10):
            wheat[(y * SIZE + x) * 4 : (y * SIZE + x) * 4 + 4] = bytes((222, 190, 74, 255))
    write("textures/wheat_crop.png", bytes(wheat))

    # Pack-defined cube/blend glass: semi-transparent centre, opaque frame, so
    # the blend and opaque regions are unambiguous.
    glass = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            edge = x == 0 or y == 0 or x == SIZE - 1 or y == SIZE - 1
            glass.extend((176, 208, 220, 255) if edge else (176, 208, 220, 160))
    write("textures/frosted_glass.png", bytes(glass))

    # Built-in icon override: `apple` is one of the documented material names.
    write("icons/apple.png", bordered((196, 62, 54, 255), (132, 34, 30, 255), width=2))


if __name__ == "__main__":
    main()
